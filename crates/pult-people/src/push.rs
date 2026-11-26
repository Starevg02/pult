//! Уведомления на устройство (Web Push): браузер или установленное приложение портала (PWA) получает
//! уведомление, даже когда вкладка закрыта.
//!
//! - Подписку создаёт браузер (`PushManager.subscribe`) с открытым ключом портала (VAPID, RFC 8292) и
//!   передаёт порталу адрес службы push и свои ключи.
//! - Портал шифрует сообщение ключами браузера (RFC 8291, `aes128gcm`): служба push (Google, Mozilla, Apple,
//!   Microsoft) видит только шифротекст. По умолчанию в сообщении нет даже темы заявки — только «новое
//!   уведомление».
//! - Адреса подписок — только известных служб push: иначе подписка заставила бы сервер ходить по
//!   произвольным адресам внутренней сети.
//! - Выключено по умолчанию: серверу нужен выход в Интернет к службам push.

use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Secret,
    audit::{self, Entry, Source},
    l10n::{self, Lang},
    rbac::perm,
    settings::{self, SettingsGroup},
};
use ring::{
    aead, agreement, hkdf,
    rand::{SecureRandom, SystemRandom},
    signature::{self, EcdsaKeyPair, KeyPair},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use utoipa::ToSchema;
use uuid::Uuid;

/// Сколько неудачных отправок подряд терпит подписка, прежде чем её удалить.
const MAX_FAILURES: i32 = 10;
/// Уведомления старше этого не отправляются: после простоя сервера телефон не должен получить пачку старья.
const FRESH_MINUTES: i32 = 15;
/// Срок жизни сообщения в службе push, секунд.
const TTL_SECS: u32 = 3600;
/// Службы push браузеров. Подписки на другие адреса не принимаются.
const PUSH_HOSTS: [&str; 4] =
    ["fcm.googleapis.com", ".push.services.mozilla.com", ".push.apple.com", ".notify.windows.com"];

// ------------------------------------------------------------------ Настройки

/// Уведомления на устройства (админка → «Система»).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct PushSettings {
    pub enabled: bool,
    /// Показывать в уведомлении тему заявки или сообщения. Выключено — только «новое уведомление»:
    /// текст на экране блокировки видит любой, кто рядом с телефоном.
    pub show_text: bool,
    /// Контакт администратора для служб push (`mailto:` или `https:`): им пишут при проблемах с рассылкой.
    pub contact: String,
}

impl SettingsGroup for PushSettings {
    const KEY: &'static str = "push";
}

/// Ключ VAPID портала. Закрытый — зашифрован ключом портала.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct PushKeys {
    /// base64 зашифрованного PKCS#8.
    private: String,
    /// Открытый ключ, base64url (65 байт, несжатая точка P-256).
    public: String,
}

impl SettingsGroup for PushKeys {
    const KEY: &'static str = "push_keys";
}

fn contact_ok(c: &str) -> bool {
    c.is_empty()
        || ((c.starts_with("mailto:") || c.starts_with("https://"))
            && c.len() <= 200
            && !c.contains(char::is_whitespace))
}

/// Адрес подписки — https и известная служба push.
pub fn endpoint_ok(endpoint: &str) -> bool {
    let Ok(u) = url::Url::parse(endpoint) else { return false };
    let host = u.host_str().unwrap_or_default().to_ascii_lowercase();
    u.scheme() == "https"
        && u.port().is_none()
        && endpoint.len() <= 2000
        && PUSH_HOSTS.iter().any(|h| if h.starts_with('.') { host.ends_with(h) } else { host == *h })
}

