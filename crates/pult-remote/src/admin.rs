//! Админка «Удалённая поддержка» (п. 8): токены регистрации агентов, устройства,
//! политика (запрещённые ПК, обязательное согласие, импорт `policy.json`), пакеты, настройки.
//! Всё — с разрешением `remote.manage` и в аудите.

use crate::{RemoteSettings, devices::safe_file_name, perm, tokens};
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::StatusCode,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser,
    audit::{self, Entry, Source},
    crypto, settings,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Наибольший размер пакета (MSI агента, helper, exe для обновления).
pub const MAX_PACKAGE_BYTES: usize = 64 * 1024 * 1024;

// ------------------------------------------------------------------ Токены регистрации

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct EnrollmentTokenRow {
    pub id: Uuid,
    pub name: String,
    pub prefix: String,
    pub created_by: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub revoked_at: Option<OffsetDateTime>,
    pub uses: i32,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_used_at: Option<OffsetDateTime>,
}

#[utoipa::path(get, path = "/api/admin/remote/tokens", tag = "remote-admin", responses((status = 200, body = Vec<EnrollmentTokenRow>)))]
pub async fn list_enrollment_tokens(
    State(core): State<Core>,
    user: CurrentUser,
) -> ApiResult<Json<Vec<EnrollmentTokenRow>>> {
    user.require(&perm::MANAGE)?;
    Ok(Json(
        sqlx::query_as(
            "SELECT t.id, t.name, t.prefix, coalesce(a.display_name, a.sam_account_name) AS created_by, t.created_at,
                    t.expires_at, t.revoked_at, t.uses, t.last_used_at
               FROM agent_enrollment_tokens t JOIN users u ON u.id = t.created_by
               JOIN ad_accounts a ON a.id = u.ad_account_id
              ORDER BY t.created_at DESC",
        )
        .fetch_all(&core.db)
        .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewEnrollmentToken {
    pub name: String,
    /// Срок, дней (1–365, по умолчанию 30).
    pub days: Option<u32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedEnrollmentToken {
    pub id: Uuid,
    /// Сам токен — показывается один раз.
    pub token: String,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
}

#[utoipa::path(post, path = "/api/admin/remote/tokens", tag = "remote-admin", request_body = NewEnrollmentToken,
    responses((status = 201, body = CreatedEnrollmentToken)))]
pub async fn create_enrollment_token(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<NewEnrollmentToken>,
) -> ApiResult<(StatusCode, Json<CreatedEnrollmentToken>)> {
    user.require(&perm::MANAGE)?;
    let name: String = b.name.trim().chars().filter(|c| !c.is_control()).take(200).collect();
    if name.is_empty() {
        return Err(ApiError::bad_request("invalid_token", "Укажите название токена."));
    }
    let days = b.days.unwrap_or(30);
    if !(1..=365).contains(&days) {
        return Err(ApiError::bad_request("invalid_token", "Срок токена — от 1 до 365 дней."));
    }
    let (token, hash) = tokens::issue(tokens::ENROLL_PREFIX)?;
    let prefix: String = token.chars().take(tokens::ENROLL_PREFIX.len() + 4).collect();
    let expires_at = OffsetDateTime::now_utc() + Duration::days(i64::from(days));
    let mut tx = core.db.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_enrollment_tokens (name, token_hash, prefix, created_by, expires_at)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(&name)
    .bind(hash)
    .bind(&prefix)
    .bind(user.user_id)
    .bind(expires_at)
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("remote.token.created", Source::Portal)
            .object("agent_enrollment_token", id)
            .details(json!({"name": name, "days": days})),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(CreatedEnrollmentToken { id, token, expires_at })))
}

#[utoipa::path(post, path = "/api/admin/remote/tokens/{id}/revoke", tag = "remote-admin", params(("id" = Uuid, Path)),
    responses((status = 204, description = "Отозван")))]
