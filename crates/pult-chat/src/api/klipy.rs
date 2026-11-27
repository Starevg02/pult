//! Поиск GIF в KLIPY (klipy.com). По правилам KLIPY запросы и сами анимации идут из браузера сотрудника
//! прямо к KLIPY: портал не проксирует и не хранит их копии. Портал выдаёт браузеру ключ приложения и
//! обезличенный номер сотрудника, а в переписке хранит ссылку на анимацию (зашифрованной, как текст).
//! Поиск включается ключом в настройках «Сообщений»; пока ключа нет — к KLIPY никто не обращается,
//! и политика CSP их адресов не пропускает.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Адреса, с которых KLIPY отдаёт анимации (раздел Network Requirements их документации).
pub const KLIPY_MEDIA: [&str; 3] =
    ["https://static.klipy.com/", "https://static1.klipy.com/", "https://static2.klipy.com/"];
/// Адрес API KLIPY: к нему обращается браузер.
pub const KLIPY_API: &str = "https://api.klipy.com";
/// Предел длины ссылки на анимацию.
const URL_MAX: usize = 500;
/// Предел подписи GIF (её показывают как альтернативный текст).
const TITLE_MAX: usize = 200;
/// Предел стороны анимации в пикселях.
const SIDE_MAX: u32 = 4096;

/// Включён ли поиск KLIPY и когда это проверено: `(секунды Unix << 2) | состояние`, где состояние 0 — ещё не
/// читали настройки, 1 — нет, 2 — да.
static KLIPY_STATE: AtomicU64 = AtomicU64::new(0);
/// Через сколько секунд перечитать настройки: ключ могли убрать в другой копии сервера или восстановлением базы.
const RECHECK_SECS: u64 = 30;

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
{
    OpenApiRouter::new().routes(routes!(klipy_config)).routes(routes!(klipy_send))
}

/// Запомнить, включён ли поиск KLIPY (после сохранения настроек).
pub(super) fn remember(on: bool) {
    KLIPY_STATE.store((now_secs() << 2) | if on { 2 } else { 1 }, Ordering::Relaxed);
}

/// Включён ли поиск KLIPY — для политики CSP на каждом ответе, поэтому настройки перечитываются не чаще раза
/// в [`RECHECK_SECS`]. Отправка GIF проверяет сами настройки, а не это значение.
pub async fn klipy_enabled(core: &Core) -> bool {
    let v = KLIPY_STATE.load(Ordering::Relaxed);
    if v & 3 != 0 && now_secs().saturating_sub(v >> 2) < RECHECK_SECS {
        return v & 3 == 2;
    }
    match settings::load::<ChatSettings>(&core.db).await {
        Ok(cfg) => {
            remember(!cfg.klipy_key.is_empty());
            !cfg.klipy_key.is_empty()
        }
        // База недоступна — без внешних адресов; прочитаем в следующий раз.
        Err(_) => false,
    }
}

/// Ключ приложения KLIPY: латиница, цифры, `-` и `_`.
pub(super) fn valid_key(key: &str) -> bool {
    (8..=128).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Ссылка на анимацию: только серверы KLIPY и только безопасные знаки (её вставляют в `<img src>`).
pub(super) fn valid_url(url: &str) -> bool {
    let Some(rest) = KLIPY_MEDIA.iter().find_map(|p| url.strip_prefix(p)) else { return false };
    url.len() <= URL_MAX
        && !rest.is_empty()
        && !rest.contains("..")
        && rest.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._~%-?&=+".contains(&b))
}

pub(super) fn valid_slug(slug: &str) -> bool {
    (1..=200).contains(&slug.len()) && slug.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Подпись без управляющих знаков и подмены направления текста, не длиннее [`TITLE_MAX`].
fn clean_gif_title(s: &str) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control() && !pult_core::files::is_spoofing(*c)).collect();
    clean.trim().chars().take(TITLE_MAX).collect()
}

/// GIF из базы годится для показа: ссылка по-прежнему только на серверы KLIPY (проверка и при чтении — на случай
/// правки базы или импорта).
pub(super) fn shown_gif(g: ChatKlipyGif) -> Option<ChatKlipyGif> {
    (valid_url(&g.url) && valid_slug(&g.slug)).then_some(g)
}

/// Что нужно браузеру для поиска KLIPY.
#[derive(Serialize, ToSchema)]
pub struct ChatKlipyConfig {
    /// Ключ приложения KLIPY (по их правилам он и так виден в запросах браузера).
    pub app_key: String,
    /// Обезличенный постоянный номер сотрудника для KLIPY: без ключа портала его не вычислить и не связать
    /// с учёткой.
    pub customer_id: String,
}

/// GIF из KLIPY в сообщении.
#[derive(Serialize, Deserialize, Clone, ToSchema)]
pub struct ChatKlipyGif {
    pub slug: String,
    pub title: String,
    /// Анимация на серверах KLIPY.
    pub url: String,
    pub width: u32,
    pub height: u32,
}