/// Открытый и закрытый ключ VAPID; создаётся при первом обращении.
async fn keys(core: &Core) -> ApiResult<(EcdsaKeyPair, String)> {
    let rng = SystemRandom::new();
    let mut k: PushKeys = settings::load(&core.db).await?;
    if k.private.is_empty() {
        let doc = EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .map_err(|_| ApiError::internal("VAPID: генерация ключа"))?;
        let pair = EcdsaKeyPair::from_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, doc.as_ref(), &rng)
            .map_err(|_| ApiError::internal("VAPID: ключ"))?;
        let blob = core.secrets.encrypt(&Secret::new(B64.encode(doc.as_ref()))).map_err(ApiError::internal)?;
        k = PushKeys { private: B64.encode(blob), public: B64.encode(pair.public_key().as_ref()) };
        // Две копии сервера могли создать ключ одновременно: остаётся первый записанный.
        sqlx::query("INSERT INTO settings (key, value) VALUES ($1, $2) ON CONFLICT (key) DO NOTHING")
            .bind(PushKeys::KEY)
            .bind(serde_json::to_value(&k).map_err(ApiError::internal)?)
            .execute(&core.db)
            .await?;
        k = settings::load(&core.db).await?;
    }
    let blob = B64.decode(&k.private).map_err(ApiError::internal)?;
    let pkcs8 = core.secrets.decrypt(&blob).map_err(ApiError::internal)?;
    let der = B64.decode(pkcs8.expose()).map_err(ApiError::internal)?;
    let pair = EcdsaKeyPair::from_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &der, &rng)
        .map_err(|_| ApiError::internal("VAPID: ключ повреждён"))?;
    Ok((pair, k.public))
}

// ------------------------------------------------------------------ Шифрование (RFC 8291)

struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn hkdf(salt: &[u8], ikm: &[u8], info: &[&[u8]], len: usize) -> Result<Vec<u8>, ring::error::Unspecified> {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(ikm);
    let mut out = vec![0u8; len];
    prk.expand(info, Len(len))?.fill(&mut out)?;
    Ok(out)
}

/// Ключ содержимого и nonce из общего секрета ECDH (RFC 8291, раздел 3.4; RFC 8188, раздел 2.2).
fn derive(
    ecdh: &[u8],
    auth: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    salt: &[u8],
) -> Result<(Vec<u8>, Vec<u8>), ring::error::Unspecified> {
    let ikm = hkdf(auth, ecdh, &[b"WebPush: info\0", ua_public, as_public], 32)?;
    let cek = hkdf(salt, &ikm, &[b"Content-Encoding: aes128gcm\0"], 16)?;
    let nonce = hkdf(salt, &ikm, &[b"Content-Encoding: nonce\0"], 12)?;
    Ok((cek, nonce))
}

/// Тело запроса `aes128gcm`: соль, размер записи, открытый ключ сервера и одна зашифрованная запись.
fn encrypt(payload: &[u8], ua_public: &[u8], auth: &[u8]) -> Result<Vec<u8>, ring::error::Unspecified> {
    let rng = SystemRandom::new();
    let mut salt = [0u8; 16];
    rng.fill(&mut salt)?;
    let ephemeral = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng)?;
    let as_public = ephemeral.compute_public_key()?;
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, ua_public);
    let ecdh = agreement::agree_ephemeral(ephemeral, &peer, |s| s.to_vec())?;
    let (cek, nonce) = derive(&ecdh, auth, ua_public, as_public.as_ref(), &salt)?;
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek)?);
    let mut record = payload.to_vec();
    record.push(2); // разделитель последней записи
    key.seal_in_place_append_tag(aead::Nonce::try_assume_unique_for_key(&nonce)?, aead::Aad::empty(), &mut record)?;
    let mut body = Vec::with_capacity(86 + record.len());
    body.extend_from_slice(&salt);
    body.extend_from_slice(&4096u32.to_be_bytes());
    body.push(u8::try_from(as_public.as_ref().len()).map_err(|_| ring::error::Unspecified)?);
    body.extend_from_slice(as_public.as_ref());
    body.extend_from_slice(&record);
    Ok(body)
}

/// Заголовок `Authorization: vapid t=…, k=…` (RFC 8292) для службы push `endpoint`.
fn vapid(pair: &EcdsaKeyPair, public: &str, endpoint: &str, contact: &str) -> Result<String, String> {
    let u = url::Url::parse(endpoint).map_err(|e| e.to_string())?;
    let aud = u.origin().ascii_serialization();
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() + 12 * 3600;
    let sub = if contact.is_empty() { "mailto:admin@localhost" } else { contact };
    let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = B64.encode(json!({"aud": aud, "exp": exp, "sub": sub}).to_string());
    let input = format!("{header}.{claims}");
    let sig = pair.sign(&SystemRandom::new(), input.as_bytes()).map_err(|_| "VAPID signing failed".to_string())?;
    Ok(format!("vapid t={input}.{}, k={public}", B64.encode(sig.as_ref())))
}

// ------------------------------------------------------------------ Отправка

