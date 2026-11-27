//! API модуля «Коммуникации». Доступ ко всему решает членство: чужой чат для сотрудника
//! не существует (404), а администраторские маршруты не отдают ни текста, ни участников личных диалогов.

use crate::kind;
use axum::{
    Json,
    extract::{DefaultBodyLimit, FromRef, Multipart, Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Secret,
    audit::{self, Entry, Source},
    events::{self, Event},
    notify,
    settings::{self, SettingsGroup},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgExecutor, Postgres, Transaction};
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

mod gifs;
pub use gifs::{ChatGif, ChatGifFromFile, ChatGifSend, GIF_MAX_MB, GIFS_MAX};

/// Сколько минут автор может править сообщение.
pub const EDIT_MINUTES: i64 = 15;
/// Длина сообщения в символах.
const MAX_BODY: usize = 4000;
/// Длина названия группового чата.
const MAX_TITLE: usize = 200;
/// Участников за один раз.
const MAX_ADD: usize = 500;
/// Сообщений за один запрос.
const PAGE: i64 = 100;
/// Вложений в одном сообщении.
pub const MAX_FILES: usize = 10;
/// Неприкреплённые вложения пользователя за сутки: без квоты один сотрудник заполнил бы базу.
const PENDING_MAX_FILES: i64 = 30;
const PENDING_MAX_MB: i64 = 300;
/// Всего загружено одним сотрудником за сутки (и отправленное тоже).
const DAILY_MAX_MB: i64 = 1024;
/// Тело загрузки: предел настройки (до 100 МБ) и запас на заголовки формы.
const UPLOAD_BODY_LIMIT: usize = 105 * 1024 * 1024;
/// Файл держится в памяти, пока шифруется и пишется: одновременно — не больше стольких загрузок.
static UPLOADS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
/// Скачивание тоже расшифровывает файл целиком в памяти.
static DOWNLOADS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(8);
/// Обезличенная статистика: меньшие числа не показываются, чтобы по ним нельзя было узнать переписку.
const MIN_COUNT: i64 = 5;
/// Реакции — только из этого списка.
pub const REACTIONS: [&str; 12] = ["👍", "❤️", "😂", "😮", "😢", "🙏", "👎", "🔥", "🎉", "👌", "✅", "🤝"];
/// Имён под реакцией.
const REACTION_NAMES: i32 = 10;
/// Поиск по переписке расшифровывает не больше стольких последних сообщений.
const SEARCH_SCAN: i64 = 5000;
/// Найденных сообщений.
const SEARCH_LIMIT: usize = 50;
/// «Печатает…» от одного сотрудника в одном чате — не чаще раза в 3 секунды.
const TYPING_EVERY: Duration = Duration::from_secs(3);
/// Когда последний раз ушло «печатает…» (на этой копии сервера).
static TYPING: LazyLock<Mutex<HashMap<(Uuid, Uuid), Instant>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// Поиск расшифровывает тысячи сообщений: одновременно — не больше стольких поисков на сервере.
static SEARCHES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
/// Поисков от одного сотрудника в минуту.
const SEARCHES_PER_MINUTE: u32 = 20;
static SEARCHED: LazyLock<Mutex<HashMap<Uuid, (Instant, u32)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// Реакций (поставить или снять) от одного сотрудника в минуту: каждая рассылает событие участникам.
const REACTIONS_PER_MINUTE: u32 = 30;
static REACTED: LazyLock<Mutex<HashMap<Uuid, (Instant, u32)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Не больше `limit` действий сотрудника за минуту (счёт на этой копии сервера).
fn within_minute(map: &Mutex<HashMap<Uuid, (Instant, u32)>>, user: Uuid, limit: u32) -> bool {
    const MINUTE: Duration = Duration::from_secs(60);
    let mut seen = map.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let now = Instant::now();
    if seen.len() > 10_000 {
        seen.retain(|_, (at, _)| now.duration_since(*at) < MINUTE);
    }
    let e = seen.entry(user).or_insert((now, 0));
    if now.duration_since(e.0) >= MINUTE {
        *e = (now, 0);
    }
    e.1 += 1;
    e.1 <= limit
}

fn slow_down() -> ApiError {
    ApiError::TooManyRequests {
        message: "Слишком часто. Повторите через минуту.".into(), retry_after_secs: 60
    }
}

/// Настройки модуля (админка): срок хранения вложений.
#[derive(Serialize, Deserialize, Default, ToSchema)]
pub struct ChatSettings {
    /// Через сколько дней стирать вложения сообщений; 0 — хранить всегда.
    #[serde(default)]
    pub file_retention_days: u32,
}

impl SettingsGroup for ChatSettings {
    const KEY: &'static str = "chat";
}

/// Предел срока хранения вложений — 10 лет.
const MAX_RETENTION_DAYS: u32 = 3650;

/// Имя сотрудника по учётке: ФИО из справочника, иначе отображаемое имя или логин из AD.
const NAME: &str =
    "coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name)), ''), a.display_name, a.sam_account_name)";
/// Соединение `u` (users) с учёткой AD и карточкой сотрудника для [`NAME`].
const PEOPLE: &str = "JOIN ad_accounts a ON a.id = u.ad_account_id LEFT JOIN persons p ON p.id = u.person_id";

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(chat_list))
        .routes(routes!(chat_people))
        .routes(routes!(chat_open_direct))
        .routes(routes!(chat_create_group))
        .routes(routes!(chat_get))
        .routes(routes!(chat_rename))
        .routes(routes!(chat_set_prefs))
        .routes(routes!(chat_mark_read))
        .routes(routes!(chat_typing))
        .routes(routes!(chat_pin))
        .routes(routes!(chat_search))
        .routes(routes!(chat_shared_files))
        .routes(routes!(chat_react))
        .routes(routes!(chat_forward))
        .routes(routes!(chat_list_messages, chat_send_message))
        .merge(OpenApiRouter::new().routes(routes!(chat_upload_file)).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)))
        .routes(routes!(chat_get_file))
        .routes(routes!(chat_edit_message))
        .routes(routes!(chat_message_versions))
        .routes(routes!(chat_add_members))
        .routes(routes!(chat_remove_member))
        .routes(routes!(chat_set_role))
        .routes(routes!(chat_leave))
        .routes(routes!(chat_member_events))
        .routes(routes!(chat_admin_list))
        .routes(routes!(chat_admin_assign_owner))
        .routes(routes!(chat_admin_stats))
        .routes(routes!(chat_admin_storage))
        .routes(routes!(chat_admin_get_settings, chat_admin_put_settings))
        .merge(gifs::routes())
}

// ------------------------------------------------------------------ Типы

/// Строка списка переписок.
#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ChatItem {
    pub id: Uuid,
    /// `direct` или `group`.
    pub kind: String,
    /// Название группового чата или имя собеседника.
    pub title: String,
    /// Собеседник в личном диалоге.
    pub peer_id: Option<Uuid>,
    /// Моя роль: `owner`, `admin`, `member`.
    pub role: String,
    /// Я участник (не вышел и не исключён).
    pub active: bool,
    /// Чат временно заблокирован: владельца нет.
    pub locked: bool,
    /// Закреплён (только у действующих).
    pub pinned: bool,
    pub hidden: bool,
    /// Без звука: не считается в непрочитанном меню и не всплывает уведомлением.
    pub muted: bool,
    pub unread: i64,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_message_at: Option<OffsetDateTime>,
    /// Начало последнего доступного мне сообщения.
    #[sqlx(skip)]
    pub preview: Option<String>,
    /// Вложений в последнем сообщении (у сообщения только из файлов текста нет).
    pub preview_files: i64,
    #[serde(skip)]
    #[schema(ignore)]
    pub preview_body: Option<Vec<u8>>,
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ChatMember {
    pub user_id: Uuid,
    pub name: String,
    pub role: String,
}

#[derive(Serialize, ToSchema)]
pub struct ChatDetail {
    pub chat: ChatItem,
    /// Действующие участники (бывшему участнику не показываются).
    pub members: Vec<ChatMember>,
    /// Могу писать: участник и чат не заблокирован.
    pub can_write: bool,
    /// Могу вести состав: владелец или администратор группового чата.
    pub can_manage: bool,
    /// До какого момента мои сообщения прочитаны собеседником (в группе — хотя бы одним участником).
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub read_upto: Option<OffsetDateTime>,
    /// Закреплённое сообщение.
    pub pinned_message: Option<ChatQuote>,
    /// Могу закреплять: участник личного диалога, владелец или администратор группового чата.
    pub can_pin: bool,
}

/// Короткий вид сообщения: ответ, закреплённое.
#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ChatQuote {
    pub id: Uuid,
    pub author_name: String,
    /// Первая строка текста (до 100 знаков).
    #[sqlx(skip)]
    pub preview: String,
    /// Вложений в сообщении.
    pub files: i64,
    #[serde(skip)]
    #[schema(ignore)]
    pub enc: Vec<u8>,
}