/// Отправить GIF из KLIPY.
#[derive(Deserialize, ToSchema)]
pub struct ChatKlipySend {
    #[serde(flatten)]
    pub gif: ChatKlipyGif,
    /// Ответ на сообщение этого чата.
    #[serde(default)]
    pub reply_to: Option<Uuid>,
}

/// Поиск GIF в KLIPY для браузера; 404 — поиск выключен (ключа нет).
#[utoipa::path(get, path = "/api/chat/klipy", tag = "chat",
    responses((status = 200, body = ChatKlipyConfig), (status = 404, body = pult_core::error::ErrorBody)))]
async fn klipy_config(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<ChatKlipyConfig>> {
    let cfg: ChatSettings = settings::load(&core.db).await?;
    remember(!cfg.klipy_key.is_empty());
    if cfg.klipy_key.is_empty() {
        return Err(ApiError::NotFound);
    }
    let customer_id = core.secrets.pseudonym("klipy-customer", user.user_id.as_bytes());
    Ok(Json(ChatKlipyConfig { app_key: cfg.klipy_key, customer_id }))
}

/// Отправить GIF из KLIPY: сообщение без текста со ссылкой на анимацию. Анимацию браузеры участников
/// грузят с серверов KLIPY; если её там уберут, в переписке останется подпись.
#[utoipa::path(post, path = "/api/chat/{id}/klipy", tag = "chat", params(("id" = Uuid, Path)), request_body = ChatKlipySend,
    responses((status = 201, body = ChatMessage), (status = 400, body = pult_core::error::ErrorBody),
              (status = 404, body = pult_core::error::ErrorBody)))]
async fn klipy_send(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ChatKlipySend>,
) -> ApiResult<(StatusCode, Json<ChatMessage>)> {
    let cfg: ChatSettings = settings::load(&core.db).await?;
    remember(!cfg.klipy_key.is_empty());
    if cfg.klipy_key.is_empty() {
        return Err(ApiError::NotFound);
    }
    let m = membership(&core.db, id, user.user_id).await?;
    if !m.active() {
        return Err(ApiError::Refused {
            code: "read_only",
            message: "Вы вышли из чата — история доступна только для чтения.".into(),
        });
    }
    m.require_unlocked()?;
    let g = ChatKlipyGif { title: clean_gif_title(&b.gif.title), ..b.gif };
    if !valid_slug(&g.slug)
        || !valid_url(&g.url)
        || !(1..=SIDE_MAX).contains(&g.width)
        || !(1..=SIDE_MAX).contains(&g.height)
    {
        return Err(ApiError::bad_request("invalid_gif", "Этот GIF не получается отправить."));
    }
    gifs::check_reply(&core, id, b.reply_to).await?;
    gifs::send_allowed(user.user_id)?;
    let blob = encrypt(&core, &serde_json::to_string(&g).map_err(|e| ApiError::Internal(e.to_string()))?)?;
    let mut tx = core.db.begin().await?;
    let mid: Uuid = sqlx::query_scalar(
        "INSERT INTO chat_messages (chat_id, author_id, body, reply_to, gif) VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(id)
    .bind(user.user_id)
    .bind(encrypt(&core, "")?)
    .bind(b.reply_to)
    .bind(blob)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("UPDATE chats SET last_message_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
    sqlx::query("UPDATE chat_members SET last_read_at = now() WHERE chat_id = $1 AND user_id = $2")
        .bind(id)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    changed(&mut tx, id, &[]).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(one_message(&core, id, mid, user.user_id, true).await?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_only_from_klipy() {
        assert!(valid_url("https://static.klipy.com/ii/935d7ab9/14/af/um0L4dFH.gif"));
        assert!(valid_url("https://static2.klipy.com/ii/a/b.webp?x=1&y=2"));
        assert!(!valid_url("https://static.klipy.com/"));
        assert!(!valid_url("https://evil.example/static.klipy.com/a.gif"));
        assert!(!valid_url("https://static.klipy.com.evil.example/a.gif"));
        assert!(!valid_url("http://static.klipy.com/a.gif"));
        assert!(!valid_url("https://static.klipy.com/a\".gif"));
        assert!(!valid_url("https://static.klipy.com/../a.gif"));
        assert!(!valid_url("https://static.klipy.com/a b.gif"));
        assert!(!valid_url("javascript:alert(1)"));
    }

    #[test]
    fn keys_and_slugs() {
        assert!(valid_key("abcDEF123_-xyz"));
        assert!(!valid_key("short"));
        assert!(!valid_key("has space in key"));
        assert!(!valid_key("ключ-кириллицей"));
        assert!(valid_slug("hello-hi-662"));
        assert!(!valid_slug(""));
        assert!(!valid_slug("a/b"));
        assert_eq!(clean_gif_title("  Hi\u{0}\nthere "), "Hithere");
        assert_eq!(clean_gif_title("gif\u{202E}fig"), "giffig");
    }
}