/// Текст уведомления на языке сотрудника.
fn message(lang: Lang, kind: &str, payload: &Value, show_text: bool) -> Value {
    let title = l10n::tr_to(lang, "Пульт: новое уведомление").into_owned();
    let mut body = String::new();
    if show_text {
        let num = payload["number"].as_i64().map(|n| format!("#{n} ")).unwrap_or_default();
        let text = ["subject", "title", "name"].iter().find_map(|k| payload[*k].as_str()).unwrap_or_default();
        body = format!("{num}{}", text.chars().take(120).collect::<String>()).trim().to_string();
    }
    if body.is_empty() {
        body = l10n::tr_to(lang, "Откройте портал, чтобы прочитать.").into_owned();
    }
    json!({"title": title, "body": body, "kind": kind, "url": "/notifications"})
}

type Sub = (Uuid, String, Vec<u8>, Vec<u8>);
/// Подписка вместе с сотрудником: id, сотрудник, адрес, p256dh, auth.
type UserSub = (Uuid, Uuid, String, Vec<u8>, Vec<u8>);

async fn send_one(
    client: &reqwest::Client,
    (pair, public): (&EcdsaKeyPair, &str),
    contact: &str,
    sub: &Sub,
    msg: &Value,
) -> Result<(), (Option<u16>, String)> {
    let (_, endpoint, p256dh, auth) = sub;
    let body = encrypt(msg.to_string().as_bytes(), p256dh, auth)
        .map_err(|_| (None, "aes128gcm encryption failed".to_string()))?;
    let auth_header = vapid(pair, public, endpoint, contact).map_err(|e| (None, e))?;
    let r = client
        .post(endpoint)
        .header("Authorization", auth_header)
        .header("TTL", TTL_SECS.to_string())
        .header("Urgency", "normal")
        .header("Content-Encoding", "aes128gcm")
        .header("Content-Type", "application/octet-stream")
        .body(body)
        .send()
        .await
        .map_err(|e| (None, e.to_string()))?;
    if r.status().is_success() { Ok(()) } else { Err((Some(r.status().as_u16()), r.status().to_string())) }
}

fn client() -> Result<reqwest::Client, ApiError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(ApiError::internal)
}

/// Записать итог отправки: успех сбрасывает счётчик; 404/410 (подписка отозвана) и много сбоев подряд
/// удаляют подписку.
async fn record(core: &Core, id: Uuid, r: &Result<(), (Option<u16>, String)>) -> Result<(), sqlx::Error> {
    match r {
        Ok(()) => {
            sqlx::query(
                "UPDATE push_subscriptions SET last_ok_at = now(), failures = 0, last_error = NULL WHERE id = $1",
            )
            .bind(id)
            .execute(&core.db)
            .await?;
        }
        Err((Some(404 | 410), _)) => {
            sqlx::query("DELETE FROM push_subscriptions WHERE id = $1").bind(id).execute(&core.db).await?;
        }
        Err((_, e)) => {
            sqlx::query(
                "WITH u AS (UPDATE push_subscriptions SET failures = failures + 1, last_error = left($2, 300)
                             WHERE id = $1 RETURNING id, failures)
                 DELETE FROM push_subscriptions p USING u WHERE p.id = u.id AND u.failures >= $3",
            )
            .bind(id)
            .bind(e)
            .bind(MAX_FAILURES)
            .execute(&core.db)
            .await?;
        }
    }
    Ok(())
}

