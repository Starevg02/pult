//! Интеграции: токены REST API с ограниченными правами (только чтение) и исходящие
//! вебхуки на события заявок с подписью HMAC-SHA256.

use crate::{HelpdeskState, perm};
use axum::{
    Json,
    extract::{FromRef, Path, State},
    http::StatusCode,
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Secret,
    audit::{self, Entry, Source},
    crypto::{random_token, sha256},
};
use ring::hmac;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use std::time::Duration;
use time::OffsetDateTime;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub const WEBHOOK_JOB: &str = "helpdesk.webhook";

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
    HelpdeskState: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(list_tokens, create_token))
        .routes(routes!(revoke_token))
        .routes(routes!(list_webhooks, create_webhook))
        .routes(routes!(update_webhook, delete_webhook))
}

// ------------------------------------------------------------------ Токены API

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ApiToken {
    pub id: Uuid,
    pub name: String,
    pub owner_name: Option<String>,
    pub permissions: Vec<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub expires_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_used_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub revoked_at: Option<OffsetDateTime>,
}

#[derive(Deserialize, ToSchema)]
pub struct TokenInput {
    pub name: String,
    /// Коды прав: токен получает только те, что есть у создателя.
    pub permissions: Vec<String>,
    /// Срок действия, дней (пусто — бессрочно).
    pub days: Option<i32>,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedToken {
    pub id: Uuid,
    /// Показывается один раз: в базе хранится только хеш.
    pub token: String,
}

#[utoipa::path(get, path = "/api/admin/api-tokens", tag = "integrations", responses((status = 200, body = Vec<ApiToken>)))]
async fn list_tokens(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<ApiToken>>> {
    user.require(&perm::ADMIN)?;
    let rows = sqlx::query_as(
        "SELECT t.id, t.name, t.permissions, t.expires_at, t.last_used_at, t.revoked_at,
                coalesce(a.display_name, a.sam_account_name) AS owner_name
           FROM api_tokens t JOIN users u ON u.id = t.owner_user_id JOIN ad_accounts a ON a.id = u.ad_account_id
          ORDER BY t.revoked_at IS NOT NULL, t.created_at DESC",
    )
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

/// Выпустить токен. Им можно только читать (GET) — создавать заявки через API нельзя.
#[utoipa::path(post, path = "/api/admin/api-tokens", tag = "integrations", request_body = TokenInput, responses((status = 201, body = CreatedToken)))]
async fn create_token(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<TokenInput>,
) -> ApiResult<(StatusCode, Json<CreatedToken>)> {
    user.require(&perm::ADMIN)?;
    if b.name.trim().is_empty() || b.permissions.is_empty() {
        return Err(ApiError::bad_request("invalid_token", "Укажите название и хотя бы одно право."));
    }
    let known: Vec<&str> = core.permissions.iter().map(|p| p.code).collect();
    for p in &b.permissions {
        if !known.contains(&p.as_str()) {
            return Err(ApiError::bad_request("invalid_token", format!("Неизвестное право «{p}».")));
        }
        if !user.grants.is_superadmin() && !user.grants.permissions.contains(p) {
            return Err(ApiError::bad_request("invalid_token", format!("Права «{p}» нет у вас самого.")));
        }
    }
    if b.days.is_some_and(|d| !(1..=3650).contains(&d)) {
        return Err(ApiError::bad_request("invalid_token", "Срок — от 1 дня до 10 лет."));
    }
    let token = format!("pult_{}", random_token(32).map_err(ApiError::internal)?);
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO api_tokens (name, owner_user_id, token_hash, permissions, expires_at)
         VALUES ($1, $2, $3, $4, CASE WHEN $5::int IS NULL THEN NULL ELSE now() + make_interval(days => $5) END) RETURNING id",
    )
    .bind(b.name.trim())
    .bind(user.user_id)
    .bind(sha256(token.as_bytes()))
    .bind(&b.permissions)
    .bind(b.days)
    .fetch_one(&core.db)
    .await?;
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("api_token.create", Source::Portal)
            .object("api_token", id)
            .details(json!({"permissions": b.permissions})),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(CreatedToken { id, token })))
}

#[utoipa::path(delete, path = "/api/admin/api-tokens/{id}", tag = "integrations", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn revoke_token(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let n = sqlx::query("UPDATE api_tokens SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL")
        .bind(id)
        .execute(&core.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("api_token.revoke", Source::Portal).object("api_token", id))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Вебхуки

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct Webhook {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    /// Виды событий истории заявки (`created`, `status`, `assigned`…); пусто — все.
    pub events: Vec<String>,
    pub is_active: bool,
    pub last_status: Option<i32>,
    pub last_error: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_delivery_at: Option<OffsetDateTime>,
}

