//! Интеграции: токены REST API с ограниченными правами (только чтение) и исходящие
//! вебхуки на события заявок: JSON с подписью HMAC-SHA256 или сообщение в мессенджер
//! (входящий вебхук Mattermost, Rocket.Chat, Slack; бот Telegram).

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
    l10n::{self, Lang, tr_to},
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
const TELEGRAM_API: &str = "https://api.telegram.org";

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
    let (id, token) = issue_token(&core.db, user.user_id, b.name.trim(), &b.permissions, b.days).await?;
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

/// Выпустить токен (права уже проверены): в базе — только хеш, сам токен возвращается один раз.
pub(crate) async fn issue_token(
    db: impl sqlx::PgExecutor<'_>,
    owner: Uuid,
    name: &str,
    permissions: &[String],
    days: Option<i32>,
) -> ApiResult<(Uuid, String)> {
    let token = format!("pult_{}", random_token(32).map_err(ApiError::internal)?);
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO api_tokens (name, owner_user_id, token_hash, permissions, expires_at)
         VALUES ($1, $2, $3, $4, CASE WHEN $5::int IS NULL THEN NULL ELSE now() + make_interval(days => $5) END) RETURNING id",
    )
    .bind(name)
    .bind(owner)
    .bind(sha256(token.as_bytes()))
    .bind(permissions)
    .bind(days)
    .fetch_one(db)
    .await?;
    Ok((id, token))
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
    /// `pult` — JSON с подписью; `chat` — Mattermost, Rocket.Chat, Slack; `telegram` — бот Telegram.
    pub format: String,
    /// Чат Telegram: числовой id или `@канал`.
    pub chat_id: Option<String>,
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
    /// `pult` (по умолчанию), `chat` или `telegram`.
    #[serde(default = "pult_format")]
    pub format: String,
    /// Для `telegram`: числовой id чата или `@канал`.
    pub chat_id: Option<String>,
    /// Секрет подписи (`pult`) или токен бота (`telegram`); пусто при правке — оставить прежний.
    pub secret: Option<String>,
    #[serde(default = "yes")]
    pub is_active: bool,
}

fn yes() -> bool {
    true
}

fn pult_format() -> String {
    "pult".into()
}

/// Куда доставлять: формат, адрес и чат.
struct Target {
    format: &'static str,
    url: String,
    chat_id: Option<String>,
}

fn target(b: &WebhookInput) -> ApiResult<Target> {
    let bad = |m: &str| ApiError::bad_request("invalid_webhook", m.to_string());
    match b.format.as_str() {
        f @ ("pult" | "chat") => {
            check_url(&b.url)?;
            Ok(Target { format: if f == "chat" { "chat" } else { "pult" }, url: b.url.trim().into(), chat_id: None })
        }
        "telegram" => {
            let chat = b
                .chat_id
                .as_deref()
                .map(str::trim)
                .filter(|c| chat_id_ok(c))
                .ok_or_else(|| bad("Чат Telegram — числовой id (например, -1001234567890) или @имя канала."))?;
            Ok(Target { format: "telegram", url: TELEGRAM_API.into(), chat_id: Some(chat.into()) })
        }
        _ => Err(bad("Неизвестный формат вебхука.")),
    }
}

