//! «Мои GIF»: личная подборка анимаций сотрудника, как сохранённые GIF в привычных мессенджерах.
//! GIF добавляют загрузкой или из переписки («Сохранить в мои GIF») и отправляют одним нажатием —
//! в чат уходит копия, она живёт по правилам вложений (срок хранения, видимость участникам).
//! Подборку видит и меняет только её владелец; содержимое хранится зашифрованным ключом портала.

use super::*;

/// GIF в подборке одного сотрудника.
pub const GIFS_MAX: i64 = 200;
/// Размер одного GIF.
pub const GIF_MAX_MB: usize = 10;
/// Объём всей подборки одного сотрудника.
pub const GIFS_TOTAL_MB: i64 = 150;
/// Пополнений подборки (загрузка, «Сохранить в мои GIF») от одного сотрудника в минуту.
const GIF_STORES_PER_MINUTE: u32 = 20;
static GIF_STORED: LazyLock<Mutex<HashMap<Uuid, (Instant, u32)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// Тело загрузки GIF с запасом на заголовки формы.
const GIF_BODY_LIMIT: usize = (GIF_MAX_MB + 1) * 1024 * 1024;
/// Отправок GIF от одного сотрудника в минуту: каждая копирует файл и рассылает событие.
const GIF_SENDS_PER_MINUTE: u32 = 20;
static GIF_SENT: LazyLock<Mutex<HashMap<Uuid, (Instant, u32)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(gif_list))
        .merge(OpenApiRouter::new().routes(routes!(gif_upload)).layer(DefaultBodyLimit::max(GIF_BODY_LIMIT)))
        .routes(routes!(gif_save_from_chat))
        .routes(routes!(gif_get, gif_delete))
        .routes(routes!(gif_send))
}

/// GIF подборки.
#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ChatGif {
    pub id: Uuid,
    pub size: i64,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
}

#[derive(Deserialize, ToSchema)]
pub struct ChatGifFromFile {
    /// Вложение переписки (GIF), которое сохранить в подборку.
    pub file: Uuid,
}

#[derive(Deserialize, ToSchema)]
pub struct ChatGifSend {
    /// Ответ на сообщение этого чата.
    #[serde(default)]
    pub reply_to: Option<Uuid>,
}

fn not_gif() -> ApiError {
    ApiError::bad_request("not_gif", "Это не GIF.")
}

fn too_big() -> ApiError {
    ApiError::bad_request("file_too_large", format!("GIF больше {GIF_MAX_MB} МБ."))
}

/// Частота пополнения подборки: каждое — запись до 10 МБ в базу.
fn store_allowed(user: Uuid) -> ApiResult<()> {
    if within_minute(&GIF_STORED, user, GIF_STORES_PER_MINUTE) { Ok(()) } else { Err(slow_down()) }
}

/// Положить GIF в подборку: тот же GIF второй раз не добавляется (поднимается наверх).
async fn store(core: &Core, user: Uuid, data: Vec<u8>) -> ApiResult<ChatGif> {
    if data.len() > GIF_MAX_MB * 1024 * 1024 {
        return Err(too_big());
    }
    // Полная сигнатура GIF87a/GIF89a, а не только «GIF8» в начале.
    let signed = data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a");
    if !signed || pult_core::files::check_type("animation.gif", &data).ok() != Some("image/gif") {
        return Err(not_gif());
    }
    let hash = pult_core::crypto::sha256(&data);
    let size = data.len() as i64;
    let enc = core.secrets.encrypt_vec(data).map_err(ApiError::internal)?;
    let mut tx = core.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('chat_gifs:' || $1::text, 0))")
        .bind(user)
        .execute(&mut *tx)
        .await?;
    let existing: Option<ChatGif> = sqlx::query_as(
        "UPDATE chat_gifs SET created_at = now() WHERE user_id = $1 AND sha256 = $2 RETURNING id, size, created_at",
    )
    .bind(user)
    .bind(&hash)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(g) = existing {
        tx.commit().await?;
        return Ok(g);
    }
    let (count, total): (i64, i64) =
        sqlx::query_as("SELECT count(*), COALESCE(sum(size), 0)::bigint FROM chat_gifs WHERE user_id = $1")
            .bind(user)
            .fetch_one(&mut *tx)
            .await?;
    if count >= GIFS_MAX {
        return Err(ApiError::bad_request(
            "too_many_gifs",
            format!("В подборке уже {GIFS_MAX} GIF — уберите ненужные."),
        ));
    }
    if total + size > GIFS_TOTAL_MB * 1024 * 1024 {
        return Err(ApiError::bad_request(
            "too_many_gifs",
            format!("Подборка заняла {GIFS_TOTAL_MB} МБ — уберите ненужные GIF."),
        ));
    }
    let g: ChatGif = sqlx::query_as(
        "INSERT INTO chat_gifs (user_id, sha256, size, data) VALUES ($1, $2, $3, $4) RETURNING id, size, created_at",
    )
    .bind(user)
    .bind(&hash)
    .bind(size)
    .bind(enc)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(g)
}