pub async fn revoke_enrollment_token(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require(&perm::MANAGE)?;
    let mut tx = core.db.begin().await?;
    let name: String = sqlx::query_scalar(
        "UPDATE agent_enrollment_tokens SET revoked_at = coalesce(revoked_at, now()) WHERE id = $1 RETURNING name",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("remote.token.revoked", Source::Portal)
            .object("agent_enrollment_token", id)
            .details(json!({"name": name})),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Политика

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct RemotePolicyRow {
    pub id: Uuid,
    pub device_pattern: String,
    /// `deny` | `consent_required`.
    pub rule: String,
    pub note: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[utoipa::path(get, path = "/api/admin/remote/policies", tag = "remote-admin", responses((status = 200, body = Vec<RemotePolicyRow>)))]
pub async fn list_remote_policies(
    State(core): State<Core>,
    user: CurrentUser,
) -> ApiResult<Json<Vec<RemotePolicyRow>>> {
    user.require(&perm::MANAGE)?;
    Ok(Json(
        sqlx::query_as(
            "SELECT id, device_pattern, rule, note, created_at FROM remote_policies ORDER BY rule, lower(device_pattern)",
        )
        .fetch_all(&core.db)
        .await?,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewRemotePolicy {
    /// Имя ПК, FQDN или шаблон со `*`.
    pub device_pattern: String,
    /// `deny` | `consent_required`.
    pub rule: String,
    #[serde(default)]
    pub note: String,
}

/// Шаблон имени ПК: символы имени хоста и `*`.
pub fn valid_pattern(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 253
        && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '*'))
        && p.chars().any(|c| c != '*' && c != '.')
}

async fn insert_policy(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: &CurrentUser,
    pattern: &str,
    rule: &str,
    note: &str,
) -> ApiResult<Option<Uuid>> {
    Ok(sqlx::query_scalar(
        "INSERT INTO remote_policies (device_pattern, rule, note, created_by) VALUES ($1, $2, $3, $4)
         ON CONFLICT ((lower(device_pattern)), rule) DO NOTHING RETURNING id",
    )
    .bind(pattern)
    .bind(rule)
    .bind(note)
    .bind(user.user_id)
    .fetch_optional(&mut **tx)
    .await?)
}

#[utoipa::path(post, path = "/api/admin/remote/policies", tag = "remote-admin", request_body = NewRemotePolicy,
    responses((status = 201, body = RemotePolicyRow), (status = 409, description = "Такое правило уже есть")))]
pub async fn create_remote_policy(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<NewRemotePolicy>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&perm::MANAGE)?;
    let pattern = b.device_pattern.trim().to_string();
    if !valid_pattern(&pattern) {
        return Err(ApiError::bad_request(
            "invalid_policy",
            "Имя ПК: буквы, цифры, дефис, точка; «*» — любые символы.",
        ));
    }
    if crate::policy::Rule::parse(&b.rule).is_none() {
        return Err(ApiError::bad_request("invalid_policy", "Неизвестное правило."));
    }
    let note: String = b.note.trim().chars().filter(|c| !c.is_control()).take(500).collect();
    let mut tx = core.db.begin().await?;
    let id = insert_policy(&mut tx, &user, &pattern, &b.rule, &note)
        .await?
        .ok_or_else(|| ApiError::Conflict { current: json!({"device_pattern": pattern, "rule": b.rule}) })?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("remote.policy.created", Source::Portal)
            .object("remote_policy", id)
            .details(json!({"device_pattern": pattern, "rule": b.rule, "note": note})),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[utoipa::path(delete, path = "/api/admin/remote/policies/{id}", tag = "remote-admin", params(("id" = Uuid, Path)),
    responses((status = 204, description = "Удалено")))]
pub async fn delete_remote_policy(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require(&perm::MANAGE)?;
    let mut tx = core.db.begin().await?;
    let (pattern, rule): (String, String) =
        sqlx::query_as("DELETE FROM remote_policies WHERE id = $1 RETURNING device_pattern, rule")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApiError::NotFound)?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("remote.policy.deleted", Source::Portal)
            .object("remote_policy", id)
            .details(json!({"device_pattern": pattern, "rule": rule})),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `policy.json` RUSSIANANYDESK: нужны только списки ПК; группы и роли переносятся ролями Pult.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "PascalCase")]
