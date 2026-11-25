//! Расширения (ADR-0005): сторонний сервис описывается манифестом `pult-extension.json`. При установке
//! суперадминистратор видит, какие права просит расширение. Портал выпускает расширению токен API с этими
//! правами (только чтение, как у любого токена), создаёт подписанный вебхук на события заявок и показывает
//! ссылки расширения в меню тем, у кого есть нужное право. Код расширения внутри портала не выполняется.
//!
//! Установить можно из файла манифеста, по его адресу (https) или из каталога: каталог — это адрес JSON-списка
//! манифестов, который задаёт суперадминистратор. Отвечает за выбор расширений организация.

use crate::integrations::{issue_token, url_ok};
use axum::{
    Json,
    extract::{FromRef, Path, State},
    http::StatusCode,
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Secret,
    audit::{self, Entry, Source},
    crypto::random_token,
    settings::{self, SettingsGroup},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use time::OffsetDateTime;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Версия формата манифеста, которую понимает портал.
pub const MANIFEST_VERSION: u32 = 1;
/// Манифест или каталог — не больше 1 МБ.
const MAX_BYTES: usize = 1024 * 1024;

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(list_extensions, install_extension))
        .routes(routes!(preview_extension))
        .routes(routes!(set_extension, remove_extension))
        .routes(routes!(get_ext_settings, put_ext_settings))
        .routes(routes!(extension_catalog))
        .routes(routes!(extension_links))
}

// ------------------------------------------------------------------ Манифест

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ManifestWebhook {
    /// Куда портал отправляет события (https).
    pub url: String,
    /// Виды событий заявки (`created`, `status`, `assigned`…); пусто — все.
    #[serde(default)]
    pub events: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ManifestLink {
    /// Подпись пункта меню.
    pub title: String,
    /// Адрес страницы расширения (https), открывается в новой вкладке.
    pub url: String,
    /// Кому показывать: код права; пусто — всем вошедшим.
    pub permission: Option<String>,
}

/// Манифест расширения (`pult-extension.json`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Manifest {
    /// Версия формата: 1.
    pub manifest_version: u32,
    /// Уникальный идентификатор: `com.example.reports`.
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub publisher: String,
    #[serde(default)]
    pub description: String,
    pub homepage: Option<String>,
    /// Права токена API (только чтение).
    #[serde(default)]
    pub permissions: Vec<String>,
    pub webhook: Option<ManifestWebhook>,
    #[serde(default)]
    pub links: Vec<ManifestLink>,
}

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::bad_request("invalid_manifest", m)
}

fn short(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.chars().count() <= max && !s.chars().any(char::is_control)
}

/// Проверка манифеста: формат, адреса только https, права — из известных порталу.
pub fn validate(core: &Core, v: Value) -> ApiResult<Manifest> {
    let m: Manifest = serde_json::from_value(v).map_err(|e| bad(format!("Манифест не читается: {e}")))?;
    if m.manifest_version != MANIFEST_VERSION {
        return Err(bad("Неподдерживаемая версия манифеста: нужна 1."));
    }
    let id_ok = (3..=100).contains(&m.id.len())
        && m.id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-' || b == b'_');
    if !id_ok {
        return Err(bad("Идентификатор — от 3 до 100 символов: строчные латинские буквы, цифры, точка, дефис."));
    }
    if !short(&m.name, 100)
        || !short(&m.version, 40)
        || m.publisher.chars().count() > 100
        || m.description.chars().count() > 2000
    {
        return Err(bad("Название (до 100 символов) и версия (до 40) обязательны, описание — до 2000 символов."));
    }
    if m.homepage.as_deref().is_some_and(|h| !url_ok(h)) {
        return Err(bad("Адрес страницы расширения — https://…"));
    }
    let known: Vec<&str> = core.permissions.iter().map(|p| p.code).collect();
    for p in m.permissions.iter().chain(m.links.iter().filter_map(|l| l.permission.as_ref())) {
        if !known.contains(&p.as_str()) {
            return Err(bad(format!("Неизвестное право «{p}».")));
        }
    }
    if let Some(w) = &m.webhook {
        if !url_ok(&w.url) {
            return Err(bad("Адрес вебхука — https://… (http — только для localhost)."));
        }
        if w.events.len() > 30
            || w.events.iter().any(|e| {
                !(1..=40).contains(&e.len()) || !e.bytes().all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'.')
            })
        {
            return Err(bad("События вебхука — коды вида `created`, `status`."));
        }
        // В событии — номер, тема и статус заявки: расширение должно просить это право явно,
        // чтобы администратор видел его при установке.
        if !w.events.is_empty() && !m.permissions.iter().any(|p| p == crate::perm::WORK.code) {
            return Err(bad(
                "Вебхук получает номера, темы и статусы заявок: добавьте в права расширения «helpdesk.ticket.work».",
            ));
        }
    }
    if m.links.len() > 10 {
        return Err(bad("Не больше 10 ссылок в меню."));
    }
    for l in &m.links {
        let https = reqwest::Url::parse(l.url.trim()).is_ok_and(|u| u.scheme() == "https");
        if !short(&l.title, 60) || !url_ok(&l.url) || !https {
            return Err(bad("Ссылка в меню: подпись до 60 символов и адрес https://…"));
        }
    }
    Ok(m)
}

