//! Вход паролем AD, выход, текущий пользователь.

use crate::{PeopleState, domain::AdHandle, sync};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use pult_ad::{AdError, BindFailure};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Secret,
    audit::{self, Actor, Entry, Source},
    rbac::ROLE_SUPERADMIN,
    session,
    settings::{self, SecuritySettings},
    totp,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Deserialize, ToSchema)]
pub struct LoginRequest {
    /// `ivanov`, `DOMAIN\ivanov` или `ivanov@domain`.
    pub login: String,
    #[schema(value_type = String, format = Password)]
    pub password: Secret,
    /// Код из приложения-аутентификатора: нужен только при входе из интернета (`/api/auth/context`).
    #[serde(default)]
    pub code: Option<String>,
}

/// Сколько неудачных попыток из интернета за сутки на один логин, после чего вход снаружи закрыт до
/// следующих суток. Короткое окно (5 за 5 минут) за месяц перебора дало бы угадать код с заметной
/// вероятностью; 20 в сутки — нет. Изнутри сети вход при этом работает.
pub const EXTERNAL_FAILURES_PER_DAY: i64 = 20;

/// Логины, вход которых снаружи проверяется прямо сейчас. Счётчик неудач читается до проверки, а пишется
/// после: без этого пачка одновременных запросов прошла бы лимит целиком (сотни кодов вместо 20 в сутки).
static EXTERNAL_IN_FLIGHT: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(Default::default);

/// Право проверить вход снаружи для логина; освобождается в конце запроса, в том числе при ошибке.
struct ExternalAttempt(String);

impl ExternalAttempt {
    fn start(login: &str) -> Option<Self> {
        let mut busy = EXTERNAL_IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        busy.insert(login.to_string()).then(|| Self(login.to_string()))
    }
}

impl Drop for ExternalAttempt {
    fn drop(&mut self) {
        EXTERNAL_IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0);
    }
}

/// Секрет-заглушка: учётка без второго фактора проверяется так же долго, как с ним, —
/// по времени ответа не узнать, кто его подключил.
const DUMMY_TOTP_KEY: [u8; totp::SECRET_LEN] = [0x5a; totp::SECRET_LEN];

/// Текущий пользователь портала.
#[derive(Serialize, ToSchema)]
pub struct Me {
    pub user_id: Uuid,
    pub person_id: Option<Uuid>,
    pub login: String,
    pub display_name: String,
    pub locale: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub is_superadmin: bool,
    /// Личные предпочтения интерфейса (тема, формат даты…).
    pub prefs: crate::profile::UserPrefs,
}

impl Me {
    fn new(u: &CurrentUser, prefs: crate::profile::UserPrefs) -> Self {
        Self {
            user_id: u.user_id,
            person_id: u.person_id,
            login: u.login.clone(),
            display_name: u.display_name.clone(),
            locale: u.locale.clone(),
            roles: u.grants.roles.iter().cloned().collect(),
            permissions: u.grants.permissions.iter().cloned().collect(),
            is_superadmin: u.grants.is_superadmin(),
            prefs,
        }
    }
}

/// Код отказа для фронтенда (текст — из `BindFailure::user_message`).
fn failure_code(f: BindFailure) -> &'static str {
    match f {
        BindFailure::InvalidCredentials | BindFailure::Other => "invalid_credentials",
        BindFailure::LogonHours => "logon_hours",
        BindFailure::Workstation => "workstation",
        BindFailure::PasswordExpired | BindFailure::MustChangePassword => "password_expired",
        BindFailure::AccountDisabled => "account_disabled",
        BindFailure::AccountExpired => "account_expired",
        BindFailure::AccountLocked => "account_locked",
    }
}

async fn attempt(core: &Core, login: &str, actor: &Actor, result: &str, external: bool) {
    let r = sqlx::query("INSERT INTO login_attempts (login, ip, result, external) VALUES ($1, $2, $3, $4)")
        .bind(login.chars().take(200).collect::<String>())
        .bind(actor.ip.map(sqlx::types::ipnetwork::IpNetwork::from))
        .bind(result)
        .bind(external)
        .execute(&core.db)
        .await;
    if let Err(e) = r {
        tracing::error!(error = %e, "не удалось записать попытку входа");
    }
}