/// Числовой id чата (у групп — с минусом) или `@имя` публичного канала.
fn chat_id_ok(c: &str) -> bool {
    let digits = c.strip_prefix('-').unwrap_or(c);
    (!digits.is_empty() && digits.len() <= 20 && digits.bytes().all(|b| b.is_ascii_digit()))
        || c.strip_prefix('@')
            .is_some_and(|n| (5..=32).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
}

/// Токен бота от @BotFather: `<id>:<ключ>`.
fn bot_token_ok(t: &str) -> bool {
    t.split_once(':').is_some_and(|(id, key)| {
        (5..=15).contains(&id.len())
            && id.bytes().all(|b| b.is_ascii_digit())
            && (30..=64).contains(&key.len())
            && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    })
}

/// Секрет по формату: подпись — от 16 символов, токен бота — по форме; входящему вебхуку чата не нужен.
fn check_secret(format: &str, s: &str) -> ApiResult<()> {
    let bad = |m: &str| Err(ApiError::bad_request("invalid_webhook", m.to_string()));
    match format {
        "pult" if s.len() < 16 => bad("Секрет подписи — не короче 16 символов."),
        "telegram" if !bot_token_ok(s) => bad("Токен бота Telegram — вида 123456789:AA… (его выдаёт @BotFather)."),
        _ => Ok(()),
    }
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
pub(crate) fn url_ok(u: &str) -> bool {
    let u = u.trim();
    if u.len() > 2000 || u.contains(char::is_whitespace) {
        return false;
    }
    let Ok(url) = reqwest::Url::parse(u) else { return false };
    if !url.username().is_empty() || url.password().is_some() || crate::egress::literal_forbidden(&url) {
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
        "SELECT id, name, url, events, format, chat_id, is_active, last_status, last_error, last_delivery_at
           FROM webhooks ORDER BY name",
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
    let to = target(&b)?;
    let secret = b.secret.as_deref().map(str::trim).unwrap_or_default();
    check_secret(to.format, secret)?;
    let enc = core.secrets.encrypt(&Secret::new(secret)).map_err(ApiError::internal)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO webhooks (name, url, events, secret, is_active, format, chat_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(b.name.trim())
    .bind(&to.url)
    .bind(&b.events)
    .bind(enc)
    .bind(b.is_active)
    .bind(to.format)
    .bind(&to.chat_id)
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
    let to = target(&b)?;
    let was: String = sqlx::query_scalar("SELECT format FROM webhooks WHERE id = $1")
        .bind(id)
        .fetch_optional(&core.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let enc = match b.secret.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => {
            check_secret(to.format, s)?;
            Some(core.secrets.encrypt(&Secret::new(s)).map_err(ApiError::internal)?)
        }
        // Прежний секрет годится только тому же формату: подпись не станет токеном бота.
        None if was != to.format => {
            check_secret(to.format, "")?;
            // Чату секрет не нужен — прежний (например, токен бота) не храним.
            Some(core.secrets.encrypt(&Secret::new("")).map_err(ApiError::internal)?)
        }
        None => None,
    };
    let n = sqlx::query(
        "UPDATE webhooks SET name = $2, url = $3, events = $4, secret = coalesce($5, secret), is_active = $6,
                format = $7, chat_id = $8
          WHERE id = $1",
    )
    .bind(id)
    .bind(b.name.trim())
    .bind(&to.url)
    .bind(&b.events)
    .bind(enc)
    .bind(b.is_active)
    .bind(to.format)
    .bind(&to.chat_id)
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
    type HookRow = (String, Vec<u8>, bool, String, Option<String>);
    let row: Option<HookRow> =
        sqlx::query_as("SELECT url, secret, is_active, format, chat_id FROM webhooks WHERE id = $1")
            .bind(hook)
            .fetch_optional(&core.db)
            .await?;
    let Some((url, secret, true, format, chat_id)) = row else { return Ok(()) };
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
    let secret = core.secrets.decrypt(&secret).map_err(ApiError::internal)?;
    let client = crate::egress::client().timeout(Duration::from_secs(10)).build().map_err(ApiError::internal)?;
    let req = match format.as_str() {
        "chat" | "telegram" => {
            let lang = l10n::default_lang(&core.db).await;
            let portal = crate::notify::ctx(core).await?.portal;
            let text = chat_text(lang, event.as_str().unwrap_or(""), &ticket, &portal);
            if format == "chat" {
                client.post(&url).json(&json!({"text": text}))
            } else {
                // Токен — в пути запроса: в ошибку и журнал адрес не попадает (`without_url` ниже).
                client.post(format!("{TELEGRAM_API}/bot{}/sendMessage", secret.expose())).json(&json!({
                    "chat_id": chat_id, "text": text, "disable_web_page_preview": true,
                }))
            }
        }
        _ => {
            let body = serde_json::to_vec(&json!({"event": event, "delivery": delivery, "ticket": ticket}))
                .map_err(ApiError::internal)?;
            client
                .post(&url)
                .header("content-type", "application/json")
                .header("x-pult-event", event.as_str().unwrap_or(""))
                .header("x-pult-delivery", delivery.as_str().unwrap_or(""))
                .header("x-pult-signature", sign(secret.expose(), &body))
                .body(body)
        }
    };
    let res = req.header("user-agent", "Pult-Webhook/1").send().await;
    let (status, error) = match res {
        Ok(r) => {
            (Some(i32::from(r.status().as_u16())), (!r.status().is_success()).then(|| format!("HTTP {}", r.status())))
        }
        Err(e) => (None, Some(e.without_url().to_string().chars().take(500).collect())),
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

/// Событие истории заявки — для сообщения в чат.
fn event_label(kind: &str) -> &'static str {
    match kind {
        "created" => "Новая заявка",
        "status" => "Изменён статус",
        "assigned" => "Изменён исполнитель",
        "priority" => "Изменён приоритет",
        "category" => "Изменена категория",
        "site" => "Изменена площадка",
        "title" => "Изменена тема",
        "line" => "Изменена линия поддержки",
        "group" => "Сменена группа",
        "impact" => "Изменены влияние и срочность",
        "reopened" => "Заявка возвращена в работу",
        "major" => "Объявлен массовый сбой",
        "merged" => "Заявка объединена с другой",
        "rated" => "Заявитель оценил решение",
        "sla_warning" => "Скоро срок SLA",
        "sla_breached" => "Просрочен срок SLA",
        "alert_resolved" => "Мониторинг: проблема устранена",
        _ => "Изменение заявки",
    }
}

fn status_label(s: &str) -> &'static str {
    match s {
        "new" => "Новая",
        "assigned" => "Назначена",
        "in_progress" => "В работе",
        "waiting" => "Ждёт ответа",
        "waiting_approval" => "Ждёт согласования",
        "on_hold" => "Отложена",
        "resolved" => "Решена",
        "closed" => "Закрыта",
        "canceled" => "Отменена",
        _ => "",
    }
}

fn priority_label(p: &str) -> &'static str {
    match p {
        "low" => "Низкий",
        "normal" => "Обычный",
        "high" => "Высокий",
        "critical" => "Критический",
        _ => "",
    }
}

/// Сообщение в чат: событие и номер, тема, статус · приоритет · площадка, ссылка. Простой текст —
/// без разметки, одинаково читается в Mattermost, Rocket.Chat, Slack и Telegram. Тексты
/// комментариев сюда не попадают: в канале их увидели бы не только участники заявки.
pub fn chat_text(lang: Lang, event: &str, t: &Value, portal: &str) -> String {
    let s = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or_default();
    // Тему пишет заявитель: «@channel», «<!here>» и «<url|текст>» не должны звать весь канал или
    // подменять ссылку. После @ — невидимый разделитель слов, угловые скобки — похожими.
    let inert = |v: &str| -> String {
        v.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .take(200)
            .collect::<String>()
            .replace('@', "@\u{2060}")
            .replace('<', "‹")
            .replace('>', "›")
            // [текст](ссылка) Markdown в Mattermost и Rocket.Chat — тоже подмена ссылки.
            .replace('[', "［")
            .replace(']', "］")
    };
    let number = t.get("number").and_then(Value::as_i64).unwrap_or_default();
    let title = inert(s("title"));
    let mut facts = Vec::new();
    let status = status_label(s("status"));
    if !status.is_empty() {
        facts.push(format!("{}: {}", tr_to(lang, "Статус"), tr_to(lang, status)));
    }
    let priority = priority_label(s("priority"));
    if !priority.is_empty() {
        facts.push(format!("{}: {}", tr_to(lang, "Приоритет"), tr_to(lang, priority)));
    }
    if !s("site").is_empty() {
        facts.push(inert(s("site")));
    }
    let mut text = format!("{} · #{number}\n{title}", tr_to(lang, event_label(event)));
    if !facts.is_empty() {
        text.push('\n');
        text.push_str(&facts.join(" · "));
    }
    if !portal.is_empty() {
        text.push_str(&format!("\n{portal}/tickets/{number}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_message() {
        let t = json!({"number": 42, "title": "Не печатает\nпринтер", "status": "in_progress", "priority": "high", "site": "Офис А"});
        assert_eq!(
            chat_text(Lang::Ru, "status", &t, "https://pult.example.test"),
            "Изменён статус · #42\nНе печатает принтер\nСтатус: В работе · Приоритет: Высокий · Офис А\nhttps://pult.example.test/tickets/42"
        );
        let en = chat_text(Lang::En, "created", &json!({"number": 7, "title": "Mouse", "status": "new"}), "");
        assert_eq!(en, "New ticket · #7\nMouse\nStatus: New");
        assert!(chat_text(Lang::Zh, "nope", &json!({}), "").starts_with(&*tr_to(Lang::Zh, "Изменение заявки")));
        let t = chat_text(Lang::Ru, "created", &json!({"number": 1, "title": "@channel <!here> <https://x|y>"}), "");
        assert!(!t.contains("@channel") && !t.contains('<') && !t.contains('>'), "{t}");
        let t = chat_text(Lang::Ru, "created", &json!({"number": 1, "title": "[x](https://evil.test)"}), "");
        assert!(!t.contains("[x]("), "{t}");
    }

    #[test]
    fn telegram_fields() {
        for ok in ["-1001234567890", "123456", "@pult_alerts"] {
            assert!(chat_id_ok(ok), "{ok}");
        }
        for bad in ["", "-", "@ab", "@имя_канала", "12 34", "https://t.me/x"] {
            assert!(!chat_id_ok(bad), "{bad}");
        }
        assert!(bot_token_ok("123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw"));
        assert!(!bot_token_ok("123456789"));
        assert!(!bot_token_ok("abc:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw"));
        assert!(!bot_token_ok("123456789:short"));
        assert!(check_secret("chat", "").is_ok(), "входящему вебхуку чата секрет не нужен");
        assert!(check_secret("pult", "short").is_err());
    }

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
        assert!(!url_ok("https://169.254.169.254/latest"), "метаданные облака");
        assert!(!url_ok("https://[::ffff:169.254.169.254]/latest"), "они же, записанные как IPv6");
    }
}