/// Скачать JSON по https: без перенаправлений, не больше 1 МБ, за 15 секунд.
async fn fetch_json(url: &str) -> ApiResult<Value> {
    if !url_ok(url) {
        return Err(bad("Адрес — https://… (http — только для localhost)."));
    }
    let failed = |e: String| ApiError::bad_request("fetch_failed", format!("Не удалось получить {url}: {e}"));
    let client = crate::egress::client()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(ApiError::internal)?;
    let mut r = client.get(url).send().await.map_err(|e| failed(e.to_string()))?;
    if !r.status().is_success() {
        return Err(failed(format!("HTTP {}", r.status().as_u16())));
    }
    let mut buf = Vec::new();
    while let Some(chunk) = r.chunk().await.map_err(|e| failed(e.to_string()))? {
        if buf.len() + chunk.len() > MAX_BYTES {
            return Err(failed("больше 1 МБ".into()));
        }
        buf.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&buf).map_err(|e| failed(e.to_string()))
}

// ------------------------------------------------------------------ Настройки и каталог

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ExtensionSettings {
    /// Адрес каталога: JSON-массив манифестов (https). Пусто — каталога нет.
    pub catalog_url: String,
}

impl SettingsGroup for ExtensionSettings {
    const KEY: &'static str = "extensions";
}

#[utoipa::path(get, path = "/api/admin/extensions/settings", tag = "integrations", responses((status = 200, body = ExtensionSettings)))]
async fn get_ext_settings(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<ExtensionSettings>> {
    user.require_superadmin()?;
    Ok(Json(settings::load(&core.db).await?))
}

#[utoipa::path(put, path = "/api/admin/extensions/settings", tag = "integrations", request_body = ExtensionSettings, responses((status = 200, body = ExtensionSettings)))]
async fn put_ext_settings(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<ExtensionSettings>,
) -> ApiResult<Json<ExtensionSettings>> {
    user.require_superadmin()?;
    let s = ExtensionSettings { catalog_url: b.catalog_url.trim().to_string() };
    if !s.catalog_url.is_empty() && !url_ok(&s.catalog_url) {
        return Err(bad("Адрес каталога — https://…"));
    }
    settings::save(&core.db, &s, Some(user.user_id)).await?;
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("extension.settings", Source::Portal).details(json!({"catalog_url": s.catalog_url})),
    )
    .await?;
    Ok(Json(s))
}

#[derive(Serialize, ToSchema)]
pub struct CatalogEntry {
    pub manifest: Manifest,
    /// Установлена версия (если установлено).
    pub installed_version: Option<String>,
}