/// Сколько неудачных попыток за окно: по логину и по IP. Попытки из интернета и изнутри считаются
/// раздельно: перебор снаружи не закрывает вход сотруднику в офисе.
async fn failures(
    core: &Core,
    login: &str,
    actor: &Actor,
    window_min: u32,
    external: bool,
) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as(
        "SELECT count(*) FILTER (WHERE lower(login) = lower($1)),
                count(*) FILTER (WHERE $2::inet IS NOT NULL AND ip = $2)
           FROM login_attempts
          WHERE ts > now() - make_interval(mins => $3) AND result NOT IN ('ok', 'throttled') AND external = $4",
    )
    .bind(login)
    .bind(actor.ip.map(sqlx::types::ipnetwork::IpNetwork::from))
    .bind(i32::try_from(window_min).unwrap_or(5))
    .bind(external)
    .fetch_one(&core.db)
    .await
}

/// Неудачных попыток из интернета на логин за сутки.
async fn external_failures_today(core: &Core, login: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM login_attempts
          WHERE external AND lower(login) = lower($1) AND ts > now() - interval '1 day'
            AND result NOT IN ('ok', 'throttled')",
    )
    .bind(login)
    .fetch_one(&core.db)
    .await
}

/// Второй фактор: код из приложения сверяется до обращения к AD — у тех, кто его подключил, подбор пароля
/// из интернета не доходит до домена и не блокирует учётку. Шаг кода занимается атомарно: один код — один вход.
/// Возвращает пользователя, чей код подошёл.
async fn second_factor(core: &Core, login: &str, code: &str) -> Result<Option<Uuid>, ApiError> {
    let rows: Vec<(Uuid, Vec<u8>, i64)> = sqlx::query_as(
        "SELECT u.id, u.totp_secret, u.totp_last_step
           FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id
          WHERE lower(a.sam_account_name) = $1 AND NOT a.is_missing
            AND u.totp_enabled_at IS NOT NULL AND u.totp_secret IS NOT NULL",
    )
    .bind(login)
    .fetch_all(&core.db)
    .await?;
    let now = totp::step_at(u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp()).unwrap_or(0));
    if rows.is_empty() {
        let _ = totp::verify(&DUMMY_TOTP_KEY, code, now, u64::MAX);
    }
    for (id, blob, last) in rows {
        let secret = match core.secrets.decrypt(&blob) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(user = %id, error = %e, "секрет второго фактора не расшифровывается");
                continue;
            }
        };
        let Some(key) = totp::base32_decode(secret.expose()) else { continue };
        let Some(step) = totp::verify(&key, code, now, u64::try_from(last).unwrap_or(0)) else { continue };
        let taken = sqlx::query("UPDATE users SET totp_last_step = $2 WHERE id = $1 AND totp_last_step < $2")
            .bind(id)
            .bind(i64::try_from(step).unwrap_or(i64::MAX))
            .execute(&core.db)
            .await?
            .rows_affected()
            == 1;
        if taken {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Нужен ли этому логину код из приложения при входе из интернета: подключил сам или роль обязывает.
/// Неизвестный логин — нет: о нём скажет AD.
async fn code_needed(core: &Core, login: &str, required_roles: &[String]) -> Result<bool, ApiError> {
    let needed: Option<bool> = sqlx::query_scalar(
        "SELECT bool_or(u.totp_enabled_at IS NOT NULL OR EXISTS (
                    SELECT 1 FROM user_roles ur JOIN roles r ON r.id = ur.role_id
                     WHERE ur.user_id = u.id AND r.code = ANY($2)))
           FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id
          WHERE lower(a.sam_account_name) = $1 AND NOT a.is_missing",
    )
    .bind(login)
    .bind(required_roles)
    .fetch_one(&core.db)
    .await?;
    Ok(needed.unwrap_or(false))
}

/// То же для уже известного пользователя — после проверки пароля в AD. Страховка на случай, когда логин
/// до AD не удалось сопоставить с учёткой (например, UPN, которого ещё нет в зеркале).
async fn user_needs_code(
    db: &mut sqlx::PgConnection,
    user_id: Uuid,
    required_roles: &[String],
) -> Result<bool, ApiError> {
    Ok(sqlx::query_scalar(
        "SELECT u.totp_enabled_at IS NOT NULL OR EXISTS (
                SELECT 1 FROM user_roles ur JOIN roles r ON r.id = ur.role_id
                 WHERE ur.user_id = u.id AND r.code = ANY($2))
           FROM users u WHERE u.id = $1",
    )
    .bind(user_id)
    .bind(required_roles)
    .fetch_one(db)
    .await?)
}