/// Реакция под сообщением.
#[derive(Serialize, ToSchema)]
pub struct ChatReaction {
    pub emoji: String,
    pub count: i64,
    /// Есть моя.
    pub mine: bool,
    /// Кто поставил (первые 10).
    pub names: Vec<String>,
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ChatMessage {
    pub id: Uuid,
    pub author_id: Uuid,
    pub author_name: String,
    #[sqlx(skip)]
    pub body: String,
    #[serde(skip)]
    #[schema(ignore)]
    pub enc: Vec<u8>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub edited_at: Option<OffsetDateTime>,
    pub version: i32,
    /// Я могу его править сейчас.
    pub editable: bool,
    #[sqlx(skip)]
    pub files: Vec<ChatFile>,
    /// Ответ на сообщение этого же чата.
    pub reply_to: Option<Uuid>,
    #[sqlx(skip)]
    pub reply: Option<ChatQuote>,
    /// Переслано: автор исходного сообщения.
    pub forwarded_from_name: Option<String>,
    #[sqlx(skip)]
    pub reactions: Vec<ChatReaction>,
}

/// Вложение сообщения (или загруженное, но ещё не отправленное).
#[derive(Serialize, ToSchema)]
pub struct ChatFile {
    pub id: Uuid,
    pub name: String,
    pub mime: String,
    pub size: i64,
    /// Картинка: показывается в переписке, остальное скачивается.
    pub image: bool,
    /// Стёрт по сроку хранения.
    pub expired: bool,
}

/// Вложение в списке «Файлы чата».
#[derive(Serialize, ToSchema)]
pub struct ChatSharedFile {
    pub file: ChatFile,
    pub message_id: Uuid,
    pub author_name: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

#[derive(ToSchema)]
#[allow(dead_code)]
pub struct ChatUploadForm {
    #[schema(format = Binary, value_type = String)]
    file: Vec<u8>,
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ChatMessageVersion {
    pub version: i32,
    #[sqlx(skip)]
    pub body: String,
    #[serde(skip)]
    #[schema(ignore)]
    pub enc: Vec<u8>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ChatPerson {
    pub id: Uuid,
    pub name: String,
    pub department: Option<String>,
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ChatMemberEvent {
    pub user_name: String,
    pub actor_name: Option<String>,
    /// `created`, `added`, `removed`, `left`, `blocked`, `admin`, `member`, `owner`, `locked`, `unlocked`.
    pub action: String,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

/// Групповой чат для суперадминистратора — без сообщений.
#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct AdminChat {
    pub id: Uuid,
    pub title: String,
    pub members: i64,
    pub owner_name: Option<String>,
    pub locked: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

/// Обезличенная статистика за месяц по всему порталу. Число меньше 5 не показывается (`null`).
#[derive(Serialize, ToSchema)]
pub struct ChatStatsMonth {
    /// ГГГГ-ММ.
    pub month: String,
    pub messages: Option<i64>,
    pub direct_messages: Option<i64>,
    pub group_messages: Option<i64>,
    pub edited: Option<i64>,
    pub senders: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct ChatStats {
    pub direct_chats: Option<i64>,
    pub group_chats: Option<i64>,
    pub months: Vec<ChatStatsMonth>,
}

#[derive(Serialize, ToSchema)]
pub struct ChatCreated {
    pub id: Uuid,
}

#[derive(Deserialize, ToSchema)]
pub struct ChatDirectInput {
    pub user_id: Uuid,
}

#[derive(Deserialize, ToSchema)]
pub struct ChatGroupInput {
    pub title: String,
    #[serde(default)]
    pub members: Vec<Uuid>,
}

#[derive(Deserialize, ToSchema)]
pub struct ChatTitleInput {
    pub title: String,
}

#[derive(Deserialize, ToSchema)]
pub struct ChatPrefsInput {
    pub pinned: Option<bool>,
    pub hidden: Option<bool>,
    pub muted: Option<bool>,
}

#[derive(Deserialize, ToSchema)]
pub struct ChatPinInput {
    /// Какое сообщение закрепить; пусто — открепить.
    pub message: Option<Uuid>,
}

#[derive(Deserialize, ToSchema)]
pub struct ChatReactInput {
    /// Эмодзи из списка; повторное нажатие снимает реакцию.
    pub emoji: String,
}

#[derive(Deserialize, ToSchema)]
pub struct ChatForwardInput {
    /// Куда переслать.
    pub to: Uuid,
}

#[derive(Deserialize, IntoParams)]
pub struct ChatSearchQuery {
    pub q: String,
}

/// Объём переписки на портале (суперадминистратору), байты.
#[derive(Serialize, ToSchema)]
pub struct ChatStorage {
    pub messages: i64,
    pub message_bytes: i64,
    pub files: i64,
    pub file_bytes: i64,
    pub expired_files: i64,
    /// Таблицы переписки на диске вместе с индексами.
    pub chat_tables_bytes: i64,
    /// Вся база портала.
    pub database_bytes: i64,
    pub file_retention_days: u32,
}

#[derive(Deserialize, ToSchema)]
pub struct ChatMessageInput {
    /// Текст; может быть пустым, если есть вложения.
    #[serde(default)]
    pub body: String,
    /// Загруженные заранее вложения (до 10), только при отправке.
    #[serde(default)]
    pub files: Vec<Uuid>,
    /// Ответ на сообщение этого чата (только при отправке).
    #[serde(default)]
    pub reply_to: Option<Uuid>,
}

#[derive(Deserialize, ToSchema)]
pub struct ChatMembersInput {
    pub user_ids: Vec<Uuid>,
}

#[derive(Deserialize, ToSchema)]
pub struct ChatRoleInput {
    /// `admin` или `member`.
    pub role: String,
}

#[derive(Deserialize, ToSchema)]
pub struct ChatOwnerInput {
    pub user_id: Uuid,
    /// Что станет с прежним владельцем: `admin`, `member` или `remove`.
    pub previous: String,
}

#[derive(Deserialize, IntoParams)]
pub struct ChatListQuery {
    /// Скрытые бывшие чаты вместо основного списка.
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Deserialize, IntoParams)]
pub struct ChatPeopleQuery {
    #[serde(default)]
    pub q: String,
}

#[derive(Deserialize, IntoParams)]
pub struct ChatPageQuery {
    /// Сообщения раньше этого.
    pub before: Option<Uuid>,
}

// ------------------------------------------------------------------ Общие проверки

/// Моё участие в чате. Не участник — 404: чужой чат не существует.
#[derive(sqlx::FromRow)]
struct Membership {
    kind: String,
    title: Option<String>,
    locked: bool,
    role: String,
    left_at: Option<OffsetDateTime>,
}

impl Membership {
    fn active(&self) -> bool {
        self.left_at.is_none()
    }
    fn group(&self) -> bool {
        self.kind == "group"
    }
    fn manager(&self) -> bool {
        self.group() && self.active() && matches!(self.role.as_str(), "owner" | "admin")
    }
    fn require_manager(&self) -> ApiResult<()> {
        if self.manager() { self.require_unlocked() } else { Err(ApiError::Forbidden) }
    }
    fn require_unlocked(&self) -> ApiResult<()> {
        if self.locked {
            Err(ApiError::Refused {
                code: "chat_locked",
                message: "Чат временно недоступен: назначается новый владелец.".into(),
            })
        } else {
            Ok(())
        }
    }
}

async fn membership(db: impl PgExecutor<'_>, chat: Uuid, user: Uuid) -> ApiResult<Membership> {
    let row: Option<Membership> = sqlx::query_as(
        "SELECT c.kind, c.title, c.locked, m.role, m.left_at
           FROM chat_members m JOIN chats c ON c.id = m.chat_id
          WHERE m.chat_id = $1 AND m.user_id = $2",
    )
    .bind(chat)
    .bind(user)
    .fetch_optional(db)
    .await?;
    row.ok_or(ApiError::NotFound)
}

fn encrypt(core: &Core, text: &str) -> ApiResult<Vec<u8>> {
    core.secrets.encrypt(&Secret::new(text)).map_err(ApiError::internal)
}

fn decrypt(core: &Core, blob: &[u8]) -> ApiResult<String> {
    Ok(core.secrets.decrypt(blob).map_err(ApiError::internal)?.expose().to_string())
}

fn clean_body(s: &str) -> ApiResult<String> {
    let s: String = s.chars().filter(|c| !pult_core::files::is_spoofing(*c)).collect();
    let s = s.trim();
    if s.is_empty() {
        return Err(ApiError::bad_request("empty_message", "Сообщение пустое."));
    }
    if s.chars().count() > MAX_BODY {
        return Err(ApiError::bad_request("message_too_long", "Сообщение длиннее 4000 символов."));
    }
    Ok(s.to_string())
}

fn clean_title(s: &str) -> ApiResult<String> {
    let s = s.trim();
    if s.is_empty() || s.chars().count() > MAX_TITLE || s.chars().any(char::is_control) {
        return Err(ApiError::bad_request("invalid_title", "Название — от 1 до 200 символов, в одну строку."));
    }
    Ok(s.to_string())
}

/// Сотрудников в одном событии: `pg_notify` принимает до 8000 байт, uuid в JSON — около 40.
const NOTIFY_CHUNK: usize = 150;

/// «Что-то поменялось в чате» — действующим участникам; браузер перечитает через API.
/// Большая группа уходит несколькими событиями: одно на всех упёрлось бы в предел `pg_notify`.
pub(crate) async fn changed(conn: &mut sqlx::PgConnection, chat: Uuid, also: &[Uuid]) -> Result<(), sqlx::Error> {
    let users: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT u FROM (
            SELECT user_id AS u FROM chat_members WHERE chat_id = $1 AND left_at IS NULL
            UNION SELECT unnest($2::uuid[])) x",
    )
    .bind(chat)
    .bind(also)
    .fetch_all(&mut *conn)
    .await?;
    for part in users.chunks(NOTIFY_CHUNK) {
        sqlx::query("SELECT pg_notify($1, json_build_object('kind', 'chat', 'users', $2::uuid[], 'chat', $3)::text)")
            .bind(events::CHANNEL)
            .bind(part)
            .bind(chat)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// Другие действующие участники для лёгких событий («прочитано», «печатает»): в большой группе
/// (больше одного события) такие события не рассылаются.
async fn others_for_signal(db: impl PgExecutor<'_>, chat: Uuid, me: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    let v: Vec<Uuid> = sqlx::query_scalar(
        "SELECT user_id FROM chat_members WHERE chat_id = $1 AND left_at IS NULL AND user_id <> $2 LIMIT $3",
    )
    .bind(chat)
    .bind(me)
    .bind(NOTIFY_CHUNK as i64 + 1)
    .fetch_all(db)
    .await?;
    Ok(if v.len() > NOTIFY_CHUNK { Vec::new() } else { v })
}

async fn log_member(
    db: impl PgExecutor<'_>,
    chat: Uuid,
    user: Uuid,
    actor: Option<Uuid>,
    action: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO chat_member_events (chat_id, user_id, actor_id, action) VALUES ($1, $2, $3, $4)")
        .bind(chat)
        .bind(user)
        .bind(actor)
        .bind(action)
        .execute(db)
        .await?;
    Ok(())
}

/// Строки списка: мои переписки (`chat` — одна), с непрочитанным и последним доступным мне сообщением.
async fn chat_items(core: &Core, me: Uuid, hidden: Option<bool>, chat: Option<Uuid>) -> ApiResult<Vec<ChatItem>> {
    let sql = format!(
        "SELECT c.id, c.kind,
                CASE WHEN c.kind = 'group' THEN c.title
                     ELSE coalesce((SELECT {NAME} FROM chat_members o JOIN users u ON u.id = o.user_id {PEOPLE}
                                     WHERE o.chat_id = c.id AND o.user_id <> $1 LIMIT 1), '') END AS title,
                CASE WHEN c.kind = 'direct'
                     THEN (SELECT o.user_id FROM chat_members o WHERE o.chat_id = c.id AND o.user_id <> $1 LIMIT 1) END AS peer_id,
                m.role, m.left_at IS NULL AS active, c.locked,
                (m.pinned AND m.left_at IS NULL) AS pinned, m.hidden, m.muted,
                (SELECT count(*) FROM chat_messages x
                  WHERE x.chat_id = c.id AND x.author_id <> $1
                    AND x.created_at > coalesce(m.last_read_at, '-infinity')
                    AND x.created_at <= coalesce(m.left_at, 'infinity')) AS unread,
                last.created_at AS last_message_at,
                CASE WHEN c.locked THEN NULL ELSE last.body END AS preview_body,
                CASE WHEN c.locked OR last.id IS NULL THEN 0
                     ELSE (SELECT count(*) FROM chat_files f WHERE f.message_id = last.id) END AS preview_files
           FROM chat_members m
           JOIN chats c ON c.id = m.chat_id
           LEFT JOIN LATERAL (SELECT x.id, x.created_at, x.body FROM chat_messages x
                               WHERE x.chat_id = c.id AND x.created_at <= coalesce(m.left_at, 'infinity')
                               ORDER BY x.created_at DESC, x.id DESC LIMIT 1) last ON true
          WHERE m.user_id = $1
            AND ($2::boolean IS NULL OR m.hidden = $2)
            AND ($3::uuid IS NULL OR c.id = $3)
          ORDER BY (m.pinned AND m.left_at IS NULL) DESC, coalesce(last.created_at, m.joined_at) DESC"
    );
    let mut items: Vec<ChatItem> = sqlx::query_as(&sql).bind(me).bind(hidden).bind(chat).fetch_all(&core.db).await?;
    for i in &mut items {
        if let Some(b) = i.preview_body.take() {
            let text = decrypt(core, &b)?;
            let line = text.lines().next().unwrap_or_default();
            if !line.is_empty() {
                i.preview = Some(line.chars().take(120).collect());
            }
        }
    }
    Ok(items)
}

// ------------------------------------------------------------------ Список и создание

#[utoipa::path(get, path = "/api/chat", tag = "chat", params(ChatListQuery),
    responses((status = 200, body = Vec<ChatItem>)))]
async fn chat_list(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<ChatListQuery>,
) -> ApiResult<Json<Vec<ChatItem>>> {
    Ok(Json(chat_items(&core, user.user_id, Some(q.hidden), None).await?))
}

/// Сотрудники, с которыми можно начать диалог или кого добавить в чат (действующие учётки).
#[utoipa::path(get, path = "/api/chat/people", tag = "chat", params(ChatPeopleQuery),
    responses((status = 200, body = Vec<ChatPerson>)))]
async fn chat_people(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<ChatPeopleQuery>,
) -> ApiResult<Json<Vec<ChatPerson>>> {
    let q = q.q.trim().to_lowercase().replace('ё', "е");
    if q.chars().count() < 2 {
        return Ok(Json(Vec::new()));
    }
    let like = format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let sql = format!(
        "SELECT u.id, {NAME} AS name, d.name AS department
           FROM users u {PEOPLE} LEFT JOIN departments d ON d.id = p.department_id
          WHERE u.is_active AND u.id <> $1
            AND (replace(lower({NAME}), chr(1105), chr(1077)) LIKE $2 OR lower(a.sam_account_name) LIKE $2)
          ORDER BY 2 LIMIT 20"
    );
    Ok(Json(sqlx::query_as(&sql).bind(user.user_id).bind(like).fetch_all(&core.db).await?))
}

async fn active_user(db: impl PgExecutor<'_>, id: Uuid) -> ApiResult<()> {
    let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND is_active)")
        .bind(id)
        .fetch_one(db)
        .await?;
    if ok {
        Ok(())
    } else {
        Err(ApiError::bad_request("unknown_user", "Сотрудник не найден или его учётка отключена."))
    }
}

/// Личный диалог с сотрудником: открыть существующий или начать.
#[utoipa::path(post, path = "/api/chat/direct", tag = "chat", request_body = ChatDirectInput,
    responses((status = 200, body = ChatCreated)))]
