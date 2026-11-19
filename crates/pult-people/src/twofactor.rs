//! Второй фактор для входа из интернета: подключение в профиле, отключение, сброс суперадминистратором.
//!
//! Подключить, сменить или отключить второй фактор можно только из сети организации: иначе укравший
//! сеанс снаружи привязал бы к учётке свой телефон.

use crate::auth::LoginParts;
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, request::Parts},
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Secret,
    audit::{self, Entry, Source},
    org::OrgSettings,
    session,
    settings::{self, SecuritySettings},
    totp,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

/// Откуда открыта страница входа.
#[derive(Serialize, ToSchema)]
pub struct AuthContext {
    /// Не из сети организации: код из приложения спросят у тех, кто его подключил, и у ролей, которым он обязателен.
    pub external: bool,
}

/// Нужен ли код при входе с этого адреса.
#[utoipa::path(get, path = "/api/auth/context", tag = "auth", responses((status = 200, body = AuthContext)))]
pub(crate) async fn auth_context(
    State(core): State<Core>,
    LoginParts(parts): LoginParts,
) -> ApiResult<Json<AuthContext>> {
    let sec: SecuritySettings = settings::load(&core.db).await?;
    Ok(Json(AuthContext { external: session::request_is_external(&parts, &sec) }))
}

#[derive(Serialize, ToSchema)]
pub struct TotpStatus {
    /// Второй фактор подключён: из интернета вход — с кодом.
    pub enabled: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub enabled_at: Option<OffsetDateTime>,
    /// Запрос из сети организации: подключать и отключать можно только отсюда.
    pub can_change: bool,
    /// Роль пользователя обязывает входить из интернета с кодом: без подключённого кода снаружи не войти.
    pub required: bool,
}

#[derive(Serialize, ToSchema)]
pub struct TotpSetup {
    /// Секрет в base32 — ввести в приложение вручную.
    pub secret: String,
    /// Ссылка `otpauth://` — открывает приложение на телефоне.
    pub uri: String,
}

#[derive(Deserialize, ToSchema)]
pub struct TotpCode {
    /// 6 цифр из приложения.
    pub code: String,
}

fn browser_only(headers: &HeaderMap) -> ApiResult<()> {
    if session::via_api_token(headers) { Err(ApiError::Forbidden) } else { Ok(()) }
}

async fn inside(core: &Core, parts: &Parts) -> ApiResult<bool> {
    let sec: SecuritySettings = settings::load(&core.db).await?;
    Ok(!session::request_is_external(parts, &sec))
}

async fn require_inside(core: &Core, parts: &Parts) -> ApiResult<()> {
    if inside(core, parts).await? {
        Ok(())
    } else {
        Err(ApiError::Refused {
            code: "outside_network",
            message: "Второй фактор подключают и отключают только из сети организации.".into(),
        })
    }
}

/// Неверных кодов при подключении и отключении за 5 минут, после чего — пауза: украденным сеансом
/// не перебрать код отключения.
const CODE_FAILURES: i64 = 5;

async fn too_many_code_failures(core: &Core, user: &CurrentUser) -> ApiResult<()> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'auth.totp_code_failed' AND object_type = 'user'
            AND object_id = $1 AND ts > now() - interval '5 minutes'",
    )
    .bind(user.user_id.to_string())
    .fetch_one(&core.db)
    .await?;
    if n >= CODE_FAILURES {
        return Err(ApiError::TooManyRequests {
            message: "Слишком много неверных кодов. Попробуйте через 5 минут.".into(),
            retry_after_secs: 300,
        });
    }
    Ok(())
}

async fn code_failed(core: &Core, user: &CurrentUser, during: &str) -> ApiResult<()> {
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("auth.totp_code_failed", Source::Portal)
            .object("user", user.user_id)
            .details(json!({"during": during})),
    )
    .await?;
    Ok(())
}

fn now_step() -> u64 {
    totp::step_at(u64::try_from(OffsetDateTime::now_utc().unix_timestamp()).unwrap_or(0))
}