/// Ключ логина для лимитов и второго фактора. UPN (`ivan.ivanov@corp…`) сопоставляется с sAMAccountName
/// по зеркалу AD: начало UPN не обязано совпадать с логином, и без этого вход по UPN обходил бы код
/// и получал бы свой отдельный лимит попыток.
async fn resolve_key(core: &Core, login: &str) -> Result<String, ApiError> {
    let l = login.trim();
    if l.contains('@') && !l.contains('\\') {
        let sams: Vec<String> = sqlx::query_scalar(
            "SELECT lower(sam_account_name) FROM ad_accounts WHERE lower(upn) = lower($1) AND NOT is_missing LIMIT 2",
        )
        .bind(l)
        .fetch_all(&core.db)
        .await?;
        if let [sam] = sams.as_slice() {
            return Ok(sam.clone());
        }
    }
    Ok(login_key(l))
}

/// Логин без домена в нижнем регистре: так он хранится в попытках и сравнивается с первым администратором.
/// `CORP\ivanov`, `ivanov@corp…` и `IVANOV` — один ключ, иначе лимит попыток на логин обходился бы
/// сменой записи.
pub fn login_key(login: &str) -> String {
    let l = login.trim();
    let l = l.rsplit_once('\\').map(|(_, u)| u).unwrap_or(l);
    let l = l.split_once('@').map(|(u, _)| u).unwrap_or(l);
    l.to_lowercase()
}