async fn chat_open_direct(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<ChatDirectInput>,
) -> ApiResult<Json<ChatCreated>> {
    if b.user_id == user.user_id {
        return Err(ApiError::bad_request("self_chat", "Нельзя написать самому себе."));
    }
    let (a, z) = if user.user_id < b.user_id { (user.user_id, b.user_id) } else { (b.user_id, user.user_id) };
    let key = format!("{a}:{z}");
    if let Some(id) = sqlx::query_scalar::<_, Uuid>("SELECT id FROM chats WHERE direct_key = $1")
        .bind(&key)
        .fetch_optional(&core.db)
        .await?
    {
        return Ok(Json(ChatCreated { id }));
    }
    active_user(&core.db, b.user_id).await?;
    let mut tx = core.db.begin().await?;
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO chats (kind, direct_key, created_by) VALUES ('direct', $1, $2)
         ON CONFLICT (direct_key) DO NOTHING RETURNING id",
    )
    .bind(&key)
    .bind(user.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(id) = id else {
        // Собеседник открыл диалог одновременно с нами.
        tx.rollback().await?;
        let id =
            sqlx::query_scalar("SELECT id FROM chats WHERE direct_key = $1").bind(&key).fetch_one(&core.db).await?;
        return Ok(Json(ChatCreated { id }));
    };
    sqlx::query("INSERT INTO chat_members (chat_id, user_id) VALUES ($1, $2), ($1, $3)")
        .bind(id)
        .bind(a)
        .bind(z)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(ChatCreated { id }))
}

/// Групповой чат: создатель — владелец, участники добавляются сразу и получают уведомление.
#[utoipa::path(post, path = "/api/chat/groups", tag = "chat", request_body = ChatGroupInput,
    responses((status = 201, body = ChatCreated)))]
async fn chat_create_group(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<ChatGroupInput>,
) -> ApiResult<(StatusCode, Json<ChatCreated>)> {
    let title = clean_title(&b.title)?;
    if b.members.len() > MAX_ADD {
        return Err(ApiError::bad_request("too_many_members", "Слишком много участников за один раз."));
    }
    let mut tx = core.db.begin().await?;
    let id: Uuid =
        sqlx::query_scalar("INSERT INTO chats (kind, title, created_by) VALUES ('group', $1, $2) RETURNING id")
            .bind(&title)
            .bind(user.user_id)
            .fetch_one(&mut *tx)
            .await?;
    sqlx::query("INSERT INTO chat_members (chat_id, user_id, role, admin_since) VALUES ($1, $2, 'owner', now())")
        .bind(id)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    log_member(&mut *tx, id, user.user_id, Some(user.user_id), "created").await?;
    let others: Vec<Uuid> = b.members.into_iter().filter(|u| *u != user.user_id).collect();
    add(&mut tx, id, &title, &others, user.user_id).await?;
    changed(&mut tx, id, &[]).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(ChatCreated { id })))
}

/// Добавить действующих сотрудников; бывшие участники возвращаются (скрытый чат снова виден).
async fn add(
    tx: &mut Transaction<'_, Postgres>,
    chat: Uuid,
    title: &str,
    users: &[Uuid],
    actor: Uuid,
) -> ApiResult<()> {
    for u in users {
        active_user(&mut **tx, *u).await?;
        let added: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO chat_members (chat_id, user_id) VALUES ($1, $2)
             ON CONFLICT (chat_id, user_id) DO UPDATE
                SET left_at = NULL, left_reason = NULL, role = 'member', admin_since = NULL, hidden = false
              WHERE chat_members.left_at IS NOT NULL
             RETURNING user_id",
        )
        .bind(chat)
        .bind(u)
        .fetch_optional(&mut **tx)
        .await?;
        if added.is_some() {
            log_member(&mut **tx, chat, *u, Some(actor), "added").await?;
            notify::notify(&mut **tx, *u, kind::ADDED, json!({"chat": chat, "title": title})).await?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ Чат

#[utoipa::path(get, path = "/api/chat/{id}", tag = "chat", params(("id" = Uuid, Path)),
    responses((status = 200, body = ChatDetail)))]
async fn chat_get(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<ChatDetail>> {
    let m = membership(&core.db, id, user.user_id).await?;
    let chat = chat_items(&core, user.user_id, None, Some(id)).await?.into_iter().next().ok_or(ApiError::NotFound)?;
    let members = if m.active() {
        let sql = format!(
            "SELECT m.user_id, {NAME} AS name, m.role
               FROM chat_members m JOIN users u ON u.id = m.user_id {PEOPLE}
              WHERE m.chat_id = $1 AND m.left_at IS NULL
              ORDER BY CASE m.role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 ELSE 2 END, 2"
        );
        sqlx::query_as(&sql).bind(id).fetch_all(&core.db).await?
    } else {
        Vec::new()
    };
    let read_upto: Option<OffsetDateTime> = if m.active() {
        sqlx::query_scalar(
            "SELECT max(last_read_at) FROM chat_members WHERE chat_id = $1 AND user_id <> $2 AND left_at IS NULL",
        )
        .bind(id)
        .bind(user.user_id)
        .fetch_one(&core.db)
        .await?
    } else {
        None
    };
    let pinned_id: Option<Uuid> =
        sqlx::query_scalar("SELECT pinned_message_id FROM chats WHERE id = $1").bind(id).fetch_one(&core.db).await?;
    let pinned_message = match (pinned_id, chat.locked) {
        (Some(p), false) => quotes(&core, id, &[p], m.left_at).await?.into_iter().next(),
        _ => None,
    };
    let can_pin = m.active() && !m.locked && (!m.group() || m.manager());
    Ok(Json(ChatDetail {
        chat,
        members,
        can_write: m.active() && !m.locked,
        can_manage: m.manager() && !m.locked,
        read_upto,
        pinned_message,
        can_pin,
    }))
}

#[utoipa::path(put, path = "/api/chat/{id}/title", tag = "chat", params(("id" = Uuid, Path)), request_body = ChatTitleInput,
    responses((status = 204)))]