/// Мой второй фактор.
#[utoipa::path(get, path = "/api/me/totp", tag = "auth", responses((status = 200, body = TotpStatus), (status = 403)))]
pub(crate) async fn get_my_totp(
    State(core): State<Core>,
    user: CurrentUser,
    LoginParts(parts): LoginParts,
) -> ApiResult<Json<TotpStatus>> {
    browser_only(&parts.headers)?;
    let enabled_at: Option<OffsetDateTime> = sqlx::query_scalar("SELECT totp_enabled_at FROM users WHERE id = $1")
        .bind(user.user_id)
        .fetch_one(&core.db)
        .await?;
    let sec: SecuritySettings = settings::load(&core.db).await?;
    let required = sec.totp_required_roles.iter().any(|r| user.grants.roles.contains(r));
    let can_change = !session::request_is_external(&parts, &sec);
    Ok(Json(TotpStatus { enabled: enabled_at.is_some(), enabled_at, can_change, required }))
}

/// Начать подключение: новый секрет. Действует после подтверждения кодом (`/api/me/totp/confirm`).
#[utoipa::path(post, path = "/api/me/totp/setup", tag = "auth",
    responses((status = 200, body = TotpSetup), (status = 400, body = pult_core::error::ErrorBody),
              (status = 403, body = pult_core::error::ErrorBody)))]
pub(crate) async fn setup_my_totp(
    State(core): State<Core>,
    user: CurrentUser,
    LoginParts(parts): LoginParts,
) -> ApiResult<Json<TotpSetup>> {
    browser_only(&parts.headers)?;
    require_inside(&core, &parts).await?;
    let secret = totp::base32_encode(&totp::new_secret().map_err(ApiError::internal)?);
    let blob = core.secrets.encrypt(&Secret::new(secret.clone())).map_err(ApiError::internal)?;
    // Подключённый не перезаписывается: сначала отключить (кодом со старого телефона) или сброс администратором.
    let stored = sqlx::query("UPDATE users SET totp_secret = $2 WHERE id = $1 AND totp_enabled_at IS NULL")
        .bind(user.user_id)
        .bind(blob)
        .execute(&core.db)
        .await?
        .rows_affected();
    if stored == 0 {
        return Err(ApiError::bad_request("totp_enabled", "Второй фактор уже подключён. Сначала отключите его."));
    }
    let org: OrgSettings = settings::load(&core.db).await?;
    let issuer = if org.name.trim().is_empty() { "Pult".to_string() } else { org.name.trim().to_string() };
    let uri = totp::otpauth_uri(&issuer, &user.login, &secret);
    Ok(Json(TotpSetup { secret, uri }))
}

/// Подтвердить подключение кодом из приложения.
#[utoipa::path(post, path = "/api/me/totp/confirm", tag = "auth", request_body = TotpCode,
    responses((status = 204), (status = 400, body = pult_core::error::ErrorBody), (status = 403, body = pult_core::error::ErrorBody)))]