pub async fn login(
    core: &Core,
    ad: &AdHandle,
    initial_superadmin: Option<&str>,
    parts: &Parts,
    req: LoginRequest,
) -> Result<Response, ApiError> {
    let actor = session::actor_from(parts, None);
    let short = login_key(&req.login);
    if short.is_empty() || short.len() > 256 {
        return Err(ApiError::bad_request("invalid_credentials", "Укажите логин и пароль."));
    }
    let key = resolve_key(core, &req.login).await?;
    let sec: SecuritySettings = settings::load(&core.db).await?;
    let external = session::request_is_external(parts, &sec);
    let _in_flight = if external {
        let Some(slot) = ExternalAttempt::start(&key) else {
            return Err(ApiError::TooManyRequests {
                message: "Вход уже проверяется. Повторите через несколько секунд.".into(),
                retry_after_secs: 5,
            });
        };
        Some(slot)
    } else {
        None
    };
    let (by_login, by_ip) = failures(core, &key, &actor, sec.login_window_minutes, external).await?;
    if by_login >= i64::from(sec.login_max_failures_per_login) || by_ip >= i64::from(sec.login_max_failures_per_ip) {
        attempt(core, &key, &actor, "throttled", external).await;
        return Err(ApiError::TooManyRequests {
            message: format!("Слишком много неудачных попыток. Попробуйте через {} мин.", sec.login_window_minutes),
            retry_after_secs: u64::from(sec.login_window_minutes) * 60,
        });
    }
    if external && external_failures_today(core, &key).await? >= EXTERNAL_FAILURES_PER_DAY {
        attempt(core, &key, &actor, "throttled", true).await;
        return Err(ApiError::TooManyRequests {
            message: "Вход из интернета для этой учётной записи закрыт на сутки: слишком много неудачных попыток. Из сети организации вход работает.".into(),
            retry_after_secs: 3600,
        });
    }
    // Из интернета у тех, кому нужен код, — сначала код из приложения, и только потом пароль в AD.
    let verified_user = if external && code_needed(core, &key, &sec.totp_required_roles).await? {
        let Some(code) = req.code.as_deref().filter(|c| !c.trim().is_empty()) else {
            return Err(ApiError::bad_request("otp_required", "Введите код из приложения-аутентификатора."));
        };
        match second_factor(core, &key, code).await? {
            Some(id) => Some(id),
            None => {
                attempt(core, &key, &actor, "invalid_otp", true).await;
                audit::record(
                    &core.db,
                    &actor,
                    Entry::new("auth.login_failed", Source::Portal)
                        .details(json!({"login": key, "reason": "invalid_otp", "external": true})),
                )
                .await?;
                // Одинаково для неверного кода и для роли, обязанной подключить код, но не подключившей.
                return Err(ApiError::bad_request(
                    "invalid_otp",
                    "Неверный код. Если вход из интернета ещё не подключён, подключите его в профиле из сети организации.",
                ));
            }
        }
    } else {
        None
    };
    let Some((domain, pool)) = ad.pool(core).await? else {
        return Err(ApiError::Unavailable {
            code: "ad_not_configured",
            message: "Вход через AD ещё не настроен.".into(),
        });
    };
    let authed = match pool.authenticate(&req.login, &req.password).await {
        Ok(u) => u,
        Err(AdError::Bind(f)) => {
            attempt(core, &key, &actor, failure_code(f), external).await;
            audit::record(
                &core.db,
                &actor,
                Entry::new("auth.login_failed", Source::Portal)
                    .details(json!({"login": key, "reason": failure_code(f), "external": external})),
            )
            .await?;
            return Err(ApiError::BadRequest { code: failure_code(f), message: f.user_message().into() });
        }
        Err(AdError::Unavailable(r)) => {
            tracing::warn!(report = %r, "вход невозможен: контроллеры домена не отвечают");
            attempt(core, &key, &actor, "ad_unavailable", external).await;
            return Err(ApiError::Unavailable {
                code: "ad_unavailable",
                message: "Нет связи с доменом. Попробуйте позже.".into(),
            });
        }
        Err(e) => {
            attempt(core, &key, &actor, "error", external).await;
            return Err(ApiError::internal(e));
        }
    };
    // Пароль больше не нужен — `Secret` затрёт его при выходе из области видимости.
    drop(req.password);

    let mut tx = core.db.begin().await?;
    // Учётка уже в зеркале? Если нет (новый сотрудник между синхронизациями) — точечная синхронизация.
    let guid = authed.object_guid.as_deref().and_then(|g| Uuid::parse_str(g).ok());
    let mut account: Option<(Uuid, bool)> = match guid {
        Some(g) => {
            sqlx::query_as("SELECT id, is_enabled AND NOT is_missing FROM ad_accounts WHERE object_guid = $1")
                .bind(g)
                .fetch_optional(&mut *tx)
                .await?
        }
        None => None,
    };
    if account.is_none() {
        match pool.account_by_dn(&authed.dn).await {
            Ok(Some(acc)) => {
                let (id, _, _) = sync::upsert(&mut tx, domain.id, &acc).await?;
                let link: sync::LinkSettings = settings::load(&mut *tx).await?;
                sync::link_account(&mut tx, id, &link).await?;
                account = Some((id, acc.is_enabled));
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "точечная синхронизация при входе не удалась"),
        }
    }
    let Some((account_id, enabled)) = account else {
        attempt(core, &key, &actor, "not_allowed", external).await;
        return Err(ApiError::Forbidden);
    };
    if !enabled {
        attempt(core, &key, &actor, "account_disabled", external).await;
        return Err(ApiError::BadRequest {
            code: "account_disabled",
            message: BindFailure::AccountDisabled.user_message().into(),
        });
    }
    let (user_id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO users (ad_account_id, person_id, last_login_at, locale)
         VALUES ($1, (SELECT id FROM persons WHERE ad_account_id = $1), now(),
                 coalesce((SELECT value->>'default_locale' FROM settings WHERE key = 'locale'), 'ru'))
         ON CONFLICT (ad_account_id) DO UPDATE SET is_active = true, last_login_at = now(),
                person_id = coalesce(users.person_id, EXCLUDED.person_id)
         RETURNING id",
    )
    .bind(account_id)
    .fetch_one(&mut *tx)
    .await?;
    let actor = Actor { user_id: Some(user_id), ..actor };
    // Логин до AD не сопоставился с учёткой, которой нужен код (UPN не из зеркала и т. п.): без кода не пускаем.
    if external && verified_user.is_none() && user_needs_code(&mut tx, user_id, &sec.totp_required_roles).await? {
        attempt(core, &key, &actor, "invalid_otp", true).await;
        return Err(ApiError::bad_request("otp_required", "Введите код из приложения-аутентификатора."));
    }
    // Код подошёл к одной учётке, а пароль — к другой (одинаковый логин в разных доменах): не пускаем.
    if verified_user.is_some_and(|v| v != user_id) {
        attempt(core, &key, &actor, "not_allowed", external).await;
        return Err(ApiError::Forbidden);
    }
    if let Some(wanted) = initial_superadmin
        && login_key(wanted) == login_key(&authed.sam_account_name)
    {
        grant_initial_superadmin(&mut tx, user_id, &actor).await?;
    }
    audit::record(
        &mut *tx,
        &actor,
        Entry::new("auth.login", Source::Portal)
            .object("user", user_id)
            .details(json!({"dc": authed.controller, "external": external})),
    )
    .await?;
    tx.commit().await?;
    attempt(core, &key, &actor, "ok", external).await;

    let s = session::create(core, user_id, &actor).await?;
    let mut headers = HeaderMap::new();
    for c in core.cookies.issue(&s.token, &s.csrf, s.max_age_secs) {
        headers.append(header::SET_COOKIE, HeaderValue::from_str(&c).map_err(ApiError::internal)?);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok((StatusCode::NO_CONTENT, headers).into_response())
}

/// Первый суперадминистратор: только пока суперадминистраторов нет.
async fn grant_initial_superadmin(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    actor: &Actor,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(7231003)").execute(&mut **tx).await?;
    let any: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM user_roles ur JOIN roles r ON r.id = ur.role_id WHERE r.code = $1)",
    )
    .bind(ROLE_SUPERADMIN)
    .fetch_one(&mut **tx)
    .await?;
    if any {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO user_roles (user_id, role_id) SELECT $1, id FROM roles WHERE code = $2 ON CONFLICT DO NOTHING",
    )
    .bind(user_id)
    .bind(ROLE_SUPERADMIN)
    .execute(&mut **tx)
    .await?;
    audit::record(
        &mut **tx,
        actor,
        Entry::new("user.roles", Source::Portal)
            .object("user", user_id)
            .details(json!({"after": [ROLE_SUPERADMIN], "initial": true})),
    )
    .await?;
    tracing::warn!(user = %user_id, "назначен первый суперадминистратор (PULT_INITIAL_SUPERADMIN)");
    Ok(())
}