async fn chat_rename(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ChatTitleInput>,
) -> ApiResult<StatusCode> {
    membership(&core.db, id, user.user_id).await?.require_manager()?;
    let title = clean_title(&b.title)?;
    let mut tx = core.db.begin().await?;
    sqlx::query("UPDATE chats SET title = $2 WHERE id = $1").bind(id).bind(title).execute(&mut *tx).await?;
    changed(&mut tx, id, &[]).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Закрепить (личный диалог или действующий групповой чат) и скрыть (только бывший групповой чат).
#[utoipa::path(put, path = "/api/chat/{id}/prefs", tag = "chat", params(("id" = Uuid, Path)), request_body = ChatPrefsInput,
    responses((status = 204)))]
async fn chat_set_prefs(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ChatPrefsInput>,
) -> ApiResult<StatusCode> {
    let m = membership(&core.db, id, user.user_id).await?;
    if b.pinned == Some(true) && m.group() && !m.active() {
        return Err(ApiError::bad_request("pin_inactive", "Закрепить можно только чат, в котором вы состоите."));
    }
    if b.hidden == Some(true) && (!m.group() || m.active()) {
        return Err(ApiError::bad_request("hide_active", "Скрыть можно только групповой чат, из которого вы вышли."));
    }
    sqlx::query(
        "UPDATE chat_members SET pinned = coalesce($3, pinned), hidden = coalesce($4, hidden), muted = coalesce($5, muted)
          WHERE chat_id = $1 AND user_id = $2",
    )
    .bind(id)
    .bind(user.user_id)
    .bind(b.pinned)
    .bind(b.hidden)
    .bind(b.muted)
    .execute(&core.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(post, path = "/api/chat/{id}/read", tag = "chat", params(("id" = Uuid, Path)),
    responses((status = 204)))]
async fn chat_mark_read(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let m = membership(&core.db, id, user.user_id).await?;
    m.require_unlocked()?;
    sqlx::query("UPDATE chat_members SET last_read_at = now() WHERE chat_id = $1 AND user_id = $2")
        .bind(id)
        .bind(user.user_id)
        .execute(&core.db)
        .await?;
    // Отметки «прочитано» у собеседников обновятся сразу.
    if m.active() {
        let others = others_for_signal(&core.db, id, user.user_id).await?;
        if !others.is_empty() {
            let e = Event {
                kind: "chat_read".into(),
                users: others,
                chat: Some(id),
                from: Some(user.user_id),
                ..Default::default()
            };
            events::publish(&core.db, &e).await?;
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// «Печатает…»: собеседникам на несколько секунд. Не чаще раза в 3 секунды; в большой группе не
/// рассылается. Ничего не хранится.
#[utoipa::path(post, path = "/api/chat/{id}/typing", tag = "chat", params(("id" = Uuid, Path)),
    responses((status = 204)))]
async fn chat_typing(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let m = membership(&core.db, id, user.user_id).await?;
    if !m.active() || m.locked {
        return Ok(StatusCode::NO_CONTENT);
    }
    {
        let mut seen = TYPING.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if seen.get(&(id, user.user_id)).is_some_and(|at| now.duration_since(*at) < TYPING_EVERY) {
            return Ok(StatusCode::NO_CONTENT);
        }
        if seen.len() > 10_000 {
            seen.retain(|_, at| now.duration_since(*at) < TYPING_EVERY);
        }
        seen.insert((id, user.user_id), now);
    }
    let others = others_for_signal(&core.db, id, user.user_id).await?;
    if !others.is_empty() {
        let e = Event {
            kind: "typing".into(),
            users: others,
            chat: Some(id),
            from: Some(user.user_id),
            ..Default::default()
        };
        events::publish(&core.db, &e).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Закрепить сообщение (одно на чат) или открепить. В личном диалоге — любой из двоих, в групповом —
/// владелец и администраторы.
#[utoipa::path(put, path = "/api/chat/{id}/pin", tag = "chat", params(("id" = Uuid, Path)), request_body = ChatPinInput,
    responses((status = 204)))]
async fn chat_pin(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ChatPinInput>,
) -> ApiResult<StatusCode> {
    let m = membership(&core.db, id, user.user_id).await?;
    if !m.active() || (m.group() && !m.manager()) {
        return Err(ApiError::Forbidden);
    }
    m.require_unlocked()?;
    if let Some(msg) = b.message {
        let found: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM chat_messages WHERE id = $1 AND chat_id = $2)")
                .bind(msg)
                .bind(id)
                .fetch_one(&core.db)
                .await?;
        if !found {
            return Err(ApiError::NotFound);
        }
    }
    let mut tx = core.db.begin().await?;
    sqlx::query("UPDATE chats SET pinned_message_id = $2 WHERE id = $1")
        .bind(id)
        .bind(b.message)
        .execute(&mut *tx)
        .await?;
    changed(&mut tx, id, &[]).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Сообщения

const MESSAGE_COLS: &str = "x.id, x.author_id, x.body AS enc, x.created_at, x.edited_at, x.version, x.reply_to,
    (SELECT coalesce(nullif(trim(concat_ws(' ', fp.last_name, fp.first_name)), ''), fa.display_name, fa.sam_account_name)
       FROM users fu JOIN ad_accounts fa ON fa.id = fu.ad_account_id LEFT JOIN persons fp ON fp.id = fu.person_id
      WHERE fu.id = x.forwarded_from) AS forwarded_from_name";

/// Первая строка текста до 100 знаков.
fn first_line(text: &str) -> String {
    text.lines().find(|l| !l.trim().is_empty()).unwrap_or_default().chars().take(100).collect()
}

/// Короткий вид сообщений этого чата (до момента выхода, если вышел).
async fn quotes(core: &Core, chat: Uuid, ids: &[Uuid], upto: Option<OffsetDateTime>) -> ApiResult<Vec<ChatQuote>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT x.id, {NAME} AS author_name, x.body AS enc,
                (SELECT count(*) FROM chat_files f WHERE f.message_id = x.id) AS files
           FROM chat_messages x JOIN users u ON u.id = x.author_id {PEOPLE}
          WHERE x.chat_id = $1 AND x.id = ANY($2) AND x.created_at <= coalesce($3, 'infinity')"
    );
    let mut v: Vec<ChatQuote> = sqlx::query_as(&sql).bind(chat).bind(ids).bind(upto).fetch_all(&core.db).await?;
    for q in &mut v {
        q.preview = first_line(&decrypt(core, &q.enc)?);
    }
    Ok(v)
}

fn message_sql(filter: &str) -> String {
    format!(
        "SELECT {MESSAGE_COLS}, {NAME} AS author_name,
                (x.author_id = $2 AND $3 AND x.created_at > now() - make_interval(mins => {EDIT_MINUTES}::int)) AS editable
           FROM chat_messages x JOIN users u ON u.id = x.author_id {PEOPLE}
          WHERE x.chat_id = $1 {filter}"
    )
}

/// Расшифровать текст и дополнить вложениями, ответами и реакциями.
/// `upto` — момент выхода бывшего участника: реакции позже него не показываются.
async fn open(
    core: &Core,
    chat: Uuid,
    me: Uuid,
    upto: Option<OffsetDateTime>,
    mut v: Vec<ChatMessage>,
) -> ApiResult<Vec<ChatMessage>> {
    for m in &mut v {
        m.body = decrypt(core, &m.enc)?;
    }
    let ids: Vec<Uuid> = v.iter().map(|m| m.id).collect();
    let rows: Vec<(Uuid, Uuid, Vec<u8>, String, i64, bool)> = sqlx::query_as(
        "SELECT message_id, id, name, mime, size, expired_at IS NOT NULL
           FROM chat_files WHERE message_id = ANY($1) ORDER BY id",
    )
    .bind(&ids)
    .fetch_all(&core.db)
    .await?;
    for (message, id, name, mime, size, expired) in rows {
        let f = ChatFile {
            id,
            name: decrypt(core, &name)?,
            image: pult_core::files::is_inline_image(&mime) && !expired,
            mime,
            size,
            expired,
        };
        if let Some(m) = v.iter_mut().find(|m| m.id == message) {
            m.files.push(f);
        }
    }
    // Ответ — на более раннее сообщение этого же чата: виден тому, кому видно само сообщение.
    let replies: Vec<Uuid> = v.iter().filter_map(|m| m.reply_to).collect();
    for q in quotes(core, chat, &replies, None).await? {
        for m in v.iter_mut().filter(|m| m.reply_to == Some(q.id)) {
            m.reply = Some(ChatQuote {
                id: q.id,
                author_name: q.author_name.clone(),
                preview: q.preview.clone(),
                files: q.files,
                enc: Vec::new(),
            });
        }
    }
    let sql = format!(
        "SELECT r.message_id, r.emoji, count(*), bool_or(r.user_id = $2),
                (array_agg({NAME} ORDER BY r.created_at))[1:$3]
           FROM chat_reactions r JOIN users u ON u.id = r.user_id {PEOPLE}
          WHERE r.message_id = ANY($1) AND r.created_at <= coalesce($4, 'infinity')
          GROUP BY r.message_id, r.emoji ORDER BY min(r.created_at)"
    );
    let rows: Vec<(Uuid, String, i64, bool, Vec<String>)> =
        sqlx::query_as(&sql).bind(&ids).bind(me).bind(REACTION_NAMES).bind(upto).fetch_all(&core.db).await?;
    for (message, emoji, count, mine, names) in rows {
        if let Some(m) = v.iter_mut().find(|m| m.id == message) {
            m.reactions.push(ChatReaction { emoji, count, mine, names });
        }
    }
    Ok(v)
}

/// Сообщения по 100, от новых к старым страницами (`before`), внутри страницы — по времени.
/// Бывшему участнику — только до момента выхода.
#[utoipa::path(get, path = "/api/chat/{id}/messages", tag = "chat", params(("id" = Uuid, Path), ChatPageQuery),
    responses((status = 200, body = Vec<ChatMessage>)))]