pub struct LegacyPolicy {
    #[serde(default)]
    pub consent_required_computers: Vec<String>,
    #[serde(default)]
    pub denied_computers: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PolicyImportResult {
    pub added: usize,
    pub skipped: usize,
    /// Строки, которые не похожи на имя ПК.
    pub invalid: Vec<String>,
}

#[utoipa::path(post, path = "/api/admin/remote/policies/import", tag = "remote-admin", request_body = LegacyPolicy,
    responses((status = 200, body = PolicyImportResult)))]
pub async fn import_remote_policy(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<LegacyPolicy>,
) -> ApiResult<Json<PolicyImportResult>> {
    user.require(&perm::MANAGE)?;
    if b.consent_required_computers.len() + b.denied_computers.len() > 5000 {
        return Err(ApiError::bad_request("invalid_policy", "Слишком много ПК в файле."));
    }
    let mut out = PolicyImportResult { added: 0, skipped: 0, invalid: Vec::new() };
    let mut tx = core.db.begin().await?;
    let note = "Импорт из policy.json";
    for (list, rule) in [(&b.denied_computers, "deny"), (&b.consent_required_computers, "consent_required")] {
        for raw in list {
            let p = raw.trim();
            if !valid_pattern(p) {
                out.invalid.push(p.chars().take(80).collect());
                continue;
            }
            match insert_policy(&mut tx, &user, p, rule, note).await? {
                Some(_) => out.added += 1,
                None => out.skipped += 1,
            }
        }
    }
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("remote.policy.imported", Source::Portal).details(json!({
            "added": out.added, "skipped": out.skipped, "invalid": out.invalid.len(),
        })),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(out))
}

// ------------------------------------------------------------------ Устройства

/// Отключить агент: ключ перестаёт действовать, сессии ПК больше не показываются, а повторная
/// регистрация отклоняется, пока администратор не включит ПК снова.
#[utoipa::path(post, path = "/api/admin/remote/devices/{id}/deactivate", tag = "remote-admin", params(("id" = Uuid, Path)),
    responses((status = 204, description = "Отключено")))]
pub async fn deactivate_device(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require(&perm::MANAGE)?;
    let mut tx = core.db.begin().await?;
    let host: String = sqlx::query_scalar(
        "UPDATE devices SET is_active = false, agent_key_hash = NULL WHERE id = $1 RETURNING hostname",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    sqlx::query("DELETE FROM device_sessions WHERE device_id = $1").bind(id).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("remote.device.deactivated", Source::Portal).object("device", id).details(json!({"hostname": host})),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Включить отключённый ПК: агент сможет зарегистрироваться заново (прежний ключ не действует).
#[utoipa::path(post, path = "/api/admin/remote/devices/{id}/activate", tag = "remote-admin", params(("id" = Uuid, Path)),
    responses((status = 204, description = "Включено")))]
pub async fn activate_device(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require(&perm::MANAGE)?;
    let mut tx = core.db.begin().await?;
    let host: String = sqlx::query_scalar(
        "UPDATE devices SET is_active = true, agent_key_hash = NULL WHERE id = $1 RETURNING hostname",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiError::NotFound)?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("remote.device.activated", Source::Portal).object("device", id).details(json!({"hostname": host})),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Настройки

#[utoipa::path(get, path = "/api/admin/remote/settings", tag = "remote-admin", responses((status = 200, body = RemoteSettings)))]
pub async fn get_remote_settings(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<RemoteSettings>> {
    user.require(&perm::MANAGE)?;
    Ok(Json(settings::load(&core.db).await?))
}

#[utoipa::path(put, path = "/api/admin/remote/settings", tag = "remote-admin", request_body = RemoteSettings,
    responses((status = 200, body = RemoteSettings)))]