pub(crate) async fn confirm_my_totp(
    State(core): State<Core>,
    user: CurrentUser,
    LoginParts(parts): LoginParts,
    Json(body): Json<TotpCode>,
) -> ApiResult<StatusCode> {
    browser_only(&parts.headers)?;
    require_inside(&core, &parts).await?;
    too_many_code_failures(&core, &user).await?;
    let mut tx = core.db.begin().await?;
    let row: (Option<Vec<u8>>, Option<OffsetDateTime>) =
        sqlx::query_as("SELECT totp_secret, totp_enabled_at FROM users WHERE id = $1 FOR UPDATE")
            .bind(user.user_id)
            .fetch_one(&mut *tx)
            .await?;
    let blob = match row {
        (_, Some(_)) => return Err(ApiError::bad_request("totp_enabled", "Второй фактор уже подключён.")),
        (None, None) => return Err(ApiError::bad_request("totp_not_started", "Сначала нажмите «Подключить».")),
        (Some(b), None) => b,
    };
    let secret = core.secrets.decrypt(&blob).map_err(ApiError::internal)?;
    let key = totp::base32_decode(secret.expose()).ok_or_else(|| ApiError::internal("секрет второго фактора"))?;
    let Some(step) = totp::verify(&key, &body.code, now_step(), 0) else {
        drop(tx);
        code_failed(&core, &user, "confirm").await?;
        return Err(ApiError::bad_request(
            "invalid_otp",
            "Код не подошёл. Проверьте время на телефоне и введите новый код.",
        ));
    };
    sqlx::query("UPDATE users SET totp_enabled_at = now(), totp_last_step = $2 WHERE id = $1")
        .bind(user.user_id)
        .bind(i64::try_from(step).unwrap_or(i64::MAX))
        .execute(&mut *tx)
        .await?;
    audit::record(&mut *tx, &user.actor, Entry::new("auth.totp_enabled", Source::Portal).object("user", user.user_id))
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Отключить свой второй фактор (нужен текущий код). Вход из интернета закрывается.
#[utoipa::path(post, path = "/api/me/totp/disable", tag = "auth", request_body = TotpCode,
    responses((status = 204), (status = 400, body = pult_core::error::ErrorBody), (status = 403, body = pult_core::error::ErrorBody)))]
pub(crate) async fn disable_my_totp(
    State(core): State<Core>,
    user: CurrentUser,
    LoginParts(parts): LoginParts,
    Json(body): Json<TotpCode>,
) -> ApiResult<StatusCode> {
    browser_only(&parts.headers)?;
    require_inside(&core, &parts).await?;
    too_many_code_failures(&core, &user).await?;
    let mut tx = core.db.begin().await?;
    let row: (Option<Vec<u8>>, Option<OffsetDateTime>, i64) =
        sqlx::query_as("SELECT totp_secret, totp_enabled_at, totp_last_step FROM users WHERE id = $1 FOR UPDATE")
            .bind(user.user_id)
            .fetch_one(&mut *tx)
            .await?;
    let (Some(blob), Some(_), last) = row else {
        return Err(ApiError::bad_request("totp_not_enabled", "Второй фактор не подключён."));
    };
    let secret = core.secrets.decrypt(&blob).map_err(ApiError::internal)?;
    let key = totp::base32_decode(secret.expose()).ok_or_else(|| ApiError::internal("секрет второго фактора"))?;
    if totp::verify(&key, &body.code, now_step(), u64::try_from(last).unwrap_or(0)).is_none() {
        drop(tx);
        code_failed(&core, &user, "disable").await?;
        return Err(ApiError::bad_request("invalid_otp", "Код не подошёл. Введите новый код из приложения."));
    }
    clear(&mut tx, user.user_id).await?;
    audit::record(&mut *tx, &user.actor, Entry::new("auth.totp_disabled", Source::Portal).object("user", user.user_id))
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn clear(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, user_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET totp_secret = NULL, totp_enabled_at = NULL, totp_last_step = 0 WHERE id = $1")
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Сбросить второй фактор пользователя (потерян телефон). Только суперадминистратор; все сеансы
/// пользователя закрываются — открытый на потерянном телефоне тоже.
#[utoipa::path(delete, path = "/api/admin/users/{id}/totp", tag = "admin", params(("id" = Uuid, Path)),
    responses((status = 204), (status = 403), (status = 404)))]
pub(crate) async fn reset_user_totp(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    if !user.grants.is_superadmin() {
        return Err(ApiError::Forbidden);
    }
    let mut tx = core.db.begin().await?;
    let had: Option<bool> =
        sqlx::query_scalar("SELECT totp_enabled_at IS NOT NULL FROM users WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(had) = had else { return Err(ApiError::NotFound) };
    clear(&mut tx, id).await?;
    let closed =
        sqlx::query("DELETE FROM sessions WHERE user_id = $1").bind(id).execute(&mut *tx).await?.rows_affected();
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("auth.totp_reset", Source::Portal)
            .object("user", id)
            .details(json!({"was_enabled": had, "sessions_closed": closed})),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