/// Разослать новые уведомления на устройства. Уведомления забираются атомарно (`SKIP LOCKED`): несколько
/// копий сервера не пришлют одно уведомление дважды. Выключено — новые уведомления просто помечаются.
pub async fn deliver(core: &Core) -> ApiResult<u64> {
    let cfg: PushSettings = settings::load(&core.db).await?;
    let rows: Vec<(Uuid, String, Value, bool, String)> = sqlx::query_as(
        "WITH batch AS (SELECT id FROM notifications WHERE pushed_at IS NULL ORDER BY created_at LIMIT 500
                        FOR UPDATE SKIP LOCKED),
              done AS (UPDATE notifications n SET pushed_at = now() FROM batch WHERE n.id = batch.id
                       RETURNING n.user_id, n.kind, n.payload, n.created_at)
         SELECT d.user_id, d.kind, d.payload, d.created_at > now() - make_interval(mins => $1), u.locale
           FROM done d JOIN users u ON u.id = d.user_id AND u.is_active
          -- «Не беспокоить»: уведомление остаётся в колокольчике, на устройство не уходит.
          WHERE NOT EXISTS (SELECT 1 FROM user_profiles p WHERE p.user_id = d.user_id AND p.presence = 'dnd')",
    )
    .bind(FRESH_MINUTES)
    .fetch_all(&core.db)
    .await?;
    if !cfg.enabled {
        return Ok(0);
    }
    let rows: Vec<_> = rows.into_iter().filter(|r| r.3).collect();
    if rows.is_empty() {
        return Ok(0);
    }
    let users: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
    let subs: Vec<UserSub> =
        sqlx::query_as("SELECT id, user_id, endpoint, p256dh, auth FROM push_subscriptions WHERE user_id = ANY($1)")
            .bind(&users)
            .fetch_all(&core.db)
            .await?;
    if subs.is_empty() {
        return Ok(0);
    }
    let (pair, public) = keys(core).await?;
    let client = client()?;
    let mut sent = 0;
    for (user, kind, payload, _, locale) in &rows {
        let msg = message(Lang::parse(locale), kind, payload, cfg.show_text);
        for (id, _, endpoint, p256dh, auth) in subs.iter().filter(|s| s.1 == *user) {
            let sub = (*id, endpoint.clone(), p256dh.clone(), auth.clone());
            let r = send_one(&client, (&pair, &public), &cfg.contact, &sub, &msg).await;
            if r.is_ok() {
                sent += 1;
            } else if let Err((code, e)) = &r {
                tracing::info!(subscription = %id, status = ?code, error = %e, "уведомление на устройство не доставлено");
            }
            record(core, *id, &r).await?;
        }
    }
    Ok(sent)
}

// ------------------------------------------------------------------ API

/// Для браузера: включены ли уведомления на устройства и открытый ключ портала для подписки.
#[derive(Serialize, ToSchema)]
pub struct PushConfig {
    pub enabled: bool,
    /// Открытый ключ VAPID (base64url), `applicationServerKey` подписки.
    pub public_key: Option<String>,
    /// Сколько устройств этого сотрудника подписано.
    pub devices: i64,
}

#[utoipa::path(get, path = "/api/me/push", tag = "auth", responses((status = 200, body = PushConfig)))]
pub(crate) async fn get_my_push(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<PushConfig>> {
    let cfg: PushSettings = settings::load(&core.db).await?;
    let devices: i64 = sqlx::query_scalar("SELECT count(*) FROM push_subscriptions WHERE user_id = $1")
        .bind(user.user_id)
        .fetch_one(&core.db)
        .await?;
    let public_key = if cfg.enabled { Some(keys(&core).await?.1) } else { None };
    Ok(Json(PushConfig { enabled: cfg.enabled, public_key, devices }))
}

/// Подписка браузера (`PushSubscription.toJSON()`).
#[derive(Deserialize, ToSchema)]
pub struct PushSubscriptionInput {
    pub endpoint: String,
    pub keys: PushSubscriptionKeys,
}

#[derive(Deserialize, ToSchema)]
pub struct PushSubscriptionKeys {
    /// Открытый ключ браузера, base64url.
    pub p256dh: String,
    /// Секрет аутентификации, base64url.
    pub auth: String,
}

fn bad_subscription() -> ApiError {
    ApiError::bad_request(
        "invalid_subscription",
        "Подписка браузера не подходит: нужен адрес известной службы push и ключи.",
    )
}

/// Подписать это устройство.
#[utoipa::path(post, path = "/api/me/push", tag = "auth", request_body = PushSubscriptionInput,
    responses((status = 204), (status = 400, body = pult_core::error::ErrorBody), (status = 403)))]