// ------------------------------------------------------------------ HTTP

pub(crate) struct LoginParts(pub Parts);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for LoginParts {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(parts.clone()))
    }
}

/// Вход паролем AD. Успех — 204 и cookie сеанса; отказ — 401 с понятной причиной.
#[utoipa::path(post, path = "/api/auth/login", tag = "auth", request_body = LoginRequest,
    responses((status = 204, description = "Вошёл: cookie сеанса и CSRF"),
              (status = 401, body = pult_core::error::ErrorBody),
              (status = 429, body = pult_core::error::ErrorBody),
              (status = 503, body = pult_core::error::ErrorBody)))]
pub(crate) async fn login_handler(
    State(core): State<Core>,
    State(people): State<PeopleState>,
    LoginParts(parts): LoginParts,
    Json(req): Json<LoginRequest>,
) -> Response {
    match login(&core, &people.ad, people.initial_superadmin.as_deref(), &parts, req).await {
        Ok(r) => r,
        Err(e) => login_error(e),
    }
}

/// Отказ во входе. Отказ AD (неверный пароль, отключена и т. п.) — 401 с кодом и текстом
/// на языке запроса: тело собирается здесь, мимо `ApiError::into_response`, где переводятся остальные ошибки.
fn login_error(e: ApiError) -> Response {
    match e {
        ApiError::BadRequest { code, message } => {
            let message = pult_core::l10n::tr(&message).into_owned();
            let body = pult_core::error::ErrorBody { error: code, message, details: None };
            (StatusCode::UNAUTHORIZED, Json(body)).into_response()
        }
        e => e.into_response(),
    }
}

/// Выход: закрыть сеанс и стереть cookie.
#[utoipa::path(post, path = "/api/auth/logout", tag = "auth", responses((status = 204)))]
pub(crate) async fn logout_handler(
    State(core): State<Core>,
    user: CurrentUser,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if let Some(t) = session::cookie(&headers, core.cookies.session_name()) {
        session::destroy(&core, t).await?;
    }
    audit::record(&core.db, &user.actor, Entry::new("auth.logout", Source::Portal).object("user", user.user_id))
        .await?;
    let mut h = HeaderMap::new();
    for c in core.cookies.clear() {
        h.append(header::SET_COOKIE, HeaderValue::from_str(&c).map_err(ApiError::internal)?);
    }
    Ok((StatusCode::NO_CONTENT, h).into_response())
}