#[derive(Deserialize, ToSchema)]
pub struct WebhookInput {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub events: Vec<String>,
    /// Секрет для подписи; пусто при правке — оставить прежний.
    pub secret: Option<String>,
    #[serde(default = "yes")]
    pub is_active: bool,
}

fn yes() -> bool {
    true
}

fn check_url(u: &str) -> ApiResult<()> {
    let ok = url_ok(u);
    if !ok {
        return Err(ApiError::bad_request("invalid_webhook", "Адрес — https://… (http — только для localhost)."));
    }
    Ok(())
}

/// https — куда угодно; http — только на этот же сервер. Разбираем адрес, а не сравниваем префикс:
/// иначе «http://localhost.example.com» ушёл бы наружу без TLS. Логин и пароль в адресе не принимаем.
fn url_ok(u: &str) -> bool {
    let u = u.trim();
    if u.len() > 2000 || u.contains(char::is_whitespace) {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(u) else { return false };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match (url.scheme(), url.host_str()) {
        ("https", Some(_)) => true,
        ("http", Some(h)) => matches!(h, "localhost" | "127.0.0.1" | "[::1]"),
        _ => false,
    }
}

#[utoipa::path(get, path = "/api/admin/webhooks", tag = "integrations", responses((status = 200, body = Vec<Webhook>)))]
async fn list_webhooks(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<Webhook>>> {
    user.require(&perm::ADMIN)?;
    let rows = sqlx::query_as(
        "SELECT id, name, url, events, is_active, last_status, last_error, last_delivery_at FROM webhooks ORDER BY name",
    )
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[utoipa::path(post, path = "/api/admin/webhooks", tag = "integrations", request_body = WebhookInput, responses((status = 201, body = Object)))]
async fn create_webhook(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<WebhookInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&perm::ADMIN)?;
    check_url(&b.url)?;
    let secret = b
        .secret
        .as_deref()
        .map(str::trim)
        .filter(|s| s.len() >= 16)
        .ok_or_else(|| ApiError::bad_request("invalid_webhook", "Секрет подписи — не короче 16 символов."))?;
    let enc = core.secrets.encrypt(&Secret::new(secret)).map_err(ApiError::internal)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO webhooks (name, url, events, secret, is_active) VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(b.name.trim())
    .bind(b.url.trim())
    .bind(&b.events)
    .bind(enc)
    .bind(b.is_active)
    .fetch_one(&core.db)
    .await?;
    audit::record(&core.db, &user.actor, Entry::new("webhook.create", Source::Portal).object("webhook", id)).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[utoipa::path(put, path = "/api/admin/webhooks/{id}", tag = "integrations", params(("id" = Uuid, Path)), request_body = WebhookInput, responses((status = 204)))]
async fn update_webhook(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<WebhookInput>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    check_url(&b.url)?;
    let enc = match b.secret.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if s.len() < 16 => {
            return Err(ApiError::bad_request("invalid_webhook", "Секрет подписи — не короче 16 символов."));
        }
        Some(s) => Some(core.secrets.encrypt(&Secret::new(s)).map_err(ApiError::internal)?),
        None => None,
    };
    let n = sqlx::query(
        "UPDATE webhooks SET name = $2, url = $3, events = $4, secret = coalesce($5, secret), is_active = $6 WHERE id = $1",
    )
    .bind(id)
    .bind(b.name.trim())
    .bind(b.url.trim())
    .bind(&b.events)
    .bind(enc)
    .bind(b.is_active)
    .execute(&core.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("webhook.update", Source::Portal).object("webhook", id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/admin/webhooks/{id}", tag = "integrations", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn delete_webhook(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let n = sqlx::query("DELETE FROM webhooks WHERE id = $1").bind(id).execute(&core.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("webhook.delete", Source::Portal).object("webhook", id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Поставить доставку вебхуков о последнем событии заявки (из `tickets::ping`).
/// Доставки по всем событиям, записанным в этой транзакции (`now()` — её начало): одна правка
/// может дать несколько событий (приоритет, затем авто-статус), и подписчик на любое из них их получит.
pub async fn enqueue(tx: &mut Transaction<'_, Postgres>, ticket: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "WITH e AS (SELECT id, kind FROM ticket_events WHERE ticket_id = $1 AND ts >= now())
         INSERT INTO jobs (id, kind, payload, max_attempts, idempotency_key)
         SELECT uuid_v7(), $2, jsonb_build_object('webhook', w.id, 'ticket', $1, 'event', e.kind, 'event_id', e.id), 6,
                'webhook:' || w.id || ':' || e.id
           FROM webhooks w CROSS JOIN e
          WHERE w.is_active AND (cardinality(w.events) = 0 OR e.kind = ANY(w.events))
         ON CONFLICT (idempotency_key) DO NOTHING",
    )
    .bind(ticket)
    .bind(WEBHOOK_JOB)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Подпись тела: `sha256=<hex HMAC-SHA256>`.
pub fn sign(secret: &str, body: &[u8]) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.as_bytes());
    let tag = hmac::sign(&key, body);
    let hex: String = tag.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256={hex}")
}

/// Доставка одного события. Ошибка — повтор задачи с задержкой.
pub async fn deliver(core: &Core, payload: &Value) -> ApiResult<()> {
    let id = |k: &str| payload.get(k).and_then(Value::as_str).and_then(|s| Uuid::parse_str(s).ok());
    let (Some(hook), Some(ticket)) = (id("webhook"), id("ticket")) else { return Ok(()) };
    let row: Option<(String, Vec<u8>, bool)> =
        sqlx::query_as("SELECT url, secret, is_active FROM webhooks WHERE id = $1")
            .bind(hook)
            .fetch_optional(&core.db)
            .await?;
    let Some((url, secret, true)) = row else { return Ok(()) };
    let ticket: Option<Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object('number', t.number, 'title', t.title, 'status', t.status, 'priority', t.priority,
                'line', t.line, 'site', s.name, 'category', c.name, 'assignee_id', t.assignee_id,
                'created_at', t.created_at, 'updated_at', t.updated_at, 'resolve_due_at', t.resolve_due_at)
           FROM tickets t LEFT JOIN sites s ON s.id = t.site_id LEFT JOIN ticket_categories c ON c.id = t.category_id
          WHERE t.id = $1",
    )
    .bind(ticket)
    .fetch_optional(&core.db)
    .await?;
    let Some(ticket) = ticket else { return Ok(()) };
    let event = payload.get("event").cloned().unwrap_or(Value::Null);
    let delivery = payload.get("event_id").cloned().unwrap_or(Value::Null);
    let body = serde_json::to_vec(&json!({"event": event, "delivery": delivery, "ticket": ticket}))
        .map_err(ApiError::internal)?;
    let secret = core.secrets.decrypt(&secret).map_err(ApiError::internal)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(ApiError::internal)?;
    let res = client
        .post(&url)
        .header("content-type", "application/json")
        .header("user-agent", "Pult-Webhook/1")
        .header("x-pult-event", event.as_str().unwrap_or(""))
        .header("x-pult-delivery", delivery.as_str().unwrap_or(""))
        .header("x-pult-signature", sign(secret.expose(), &body))
        .body(body)
        .send()
        .await;
    let (status, error) = match &res {
        Ok(r) => {
            (Some(i32::from(r.status().as_u16())), (!r.status().is_success()).then(|| format!("HTTP {}", r.status())))
        }
        Err(e) => (None, Some(e.to_string().chars().take(500).collect())),
    };
    sqlx::query("UPDATE webhooks SET last_status = $2, last_error = $3, last_delivery_at = now() WHERE id = $1")
        .bind(hook)
        .bind(status)
        .bind(&error)
        .execute(&core.db)
        .await?;
    match error {
        Some(e) => Err(ApiError::Unavailable { code: "webhook_failed", message: e }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_is_hmac_sha256() {
        // RFC 4231, тест 2.
        assert_eq!(
            sign("Jefe", b"what do ya want for nothing?"),
            "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn webhook_urls() {
        assert!(url_ok("https://hooks.example.test/pult"));
        assert!(url_ok("http://localhost:8080/x"));
        assert!(!url_ok("http://hooks.example.test/x"), "без TLS наружу нельзя");
        assert!(!url_ok("file:///etc/passwd"));
        assert!(!url_ok("http://localhost.example.test/x"), "префикс localhost — не localhost");
        assert!(!url_ok("http://127.0.0.1.example.test/x"));
        assert!(!url_ok("https://user:pass@hooks.example.test/x"), "учётка в адресе");
        assert!(url_ok("http://127.0.0.1:9000/x"));
        assert!(url_ok("http://[::1]:9000/x"));
    }
}