async fn chat_list_messages(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Query(q): Query<ChatPageQuery>,
) -> ApiResult<Json<Vec<ChatMessage>>> {
    let m = membership(&core.db, id, user.user_id).await?;
    m.require_unlocked()?;
    let sql = message_sql(&format!(
        "AND x.created_at <= coalesce($4, 'infinity') AND ($5::uuid IS NULL OR x.id < $5)
         ORDER BY x.id DESC LIMIT {PAGE}"
    ));
    let mut v: Vec<ChatMessage> = sqlx::query_as(&sql)
        .bind(id)
        .bind(user.user_id)
        .bind(m.active())
        .bind(m.left_at)
        .bind(q.before)
        .fetch_all(&core.db)
        .await?;
    v.reverse();
    Ok(Json(open(&core, id, user.user_id, m.left_at, v).await?))
}

async fn one_message(core: &Core, chat: Uuid, id: Uuid, me: Uuid, active: bool) -> ApiResult<ChatMessage> {
    let sql = message_sql("AND x.id = $4");
    let m: ChatMessage = sqlx::query_as(&sql).bind(chat).bind(me).bind(active).bind(id).fetch_one(&core.db).await?;
    Ok(open(core, chat, me, None, vec![m]).await?.remove(0))
}

#[utoipa::path(post, path = "/api/chat/{id}/messages", tag = "chat", params(("id" = Uuid, Path)), request_body = ChatMessageInput,
    responses((status = 201, body = ChatMessage)))]
async fn chat_send_message(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ChatMessageInput>,
) -> ApiResult<(StatusCode, Json<ChatMessage>)> {
    let m = membership(&core.db, id, user.user_id).await?;
    if !m.active() {
        return Err(ApiError::Refused {
            code: "read_only",
            message: "Вы вышли из чата — история доступна только для чтения.".into(),
        });
    }
    m.require_unlocked()?;
    if b.files.len() > MAX_FILES {
        return Err(ApiError::bad_request("too_many_files", "Не больше 10 вложений в одном сообщении."));
    }
    let mut files = b.files.clone();
    files.sort();
    files.dedup();
    // Только файлы: текст не обязателен.
    let body = if files.is_empty() || !b.body.trim().is_empty() { clean_body(&b.body)? } else { String::new() };
    if let Some(r) = b.reply_to {
        let found: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM chat_messages WHERE id = $1 AND chat_id = $2)")
                .bind(r)
                .bind(id)
                .fetch_one(&core.db)
                .await?;
        if !found {
            return Err(ApiError::bad_request("invalid_reply", "Сообщение, на которое вы отвечаете, не найдено."));
        }
    }
    let enc = encrypt(&core, &body)?;
    let mut tx = core.db.begin().await?;
    let mid: Uuid = sqlx::query_scalar(
        "INSERT INTO chat_messages (chat_id, author_id, body, reply_to) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(id)
    .bind(user.user_id)
    .bind(enc)
    .bind(b.reply_to)
    .fetch_one(&mut *tx)
    .await?;
    if !files.is_empty() {
        let n = sqlx::query(
            "UPDATE chat_files SET message_id = $1
              WHERE id = ANY($2) AND chat_id = $3 AND uploaded_by = $4 AND message_id IS NULL",
        )
        .bind(mid)
        .bind(&files)
        .bind(id)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if n != files.len() as u64 {
            return Err(ApiError::bad_request("invalid_files", "Вложение не найдено — загрузите его снова."));
        }
    }
    sqlx::query("UPDATE chats SET last_message_at = now() WHERE id = $1").bind(id).execute(&mut *tx).await?;
    // Своё сообщение прочитано.
    sqlx::query("UPDATE chat_members SET last_read_at = now() WHERE chat_id = $1 AND user_id = $2")
        .bind(id)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    changed(&mut tx, id, &[]).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(one_message(&core, id, mid, user.user_id, true).await?)))
}

/// Загрузить вложение в чат. Прикрепляется к сообщению при отправке; типы и предел размера — как у
/// вложений заявок (настройка Helpdesk). Имя и содержимое шифруются ключом портала.
#[utoipa::path(post, path = "/api/chat/{id}/files", tag = "chat", params(("id" = Uuid, Path)),
    request_body(content_type = "multipart/form-data", content = inline(ChatUploadForm)),
    responses((status = 200, body = ChatFile), (status = 400, body = pult_core::error::ErrorBody)))]
async fn chat_upload_file(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    mut form: Multipart,
) -> ApiResult<Json<ChatFile>> {
    let m = membership(&core.db, id, user.user_id).await?;
    if !m.active() {
        return Err(ApiError::Refused {
            code: "read_only",
            message: "Вы вышли из чата — история доступна только для чтения.".into(),
        });
    }
    m.require_unlocked()?;
    upload_quota(&core.db, user.user_id, 0, false).await?;
    let _slot = UPLOADS.try_acquire().map_err(|_| ApiError::TooManyRequests {
        message: "Сервер сейчас принимает много файлов. Повторите через минуту.".into(),
        retry_after_secs: 60,
    })?;
    let max_mb = pult_core::files::max_file_mb(&core.db).await?;
    let limit = usize::try_from(max_mb).unwrap_or(usize::MAX).saturating_mul(1024 * 1024);
    let bad = |m: &str| ApiError::bad_request("invalid_file", m);
    let too_big = || ApiError::bad_request("file_too_large", format!("Файл больше {max_mb} МБ."));
    let broken = |e: axum::extract::multipart::MultipartError, m: &str| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE { too_big() } else { bad(m) }
    };
    // Поле `file`; другие поля формы пропускаются.
    let mut field = loop {
        match form.next_field().await.map_err(|e| broken(e, "Файл не получен."))? {
            Some(f) if f.name() == Some("file") => break f,
            Some(_) => continue,
            None => return Err(bad("Файл не получен.")),
        }
    };
    let name = pult_core::files::clean_name(field.file_name().unwrap_or("file"));
    let mut data = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|e| broken(e, "Загрузка оборвалась."))? {
        if data.len() + chunk.len() > limit {
            return Err(too_big());
        }
        data.extend_from_slice(&chunk);
    }
    if data.is_empty() {
        return Err(bad("Файл пустой."));
    }
    let mime = pult_core::files::check_type(&name, &data).map_err(|m| ApiError::bad_request("invalid_file", m))?;
    let size = data.len() as i64;
    let enc = core.secrets.encrypt_vec(data).map_err(ApiError::internal)?;
    // Квота ещё раз — под блокировкой сотрудника: параллельные загрузки не проскочат её вместе.
    let mut tx = core.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('chat_files:' || $1::text, 0))")
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    upload_quota(&mut *tx, user.user_id, size, false).await?;
    let fid: Uuid = sqlx::query_scalar(
        "INSERT INTO chat_files (chat_id, uploaded_by, name, mime, size, data) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(id)
    .bind(user.user_id)
    .bind(encrypt(&core, &name)?)
    .bind(mime)
    .bind(size)
    .bind(enc)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(ChatFile {
        id: fid,
        name,
        mime: mime.to_string(),
        size,
        image: pult_core::files::is_inline_image(mime),
        expired: false,
    }))
}

/// Квоты загрузки за сутки: неотправленные (число и объём) и всё загруженное; `extra` — размер нового файла.
/// `sent` — файлы сразу уходят в сообщение (пересылка): предел неотправленных к ним не относится.
async fn upload_quota(db: impl PgExecutor<'_>, user: Uuid, extra: i64, sent: bool) -> ApiResult<()> {
    let (pending, pending_bytes, total): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE message_id IS NULL),
                coalesce(sum(size) FILTER (WHERE message_id IS NULL), 0)::bigint,
                coalesce(sum(size), 0)::bigint
           FROM chat_files WHERE uploaded_by = $1 AND created_at > now() - interval '1 day'",
    )
    .bind(user)
    .fetch_one(db)
    .await?;
    let mb = 1024 * 1024;
    if !sent && (pending >= PENDING_MAX_FILES || pending_bytes + extra > PENDING_MAX_MB * mb) {
        return Err(ApiError::TooManyRequests {
            message: "Слишком много файлов загружено и не отправлено. Отправьте их или повторите завтра.".into(),
            retry_after_secs: 3600,
        });
    }
    if total + extra > DAILY_MAX_MB * mb {
        return Err(ApiError::TooManyRequests {
            message: format!("За сутки загружено больше {DAILY_MAX_MB} МБ файлов. Повторите завтра."),
            retry_after_secs: 3600,
        });
    }
    Ok(())
}

/// Скачать вложение. Видят участники, которым видно сообщение (бывший участник — до выхода), а
/// неотправленное — только тот, кто загрузил.
#[utoipa::path(get, path = "/api/chat/files/{file}", tag = "chat", params(("file" = Uuid, Path)),
    responses((status = 200, description = "Содержимое файла"), (status = 404, body = pult_core::error::ErrorBody)))]
async fn chat_get_file(State(core): State<Core>, user: CurrentUser, Path(file): Path<Uuid>) -> ApiResult<Response> {
    #[derive(sqlx::FromRow)]
    struct FileRow {
        chat_id: Uuid,
        sent_at: Option<OffsetDateTime>,
        uploaded_by: Uuid,
        name: Vec<u8>,
        mime: String,
        expired: bool,
    }
    let row: Option<FileRow> = sqlx::query_as(
        "SELECT f.chat_id, x.created_at AS sent_at, f.uploaded_by, f.name, f.mime, f.expired_at IS NOT NULL AS expired
           FROM chat_files f LEFT JOIN chat_messages x ON x.id = f.message_id
          WHERE f.id = $1",
    )
    .bind(file)
    .fetch_optional(&core.db)
    .await?;
    let FileRow { chat_id: chat, sent_at, uploaded_by: uploader, name, mime, expired } =
        row.ok_or(ApiError::NotFound)?;
    let m = membership(&core.db, chat, user.user_id).await?;
    m.require_unlocked()?;
    let visible = match (sent_at, m.left_at) {
        (None, _) => uploader == user.user_id,
        (Some(_), None) => true,
        (Some(sent), Some(left)) => sent <= left,
    };
    if !visible {
        return Err(ApiError::NotFound);
    }
    if expired {
        return Err(ApiError::Gone);
    }
    let _slot = DOWNLOADS.try_acquire().map_err(|_| ApiError::TooManyRequests {
        message: "Сервер сейчас отдаёт много файлов. Повторите через минуту.".into(),
        retry_after_secs: 30,
    })?;
    let enc: Vec<u8> =
        sqlx::query_scalar("SELECT data FROM chat_files WHERE id = $1").bind(file).fetch_one(&core.db).await?;
    let data = core.secrets.decrypt_bytes(&enc).map_err(ApiError::internal)?;
    let name = decrypt(&core, &name)?;
    let kind = if pult_core::files::is_inline_image(&mime) { "inline" } else { "attachment" };
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CONTENT_DISPOSITION, pult_core::files::content_disposition(kind, &name)),
            // Переписка не остаётся в кэше браузера на общем компьютере.
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        data,
    )
        .into_response())
}