/// Кто я: роли и разрешения (фронтенд прячет недоступное; проверяет всё равно сервер).
#[utoipa::path(get, path = "/api/me", tag = "auth", responses((status = 200, body = Me), (status = 401)))]
pub(crate) async fn me_handler(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Me>> {
    let prefs = crate::profile::load_prefs(&core.db, user.user_id).await?;
    Ok(Json(Me::new(&user, prefs)))
}

#[derive(Deserialize, Serialize, ToSchema)]
pub struct UserSettingsBody {
    /// Колонки справочника, фильтры и прочие личные настройки интерфейса.
    #[schema(value_type = Object)]
    pub settings: serde_json::Value,
}

/// Личные настройки интерфейса.
#[utoipa::path(get, path = "/api/me/settings", tag = "auth", responses((status = 200, body = UserSettingsBody)))]
pub(crate) async fn get_my_settings(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<UserSettingsBody>> {
    let settings: serde_json::Value =
        sqlx::query_scalar("SELECT settings FROM users WHERE id = $1").bind(user.user_id).fetch_one(&core.db).await?;
    Ok(Json(UserSettingsBody { settings }))
}

#[utoipa::path(put, path = "/api/me/settings", tag = "auth", request_body = UserSettingsBody, responses((status = 204)))]
pub(crate) async fn put_my_settings(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<UserSettingsBody>,
) -> ApiResult<StatusCode> {
    if !b.settings.is_object() || b.settings.to_string().len() > 32 * 1024 {
        return Err(ApiError::bad_request("invalid_settings", "Настройки — объект JSON до 32 КБ."));
    }
    // Предпочтения (`prefs`) меняются только через `/api/me/prefs` — здесь их не затереть.
    sqlx::query(
        "UPDATE users SET settings = CASE WHEN settings ? 'prefs'
                    THEN ($2::jsonb - 'prefs') || jsonb_build_object('prefs', settings->'prefs')
                    ELSE $2::jsonb - 'prefs' END
          WHERE id = $1",
    )
    .bind(user.user_id)
    .bind(&b.settings)
    .execute(&core.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct LocaleBody {
    /// `ru`, `en` или `zh`.
    pub locale: String,
}

/// Язык интерфейса пользователя: на нём приходят письма.
#[utoipa::path(put, path = "/api/me/locale", tag = "auth", request_body = LocaleBody, responses((status = 204)))]
pub(crate) async fn put_my_locale(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<LocaleBody>,
) -> ApiResult<StatusCode> {
    if !pult_core::l10n::Lang::is_code(&b.locale) {
        return Err(ApiError::bad_request("invalid_locale", "Язык — ru, en или zh."));
    }
    sqlx::query("UPDATE users SET locale = $2 WHERE id = $1")
        .bind(user.user_id)
        .bind(&b.locale)
        .execute(&core.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_external_check_per_login_at_a_time() {
        // Пачка одновременных запросов снаружи не обходит счётчик неудач: второй ждёт первого.
        let first = ExternalAttempt::start("ivanov-ext-test").unwrap();
        assert!(ExternalAttempt::start("ivanov-ext-test").is_none());
        assert!(ExternalAttempt::start("petrova-ext-test").is_some(), "другие логины не ждут");
        drop(first);
        assert!(ExternalAttempt::start("ivanov-ext-test").is_some(), "место освобождается");
    }

    #[tokio::test]
    async fn login_errors_follow_request_language() {
        use pult_core::l10n::{Lang, with_lang};
        let body = |r: Response| async move {
            let b = axum::body::to_bytes(r.into_body(), 4096).await.unwrap();
            serde_json::from_slice::<serde_json::Value>(&b).unwrap()["message"].as_str().unwrap().to_string()
        };
        let empty = || ApiError::bad_request("invalid_credentials", "Укажите логин и пароль.");
        let wrong = || ApiError::BadRequest {
            code: "invalid_credentials",
            message: pult_ad::BindFailure::InvalidCredentials.user_message().into(),
        };
        let r = with_lang(Lang::En, async { login_error(empty()) }).await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body(r).await, "Enter your login and password.");
        let en = body(with_lang(Lang::En, async { login_error(wrong()) }).await).await;
        assert!(!en.chars().any(pult_core::l10n::is_cyrillic), "отказ AD — тоже на английском: {en}");
        assert_eq!(body(login_error(empty())).await, "Укажите логин и пароль.", "без языка — русский");
    }

    #[test]
    fn login_keys() {
        assert_eq!(login_key("CORP\\Ivanov"), "ivanov");
        assert_eq!(login_key(" Ivanov@corp.example.local "), "ivanov");
        assert_eq!(login_key("ivanov"), "ivanov");
    }

    #[test]
    fn password_not_in_debug() {
        let r: LoginRequest = serde_json::from_str(r#"{"login":"ivanov","password":"Sup3r-Secret!"}"#).unwrap();
        assert!(!format!("{:?}", r.password).contains("Sup3r"));
    }
}