/// Моя подборка GIF, недавние сверху.
#[utoipa::path(get, path = "/api/chat/gifs", tag = "chat", responses((status = 200, body = Vec<ChatGif>)))]
async fn gif_list(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<ChatGif>>> {
    let rows: Vec<ChatGif> = sqlx::query_as(
        "SELECT id, size, created_at FROM chat_gifs WHERE user_id = $1 ORDER BY created_at DESC, id DESC LIMIT $2",
    )
    .bind(user.user_id)
    .bind(GIFS_MAX)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

/// Добавить GIF в подборку с компьютера.
#[utoipa::path(post, path = "/api/chat/gifs", tag = "chat",
    request_body(content_type = "multipart/form-data", content = inline(ChatUploadForm)),
    responses((status = 200, body = ChatGif), (status = 400, body = pult_core::error::ErrorBody)))]
async fn gif_upload(State(core): State<Core>, user: CurrentUser, mut form: Multipart) -> ApiResult<Json<ChatGif>> {
    store_allowed(user.user_id)?;
    let _slot = UPLOADS.try_acquire().map_err(|_| ApiError::TooManyRequests {
        message: "Сервер сейчас принимает много файлов. Повторите через минуту.".into(),
        retry_after_secs: 60,
    })?;
    let limit = GIF_MAX_MB * 1024 * 1024;
    let bad = |m: &str| ApiError::bad_request("invalid_file", m);
    let broken = |e: axum::extract::multipart::MultipartError, m: &str| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE { too_big() } else { bad(m) }
    };
    let mut field = loop {
        match form.next_field().await.map_err(|e| broken(e, "Файл не получен."))? {
            Some(f) if f.name() == Some("file") => break f,
            Some(_) => continue,
            None => return Err(bad("Файл не получен.")),
        }
    };
    let mut data = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|e| broken(e, "Загрузка оборвалась."))? {
        if data.len() + chunk.len() > limit {
            return Err(too_big());
        }
        data.extend_from_slice(&chunk);
    }
    Ok(Json(store(&core, user.user_id, data).await?))
}

/// Сохранить GIF из переписки. Доступен тот же файл, что и для скачивания: видимое мне сообщение.
#[utoipa::path(post, path = "/api/chat/gifs/from-file", tag = "chat", request_body = ChatGifFromFile,
    responses((status = 200, body = ChatGif), (status = 400, body = pult_core::error::ErrorBody)))]
async fn gif_save_from_chat(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<ChatGifFromFile>,
) -> ApiResult<Json<ChatGif>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        chat_id: Uuid,
        sent_at: Option<OffsetDateTime>,
        mime: String,
        expired: bool,
    }
    let row: Option<Row> = sqlx::query_as(
        "SELECT f.chat_id, x.created_at AS sent_at, f.mime, f.expired_at IS NOT NULL AS expired
           FROM chat_files f LEFT JOIN chat_messages x ON x.id = f.message_id
          WHERE f.id = $1",
    )
    .bind(b.file)
    .fetch_optional(&core.db)
    .await?;
    let Row { chat_id, sent_at, mime, expired } = row.ok_or(ApiError::NotFound)?;
    let m = membership(&core.db, chat_id, user.user_id).await?;
    m.require_unlocked()?;
    // Только отправленное и видимое мне (вышедший — до выхода).
    let visible = match (sent_at, m.left_at) {
        (None, _) => false,
        (Some(_), None) => true,
        (Some(sent), Some(left)) => sent <= left,
    };
    if !visible {
        return Err(ApiError::NotFound);
    }
    if expired {
        return Err(ApiError::Gone);
    }
    if mime != "image/gif" {
        return Err(not_gif());
    }
    store_allowed(user.user_id)?;
    let _slot = DOWNLOADS.try_acquire().map_err(|_| ApiError::TooManyRequests {
        message: "Сервер сейчас отдаёт много файлов. Повторите через минуту.".into(),
        retry_after_secs: 30,
    })?;
    let enc: Vec<u8> =
        sqlx::query_scalar("SELECT data FROM chat_files WHERE id = $1").bind(b.file).fetch_one(&core.db).await?;
    let data = core.secrets.decrypt_bytes(&enc).map_err(ApiError::internal)?;
    Ok(Json(store(&core, user.user_id, data).await?))
}

/// Содержимое GIF из моей подборки.
#[utoipa::path(get, path = "/api/chat/gifs/{gif}", tag = "chat", params(("gif" = Uuid, Path)),
    responses((status = 200, description = "GIF"), (status = 304, description = "Не изменился"), (status = 404, body = pult_core::error::ErrorBody)))]