/// Правка своего сообщения в течение 15 минут; прежний текст остаётся в версиях.
#[utoipa::path(put, path = "/api/chat/{id}/messages/{message}", tag = "chat",
    params(("id" = Uuid, Path), ("message" = Uuid, Path)), request_body = ChatMessageInput,
    responses((status = 200, body = ChatMessage)))]
async fn chat_edit_message(
    State(core): State<Core>,
    user: CurrentUser,
    Path((id, message)): Path<(Uuid, Uuid)>,
    Json(b): Json<ChatMessageInput>,
) -> ApiResult<Json<ChatMessage>> {
    let m = membership(&core.db, id, user.user_id).await?;
    if !m.active() {
        return Err(ApiError::Refused {
            code: "read_only",
            message: "Вы вышли из чата — история доступна только для чтения.".into(),
        });
    }
    m.require_unlocked()?;
    let body = clean_body(&b.body)?;
    let mut tx = core.db.begin().await?;
    let row: Option<(Uuid, Vec<u8>, i32, OffsetDateTime, bool)> = sqlx::query_as(
        "SELECT author_id, body, version, coalesce(edited_at, created_at),
                created_at > now() - make_interval(mins => $3::int)
           FROM chat_messages WHERE id = $1 AND chat_id = $2 FOR UPDATE",
    )
    .bind(message)
    .bind(id)
    .bind(EDIT_MINUTES as i32)
    .fetch_optional(&mut *tx)
    .await?;
    let (author, old, version, since, fresh) = row.ok_or(ApiError::NotFound)?;
    if author != user.user_id {
        return Err(ApiError::Forbidden);
    }
    if !fresh {
        return Err(ApiError::Refused {
            code: "edit_expired",
            message: "Править сообщение можно только 15 минут после отправки.".into(),
        });
    }
    if decrypt(&core, &old)? == body {
        tx.rollback().await?;
        return Ok(Json(one_message(&core, id, message, user.user_id, true).await?));
    }
    sqlx::query("INSERT INTO chat_message_versions (message_id, version, body, created_at) VALUES ($1, $2, $3, $4)")
        .bind(message)
        .bind(version)
        .bind(old)
        .bind(since)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE chat_messages SET body = $2, edited_at = now(), version = version + 1 WHERE id = $1")
        .bind(message)
        .bind(encrypt(&core, &body)?)
        .execute(&mut *tx)
        .await?;
    changed(&mut tx, id, &[]).await?;
    tx.commit().await?;
    Ok(Json(one_message(&core, id, message, user.user_id, true).await?))
}

/// Прежние версии: в личном диалоге — обоим, в групповом чате — автору, владельцу и администраторам.
#[utoipa::path(get, path = "/api/chat/{id}/messages/{message}/versions", tag = "chat",
    params(("id" = Uuid, Path), ("message" = Uuid, Path)),
    responses((status = 200, body = Vec<ChatMessageVersion>)))]
async fn chat_message_versions(
    State(core): State<Core>,
    user: CurrentUser,
    Path((id, message)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<Vec<ChatMessageVersion>>> {
    let m = membership(&core.db, id, user.user_id).await?;
    m.require_unlocked()?;
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT author_id FROM chat_messages WHERE id = $1 AND chat_id = $2 AND created_at <= coalesce($3, 'infinity')",
    )
    .bind(message)
    .bind(id)
    .bind(m.left_at)
    .fetch_optional(&core.db)
    .await?;
    let (author,) = row.ok_or(ApiError::NotFound)?;
    if m.group() && author != user.user_id && !m.manager() {
        return Err(ApiError::Forbidden);
    }
    let mut v: Vec<ChatMessageVersion> = sqlx::query_as(
        "SELECT version, body AS enc, created_at FROM chat_message_versions WHERE message_id = $1 ORDER BY version",
    )
    .bind(message)
    .fetch_all(&core.db)
    .await?;
    for x in &mut v {
        x.body = decrypt(&core, &x.enc)?;
    }
    Ok(Json(v))
}

/// Сообщение этого чата, видимое мне: автор, исходный автор пересылки, зашифрованный текст.
async fn visible_message(
    db: impl PgExecutor<'_>,
    chat: Uuid,
    message: Uuid,
    upto: Option<OffsetDateTime>,
) -> ApiResult<(Uuid, Option<Uuid>, Vec<u8>)> {
    let row: Option<(Uuid, Option<Uuid>, Vec<u8>)> = sqlx::query_as(
        "SELECT author_id, forwarded_from, body FROM chat_messages
          WHERE id = $1 AND chat_id = $2 AND created_at <= coalesce($3, 'infinity')",
    )
    .bind(message)
    .bind(chat)
    .bind(upto)
    .fetch_optional(db)
    .await?;
    row.ok_or(ApiError::NotFound)
}

/// Поставить или снять реакцию (эмодзи из списка).
#[utoipa::path(put, path = "/api/chat/{id}/messages/{message}/reactions", tag = "chat",
    params(("id" = Uuid, Path), ("message" = Uuid, Path)), request_body = ChatReactInput,
    responses((status = 204)))]
async fn chat_react(
    State(core): State<Core>,
    user: CurrentUser,
    Path((id, message)): Path<(Uuid, Uuid)>,
    Json(b): Json<ChatReactInput>,
) -> ApiResult<StatusCode> {
    let m = membership(&core.db, id, user.user_id).await?;
    if !m.active() {
        return Err(ApiError::Refused {
            code: "read_only",
            message: "Вы вышли из чата — история доступна только для чтения.".into(),
        });
    }
    m.require_unlocked()?;
    if !REACTIONS.contains(&b.emoji.as_str()) {
        return Err(ApiError::bad_request("invalid_reaction", "Такой реакции нет."));
    }
    if !within_minute(&REACTED, user.user_id, REACTIONS_PER_MINUTE) {
        return Err(slow_down());
    }
    visible_message(&core.db, id, message, None).await?;
    let mut tx = core.db.begin().await?;
    let removed = sqlx::query("DELETE FROM chat_reactions WHERE message_id = $1 AND user_id = $2 AND emoji = $3")
        .bind(message)
        .bind(user.user_id)
        .bind(&b.emoji)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if removed == 0 {
        sqlx::query(
            "INSERT INTO chat_reactions (message_id, user_id, emoji) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(message)
        .bind(user.user_id)
        .bind(&b.emoji)
        .execute(&mut *tx)
        .await?;
    }
    changed(&mut tx, id, &[]).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Переслать сообщение в другой свой чат: текст и вложения копируются, видно, чьё оно.
/// Копии вложений считаются в суточной квоте пересылающего.
#[utoipa::path(post, path = "/api/chat/{id}/messages/{message}/forward", tag = "chat",
    params(("id" = Uuid, Path), ("message" = Uuid, Path)), request_body = ChatForwardInput,
    responses((status = 201, body = ChatMessage)))]
async fn chat_forward(
    State(core): State<Core>,
    user: CurrentUser,
    Path((id, message)): Path<(Uuid, Uuid)>,
    Json(b): Json<ChatForwardInput>,
) -> ApiResult<(StatusCode, Json<ChatMessage>)> {
    let src = membership(&core.db, id, user.user_id).await?;
    src.require_unlocked()?;
    let (author, origin, enc) = visible_message(&core.db, id, message, src.left_at).await?;
    let dst = membership(&core.db, b.to, user.user_id).await?;
    if !dst.active() {
        return Err(ApiError::Refused {
            code: "read_only",
            message: "Вы вышли из чата — история доступна только для чтения.".into(),
        });
    }
    dst.require_unlocked()?;
    let mut tx = core.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('chat_files:' || $1::text, 0))")
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    let bytes: i64 = sqlx::query_scalar(
        "SELECT coalesce(sum(size), 0)::bigint FROM chat_files WHERE message_id = $1 AND expired_at IS NULL",
    )
    .bind(message)
    .fetch_one(&mut *tx)
    .await?;
    if bytes > 0 {
        upload_quota(&mut *tx, user.user_id, bytes, true).await?;
    }
    let mid: Uuid = sqlx::query_scalar(
        "INSERT INTO chat_messages (chat_id, author_id, body, forwarded_from) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(b.to)
    .bind(user.user_id)
    .bind(enc)
    .bind(origin.unwrap_or(author))
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO chat_files (chat_id, message_id, uploaded_by, name, mime, size, data, kept_since)
         SELECT $1, $2, $3, name, mime, size, data, coalesce(kept_since, created_at) FROM chat_files
          WHERE message_id = $4 AND expired_at IS NULL ORDER BY id",
    )
    .bind(b.to)
    .bind(mid)
    .bind(user.user_id)
    .bind(message)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE chats SET last_message_at = now() WHERE id = $1").bind(b.to).execute(&mut *tx).await?;
    sqlx::query("UPDATE chat_members SET last_read_at = now() WHERE chat_id = $1 AND user_id = $2")
        .bind(b.to)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    changed(&mut tx, b.to, &[]).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(one_message(&core, b.to, mid, user.user_id, true).await?)))
}

/// Поиск по тексту переписки: текст зашифрован, поэтому сервер расшифровывает последние 5000
/// доступных мне сообщений и ищет в них. Найденное — от новых к старым, не больше 50.
#[utoipa::path(get, path = "/api/chat/{id}/search", tag = "chat", params(("id" = Uuid, Path), ChatSearchQuery),
    responses((status = 200, body = Vec<ChatMessage>)))]