/// Расширения из каталога; негодные манифесты пропускаются.
#[utoipa::path(get, path = "/api/admin/extensions/catalog", tag = "integrations", responses((status = 200, body = Vec<CatalogEntry>)))]
async fn extension_catalog(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<CatalogEntry>>> {
    user.require_superadmin()?;
    let s: ExtensionSettings = settings::load(&core.db).await?;
    if s.catalog_url.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let Value::Array(items) = fetch_json(&s.catalog_url).await? else {
        return Err(bad("Каталог — JSON-массив манифестов."));
    };
    let installed: Vec<(String, String)> =
        sqlx::query_as("SELECT ext_id, version FROM extensions").fetch_all(&core.db).await?;
    let mut out = Vec::new();
    for v in items.into_iter().take(500) {
        match validate(&core, v) {
            Ok(m) => {
                let installed_version = installed.iter().find(|(id, _)| *id == m.id).map(|(_, v)| v.clone());
                out.push(CatalogEntry { manifest: m, installed_version });
            }
            Err(e) => {
                tracing::warn!(error = %e, "манифест из каталога пропущен");
            }
        }
    }
    Ok(Json(out))
}

// ------------------------------------------------------------------ Установка

#[derive(Deserialize, ToSchema)]
pub struct ManifestSource {
    /// Адрес манифеста (https).
    pub url: Option<String>,
    /// Сам манифест (из файла).
    #[schema(value_type = Option<Object>)]
    pub manifest: Option<Value>,
}

#[derive(Serialize, ToSchema)]
pub struct PermissionInfo {
    pub code: String,
    pub description: String,
}

#[derive(Serialize, ToSchema)]
pub struct ManifestPreview {
    pub manifest: Manifest,
    /// Что расширение сможет читать через API.
    pub permissions: Vec<PermissionInfo>,
    pub already_installed: bool,
}

async fn source(core: &Core, b: &ManifestSource) -> ApiResult<(Manifest, Option<String>)> {
    match (&b.manifest, b.url.as_deref().map(str::trim).filter(|u| !u.is_empty())) {
        (Some(v), _) => Ok((validate(core, v.clone())?, None)),
        (None, Some(u)) => Ok((validate(core, fetch_json(u).await?)?, Some(u.to_string()))),
        (None, None) => Err(bad("Нужен файл манифеста или его адрес.")),
    }
}

/// Проверить манифест и показать, что расширение получит, — до установки.
#[utoipa::path(post, path = "/api/admin/extensions/preview", tag = "integrations", request_body = ManifestSource, responses((status = 200, body = ManifestPreview)))]
async fn preview_extension(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<ManifestSource>,
) -> ApiResult<Json<ManifestPreview>> {
    user.require_superadmin()?;
    let (m, _) = source(&core, &b).await?;
    let permissions = m
        .permissions
        .iter()
        .filter_map(|c| core.permissions.iter().find(|p| p.code == c))
        .map(|p| PermissionInfo { code: p.code.into(), description: pult_core::l10n::tr(p.description).into_owned() })
        .collect();
    let already_installed: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM extensions WHERE ext_id = $1)")
        .bind(&m.id)
        .fetch_one(&core.db)
        .await?;
    Ok(Json(ManifestPreview { manifest: m, permissions, already_installed }))
}

#[derive(Serialize, ToSchema)]
pub struct InstalledExtension {
    pub id: Uuid,
    /// Токен API для настроек расширения — показывается один раз.
    pub token: Option<String>,
    /// Секрет подписи вебхука (HMAC-SHA256) — показывается один раз.
    pub webhook_secret: Option<String>,
}

/// Токен и вебхук расширения; возвращает то, что показывается один раз.
async fn grant(
    core: &Core,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: &CurrentUser,
    m: &Manifest,
) -> ApiResult<(Option<Uuid>, Option<String>, Option<Uuid>, Option<String>)> {
    let (token_id, token) = if m.permissions.is_empty() {
        (None, None)
    } else {
        let (id, t) =
            issue_token(&mut **tx, user.user_id, &format!("{} {}", m.name, m.version), &m.permissions, None).await?;
        (Some(id), Some(t))
    };
    let (webhook_id, secret) = match &m.webhook {
        None => (None, None),
        Some(w) => {
            let secret = random_token(24).map_err(ApiError::internal)?;
            let enc = core.secrets.encrypt(&Secret::new(secret.clone())).map_err(ApiError::internal)?;
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO webhooks (name, url, events, secret, is_active, format) VALUES ($1, $2, $3, $4, true, 'pult') RETURNING id",
            )
            .bind(format!("{} {}", m.name, m.version))
            .bind(w.url.trim())
            .bind(&w.events)
            .bind(enc)
            .fetch_one(&mut **tx)
            .await?;
            (Some(id), Some(secret))
        }
    };
    Ok((token_id, token, webhook_id, secret))
}