async fn gif_get(
    State(core): State<Core>,
    user: CurrentUser,
    Path(gif): Path<Uuid>,
    headers: axum::http::HeaderMap,
) -> ApiResult<Response> {
    // Метка содержимого — его SHA-256: повторное открытие подборки получает 304 без расшифровки.
    let hash: Option<Vec<u8>> = sqlx::query_scalar("SELECT sha256 FROM chat_gifs WHERE id = $1 AND user_id = $2")
        .bind(gif)
        .bind(user.user_id)
        .fetch_optional(&core.db)
        .await?;
    let hex: String = hash.ok_or(ApiError::NotFound)?.iter().map(|b| format!("{b:02x}")).collect();
    let etag = format!("\"{hex}\"");
    // no-cache: браузер хранит копию, но перед показом спрашивает сервер — после выхода из учётки
    // (на общем компьютере) GIF без входа не покажется.
    let cache = (header::CACHE_CONTROL, "private, no-cache".to_string());
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag), cache]).into_response());
    }
    let _slot = DOWNLOADS.try_acquire().map_err(|_| ApiError::TooManyRequests {
        message: "Сервер сейчас отдаёт много файлов. Повторите через минуту.".into(),
        retry_after_secs: 30,
    })?;
    let enc: Option<Vec<u8>> = sqlx::query_scalar("SELECT data FROM chat_gifs WHERE id = $1 AND user_id = $2")
        .bind(gif)
        .bind(user.user_id)
        .fetch_optional(&core.db)
        .await?;
    let data = core.secrets.decrypt_bytes(&enc.ok_or(ApiError::NotFound)?).map_err(ApiError::internal)?;
    Ok(([(header::CONTENT_TYPE, "image/gif".to_string()), (header::ETAG, etag), cache], data).into_response())
}

/// Убрать GIF из подборки. Отправленные копии остаются в переписке.
#[utoipa::path(delete, path = "/api/chat/gifs/{gif}", tag = "chat", params(("gif" = Uuid, Path)),
    responses((status = 204), (status = 404, body = pult_core::error::ErrorBody)))]
async fn gif_delete(State(core): State<Core>, user: CurrentUser, Path(gif): Path<Uuid>) -> ApiResult<StatusCode> {
    let n = sqlx::query("DELETE FROM chat_gifs WHERE id = $1 AND user_id = $2")
        .bind(gif)
        .bind(user.user_id)
        .execute(&core.db)
        .await?
        .rows_affected();
    if n == 0 { Err(ApiError::NotFound) } else { Ok(StatusCode::NO_CONTENT) }
}

/// Отправить GIF из подборки в чат: сообщение с копией GIF без текста.
#[utoipa::path(post, path = "/api/chat/{id}/gifs/{gif}", tag = "chat",
    params(("id" = Uuid, Path), ("gif" = Uuid, Path)), request_body = ChatGifSend,
    responses((status = 201, body = ChatMessage), (status = 404, body = pult_core::error::ErrorBody)))]
async fn gif_send(
    State(core): State<Core>,
    user: CurrentUser,
    Path((id, gif)): Path<(Uuid, Uuid)>,
    Json(b): Json<ChatGifSend>,
) -> ApiResult<(StatusCode, Json<ChatMessage>)> {
    let m = membership(&core.db, id, user.user_id).await?;
    if !m.active() {
        return Err(ApiError::Refused {
            code: "read_only",
            message: "Вы вышли из чата — история доступна только для чтения.".into(),
        });
    }
    m.require_unlocked()?;
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
    if !within_minute(&GIF_SENT, user.user_id, GIF_SENDS_PER_MINUTE) {
        return Err(slow_down());
    }
    let mut tx = core.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('chat_files:' || $1::text, 0))")
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    // FOR SHARE: одновременное «Убрать из подборки» ждёт, пока копия не ляжет в чат.
    let size: Option<i64> = sqlx::query_scalar("SELECT size FROM chat_gifs WHERE id = $1 AND user_id = $2 FOR SHARE")
        .bind(gif)
        .bind(user.user_id)
        .fetch_optional(&mut *tx)
        .await?;
    let size = size.ok_or(ApiError::NotFound)?;
    upload_quota(&mut *tx, user.user_id, size, true).await?;
    let mid: Uuid = sqlx::query_scalar(
        "INSERT INTO chat_messages (chat_id, author_id, body, reply_to) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(id)
    .bind(user.user_id)
    .bind(encrypt(&core, "")?)
    .bind(b.reply_to)
    .fetch_one(&mut *tx)
    .await?;
    let copied = sqlx::query(
        "INSERT INTO chat_files (chat_id, message_id, uploaded_by, name, mime, size, data)
         SELECT $1, $2, $3, $4, 'image/gif', size, data FROM chat_gifs WHERE id = $5 AND user_id = $3",
    )
    .bind(id)
    .bind(mid)
    .bind(user.user_id)
    .bind(encrypt(&core, "animation.gif")?)
    .bind(gif)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if copied != 1 {
        return Err(ApiError::NotFound);
    }
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