async fn chat_search(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Query(q): Query<ChatSearchQuery>,
) -> ApiResult<Json<Vec<ChatMessage>>> {
    let m = membership(&core.db, id, user.user_id).await?;
    m.require_unlocked()?;
    let needle = q.q.trim().to_lowercase().replace('ё', "е");
    if needle.chars().count() < 2 {
        return Ok(Json(Vec::new()));
    }
    if !within_minute(&SEARCHED, user.user_id, SEARCHES_PER_MINUTE) {
        return Err(slow_down());
    }
    let _slot = SEARCHES.try_acquire().map_err(|_| ApiError::TooManyRequests {
        message: "Сервер сейчас занят поиском. Повторите через несколько секунд.".into(),
        retry_after_secs: 5,
    })?;
    let rows: Vec<(Uuid, Vec<u8>)> = sqlx::query_as(
        "SELECT id, body FROM chat_messages WHERE chat_id = $1 AND created_at <= coalesce($2, 'infinity')
          ORDER BY created_at DESC, id DESC LIMIT $3",
    )
    .bind(id)
    .bind(m.left_at)
    .bind(SEARCH_SCAN)
    .fetch_all(&core.db)
    .await?;
    // Расшифровка тысяч сообщений — не в потоке обработки запросов.
    let secrets = core.secrets.clone();
    let found = tokio::task::spawn_blocking(move || -> ApiResult<Vec<Uuid>> {
        let mut found = Vec::new();
        let mut text = String::new();
        for (mid, enc) in rows {
            let plain = secrets.decrypt(&enc).map_err(ApiError::internal)?;
            text.clear();
            text.extend(plain.expose().chars().flat_map(char::to_lowercase).map(|c| if c == 'ё' { 'е' } else { c }));
            if text.contains(&needle) {
                found.push(mid);
                if found.len() >= SEARCH_LIMIT {
                    break;
                }
            }
        }
        Ok(found)
    })
    .await
    .map_err(ApiError::internal)??;
    if found.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let sql = message_sql("AND x.id = ANY($4) ORDER BY x.created_at DESC, x.id DESC");
    let v: Vec<ChatMessage> =
        sqlx::query_as(&sql).bind(id).bind(user.user_id).bind(m.active()).bind(&found).fetch_all(&core.db).await?;
    Ok(Json(open(&core, id, user.user_id, m.left_at, v).await?))
}

/// Файлы чата: отправленные вложения, от новых к старым (последние 200).
#[utoipa::path(get, path = "/api/chat/{id}/files", tag = "chat", params(("id" = Uuid, Path)),
    responses((status = 200, body = Vec<ChatSharedFile>)))]
async fn chat_shared_files(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<ChatSharedFile>>> {
    let m = membership(&core.db, id, user.user_id).await?;
    m.require_unlocked()?;
    let sql = format!(
        "SELECT f.id, f.name, f.mime, f.size, f.expired_at IS NOT NULL, f.message_id, {NAME}, x.created_at
           FROM chat_files f JOIN chat_messages x ON x.id = f.message_id
           JOIN users u ON u.id = x.author_id {PEOPLE}
          WHERE f.chat_id = $1 AND x.created_at <= coalesce($2, 'infinity')
          ORDER BY x.created_at DESC, f.id LIMIT 200"
    );
    type Row = (Uuid, Vec<u8>, String, i64, bool, Uuid, String, OffsetDateTime);
    let rows: Vec<Row> = sqlx::query_as(&sql).bind(id).bind(m.left_at).fetch_all(&core.db).await?;
    let mut out = Vec::with_capacity(rows.len());
    for (fid, name, mime, size, expired, message_id, author_name, created_at) in rows {
        out.push(ChatSharedFile {
            file: ChatFile {
                id: fid,
                name: decrypt(&core, &name)?,
                image: pult_core::files::is_inline_image(&mime) && !expired,
                mime,
                size,
                expired,
            },
            message_id,
            author_name,
            created_at,
        });
    }
    Ok(Json(out))
}

// ------------------------------------------------------------------ Состав

#[utoipa::path(post, path = "/api/chat/{id}/members", tag = "chat", params(("id" = Uuid, Path)), request_body = ChatMembersInput,
    responses((status = 204)))]
async fn chat_add_members(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ChatMembersInput>,
) -> ApiResult<StatusCode> {
    let m = membership(&core.db, id, user.user_id).await?;
    m.require_manager()?;
    if b.user_ids.len() > MAX_ADD {
        return Err(ApiError::bad_request("too_many_members", "Слишком много участников за один раз."));
    }
    let mut tx = core.db.begin().await?;
    add(&mut tx, id, m.title.as_deref().unwrap_or_default(), &b.user_ids, user.user_id).await?;
    changed(&mut tx, id, &[]).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Выбыть из чата: роль сбрасывается, история до этого момента остаётся для чтения.
async fn depart(db: impl PgExecutor<'_>, chat: Uuid, user: Uuid, reason: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE chat_members SET left_at = now(), left_reason = $3, role = 'member', admin_since = NULL
          WHERE chat_id = $1 AND user_id = $2",
    )
    .bind(chat)
    .bind(user)
    .bind(reason)
    .execute(db)
    .await?;
    Ok(())
}

async fn target_role(db: impl PgExecutor<'_>, chat: Uuid, user: Uuid) -> ApiResult<String> {
    sqlx::query_scalar("SELECT role FROM chat_members WHERE chat_id = $1 AND user_id = $2 AND left_at IS NULL")
        .bind(chat)
        .bind(user)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)
}

/// Исключить: владелец — любого, администратор — обычного участника. Владельца исключить нельзя.
#[utoipa::path(delete, path = "/api/chat/{id}/members/{user}", tag = "chat",
    params(("id" = Uuid, Path), ("user" = Uuid, Path)), responses((status = 204)))]
async fn chat_remove_member(
    State(core): State<Core>,
    user: CurrentUser,
    Path((id, target)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let m = membership(&core.db, id, user.user_id).await?;
    m.require_manager()?;
    if target == user.user_id {
        return Err(ApiError::bad_request("use_leave", "Чтобы выйти из чата, используйте «Выйти»."));
    }
    let mut tx = core.db.begin().await?;
    let role = target_role(&mut *tx, id, target).await?;
    if role == "owner" || (role == "admin" && m.role != "owner") {
        return Err(ApiError::Forbidden);
    }
    depart(&mut *tx, id, target, "removed").await?;
    log_member(&mut *tx, id, target, Some(user.user_id), "removed").await?;
    notify::notify(&mut *tx, target, kind::REMOVED, json!({"chat": id, "title": m.title})).await?;
    changed(&mut tx, id, &[target]).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Назначить или снять администратора чата (владелец и администраторы; владельца это не касается).
#[utoipa::path(put, path = "/api/chat/{id}/members/{user}/role", tag = "chat",
    params(("id" = Uuid, Path), ("user" = Uuid, Path)), request_body = ChatRoleInput, responses((status = 204)))]
async fn chat_set_role(
    State(core): State<Core>,
    user: CurrentUser,
    Path((id, target)): Path<(Uuid, Uuid)>,
    Json(b): Json<ChatRoleInput>,
) -> ApiResult<StatusCode> {
    let m = membership(&core.db, id, user.user_id).await?;
    m.require_manager()?;
    if !matches!(b.role.as_str(), "admin" | "member") {
        return Err(ApiError::bad_request("invalid_role", "Роль — администратор или участник."));
    }
    let mut tx = core.db.begin().await?;
    let role = target_role(&mut *tx, id, target).await?;
    if role == "owner" {
        return Err(ApiError::Forbidden);
    }
    if role != b.role {
        sqlx::query(
            "UPDATE chat_members SET role = $3, admin_since = CASE WHEN $3 = 'admin' THEN now() END
              WHERE chat_id = $1 AND user_id = $2",
        )
        .bind(id)
        .bind(target)
        .bind(&b.role)
        .execute(&mut *tx)
        .await?;
        log_member(&mut *tx, id, target, Some(user.user_id), &b.role).await?;
        changed(&mut tx, id, &[]).await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Выйти из группового чата. Владелец выйти не может: владение передаёт суперадминистратор.
#[utoipa::path(post, path = "/api/chat/{id}/leave", tag = "chat", params(("id" = Uuid, Path)),
    responses((status = 204)))]
async fn chat_leave(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    let m = membership(&core.db, id, user.user_id).await?;
    if !m.group() {
        return Err(ApiError::bad_request("direct_leave", "Из личного диалога выйти нельзя."));
    }
    if !m.active() {
        return Ok(StatusCode::NO_CONTENT);
    }
    if m.role == "owner" {
        return Err(ApiError::bad_request(
            "owner_cannot_leave",
            "Владелец не может выйти из чата: сначала нужно передать владение.",
        ));
    }
    let mut tx = core.db.begin().await?;
    depart(&mut *tx, id, user.user_id, "left").await?;
    log_member(&mut *tx, id, user.user_id, Some(user.user_id), "left").await?;
    changed(&mut tx, id, &[user.user_id]).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// История состава — владельцу и администраторам чата.
#[utoipa::path(get, path = "/api/chat/{id}/member-events", tag = "chat", params(("id" = Uuid, Path)),
    responses((status = 200, body = Vec<ChatMemberEvent>)))]
async fn chat_member_events(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<ChatMemberEvent>>> {
    if !membership(&core.db, id, user.user_id).await?.manager() {
        return Err(ApiError::Forbidden);
    }
    let sql = format!(
        "SELECT (SELECT {NAME} FROM users u {PEOPLE} WHERE u.id = e.user_id) AS user_name,
                (SELECT {NAME} FROM users u {PEOPLE} WHERE u.id = e.actor_id) AS actor_name,
                e.action, e.created_at
           FROM chat_member_events e WHERE e.chat_id = $1 ORDER BY e.created_at DESC, e.id DESC LIMIT 500"
    );
    Ok(Json(sqlx::query_as(&sql).bind(id).fetch_all(&core.db).await?))
}

// ------------------------------------------------------------------ Суперадминистратор

/// Групповые чаты: название, число участников, владелец, блокировка. Без сообщений; личных диалогов здесь нет.
#[utoipa::path(get, path = "/api/admin/chats", tag = "chat", responses((status = 200, body = Vec<AdminChat>)))]
async fn chat_admin_list(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<AdminChat>>> {
    user.require_superadmin()?;
    let sql = format!(
        "SELECT c.id, c.title,
                (SELECT count(*) FROM chat_members m WHERE m.chat_id = c.id AND m.left_at IS NULL) AS members,
                (SELECT {NAME} FROM chat_members m JOIN users u ON u.id = m.user_id {PEOPLE}
                  WHERE m.chat_id = c.id AND m.role = 'owner' AND m.left_at IS NULL) AS owner_name,
                c.locked, c.created_at
           FROM chats c WHERE c.kind = 'group'
          ORDER BY c.locked DESC, lower(c.title)"
    );
    Ok(Json(sqlx::query_as(&sql).fetch_all(&core.db).await?))
}

/// Назначить владельца группового чата (в том числе себя). Сразу, без согласия; прежний владелец
/// становится администратором, участником или выбывает. Заблокированный чат разблокируется.
#[utoipa::path(post, path = "/api/admin/chats/{id}/owner", tag = "chat", params(("id" = Uuid, Path)), request_body = ChatOwnerInput,
    responses((status = 204)))]