/// Отозвать токен и выключить вебхук расширения.
async fn withdraw(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE api_tokens SET revoked_at = now() WHERE revoked_at IS NULL AND id = (SELECT token_id FROM extensions WHERE id = $1)",
    )
    .bind(id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE webhooks SET is_active = false WHERE id = (SELECT webhook_id FROM extensions WHERE id = $1)")
        .bind(id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[utoipa::path(post, path = "/api/admin/extensions", tag = "integrations", request_body = ManifestSource, responses((status = 201, body = InstalledExtension)))]
async fn install_extension(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<ManifestSource>,
) -> ApiResult<(StatusCode, Json<InstalledExtension>)> {
    user.require_superadmin()?;
    let (m, source_url) = source(&core, &b).await?;
    let mut tx = core.db.begin().await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM extensions WHERE ext_id = $1)")
        .bind(&m.id)
        .fetch_one(&mut *tx)
        .await?;
    if exists {
        return Err(ApiError::bad_request(
            "already_installed",
            "Это расширение уже установлено: чтобы обновить, удалите его и установите заново.",
        ));
    }
    let (token_id, token, webhook_id, webhook_secret) = grant(&core, &mut tx, &user, &m).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO extensions (ext_id, name, version, manifest, source_url, token_id, webhook_id, installed_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(&m.id)
    .bind(&m.name)
    .bind(&m.version)
    .bind(serde_json::to_value(&m).map_err(ApiError::internal)?)
    .bind(&source_url)
    .bind(token_id)
    .bind(webhook_id)
    .bind(user.user_id)
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("extension.install", Source::Portal).object("extension", id).details(json!({
            "ext_id": m.id, "version": m.version, "permissions": m.permissions,
            "webhook": m.webhook.as_ref().map(|w| &w.url), "source_url": source_url,
        })),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(InstalledExtension { id, token, webhook_secret })))
}

#[derive(Serialize, ToSchema)]
pub struct Extension {
    pub id: Uuid,
    pub manifest: Manifest,
    pub source_url: Option<String>,
    pub enabled: bool,
    pub installed_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub installed_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub token_last_used_at: Option<OffsetDateTime>,
    pub webhook_last_status: Option<i32>,
    pub webhook_last_error: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ExtensionRow {
    id: Uuid,
    manifest: Value,
    source_url: Option<String>,
    enabled: bool,
    installed_by_name: Option<String>,
    installed_at: OffsetDateTime,
    token_last_used_at: Option<OffsetDateTime>,
    webhook_last_status: Option<i32>,
    webhook_last_error: Option<String>,
}

#[utoipa::path(get, path = "/api/admin/extensions", tag = "integrations", responses((status = 200, body = Vec<Extension>)))]
async fn list_extensions(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<Extension>>> {
    user.require_superadmin()?;
    let rows: Vec<ExtensionRow> = sqlx::query_as(
        "SELECT e.id, e.manifest, e.source_url, e.enabled, e.installed_at,
                coalesce(a.display_name, a.sam_account_name) AS installed_by_name,
                t.last_used_at AS token_last_used_at, w.last_status AS webhook_last_status, w.last_error AS webhook_last_error
           FROM extensions e
           JOIN users u ON u.id = e.installed_by JOIN ad_accounts a ON a.id = u.ad_account_id
           LEFT JOIN api_tokens t ON t.id = e.token_id
           LEFT JOIN webhooks w ON w.id = e.webhook_id
          ORDER BY lower(e.name)",
    )
    .fetch_all(&core.db)
    .await?;
    let mut out = Vec::new();
    for r in rows {
        out.push(Extension {
            id: r.id,
            manifest: serde_json::from_value(r.manifest).map_err(ApiError::internal)?,
            source_url: r.source_url,
            enabled: r.enabled,
            installed_by_name: r.installed_by_name,
            installed_at: r.installed_at,
            token_last_used_at: r.token_last_used_at,
            webhook_last_status: r.webhook_last_status,
            webhook_last_error: r.webhook_last_error,
        });
    }
    Ok(Json(out))
}

#[derive(Deserialize, ToSchema)]
pub struct ExtensionState {
    pub enabled: bool,
}

/// Выключить: токен отзывается, вебхук и ссылки выключаются. Включить: новый токен и секрет (показываются один раз).
#[utoipa::path(put, path = "/api/admin/extensions/{id}", tag = "integrations", params(("id" = Uuid, Path)), request_body = ExtensionState,
    responses((status = 200, body = InstalledExtension)))]