pub async fn put_remote_settings(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<RemoteSettings>,
) -> ApiResult<Json<RemoteSettings>> {
    user.require(&perm::MANAGE)?;
    if !(1..=60).contains(&b.online_minutes) || !(30..=600).contains(&b.report_interval_secs) {
        return Err(ApiError::bad_request(
            "invalid_settings",
            "«В сети» — от 1 до 60 минут, интервал отчётов — от 30 до 600 секунд.",
        ));
    }
    if let Some(p) = b.update_package_id {
        let ok: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM agent_packages WHERE id = $1 AND kind = 'agent_update')")
                .bind(p)
                .fetch_one(&core.db)
                .await?;
        if !ok {
            return Err(ApiError::bad_request("invalid_settings", "Пакет обновления агента не найден."));
        }
    }
    let mut tx = core.db.begin().await?;
    let before: RemoteSettings = settings::load(&mut *tx).await?;
    settings::save(&mut *tx, &b, Some(user.user_id)).await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("remote.settings.updated", Source::Portal).details(audit::diff(
            &serde_json::to_value(&before).unwrap_or_default(),
            &serde_json::to_value(&b).unwrap_or_default(),
        )),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(b))
}

// ------------------------------------------------------------------ Пакеты

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct AgentPackageRow {
    pub id: Uuid,
    /// `agent_msi` | `helper` | `agent_update`.
    pub kind: String,
    pub version: String,
    pub file_name: String,
    /// SHA-256, hex.
    pub sha256: String,
    pub size: i64,
    pub signed: bool,
    pub is_current: bool,
    pub uploaded_by: String,
    #[serde(with = "time::serde::rfc3339")]
    pub uploaded_at: OffsetDateTime,
}

#[utoipa::path(get, path = "/api/admin/remote/packages", tag = "remote-admin", responses((status = 200, body = Vec<AgentPackageRow>)))]
pub async fn list_packages(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<AgentPackageRow>>> {
    user.require(&perm::MANAGE)?;
    Ok(Json(
        sqlx::query_as(
            "SELECT p.id, p.kind, p.version, p.file_name, encode(p.sha256, 'hex') AS sha256, p.size,
                    p.signature IS NOT NULL AS signed, p.is_current,
                    coalesce(a.display_name, a.sam_account_name) AS uploaded_by, p.uploaded_at
               FROM agent_packages p JOIN users u ON u.id = p.uploaded_by JOIN ad_accounts a ON a.id = u.ad_account_id
              ORDER BY p.uploaded_at DESC",
        )
        .fetch_all(&core.db)
        .await?,
    ))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct PackageUploadQuery {
    /// `agent_msi` | `helper` | `agent_update`.
    pub kind: String,
    pub version: String,
    pub file_name: String,
    /// Для `agent_update`: подпись Ed25519 (base64) над `pult-agent-update\n{version}\n{sha256}\n`.
    pub signature: Option<String>,
}

/// Загрузить пакет (тело запроса — сам файл). MSI и helper сразу становятся текущими;
/// обновление агента назначается отдельно (настройки → «Обновить агенты»).
#[utoipa::path(post, path = "/api/admin/remote/packages", tag = "remote-admin", params(PackageUploadQuery),
    request_body(content = Vec<u8>, content_type = "application/octet-stream"),
    responses((status = 201, body = AgentPackageRow), (status = 400, description = "Неверный пакет или подпись")))]