pub(crate) async fn subscribe_my_push(
    State(core): State<Core>,
    user: CurrentUser,
    headers: HeaderMap,
    Json(b): Json<PushSubscriptionInput>,
) -> ApiResult<StatusCode> {
    if pult_core::session::via_api_token(&headers) {
        return Err(ApiError::Forbidden);
    }
    let cfg: PushSettings = settings::load(&core.db).await?;
    if !cfg.enabled {
        return Err(ApiError::Forbidden);
    }
    let decode = |s: &str| B64.decode(s.trim_end_matches('=')).ok();
    let (Some(p256dh), Some(auth)) = (decode(&b.keys.p256dh), decode(&b.keys.auth)) else {
        return Err(bad_subscription());
    };
    if !endpoint_ok(&b.endpoint) || p256dh.len() != 65 || p256dh.first() != Some(&4) || auth.len() != 16 {
        return Err(bad_subscription());
    }
    let ua = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().take(300).collect::<String>());
    // Тот же адрес у другого сотрудника (общий компьютер) — подписка переходит к вошедшему.
    sqlx::query(
        "INSERT INTO push_subscriptions (user_id, endpoint, p256dh, auth, user_agent) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (endpoint) DO UPDATE SET user_id = excluded.user_id, p256dh = excluded.p256dh,
             auth = excluded.auth, user_agent = excluded.user_agent, failures = 0, last_error = NULL",
    )
    .bind(user.user_id)
    .bind(&b.endpoint)
    .bind(p256dh)
    .bind(auth)
    .bind(ua)
    .execute(&core.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct PushEndpoint {
    pub endpoint: String,
}

/// Отписать устройство.
#[utoipa::path(delete, path = "/api/me/push", tag = "auth", request_body = PushEndpoint, responses((status = 204)))]
pub(crate) async fn unsubscribe_my_push(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<PushEndpoint>,
) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM push_subscriptions WHERE user_id = $1 AND endpoint = $2")
        .bind(user.user_id)
        .bind(&b.endpoint)
        .execute(&core.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
pub struct PushTestResult {
    pub sent: u64,
    pub failed: u64,
}

/// Пробное уведомление — не чаще раза в полминуты: иначе кнопкой можно засыпать службу push запросами.
const TEST_EVERY: std::time::Duration = std::time::Duration::from_secs(30);
static LAST_TEST: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<uuid::Uuid, std::time::Instant>>> =
    std::sync::LazyLock::new(Default::default);

fn test_allowed(user: uuid::Uuid) -> bool {
    let now = std::time::Instant::now();
    let mut last = LAST_TEST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    last.retain(|_, t| now.duration_since(*t) < TEST_EVERY);
    if last.contains_key(&user) {
        return false;
    }
    last.insert(user, now);
    true
}

/// Проверка: пробное уведомление на все устройства сотрудника.
#[utoipa::path(post, path = "/api/me/push/test", tag = "auth", responses((status = 200, body = PushTestResult), (status = 403)))]
pub(crate) async fn test_my_push(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<PushTestResult>> {
    let cfg: PushSettings = settings::load(&core.db).await?;
    if !cfg.enabled {
        return Err(ApiError::Forbidden);
    }
    if !test_allowed(user.user_id) {
        return Err(ApiError::TooManyRequests {
            message: "Пробное уведомление уже отправлено: повторить можно через полминуты.".into(),
            retry_after_secs: TEST_EVERY.as_secs(),
        });
    }
    let subs: Vec<Sub> =
        sqlx::query_as("SELECT id, endpoint, p256dh, auth FROM push_subscriptions WHERE user_id = $1 LIMIT 20")
            .bind(user.user_id)
            .fetch_all(&core.db)
            .await?;
    let (pair, public) = keys(&core).await?;
    let client = client()?;
    let msg = json!({
        "title": l10n::tr("Пульт: проверка уведомлений").into_owned(),
        "body": l10n::tr("Уведомления на это устройство работают.").into_owned(),
        "kind": "push.test",
        "url": "/profile",
    });
    let (mut sent, mut failed) = (0, 0);
    for s in &subs {
        let r = send_one(&client, (&pair, &public), &cfg.contact, s, &msg).await;
        if r.is_ok() {
            sent += 1
        } else {
            failed += 1
        }
        record(&core, s.0, &r).await?;
    }
    Ok(Json(PushTestResult { sent, failed }))
}

#[utoipa::path(get, path = "/api/admin/push", tag = "admin", responses((status = 200, body = PushSettings)))]
pub(crate) async fn get_push_settings(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<PushSettings>> {
    user.require(&perm::SETTINGS_MANAGE)?;
    Ok(Json(settings::load(&core.db).await?))
}

#[utoipa::path(put, path = "/api/admin/push", tag = "admin", request_body = PushSettings,
    responses((status = 200, body = PushSettings), (status = 400, body = pult_core::error::ErrorBody)))]
pub(crate) async fn put_push_settings(
    State(core): State<Core>,
    user: CurrentUser,
    Json(mut b): Json<PushSettings>,
) -> ApiResult<Json<PushSettings>> {
    user.require(&perm::SETTINGS_MANAGE)?;
    b.contact = b.contact.trim().to_string();
    if !contact_ok(&b.contact) {
        return Err(ApiError::bad_request("invalid_settings", "Контакт — mailto:адрес или https://…"));
    }
    let mut tx = core.db.begin().await?;
    let before: PushSettings = settings::load(&mut *tx).await?;
    settings::save(&mut *tx, &b, Some(user.user_id)).await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("settings.update", Source::Portal).object("settings", "push").details(json!({
            "enabled": {"old": before.enabled, "new": b.enabled},
            "show_text": {"old": before.show_text, "new": b.show_text},
            "contact": {"old": before.contact, "new": b.contact},
        })),
    )
    .await?;
    tx.commit().await?;
    if b.enabled {
        keys(&core).await?;
    }
    Ok(Json(b))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Браузер расшифровывает то, что зашифровал портал: ключ браузера — настоящий ECDH P-256, разбор
    /// заголовка и вывод ключей — по RFC 8188/8291.
    #[test]
    fn browser_can_decrypt() {
        let rng = SystemRandom::new();
        let ua = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).unwrap();
        let ua_public = ua.compute_public_key().unwrap().as_ref().to_vec();
        let mut auth = [0u8; 16];
        rng.fill(&mut auth).unwrap();
        let body = encrypt(br#"{"title":"t"}"#, &ua_public, &auth).unwrap();
        let (salt, rest) = body.split_at(16);
        assert_eq!(&rest[..4], &4096u32.to_be_bytes());
        let idlen = usize::from(rest[4]);
        assert_eq!(idlen, 65);
        let (as_public, record) = rest[5..].split_at(idlen);
        let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, as_public);
        let ecdh = agreement::agree_ephemeral(ua, &peer, |s| s.to_vec()).unwrap();
        let (cek, nonce) = derive(&ecdh, &auth, &ua_public, as_public, salt).unwrap();
        let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).unwrap());
        let mut buf = record.to_vec();
        let plain = key
            .open_in_place(aead::Nonce::try_assume_unique_for_key(&nonce).unwrap(), aead::Aad::empty(), &mut buf)
            .unwrap();
        assert_eq!(plain, b"{\"title\":\"t\"}\x02");
    }

    #[test]
    fn vapid_token_is_signed_es256() {
        let rng = SystemRandom::new();
        let doc = EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, doc.as_ref(), &rng).unwrap();
        let public = B64.encode(pair.public_key().as_ref());
        let h = vapid(&pair, &public, "https://fcm.googleapis.com/fcm/send/abc", "mailto:it@example.test").unwrap();
        let t = h.strip_prefix("vapid t=").unwrap().split(", k=").next().unwrap();
        let (input, sig) = t.rsplit_once('.').unwrap();
        let claims: Value = serde_json::from_slice(&B64.decode(input.split('.').nth(1).unwrap()).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://fcm.googleapis.com");
        assert_eq!(claims["sub"], "mailto:it@example.test");
        let key = signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, pair.public_key().as_ref());
        key.verify(input.as_bytes(), &B64.decode(sig).unwrap()).unwrap();
        assert!(h.ends_with(&format!("k={public}")));
    }

    #[test]
    fn only_known_push_services() {
        assert!(endpoint_ok("https://fcm.googleapis.com/fcm/send/x"));
        assert!(endpoint_ok("https://updates.push.services.mozilla.com/wpush/v2/x"));
        assert!(endpoint_ok("https://web.push.apple.com/x"));
        assert!(endpoint_ok("https://wns2-db5p.notify.windows.com/w/?token=x"));
        assert!(!endpoint_ok("http://fcm.googleapis.com/x"), "только https");
        assert!(!endpoint_ok("https://10.0.0.5/x"), "внутренняя сеть");
        assert!(!endpoint_ok("https://evilpush.apple.com.attacker.test/x"));
        assert!(!endpoint_ok("https://fcm.googleapis.com:8443/x"));
        assert!(contact_ok("") && contact_ok("mailto:it@example.test") && !contact_ok("it@example.test"));
    }

    #[test]
    fn message_hides_text_unless_allowed() {
        let p = json!({"number": 1300, "subject": "Не печатает принтер"});
        assert_eq!(message(Lang::Ru, "ticket.reply", &p, true)["body"], "#1300 Не печатает принтер");
        assert_eq!(message(Lang::Ru, "ticket.reply", &p, false)["body"], "Откройте портал, чтобы прочитать.");
        assert_eq!(message(Lang::Ru, "x", &json!({}), true)["body"], "Откройте портал, чтобы прочитать.");
    }
}