async fn chat_admin_assign_owner(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ChatOwnerInput>,
) -> ApiResult<StatusCode> {
    user.require_superadmin()?;
    if !matches!(b.previous.as_str(), "admin" | "member" | "remove") {
        return Err(ApiError::bad_request(
            "invalid_previous",
            "Прежний владелец: администратор, участник или исключить.",
        ));
    }
    let mut tx = core.db.begin().await?;
    let row: Option<(String, Option<String>, bool)> =
        sqlx::query_as("SELECT kind, title, locked FROM chats WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let (chat_kind, title, locked) = row.ok_or(ApiError::NotFound)?;
    if chat_kind != "group" {
        return Err(ApiError::NotFound);
    }
    active_user(&mut *tx, b.user_id).await?;
    let previous: Option<Uuid> = sqlx::query_scalar(
        "SELECT user_id FROM chat_members WHERE chat_id = $1 AND role = 'owner' AND left_at IS NULL",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    if previous == Some(b.user_id) {
        return Err(ApiError::bad_request("already_owner", "Этот сотрудник уже владелец чата."));
    }
    if let Some(p) = previous {
        if b.previous == "remove" {
            depart(&mut *tx, id, p, "removed").await?;
            log_member(&mut *tx, id, p, Some(user.user_id), "removed").await?;
            notify::notify(&mut *tx, p, kind::REMOVED, json!({"chat": id, "title": title})).await?;
        } else {
            sqlx::query(
                "UPDATE chat_members SET role = $3, admin_since = CASE WHEN $3 = 'admin' THEN now() END
                  WHERE chat_id = $1 AND user_id = $2",
            )
            .bind(id)
            .bind(p)
            .bind(&b.previous)
            .execute(&mut *tx)
            .await?;
            log_member(&mut *tx, id, p, Some(user.user_id), &b.previous).await?;
        }
    }
    sqlx::query(
        "INSERT INTO chat_members (chat_id, user_id, role, admin_since) VALUES ($1, $2, 'owner', now())
         ON CONFLICT (chat_id, user_id) DO UPDATE
            SET role = 'owner', left_at = NULL, left_reason = NULL, hidden = false,
                admin_since = CASE WHEN chat_members.left_at IS NULL THEN coalesce(chat_members.admin_since, now()) ELSE now() END",
    )
    .bind(id)
    .bind(b.user_id)
    .execute(&mut *tx)
    .await?;
    log_member(&mut *tx, id, b.user_id, Some(user.user_id), "owner").await?;
    if locked {
        sqlx::query("UPDATE chats SET locked = false WHERE id = $1").bind(id).execute(&mut *tx).await?;
        log_member(&mut *tx, id, b.user_id, Some(user.user_id), "unlocked").await?;
        let members: Vec<Uuid> =
            sqlx::query_scalar("SELECT user_id FROM chat_members WHERE chat_id = $1 AND left_at IS NULL")
                .bind(id)
                .fetch_all(&mut *tx)
                .await?;
        for u in members {
            notify::notify(&mut *tx, u, kind::UNLOCKED, json!({"chat": id, "title": title})).await?;
        }
    } else {
        if b.user_id != user.user_id {
            notify::notify(&mut *tx, b.user_id, kind::OWNER, json!({"chat": id, "title": title})).await?;
        }
        // Владельца назначил суперадминистратор (в том числе себя): участники должны об этом знать —
        // новый владелец читает всю историю чата.
        let others: Vec<Uuid> = sqlx::query_scalar(
            "SELECT user_id FROM chat_members
              WHERE chat_id = $1 AND left_at IS NULL AND user_id <> $2 AND user_id <> $3",
        )
        .bind(id)
        .bind(b.user_id)
        .bind(user.user_id)
        .fetch_all(&mut *tx)
        .await?;
        for u in others {
            notify::notify(&mut *tx, u, kind::OWNER_CHANGED, json!({"chat": id, "title": title})).await?;
        }
    }
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("chat.owner.assign", Source::Portal).object("chat", id).details(json!({
            "previous_owner": previous,
            "new_owner": b.user_id,
            "previous_becomes": b.previous,
            "self_assigned": b.user_id == user.user_id,
            "unlocked": locked,
        })),
    )
    .await?;
    let mut also = vec![b.user_id];
    also.extend(previous);
    changed(&mut tx, id, &also).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Объём переписки: сколько сообщений и файлов и сколько места они занимают в базе.
#[utoipa::path(get, path = "/api/admin/chats/storage", tag = "chat", responses((status = 200, body = ChatStorage)))]
async fn chat_admin_storage(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<ChatStorage>> {
    user.require_superadmin()?;
    let (messages, message_bytes): (i64, i64) =
        sqlx::query_as("SELECT count(*), coalesce(sum(octet_length(body)), 0)::bigint FROM chat_messages")
            .fetch_one(&core.db)
            .await?;
    let (files, file_bytes, expired_files): (i64, i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE expired_at IS NULL), coalesce(sum(size) FILTER (WHERE expired_at IS NULL), 0)::bigint,
                count(*) FILTER (WHERE expired_at IS NOT NULL)
           FROM chat_files WHERE message_id IS NOT NULL",
    )
    .fetch_one(&core.db)
    .await?;
    let (chat_tables_bytes, database_bytes): (i64, i64) = sqlx::query_as(
        "SELECT (pg_total_relation_size('chat_messages') + pg_total_relation_size('chat_message_versions')
                 + pg_total_relation_size('chat_files') + pg_total_relation_size('chat_reactions')
                 + pg_total_relation_size('chat_gifs'))::bigint,
                pg_database_size(current_database())::bigint",
    )
    .fetch_one(&core.db)
    .await?;
    let cfg: ChatSettings = settings::load(&core.db).await?;
    Ok(Json(ChatStorage {
        messages,
        message_bytes,
        files,
        file_bytes,
        expired_files,
        chat_tables_bytes,
        database_bytes,
        file_retention_days: cfg.file_retention_days,
    }))
}

#[utoipa::path(get, path = "/api/admin/chats/settings", tag = "chat", responses((status = 200, body = ChatSettings)))]
async fn chat_admin_get_settings(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<ChatSettings>> {
    user.require_superadmin()?;
    Ok(Json(settings::load(&core.db).await?))
}

/// Срок хранения вложений. Стирается только содержимое: в переписке остаётся имя файла с отметкой.
#[utoipa::path(put, path = "/api/admin/chats/settings", tag = "chat", request_body = ChatSettings,
    responses((status = 204)))]
async fn chat_admin_put_settings(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<ChatSettings>,
) -> ApiResult<StatusCode> {
    user.require_superadmin()?;
    if b.file_retention_days > MAX_RETENTION_DAYS {
        return Err(ApiError::bad_request("invalid_retention", "Срок хранения — не больше 3650 дней."));
    }
    let mut tx = core.db.begin().await?;
    settings::save(&mut *tx, &b, Some(user.user_id)).await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("chat.settings", Source::Portal).details(json!({"file_retention_days": b.file_retention_days})),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

fn shown(n: i64) -> Option<i64> {
    (n >= MIN_COUNT).then_some(n)
}

/// Обезличенная статистика по всему порталу по месяцам (без разбивки по людям, отделам и чатам).
#[utoipa::path(get, path = "/api/admin/chats/stats", tag = "chat", responses((status = 200, body = ChatStats)))]
async fn chat_admin_stats(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<ChatStats>> {
    user.require_superadmin()?;
    let (direct, group): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE kind = 'direct'), count(*) FILTER (WHERE kind = 'group') FROM chats",
    )
    .fetch_one(&core.db)
    .await?;
    let rows: Vec<(String, i64, i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT to_char(date_trunc('month', x.created_at), 'YYYY-MM'),
                count(*), count(*) FILTER (WHERE c.kind = 'direct'), count(*) FILTER (WHERE c.kind = 'group'),
                count(*) FILTER (WHERE x.edited_at IS NOT NULL), count(DISTINCT x.author_id)
           FROM chat_messages x JOIN chats c ON c.id = x.chat_id
          GROUP BY 1 ORDER BY 1 DESC LIMIT 36",
    )
    .fetch_all(&core.db)
    .await?;
    let months = rows
        .into_iter()
        .map(|(month, all, d, g, e, s)| ChatStatsMonth {
            month,
            messages: shown(all),
            direct_messages: shown(d),
            group_messages: shown(g),
            edited: shown(e),
            senders: shown(s),
        })
        .collect();
    Ok(Json(ChatStats { direct_chats: shown(direct), group_chats: shown(group), months }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_and_title_rules() {
        assert!(clean_body("  ").is_err());
        assert_eq!(clean_body(" привет \n").unwrap(), "привет");
        assert!(clean_body(&"я".repeat(MAX_BODY + 1)).is_err());
        assert!(clean_body(&"я".repeat(MAX_BODY)).is_ok());
        assert!(clean_title("a\nb").is_err());
        assert!(clean_title(&"x".repeat(MAX_TITLE + 1)).is_err());
        assert_eq!(clean_title("  Отдел ИТ ").unwrap(), "Отдел ИТ");
        assert_eq!(shown(4), None);
        assert_eq!(shown(5), Some(5));
        assert_eq!(first_line("\n  \nпервая строка\nвторая"), "первая строка");
        assert!(REACTIONS.iter().all(|e| (1..=16).contains(&e.len())), "влезают в столбец");
    }
}