pub async fn upload_package(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<PackageUploadQuery>,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&perm::MANAGE)?;
    let bad = |m: &str| ApiError::bad_request("invalid_package", m.to_string());
    if !matches!(q.kind.as_str(), "agent_msi" | "helper" | "agent_update") {
        return Err(bad("Неизвестный вид пакета."));
    }
    let version = q.version.trim();
    if version.is_empty()
        || version.len() > 50
        || !version.chars().all(|c| c.is_ascii_alphanumeric() || ".-+_".contains(c))
    {
        return Err(bad("Версия: буквы, цифры, точка, дефис."));
    }
    if body.is_empty() || body.len() > MAX_PACKAGE_BYTES {
        return Err(bad("Файл пустой или больше 64 МБ."));
    }
    let sha = crypto::sha256(&body);
    let signature = match (q.kind.as_str(), q.signature.as_deref().map(str::trim).filter(|s| !s.is_empty())) {
        ("agent_update", None) => return Err(bad("Обновление агента без подписи не принимается.")),
        (_, Some(s)) => {
            let sig = STANDARD.decode(s).map_err(|_| bad("Подпись — base64."))?;
            if sig.len() != 64 {
                return Err(bad("Подпись Ed25519 — 64 байта."));
            }
            Some(sig)
        }
        (_, None) => None,
    };
    let file_name = safe_file_name(&q.file_name);
    let current = q.kind != "agent_update";
    let mut tx = core.db.begin().await?;
    if current {
        sqlx::query("UPDATE agent_packages SET is_current = false WHERE kind = $1 AND is_current")
            .bind(&q.kind)
            .execute(&mut *tx)
            .await?;
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO agent_packages (kind, version, file_name, sha256, size, signature, data, is_current, uploaded_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(&q.kind)
    .bind(version)
    .bind(&file_name)
    .bind(&sha)
    .bind(body.len() as i64)
    .bind(&signature)
    .bind(body.as_ref())
    .bind(current)
    .bind(user.user_id)
    .fetch_one(&mut *tx)
    .await?;
    let sha_hex: String = sha.iter().map(|b| format!("{b:02x}")).collect();
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("remote.package.uploaded", Source::Portal).object("agent_package", id).details(json!({
            "kind": q.kind, "version": version, "file_name": file_name, "sha256": sha_hex, "size": body.len(),
            "signed": signature.is_some(),
        })),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id, "sha256": sha_hex}))))
}

/// Сделать пакет текущим: MSI/helper — для скачивания; обновление агента — назначить всем агентам.
#[utoipa::path(post, path = "/api/admin/remote/packages/{id}/current", tag = "remote-admin", params(("id" = Uuid, Path)),
    responses((status = 204, description = "Готово")))]
pub async fn make_current(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&perm::MANAGE)?;
    let mut tx = core.db.begin().await?;
    let (kind, version): (String, String) =
        sqlx::query_as("SELECT kind, version FROM agent_packages WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApiError::NotFound)?;
    sqlx::query("UPDATE agent_packages SET is_current = false WHERE kind = $1 AND is_current")
        .bind(&kind)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE agent_packages SET is_current = true WHERE id = $1").bind(id).execute(&mut *tx).await?;
    if kind == "agent_update" {
        let mut cfg: RemoteSettings = settings::load(&mut *tx).await?;
        cfg.update_package_id = Some(id);
        settings::save(&mut *tx, &cfg, Some(user.user_id)).await?;
    }
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("remote.package.current", Source::Portal)
            .object("agent_package", id)
            .details(json!({"kind": kind, "version": version})),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(valid_pattern("Kassa-Pc"));
        assert!(valid_pattern("ws-*"));
        assert!(valid_pattern("*.buh.local"));
        assert!(!valid_pattern("*"), "«все ПК» шаблоном не задаётся");
        assert!(!valid_pattern("*.*"));
        assert!(!valid_pattern("ws 1"));
        assert!(!valid_pattern("ws;rm"));
        assert!(!valid_pattern(""));
    }

    #[test]
    fn legacy_policy_json() {
        let p: LegacyPolicy = serde_json::from_value(json!({
            "Domain": "X", "ConsentRequiredComputers": ["a", "b"], "DeniedComputers": [],
            "FullAccessGroups": ["Domain Admins"], "UserScan": {"RecencyDays": 45}
        }))
        .unwrap();
        assert_eq!(p.consent_required_computers, vec!["a", "b"]);
        assert!(p.denied_computers.is_empty());
    }
}