async fn set_extension(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ExtensionState>,
) -> ApiResult<Json<InstalledExtension>> {
    user.require_superadmin()?;
    let mut tx = core.db.begin().await?;
    let row: Option<(Value, bool)> =
        sqlx::query_as("SELECT manifest, enabled FROM extensions WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let (manifest, enabled) = row.ok_or(ApiError::NotFound)?;
    let mut shown = InstalledExtension { id, token: None, webhook_secret: None };
    if enabled == b.enabled {
        return Ok(Json(shown));
    }
    withdraw(&mut tx, id).await?;
    if b.enabled {
        let m: Manifest = serde_json::from_value(manifest).map_err(ApiError::internal)?;
        let old_webhook: Option<Uuid> =
            sqlx::query_scalar("SELECT webhook_id FROM extensions WHERE id = $1").bind(id).fetch_one(&mut *tx).await?;
        let (token_id, token, webhook_id, secret) = grant(&core, &mut tx, &user, &m).await?;
        sqlx::query(
            "UPDATE extensions SET enabled = true, token_id = $2, webhook_id = $3, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(token_id)
        .bind(webhook_id)
        .execute(&mut *tx)
        .await?;
        if let Some(w) = old_webhook {
            sqlx::query("DELETE FROM webhooks WHERE id = $1").bind(w).execute(&mut *tx).await?;
        }
        shown.token = token;
        shown.webhook_secret = secret;
    } else {
        sqlx::query("UPDATE extensions SET enabled = false, updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new(if b.enabled { "extension.enable" } else { "extension.disable" }, Source::Portal)
            .object("extension", id),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(shown))
}

/// Удалить: токен отзывается, вебхук удаляется.
#[utoipa::path(delete, path = "/api/admin/extensions/{id}", tag = "integrations", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn remove_extension(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require_superadmin()?;
    let mut tx = core.db.begin().await?;
    withdraw(&mut tx, id).await?;
    let row: Option<(String, Option<Uuid>)> =
        sqlx::query_as("DELETE FROM extensions WHERE id = $1 RETURNING ext_id, webhook_id")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let (ext_id, webhook) = row.ok_or(ApiError::NotFound)?;
    if let Some(w) = webhook {
        sqlx::query("DELETE FROM webhooks WHERE id = $1").bind(w).execute(&mut *tx).await?;
    }
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("extension.remove", Source::Portal).object("extension", id).details(json!({"ext_id": ext_id})),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
pub struct ExtensionLink {
    pub extension: String,
    pub title: String,
    pub url: String,
}

/// Ссылки включённых расширений для меню — те, на которые у пользователя есть право.
#[utoipa::path(get, path = "/api/extensions/links", tag = "integrations", responses((status = 200, body = Vec<ExtensionLink>)))]
async fn extension_links(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<ExtensionLink>>> {
    let rows: Vec<(Value,)> = sqlx::query_as("SELECT manifest FROM extensions WHERE enabled ORDER BY lower(name)")
        .fetch_all(&core.db)
        .await?;
    let mut out = Vec::new();
    for (v,) in rows {
        let Ok(m) = serde_json::from_value::<Manifest>(v) else { continue };
        for l in m.links {
            let allowed = l
                .permission
                .as_deref()
                .is_none_or(|p| user.grants.is_superadmin() || user.grants.permissions.contains(p));
            if allowed {
                out.push(ExtensionLink { extension: m.name.clone(), title: l.title, url: l.url });
            }
        }
    }
    Ok(Json(out))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn core() -> Core {
        Core {
            db: sqlx::PgPool::connect_lazy("postgres://localhost/none").unwrap(),
            secrets: std::sync::Arc::new(pult_core::crypto::SecretBox::new(&Secret::new("k".repeat(40)))),
            cookies: pult_core::session::CookiePolicy { secure: true },
            permissions: std::sync::Arc::new(crate::permissions()),
            events: pult_core::events::Hub::default(),
            insecure_mail: false,
            org: Default::default(),
        }
    }

    #[tokio::test]
    async fn manifest_rules() {
        let c = core();
        let good = json!({
            "manifest_version": 1, "id": "com.example.sla", "name": "SLA", "version": "1.0.0",
            "permissions": ["helpdesk.reports", "helpdesk.ticket.work"],
            "webhook": {"url": "https://ext.example/hook", "events": ["created", "status"]},
            "links": [{"title": "Отчёт", "url": "https://ext.example/", "permission": "helpdesk.reports"}]
        });
        assert!(validate(&c, good.clone()).is_ok());
        let with = |k: &str, v: Value| {
            let mut g = good.clone();
            g[k] = v;
            g
        };
        assert!(validate(&c, with("manifest_version", json!(2))).is_err());
        assert!(validate(&c, with("id", json!("Com Example"))).is_err());
        assert!(validate(&c, with("permissions", json!(["no.such.right"]))).is_err());
        assert!(
            validate(&c, with("permissions", json!(["helpdesk.reports"]))).is_err(),
            "события заявок — только с правом на заявки"
        );
        assert!(validate(&c, with("links", json!([{"title": "x", "url": "HTTP://localhost/"}]))).is_err());
        assert!(validate(&c, with("webhook", json!({"url": "http://ext.example/hook"}))).is_err(), "только https");
        assert!(validate(&c, with("links", json!([{"title": "x", "url": "javascript:alert(1)"}]))).is_err());
        assert!(
            validate(&c, with("links", json!([{"title": "x", "url": "http://localhost/"}]))).is_err(),
            "ссылка — только https"
        );
    }
}
