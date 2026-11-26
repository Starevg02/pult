//! Профиль для коллег: присутствие («в сети», «нет на месте», «не беспокоить», «невидимка») и свой статус
//! со сроком, аватар, «о себе» и стена с ответами и отметками «нравится».
//!
//! Присутствие считается по сигналу открытой вкладки (`/api/me/heartbeat`, раз в минуту): нет сигнала
//! дольше [`ONLINE_SECS`] — «не в сети». «Нет на месте» ставит сама вкладка, когда пользователь долго ничего
//! не делает. «Не беспокоить» глушит уведомления на устройства. Аватар уменьшается в браузере и хранится в
//! базе; анимированный GIF уменьшает сервер ([`shrink_gif`]). Принимаются только JPEG, PNG, WebP и GIF, тип
//! проверяется по сигнатуре файла, а не по заявленному.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Permission,
    audit::{self, Entry, Source},
    crypto::sha256,
    events::{self, Event},
    notify,
    rbac::ROLE_SUPERADMIN,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Удалять чужие аватары и записи на стенах.
pub const MODERATE: Permission = Permission {
    code: "people.profile.moderate",
    description: "Удалять неподходящие аватары и записи на стенах сотрудников",
    default_roles: &[ROLE_SUPERADMIN],
};

/// Виды уведомлений стены.
pub mod kind {
    pub const WALL_POST: &str = "people.wall_post";
    pub const WALL_REPLY: &str = "people.wall_reply";
}

/// Без сигнала вкладки дольше этого — «не в сети».
pub const ONLINE_SECS: i64 = 150;
/// Предел хранимого аватара: неподвижная картинка после сжатия в браузере — десятки КБ, уменьшенный GIF — больше.
pub const AVATAR_MAX_BYTES: usize = 1024 * 1024;
/// Сторона квадрата аватара (так же уменьшает браузер).
pub const AVATAR_SIDE: u32 = 256;
/// Исходный GIF: до 15 МБ, до 2048 точек по стороне, до 300 кадров — дальше уменьшение слишком дорого.
pub const GIF_INPUT_MAX_BYTES: usize = 15 * 1024 * 1024;
const GIF_MAX_SIDE: u32 = 2048;
const GIF_MAX_FRAMES: usize = 300;
/// Тело запроса с GIF в base64 (на треть длиннее данных) и запасом на JSON.
pub const AVATAR_BODY_LIMIT: usize = GIF_INPUT_MAX_BYTES / 3 * 4 + 64 * 1024;
/// Одновременно уменьшается не больше двух GIF: работа для процессора, а не для очереди запросов. Место
/// держит сама работа (а не запрос): оборванный запрос не освобождает его, пока GIF ещё уменьшается.
static GIF_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
pub const ABOUT_MAX: usize = 1000;
pub const STATUS_TEXT_MAX: usize = 100;
pub const STATUS_EMOJI_MAX: usize = 16;
pub const POST_MAX: usize = 2000;
/// Не больше стольких записей и ответов за [`POST_WINDOW_MINUTES`]: стена — не канал рассылки.
pub const POSTS_PER_WINDOW: i64 = 30;
pub const POST_WINDOW_MINUTES: i32 = 10;
const PAGE: i64 = 20;
const REPLIES_PER_POST: i64 = 50;
const LOOKUP_MAX: usize = 200;

#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PresenceMode {
    /// «В сети» или «нет на месте» по активности.
    Auto,
    Away,
    Dnd,
    /// Коллеги видят «не в сети».
    Invisible,
}

/// Что видят коллеги.
#[derive(Serialize, ToSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    Online,
    Away,
    Dnd,
    Offline,
}

#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WallMode {
    /// Пишут все коллеги.
    Everyone,
    /// Записи — только владелец, коллеги отвечают.
    Me,
    /// Стена скрыта.
    Off,
}

fn mode_of(s: &str) -> PresenceMode {
    match s {
        "away" => PresenceMode::Away,
        "dnd" => PresenceMode::Dnd,
        "invisible" => PresenceMode::Invisible,
        _ => PresenceMode::Auto,
    }
}

fn mode_str(m: PresenceMode) -> &'static str {
    match m {
        PresenceMode::Auto => "auto",
        PresenceMode::Away => "away",
        PresenceMode::Dnd => "dnd",
        PresenceMode::Invisible => "invisible",
    }
}

fn wall_of(s: &str) -> WallMode {
    match s {
        "me" => WallMode::Me,
        "off" => WallMode::Off,
        _ => WallMode::Everyone,
    }
}

fn wall_str(m: WallMode) -> &'static str {
    match m {
        WallMode::Everyone => "everyone",
        WallMode::Me => "me",
        WallMode::Off => "off",
    }
}

/// Присутствие для коллег: выбранный режим, свежесть сигнала и «нет действий» от вкладки.
pub fn presence_of(mode: PresenceMode, seen_secs_ago: Option<i64>, idle: bool) -> Presence {
    let connected = seen_secs_ago.is_some_and(|s| s <= ONLINE_SECS);
    match mode {
        _ if !connected => Presence::Offline,
        PresenceMode::Invisible => Presence::Offline,
        PresenceMode::Dnd => Presence::Dnd,
        PresenceMode::Away => Presence::Away,
        PresenceMode::Auto if idle => Presence::Away,
        PresenceMode::Auto => Presence::Online,
    }
}

/// Версия аватара для адреса картинки (`?v=`): меняется вместе с содержимым.
fn avatar_version(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// Тип картинки по сигнатуре; что-то другое (SVG, HTML под видом картинки) не принимается.
pub fn sniff_image(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some("image/webp")
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum GifError {
    /// Не читается или больше [`GIF_MAX_SIDE`] по стороне.
    Unreadable,
    TooManyFrames,
    /// Даже на самой грубой ступени больше [`AVATAR_MAX_BYTES`].
    TooLarge,
}

/// Ступени уменьшения: сторона, каждый какой кадр оставить, допуск «пиксель не изменился» (по каналу).
/// Сначала — полный квадрат и все кадры; дальше реже кадры и меньше сторона, пока не уложится в предел.
const GIF_STEPS: [(u32, usize, u8); 6] =
    [(256, 1, 4), (256, 2, 8), (192, 2, 12), (160, 3, 16), (128, 4, 20), (96, 6, 24)];
/// Дольше этого ступени не перебираются: работа на сервере, а не бесконечная очередь.
const GIF_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
/// Индекс прозрачного цвета в палитре кадра.
const CLEAR: u8 = 255;

/// Кадр после обрезки в квадрат и уменьшения: RGBA и задержка в миллисекундах.
struct GifFrame {
    rgba: image::RgbaImage,
    delay_ms: u32,
}

/// Уменьшить GIF до квадрата [`AVATAR_SIDE`]: центр кадра, бесконечный повтор, задержки сохраняются.
/// В каждый кадр пишется только изменившаяся часть (остальное прозрачно), одинаковые кадры склеиваются.
/// Если результат больше [`AVATAR_MAX_BYTES`], кадры прореживаются и сторона уменьшается ([`GIF_STEPS`]).
/// Маленький GIF (не больше квадрата и предела) остаётся как есть, если все его кадры читаются.
pub fn shrink_gif(data: &[u8]) -> Result<Vec<u8>, GifError> {
    shrink_gif_within(data, GIF_BUDGET)
}

/// [`shrink_gif`] с заданным сроком работы.
fn shrink_gif_within(data: &[u8], budget: std::time::Duration) -> Result<Vec<u8>, GifError> {
    use image::{AnimationDecoder, ImageDecoder, Limits, codecs::gif, imageops};
    let mut dec = gif::GifDecoder::new(std::io::Cursor::new(data)).map_err(|_| GifError::Unreadable)?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(GIF_MAX_SIDE);
    limits.max_image_height = Some(GIF_MAX_SIDE);
    limits.max_alloc = Some(64 * 1024 * 1024);
    dec.set_limits(limits).map_err(|_| GifError::Unreadable)?;
    let (w, h) = dec.dimensions();
    if w == 0 || h == 0 {
        return Err(GifError::Unreadable);
    }
    let as_is = w <= AVATAR_SIDE && h <= AVATAR_SIDE && data.len() <= AVATAR_MAX_BYTES;
    let side = w.min(h);
    let (x, y) = ((w - side) / 2, (h - side) / 2);
    let out_side = side.min(AVATAR_SIDE);
    let started = std::time::Instant::now();
    let mut frames = Vec::new();
    for (i, frame) in dec.into_frames().enumerate() {
        if i >= GIF_MAX_FRAMES {
            return Err(GifError::TooManyFrames);
        }
        if started.elapsed() > budget {
            return Err(GifError::TooLarge);
        }
        let frame = frame.map_err(|_| GifError::Unreadable)?;
        if as_is {
            continue;
        }
        let (numer, denom) = frame.delay().numer_denom_ms();
        let delay_ms = if denom == 0 { 0 } else { numer / denom };
        let full = frame.into_buffer();
        let square = imageops::crop_imm(&full, x, y, side, side).to_image();
        let rgba = if out_side < side {
            imageops::resize(&square, out_side, out_side, imageops::FilterType::Triangle)
        } else {
            square
        };
        frames.push(GifFrame { rgba, delay_ms });
    }
    // Маленький GIF хранится как есть, но только если все кадры прочитались и их не больше предела.
    if as_is {
        return Ok(data.to_vec());
    }
    if frames.is_empty() {
        return Err(GifError::Unreadable);
    }
    // С прозрачностью разностные кадры не годятся: прозрачная точка значит «оставить прежнюю».
    let opaque = frames.iter().all(|f| f.rgba.pixels().all(|p| p[3] == 255));
    // Размер прошлой попытки: ступень, которая по оценке (площадь кадра и число кадров) всё равно выйдет
    // заметно больше предела, пропускается — не тратить на неё время.
    let mut last: Option<(f64, u32, usize)> = None;
    for (i, (step_side, every, tolerance)) in GIF_STEPS.into_iter().enumerate() {
        if started.elapsed() > budget {
            break;
        }
        let side = step_side.min(out_side);
        // Последняя, самая грубая ступень пробуется всегда.
        if let Some((bytes, s, e)) = last
            && i + 1 < GIF_STEPS.len()
        {
            let guess = bytes * (f64::from(side) / f64::from(s)).powi(2) * (e as f64 / every as f64);
            if guess > AVATAR_MAX_BYTES as f64 * 1.5 {
                continue;
            }
        }
        let out = encode_gif(&frames, side, every, if opaque { Some(tolerance) } else { None })?;
        if out.len() <= AVATAR_MAX_BYTES {
            return Ok(out);
        }
        last = Some((out.len() as f64, side, every));
    }
    Err(GifError::TooLarge)
}

/// Записать кадры квадратом `side`, оставив каждый `every`-й (задержки пропущенных прибавляются).
/// `diff` — допуск разностных кадров; `None` — каждый кадр целиком (для GIF с прозрачностью).
fn encode_gif(frames: &[GifFrame], side: u32, every: usize, diff: Option<u8>) -> Result<Vec<u8>, GifError> {
    use image::imageops;
    let n = side as usize;
    let mut out = Vec::new();
    let mut enc = ::gif::Encoder::new(&mut out, side as u16, side as u16, &[]).map_err(|_| GifError::Unreadable)?;
    enc.set_repeat(::gif::Repeat::Infinite).map_err(|_| GifError::Unreadable)?;
    // Что сейчас на экране (исходные цвета последних записанных точек): с ним сравнивается следующий кадр.
    let mut shown: Option<Vec<u8>> = None;
    let mut pending: Option<::gif::Frame<'static>> = None;
    for group in frames.chunks(every.max(1)) {
        let delay_cs = (group.iter().map(|f| u64::from(f.delay_ms)).sum::<u64>() / 10).min(u64::from(u16::MAX)) as u16;
        let img = if side == group[0].rgba.width() {
            group[0].rgba.clone()
        } else {
            imageops::resize(&group[0].rgba, side, side, imageops::FilterType::Triangle)
        };
        let px = img.into_raw();
        // Прямоугольник изменившихся точек (весь кадр — для первого и без разностей).
        let changed = |i: usize, prev: &[u8], tol: u8| (0..3).any(|c| px[i * 4 + c].abs_diff(prev[i * 4 + c]) > tol);
        let (mut x0, mut y0, mut x1, mut y1) = (0, 0, n, n);
        if let (Some(tol), Some(prev)) = (diff, shown.as_deref()) {
            (x0, y0, x1, y1) = (n, n, 0, 0);
            for yy in 0..n {
                for xx in 0..n {
                    if changed(yy * n + xx, prev, tol) {
                        (x0, y0, x1, y1) = (x0.min(xx), y0.min(yy), x1.max(xx + 1), y1.max(yy + 1));
                    }
                }
            }
            if x0 >= x1 {
                // Ничего не изменилось: дольше показывать прежний кадр.
                if let Some(p) = pending.as_mut() {
                    p.delay = p.delay.saturating_add(delay_cs);
                }
                continue;
            }
        }
        let (rw, rh) = (x1 - x0, y1 - y0);
        let mut rect = Vec::with_capacity(rw * rh * 4);
        let mut solid = Vec::with_capacity(rw * rh);
        for yy in y0..y1 {
            for xx in x0..x1 {
                let i = yy * n + xx;
                rect.extend_from_slice(&px[i * 4..i * 4 + 4]);
                let keep = match (diff, shown.as_deref()) {
                    (Some(tol), Some(prev)) => changed(i, prev, tol),
                    _ => px[i * 4 + 3] >= 128,
                };
                solid.push(keep);
            }
        }
        if diff.is_some() {
            let screen = shown.get_or_insert_with(|| px.clone());
            for (k, &keep) in solid.iter().enumerate() {
                if keep {
                    let i = (y0 + k / rw) * n + x0 + k % rw;
                    screen[i * 4..i * 4 + 4].copy_from_slice(&px[i * 4..i * 4 + 4]);
                }
            }
        }
        let (palette, buffer) = indexed(&rect, &solid);
        let frame = ::gif::Frame {
            delay: delay_cs,
            dispose: if diff.is_some() { ::gif::DisposalMethod::Keep } else { ::gif::DisposalMethod::Background },
            transparent: Some(CLEAR),
            left: x0 as u16,
            top: y0 as u16,
            width: rw as u16,
            height: rh as u16,
            palette: Some(palette),
            buffer: std::borrow::Cow::Owned(buffer),
            ..::gif::Frame::default()
        };
        if let Some(p) = pending.replace(frame) {
            enc.write_frame(&p).map_err(|_| GifError::Unreadable)?;
        }
    }
    if let Some(p) = pending {
        enc.write_frame(&p).map_err(|_| GifError::Unreadable)?;
    }
    drop(enc);
    Ok(out)
}

/// Палитра (256 цветов, последний — прозрачный) и индексы точек: точная, если цветов не больше 255,
/// иначе NeuQuant по видимым точкам.
fn indexed(rgba: &[u8], solid: &[bool]) -> (Vec<u8>, Vec<u8>) {
    let mut exact = std::collections::HashMap::<[u8; 3], u8>::new();
    let mut fits = true;
    for (p, _) in rgba.chunks_exact(4).zip(solid).filter(|(_, s)| **s) {
        let c = [p[0], p[1], p[2]];
        if !exact.contains_key(&c) {
            if exact.len() == usize::from(CLEAR) {
                fits = false;
                break;
            }
            let i = exact.len() as u8;
            exact.insert(c, i);
        }
    }
    let mut palette = vec![0u8; 256 * 3];
    let buffer = if fits {
        for (c, &i) in &exact {
            palette[usize::from(i) * 3..usize::from(i) * 3 + 3].copy_from_slice(c);
        }
        rgba.chunks_exact(4).zip(solid).map(|(p, &s)| if s { exact[&[p[0], p[1], p[2]]] } else { CLEAR }).collect()
    } else {
        let visible: Vec<u8> =
            rgba.chunks_exact(4).zip(solid).filter(|(_, s)| **s).flat_map(|(p, _)| [p[0], p[1], p[2], 255]).collect();
        let nq = color_quant::NeuQuant::new(10, usize::from(CLEAR), &visible);
        palette[..usize::from(CLEAR) * 3].copy_from_slice(&nq.color_map_rgb());
        rgba.chunks_exact(4)
            .zip(solid)
            .map(|(p, &s)| if s { nq.index_of(&[p[0], p[1], p[2], 255]) as u8 } else { CLEAR })
            .collect()
    };
    (palette, buffer)
}

/// Убрать управляющие символы (кроме перевода строки в многострочных полях) и пробелы по краям.
fn clean(s: &str, multiline: bool) -> String {
    s.chars()
        .filter(|c| (!c.is_control() || (multiline && *c == '\n')) && !pult_core::files::is_spoofing(*c))
        .collect::<String>()
        .trim()
        .to_string()
}

fn too_long(message: String) -> ApiError {
    ApiError::bad_request("too_long", message)
}

// ------------------------------------------------------------------ Мой профиль

#[derive(Serialize, ToSchema)]
pub struct MyCard {
    pub presence: PresenceMode,
    pub status_emoji: Option<String>,
    pub status_text: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub status_until: Option<OffsetDateTime>,
    pub about: String,
    pub wall_mode: WallMode,
    /// Версия аватара (`/api/users/{id}/avatar?v=…`); нет аватара — инициалы.
    pub avatar: Option<String>,
}

/// Режим, эмодзи, текст, срок, «о себе», стена, аватар; у нового пользователя строки профиля ещё нет.
type MyRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<OffsetDateTime>,
    Option<String>,
    Option<String>,
    Option<String>,
);

async fn my_card(db: &sqlx::PgPool, user_id: Uuid) -> ApiResult<MyCard> {
    let row: Option<MyRow> = sqlx::query_as(
        "SELECT p.presence, p.status_emoji, p.status_text, p.status_until, p.about, p.wall_mode, a.sha256
           FROM users u
           LEFT JOIN user_profiles p ON p.user_id = u.id
           LEFT JOIN user_avatars a ON a.user_id = u.id
          WHERE u.id = $1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    let (presence, emoji, text, until, about, wall, sha) = row.unwrap_or_default();
    // Срок вышел — своего статуса уже нет.
    let expired = until.is_some_and(|u| u <= OffsetDateTime::now_utc());
    Ok(MyCard {
        presence: mode_of(presence.as_deref().unwrap_or("auto")),
        status_emoji: if expired { None } else { emoji },
        status_text: if expired { None } else { text },
        status_until: if expired { None } else { until },
        about: about.unwrap_or_default(),
        wall_mode: wall_of(wall.as_deref().unwrap_or("everyone")),
        avatar: sha.as_deref().map(avatar_version),
    })
}

/// Мой статус, «о себе», режим стены и аватар.
#[utoipa::path(get, path = "/api/me/card", tag = "profile", responses((status = 200, body = MyCard)))]
pub(crate) async fn get_my_card(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<MyCard>> {
    Ok(Json(my_card(&core.db, user.user_id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct StatusInput {
    pub presence: PresenceMode,
    #[serde(default)]
    pub status_emoji: Option<String>,
    #[serde(default)]
    pub status_text: Option<String>,
    /// Когда свой статус пропадёт; нет — пока не сменят.
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub status_until: Option<OffsetDateTime>,
}

/// Режим присутствия и свой статус (эмодзи, текст, срок).
#[utoipa::path(put, path = "/api/me/status", tag = "profile", request_body = StatusInput,
    responses((status = 200, body = MyCard), (status = 400, body = pult_core::error::ErrorBody)))]
pub(crate) async fn put_my_status(
    State(core): State<Core>,
    user: CurrentUser,
    Json(req): Json<StatusInput>,
) -> ApiResult<Json<MyCard>> {
    let emoji = req.status_emoji.as_deref().map(|s| clean(s, false)).filter(|s| !s.is_empty());
    let text = req.status_text.as_deref().map(|s| clean(s, false)).filter(|s| !s.is_empty());
    if emoji.as_ref().is_some_and(|e| e.chars().count() > STATUS_EMOJI_MAX) {
        return Err(too_long(format!("Значок статуса — не длиннее {STATUS_EMOJI_MAX} символов.")));
    }
    if text.as_ref().is_some_and(|t| t.chars().count() > STATUS_TEXT_MAX) {
        return Err(too_long(format!("Статус — не длиннее {STATUS_TEXT_MAX} символов.")));
    }
    let until = if emoji.is_none() && text.is_none() { None } else { req.status_until };
    if until.is_some_and(|u| u <= OffsetDateTime::now_utc()) {
        return Err(ApiError::bad_request("invalid_until", "Срок статуса уже прошёл."));
    }
    sqlx::query(
        "INSERT INTO user_profiles (user_id, presence, status_emoji, status_text, status_until)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (user_id) DO UPDATE SET presence = $2, status_emoji = $3, status_text = $4,
                status_until = $5, updated_at = now()",
    )
    .bind(user.user_id)
    .bind(mode_str(req.presence))
    .bind(&emoji)
    .bind(&text)
    .bind(until)
    .execute(&core.db)
    .await?;
    Ok(Json(my_card(&core.db, user.user_id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct AboutInput {
    pub about: String,
    pub wall_mode: WallMode,
}

/// «О себе» и кто пишет на моей стене.
#[utoipa::path(put, path = "/api/me/about", tag = "profile", request_body = AboutInput,
    responses((status = 200, body = MyCard), (status = 400, body = pult_core::error::ErrorBody)))]
pub(crate) async fn put_my_about(
    State(core): State<Core>,
    user: CurrentUser,
    Json(req): Json<AboutInput>,
) -> ApiResult<Json<MyCard>> {
    let about = clean(&req.about, true);
    if about.chars().count() > ABOUT_MAX {
        return Err(too_long(format!("«О себе» — не длиннее {ABOUT_MAX} символов.")));
    }
    sqlx::query(
        "INSERT INTO user_profiles (user_id, about, wall_mode) VALUES ($1, $2, $3)
         ON CONFLICT (user_id) DO UPDATE SET about = $2, wall_mode = $3, updated_at = now()",
    )
    .bind(user.user_id)
    .bind(&about)
    .bind(wall_str(req.wall_mode))
    .execute(&core.db)
    .await?;
    Ok(Json(my_card(&core.db, user.user_id).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct Heartbeat {
    /// Пользователь давно ничего не делает во вкладке.
    #[serde(default)]
    pub idle: bool,
}

/// Сигнал открытой вкладки — раз в минуту и при смене «нет действий».
#[utoipa::path(post, path = "/api/me/heartbeat", tag = "profile", request_body = Heartbeat, responses((status = 204)))]
pub(crate) async fn heartbeat(
    State(core): State<Core>,
    user: CurrentUser,
    Json(req): Json<Heartbeat>,
) -> ApiResult<StatusCode> {
    sqlx::query(
        "INSERT INTO user_profiles (user_id, seen_at, idle) VALUES ($1, now(), $2)
         ON CONFLICT (user_id) DO UPDATE SET seen_at = now(), idle = $2",
    )
    .bind(user.user_id)
    .bind(req.idle)
    .execute(&core.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct AvatarInput {
    /// Картинка в base64: JPEG, PNG или WebP до 1 МБ (браузер уже уменьшил её) либо GIF до 15 МБ —
    /// его уменьшает сервер, анимация сохраняется.
    pub data: String,
}

/// Поставить аватар. Браузер заранее обрезает картинку до квадрата и уменьшает её; GIF приходит как есть
/// (перерисовка в браузере оставила бы один кадр) и уменьшается здесь же.
#[utoipa::path(put, path = "/api/me/avatar", tag = "profile", request_body = AvatarInput,
    responses((status = 200, body = MyCard), (status = 400, body = pult_core::error::ErrorBody)))]
pub(crate) async fn put_my_avatar(
    State(core): State<Core>,
    user: CurrentUser,
    Json(req): Json<AvatarInput>,
) -> ApiResult<Json<MyCard>> {
    // base64 длиннее данных на треть: слишком длинную строку не декодируем вовсе. Больше 1 МБ — только GIF
    // (в base64 он начинается с «R0lG»).
    if req.data.len() > GIF_INPUT_MAX_BYTES / 3 * 4 + 8 {
        return Err(ApiError::bad_request("avatar_too_large", "GIF больше 15 МБ."));
    }
    if req.data.len() > AVATAR_MAX_BYTES / 3 * 4 + 8 && !req.data.trim_start().starts_with("R0lG") {
        return Err(ApiError::bad_request("avatar_too_large", "Картинка больше 1 МБ."));
    }
    let data = base64::engine::general_purpose::STANDARD
        .decode(req.data.trim())
        .map_err(|_| ApiError::bad_request("invalid_image", "Картинка не прочиталась."))?;
    let mime = sniff_image(&data)
        .ok_or_else(|| ApiError::bad_request("invalid_image", "Подходят только JPEG, PNG, WebP и GIF."))?;
    let data = if mime == "image/gif" {
        let slot = GIF_SLOTS.try_acquire().map_err(|_| ApiError::TooManyRequests {
            message: "Сервер сейчас уменьшает другие GIF. Повторите через минуту.".into(),
            retry_after_secs: 60,
        })?;
        tokio::task::spawn_blocking(move || {
            let _slot = slot;
            shrink_gif(&data)
        })
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map_err(|e| match e {
            GifError::Unreadable => {
                ApiError::bad_request("invalid_image", "GIF не прочитался или больше 2048 точек по стороне.")
            }
            GifError::TooManyFrames => ApiError::bad_request("avatar_too_large", "В GIF больше 300 кадров."),
            GifError::TooLarge => ApiError::bad_request(
                "avatar_too_large",
                "GIF не уменьшается до 1 МБ даже с пропуском кадров. Сократите его.",
            ),
        })?
    } else {
        data
    };
    if data.len() > AVATAR_MAX_BYTES {
        return Err(ApiError::bad_request("avatar_too_large", "Картинка больше 1 МБ."));
    }
    let sha: String = sha256(&data).iter().map(|b| format!("{b:02x}")).collect();
    sqlx::query(
        "INSERT INTO user_avatars (user_id, mime, data, sha256) VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id) DO UPDATE SET mime = $2, data = $3, sha256 = $4, updated_at = now()",
    )
    .bind(user.user_id)
    .bind(mime)
    .bind(&data)
    .bind(&sha)
    .execute(&core.db)
    .await?;
    Ok(Json(my_card(&core.db, user.user_id).await?))
}

/// Убрать свой аватар — снова инициалы.
#[utoipa::path(delete, path = "/api/me/avatar", tag = "profile", responses((status = 204)))]
pub(crate) async fn delete_my_avatar(State(core): State<Core>, user: CurrentUser) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM user_avatars WHERE user_id = $1").bind(user.user_id).execute(&core.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Убрать чужой неподходящий аватар. Пишется в аудит.
#[utoipa::path(delete, path = "/api/users/{id}/avatar", tag = "profile", params(("id" = Uuid, Path)),
    responses((status = 204), (status = 403), (status = 404)))]
pub(crate) async fn moderate_avatar(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require(&MODERATE)?;
    let mut tx = core.db.begin().await?;
    let gone = sqlx::query("DELETE FROM user_avatars WHERE user_id = $1").bind(id).execute(&mut *tx).await?;
    if gone.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&mut *tx, &user.actor, Entry::new("profile.avatar_removed", Source::Portal).object("user", id))
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Аватар сотрудника. Адрес с `?v=` меняется вместе с картинкой, поэтому браузер держит её в кэше.
#[utoipa::path(get, path = "/api/users/{id}/avatar", tag = "profile", params(("id" = Uuid, Path)),
    responses((status = 200, description = "Картинка"), (status = 304), (status = 404)))]
pub(crate) async fn get_avatar(
    State(core): State<Core>,
    _user: CurrentUser,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let row: Option<(String, String, Vec<u8>)> = sqlx::query_as(
        "SELECT a.mime, a.sha256, a.data FROM user_avatars a JOIN users u ON u.id = a.user_id
           JOIN ad_accounts acc ON acc.id = u.ad_account_id
          WHERE a.user_id = $1 AND u.is_active AND acc.is_enabled AND NOT acc.is_missing",
    )
    .bind(id)
    .fetch_optional(&core.db)
    .await?;
    let (mime, sha, data) = row.ok_or(ApiError::NotFound)?;
    let etag = format!("\"{}\"", avatar_version(&sha));
    // Час, а не сутки: убранный модератором аватар быстро пропадает и у коллег.
    let cache = (header::CACHE_CONTROL, "private, max-age=3600".to_string());
    let matches = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim().trim_start_matches("W/") == etag));
    if matches {
        return Ok((StatusCode::NOT_MODIFIED, [cache, (header::ETAG, etag)]).into_response());
    }
    Ok(([(header::CONTENT_TYPE, mime), cache, (header::ETAG, etag)], data).into_response())
}

// ------------------------------------------------------------------ Коллеги

#[derive(Serialize, ToSchema)]
pub struct PresenceEntry {
    pub user_id: Uuid,
    pub person_id: Option<Uuid>,
    pub presence: Presence,
    pub status_emoji: Option<String>,
    pub status_text: Option<String>,
    pub avatar: Option<String>,
}

#[derive(Deserialize, IntoParams)]
pub struct PresenceQuery {
    /// Пользователи через запятую.
    pub users: Option<String>,
    /// Сотрудники справочника через запятую — найдутся их учётки портала.
    pub persons: Option<String>,
}

fn ids(s: Option<&str>) -> ApiResult<Vec<Uuid>> {
    let list: Vec<Uuid> = s
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(|x| x.parse::<Uuid>().map_err(|_| ApiError::bad_request("invalid_id", "Неверный идентификатор.")))
        .collect::<Result<_, _>>()?;
    if list.len() > LOOKUP_MAX {
        return Err(ApiError::bad_request("too_many", format!("Не больше {LOOKUP_MAX} за раз.")));
    }
    Ok(list)
}

/// Строка присутствия: пользователь, сотрудник, режим, секунд с сигнала, «нет действий», свой статус, аватар.
type PresenceRow =
    (Uuid, Option<Uuid>, Option<String>, Option<i64>, Option<bool>, Option<String>, Option<String>, Option<String>);

const PRESENCE_SELECT: &str = "SELECT u.id, u.person_id, p.presence,
        extract(epoch FROM now() - p.seen_at)::bigint, p.idle,
        CASE WHEN p.status_until IS NULL OR p.status_until > now() THEN p.status_emoji END,
        CASE WHEN p.status_until IS NULL OR p.status_until > now() THEN p.status_text END,
        av.sha256
   FROM users u
   JOIN ad_accounts acc ON acc.id = u.ad_account_id
   LEFT JOIN user_profiles p ON p.user_id = u.id
   LEFT JOIN user_avatars av ON av.user_id = u.id";

fn entry(r: PresenceRow) -> PresenceEntry {
    let mode = mode_of(r.2.as_deref().unwrap_or("auto"));
    let presence = presence_of(mode, r.3, r.4.unwrap_or(false));
    // «Невидимка» не показывает и свой статус: иначе видно, что человек на месте.
    let hidden = mode == PresenceMode::Invisible;
    PresenceEntry {
        user_id: r.0,
        person_id: r.1,
        presence,
        status_emoji: if hidden { None } else { r.5 },
        status_text: if hidden { None } else { r.6 },
        avatar: r.7.as_deref().map(avatar_version),
    }
}

/// Присутствие, свой статус и аватар коллег — для списков (сообщения, справочник).
#[utoipa::path(get, path = "/api/users/presence", tag = "profile", params(PresenceQuery),
    responses((status = 200, body = Vec<PresenceEntry>), (status = 400, body = pult_core::error::ErrorBody)))]
pub(crate) async fn presence_lookup(
    State(core): State<Core>,
    _user: CurrentUser,
    Query(q): Query<PresenceQuery>,
) -> ApiResult<Json<Vec<PresenceEntry>>> {
    let users = ids(q.users.as_deref())?;
    let persons = ids(q.persons.as_deref())?;
    if users.is_empty() && persons.is_empty() {
        return Ok(Json(Vec::new()));
    }
    let sql = format!(
        "{PRESENCE_SELECT}
          WHERE (u.id = ANY($1) OR u.person_id = ANY($2)) AND u.is_active AND acc.is_enabled AND NOT acc.is_missing"
    );
    let rows: Vec<PresenceRow> = sqlx::query_as(&sql).bind(&users).bind(&persons).fetch_all(&core.db).await?;
    Ok(Json(rows.into_iter().map(entry).collect()))
}

#[derive(Serialize, ToSchema)]
pub struct UserCard {
    pub user_id: Uuid,
    pub display_name: String,
    pub is_me: bool,
    pub presence: Presence,
    pub status_emoji: Option<String>,
    pub status_text: Option<String>,
    pub avatar: Option<String>,
    pub about: String,
    pub wall_mode: WallMode,
    /// Можно написать на стене (не ответ, а новую запись).
    pub can_post: bool,
    /// Можно убрать аватар и чужие записи.
    pub can_moderate: bool,
    pub person: Option<crate::profile::ProfilePerson>,
}

type CardPersonRow = (
    Uuid,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Страница сотрудника: имя, присутствие, свой статус, «о себе», карточка справочника.
#[utoipa::path(get, path = "/api/users/{id}/card", tag = "profile", params(("id" = Uuid, Path)),
    responses((status = 200, body = UserCard), (status = 404)))]
pub(crate) async fn get_user_card(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<UserCard>> {
    let sql = format!(
        "{PRESENCE_SELECT}
          WHERE u.id = $1 AND u.is_active AND acc.is_enabled AND NOT acc.is_missing"
    );
    let row: PresenceRow = sqlx::query_as(&sql).bind(id).fetch_optional(&core.db).await?.ok_or(ApiError::NotFound)?;
    let (name, about, wall): (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT coalesce(a.display_name, a.sam_account_name), p.about, p.wall_mode
           FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id
           LEFT JOIN user_profiles p ON p.user_id = u.id WHERE u.id = $1",
    )
    .bind(id)
    .fetch_one(&core.db)
    .await?;
    let person = match row.1 {
        Some(pid) => sqlx::query_as::<_, CardPersonRow>(
            "SELECT p.id, p.last_name, p.first_name, p.middle_name, d.name, pos.name, s.name, p.room,
                    p.work_phone, p.internal_ext, p.mobile_phone, p.email
               FROM persons p
               LEFT JOIN departments d ON d.id = p.department_id
               LEFT JOIN positions pos ON pos.id = p.position_id
               LEFT JOIN sites s ON s.id = p.site_id
              WHERE p.id = $1 AND NOT p.is_archived",
        )
        .bind(pid)
        .fetch_optional(&core.db)
        .await?
        .map(|r| crate::profile::ProfilePerson {
            id: r.0,
            last_name: r.1,
            first_name: r.2,
            middle_name: r.3,
            department: r.4,
            position: r.5,
            site: r.6,
            room: r.7,
            work_phone: r.8,
            internal_ext: r.9,
            mobile_phone: r.10,
            email: r.11,
        }),
        None => None,
    };
    let is_me = id == user.user_id;
    let mut e = entry(row);
    let wall_mode = wall_of(wall.as_deref().unwrap_or("everyone"));
    if is_me {
        // Себе — что выбрано, а не «не в сети» у невидимки.
        let mine = my_card(&core.db, user.user_id).await?;
        e.status_emoji = mine.status_emoji;
        e.status_text = mine.status_text;
    }
    Ok(Json(UserCard {
        user_id: id,
        display_name: name,
        is_me,
        presence: e.presence,
        status_emoji: e.status_emoji,
        status_text: e.status_text,
        avatar: e.avatar,
        about: about.unwrap_or_default(),
        wall_mode,
        can_post: can_post(wall_mode, is_me),
        can_moderate: user.grants.has(&MODERATE),
        person,
    }))
}

fn can_post(mode: WallMode, owner: bool) -> bool {
    match mode {
        WallMode::Everyone => true,
        WallMode::Me => owner,
        WallMode::Off => false,
    }
}

// ------------------------------------------------------------------ Стена

#[derive(Serialize, ToSchema)]
pub struct WallAuthor {
    pub user_id: Uuid,
    pub name: String,
    pub avatar: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct WallPost {
    pub id: Uuid,
    pub author: WallAuthor,
    /// Чья стена (в ленте запись может быть на чужой стене).
    pub wall: WallAuthor,
    /// Пусто у удалённой записи.
    pub body: String,
    pub deleted: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    pub likes: i64,
    pub liked: bool,
    pub can_delete: bool,
    /// Ответы — только у записей верхнего уровня, старые сверху.
    /// `no_recursion`: без него сбор схем OpenAPI уходит в бесконечную рекурсию по самому типу.
    #[schema(no_recursion)]
    pub replies: Vec<WallPost>,
}

#[derive(Serialize, ToSchema)]
pub struct WallPage {
    pub posts: Vec<WallPost>,
    /// Есть записи старше последней.
    pub more: bool,
}

#[derive(Deserialize, IntoParams)]
pub struct WallQuery {
    /// Записи старше этой (id последней показанной).
    pub before: Option<Uuid>,
}

#[derive(sqlx::FromRow)]
struct PostRow {
    id: Uuid,
    author_id: Uuid,
    author_name: String,
    author_avatar: Option<String>,
    wall_user_id: Uuid,
    wall_name: String,
    wall_avatar: Option<String>,
    body: String,
    deleted: bool,
    created_at: OffsetDateTime,
    parent_id: Option<Uuid>,
    likes: i64,
    liked: bool,
}

/// `$2` — кто смотрит (его отметка «нравится»). `ou`/`oa`/`op` — владелец стены, его учётка и профиль.
const POST_SELECT: &str = "SELECT w.id, w.author_id, coalesce(a.display_name, a.sam_account_name) AS author_name,
        av.sha256 AS author_avatar, w.wall_user_id, coalesce(oa.display_name, oa.sam_account_name) AS wall_name,
        oav.sha256 AS wall_avatar, CASE WHEN w.deleted_at IS NULL THEN w.body ELSE '' END AS body,
        w.deleted_at IS NOT NULL AS deleted, w.created_at, w.parent_id,
        (SELECT count(*) FROM wall_likes l WHERE l.post_id = w.id) AS likes,
        EXISTS (SELECT 1 FROM wall_likes l WHERE l.post_id = w.id AND l.user_id = $2) AS liked
   FROM wall_posts w
   JOIN users u ON u.id = w.author_id
   JOIN ad_accounts a ON a.id = u.ad_account_id
   LEFT JOIN user_avatars av ON av.user_id = w.author_id
   JOIN users ou ON ou.id = w.wall_user_id
   JOIN ad_accounts oa ON oa.id = ou.ad_account_id
   LEFT JOIN user_avatars oav ON oav.user_id = w.wall_user_id
   LEFT JOIN user_profiles op ON op.user_id = w.wall_user_id";

fn post(r: PostRow, user: &CurrentUser) -> WallPost {
    WallPost {
        id: r.id,
        can_delete: !r.deleted
            && (r.author_id == user.user_id || r.wall_user_id == user.user_id || user.grants.has(&MODERATE)),
        author: WallAuthor {
            user_id: r.author_id,
            name: r.author_name,
            avatar: r.author_avatar.as_deref().map(avatar_version),
        },
        wall: WallAuthor {
            user_id: r.wall_user_id,
            name: r.wall_name,
            avatar: r.wall_avatar.as_deref().map(avatar_version),
        },
        body: r.body,
        deleted: r.deleted,
        created_at: r.created_at,
        likes: r.likes,
        liked: r.liked,
        replies: Vec::new(),
    }
}

/// Страница записей верхнего уровня, новые сверху, по [`PAGE`]; к каждой — до [`REPLIES_PER_POST`] ответов.
/// `wall` — одна стена; `None` — лента: все стены действующих сотрудников, кроме выключенных.
async fn page_of(
    db: &sqlx::PgPool,
    user: &CurrentUser,
    wall: Option<Uuid>,
    before: Option<Uuid>,
) -> ApiResult<WallPage> {
    // Удалённая запись без ответов не показывается: от неё ничего не осталось.
    let sql = format!(
        "{POST_SELECT}
          WHERE w.parent_id IS NULL
            AND ($1::uuid IS NULL OR w.wall_user_id = $1)
            AND ($1::uuid IS NOT NULL OR (ou.is_active AND oa.is_enabled AND NOT oa.is_missing
                                          AND coalesce(op.wall_mode, 'everyone') <> 'off'))
            AND (w.deleted_at IS NULL OR EXISTS (SELECT 1 FROM wall_posts r WHERE r.parent_id = w.id AND r.deleted_at IS NULL))
            AND ($3::uuid IS NULL OR (w.created_at, w.id) < (SELECT created_at, id FROM wall_posts WHERE id = $3))
          ORDER BY w.created_at DESC, w.id DESC
          LIMIT $4"
    );
    let mut rows: Vec<PostRow> =
        sqlx::query_as(&sql).bind(wall).bind(user.user_id).bind(before).bind(PAGE + 1).fetch_all(db).await?;
    let more = rows.len() as i64 > PAGE;
    rows.truncate(PAGE as usize);
    let mut posts: Vec<WallPost> = rows.into_iter().map(|r| post(r, user)).collect();
    let parents: Vec<Uuid> = posts.iter().map(|p| p.id).collect();
    // Предел ответов — на каждую запись, а не на всю страницу.
    let sql = format!(
        "WITH r AS (SELECT id, row_number() OVER (PARTITION BY parent_id ORDER BY created_at, id) AS n
                      FROM wall_posts WHERE parent_id = ANY($1) AND deleted_at IS NULL)
         {POST_SELECT}
         JOIN r ON r.id = w.id
         WHERE r.n <= $3
         ORDER BY w.created_at, w.id"
    );
    let replies: Vec<PostRow> =
        sqlx::query_as(&sql).bind(&parents).bind(user.user_id).bind(REPLIES_PER_POST).fetch_all(db).await?;
    for r in replies {
        if let Some(parent) = posts.iter_mut().find(|p| Some(p.id) == r.parent_id) {
            parent.replies.push(post(r, user));
        }
    }
    Ok(WallPage { posts, more })
}

/// Записи стен изменились: открытые ленты и стены перечитываются (в событии нет текста).
async fn wall_changed(db: impl sqlx::PgExecutor<'_>) -> ApiResult<()> {
    events::publish(db, &Event { kind: "wall".into(), permission: Some("*".into()), ..Default::default() }).await?;
    Ok(())
}

/// Стена открыта для чтения: владелец активен и не выключил её. Возвращает режим.
async fn open_wall(db: &sqlx::PgPool, owner: Uuid, viewer: Uuid) -> ApiResult<WallMode> {
    let row: Option<Option<String>> = sqlx::query_scalar(
        "SELECT p.wall_mode FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id
           LEFT JOIN user_profiles p ON p.user_id = u.id
          WHERE u.id = $1 AND u.is_active AND a.is_enabled AND NOT a.is_missing",
    )
    .bind(owner)
    .fetch_optional(db)
    .await?;
    let mode = wall_of(row.ok_or(ApiError::NotFound)?.as_deref().unwrap_or("everyone"));
    if mode == WallMode::Off && owner != viewer {
        return Err(ApiError::NotFound);
    }
    Ok(mode)
}

/// Записи стены, новые сверху, по [`PAGE`]; к каждой — ответы.
#[utoipa::path(get, path = "/api/users/{id}/wall", tag = "profile", params(("id" = Uuid, Path), WallQuery),
    responses((status = 200, body = WallPage), (status = 404)))]
pub(crate) async fn get_wall(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Query(q): Query<WallQuery>,
) -> ApiResult<Json<WallPage>> {
    open_wall(&core.db, id, user.user_id).await?;
    Ok(Json(page_of(&core.db, &user, Some(id), q.before).await?))
}

/// Лента: записи со всех открытых стен, новые сверху. Новые записи приходят событием `wall`.
#[utoipa::path(get, path = "/api/feed", tag = "profile", params(WallQuery), responses((status = 200, body = WallPage)))]
pub(crate) async fn get_feed(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<WallQuery>,
) -> ApiResult<Json<WallPage>> {
    Ok(Json(page_of(&core.db, &user, None, q.before).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct NewWallPost {
    pub body: String,
    /// Ответ на запись этой стены.
    #[serde(default)]
    pub parent_id: Option<Uuid>,
}

/// Написать на стене или ответить на запись. Владелец стены и автор записи получают уведомление.
#[utoipa::path(post, path = "/api/users/{id}/wall", tag = "profile", params(("id" = Uuid, Path)), request_body = NewWallPost,
    responses((status = 200, body = WallPost), (status = 400, body = pult_core::error::ErrorBody), (status = 403),
        (status = 404), (status = 429, body = pult_core::error::ErrorBody)))]
pub(crate) async fn create_wall_post(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(req): Json<NewWallPost>,
) -> ApiResult<Json<WallPost>> {
    let mode = open_wall(&core.db, id, user.user_id).await?;
    let body = clean(&req.body, true);
    if body.is_empty() {
        return Err(ApiError::bad_request("empty", "Напишите текст."));
    }
    if body.chars().count() > POST_MAX {
        return Err(too_long(format!("Запись — не длиннее {POST_MAX} символов.")));
    }
    let owner = id == user.user_id;
    // Ответ — на живую запись верхнего уровня этой же стены.
    let parent_author: Option<Uuid> = match req.parent_id {
        Some(pid) => Some(
            sqlx::query_scalar(
                "SELECT author_id FROM wall_posts
                  WHERE id = $1 AND wall_user_id = $2 AND parent_id IS NULL AND deleted_at IS NULL",
            )
            .bind(pid)
            .bind(id)
            .fetch_optional(&core.db)
            .await?
            .ok_or(ApiError::NotFound)?,
        ),
        None => None,
    };
    if parent_author.is_none() && !can_post(mode, owner) {
        return Err(ApiError::Refused {
            code: "wall_closed",
            message: "Записи на этой стене пишет только владелец.".into(),
        });
    }
    if mode == WallMode::Off {
        return Err(ApiError::Refused { code: "wall_closed", message: "Стена выключена.".into() });
    }
    // Подсчёт и запись — под блокировкой автора: параллельные запросы не проскочат предел вместе.
    let mut tx = core.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('wall_posts:' || $1::text, 0))")
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    let recent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM wall_posts WHERE author_id = $1 AND created_at > now() - make_interval(mins => $2)",
    )
    .bind(user.user_id)
    .bind(POST_WINDOW_MINUTES)
    .fetch_one(&mut *tx)
    .await?;
    if recent >= POSTS_PER_WINDOW {
        return Err(ApiError::TooManyRequests {
            message: format!("Слишком много записей подряд. Подождите {POST_WINDOW_MINUTES} минут."),
            retry_after_secs: u64::try_from(POST_WINDOW_MINUTES).unwrap_or(10) * 60,
        });
    }
    let post_id: Uuid = sqlx::query_scalar(
        "INSERT INTO wall_posts (wall_user_id, author_id, parent_id, body) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(id)
    .bind(user.user_id)
    .bind(req.parent_id)
    .bind(&body)
    .fetch_one(&mut *tx)
    .await?;
    let excerpt: String = body.chars().take(100).collect();
    let payload = |k: &str| {
        json!({"wall": id, "post": req.parent_id.unwrap_or(post_id), "author": user.display_name, "kind": k,
               "title": format!("{}: {excerpt}", user.display_name)})
    };
    let mut told = vec![user.user_id];
    if let Some(author) = parent_author
        && !told.contains(&author)
    {
        notify::notify(&mut *tx, author, kind::WALL_REPLY, payload(kind::WALL_REPLY)).await?;
        told.push(author);
    }
    if !told.contains(&id) {
        let k = if parent_author.is_some() { kind::WALL_REPLY } else { kind::WALL_POST };
        notify::notify(&mut *tx, id, k, payload(k)).await?;
    }
    wall_changed(&mut *tx).await?;
    tx.commit().await?;
    let sql = format!("{POST_SELECT} WHERE w.id = $1");
    let row: PostRow = sqlx::query_as(&sql).bind(post_id).bind(user.user_id).fetch_one(&core.db).await?;
    Ok(Json(post(row, &user)))
}

/// Удалить запись: автор, владелец стены или модератор. Удаление модератором пишется в аудит.
#[utoipa::path(delete, path = "/api/wall/{id}", tag = "profile", params(("id" = Uuid, Path)),
    responses((status = 204), (status = 403), (status = 404)))]
pub(crate) async fn delete_wall_post(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let mut tx = core.db.begin().await?;
    let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT wall_user_id, author_id, body FROM wall_posts WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let (wall, author, body) = row.ok_or(ApiError::NotFound)?;
    let own = author == user.user_id || wall == user.user_id;
    if !own {
        user.require(&MODERATE)?;
    }
    sqlx::query("UPDATE wall_posts SET body = '', deleted_at = now(), deleted_by = $2 WHERE id = $1")
        .bind(id)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    if !own {
        audit::record(
            &mut *tx,
            &user.actor,
            Entry::new("profile.wall_post_removed", Source::Portal)
                .object("wall_post", id)
                .details(json!({"wall": wall, "author": author, "text": body.chars().take(300).collect::<String>()})),
        )
        .await?;
    }
    wall_changed(&mut *tx).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
pub struct Likes {
    pub likes: i64,
    pub liked: bool,
}

async fn like(core: &Core, user: &CurrentUser, id: Uuid, on: bool) -> ApiResult<Json<Likes>> {
    let wall: Uuid = sqlx::query_scalar("SELECT wall_user_id FROM wall_posts WHERE id = $1 AND deleted_at IS NULL")
        .bind(id)
        .fetch_optional(&core.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    open_wall(&core.db, wall, user.user_id).await?;
    if on {
        sqlx::query("INSERT INTO wall_likes (post_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
    } else {
        sqlx::query("DELETE FROM wall_likes WHERE post_id = $1 AND user_id = $2")
    }
    .bind(id)
    .bind(user.user_id)
    .execute(&core.db)
    .await?;
    let likes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM wall_likes WHERE post_id = $1").bind(id).fetch_one(&core.db).await?;
    Ok(Json(Likes { likes, liked: on }))
}

/// Отметить «нравится».
#[utoipa::path(put, path = "/api/wall/{id}/like", tag = "profile", params(("id" = Uuid, Path)),
    responses((status = 200, body = Likes), (status = 404)))]
pub(crate) async fn like_post(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Likes>> {
    like(&core, &user, id, true).await
}

/// Снять «нравится».
#[utoipa::path(delete, path = "/api/wall/{id}/like", tag = "profile", params(("id" = Uuid, Path)),
    responses((status = 200, body = Likes), (status = 404)))]
pub(crate) async fn unlike_post(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Likes>> {
    like(&core, &user, id, false).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_follows_mode_and_signal() {
        assert_eq!(presence_of(PresenceMode::Auto, Some(10), false), Presence::Online);
        assert_eq!(presence_of(PresenceMode::Auto, Some(10), true), Presence::Away);
        assert_eq!(presence_of(PresenceMode::Auto, Some(ONLINE_SECS + 1), false), Presence::Offline);
        assert_eq!(presence_of(PresenceMode::Auto, None, false), Presence::Offline);
        assert_eq!(presence_of(PresenceMode::Dnd, Some(10), true), Presence::Dnd);
        assert_eq!(presence_of(PresenceMode::Dnd, None, false), Presence::Offline, "вкладка закрыта — не в сети");
        assert_eq!(presence_of(PresenceMode::Away, Some(10), false), Presence::Away);
        assert_eq!(presence_of(PresenceMode::Invisible, Some(10), false), Presence::Offline);
    }

    #[test]
    fn only_real_images_pass() {
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"GIF89a\x01\0\x01\0"), Some("image/gif"));
        assert_eq!(sniff_image(b"GIF87a\x01\0\x01\0"), Some("image/gif"));
        assert_eq!(sniff_image(b"GIF90a"), None);
        assert_eq!(sniff_image(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"), None);
        assert_eq!(sniff_image(b"<html><script>"), None);
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WAVE"), None);
    }

    /// GIF из кадров заданного размера: каждый кадр своего цвета.
    fn gif_of(w: u32, h: u32, frames: u8) -> Vec<u8> {
        use image::{Delay, Frame, RgbaImage, codecs::gif::GifEncoder};
        let mut out = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut out);
            for i in 0..frames {
                let img = RgbaImage::from_pixel(w, h, image::Rgba([i.wrapping_mul(60), 100, 200, 255]));
                enc.encode_frame(Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(100, 1))).unwrap();
            }
        }
        out
    }

    fn frames_of(data: &[u8]) -> (u32, u32, usize) {
        use image::{AnimationDecoder, ImageDecoder, codecs::gif::GifDecoder};
        let dec = GifDecoder::new(std::io::Cursor::new(data)).unwrap();
        let (w, h) = dec.dimensions();
        (w, h, dec.into_frames().count())
    }

    #[test]
    fn gif_is_shrunk_to_square_with_all_frames() {
        let big = gif_of(600, 400, 3);
        let small = shrink_gif(&big).unwrap();
        assert_eq!(frames_of(&small), (256, 256, 3), "квадрат 256, все кадры на месте");
        assert_eq!(sniff_image(&small), Some("image/gif"));
        // Маленький GIF не перекодируется.
        let tiny = gif_of(64, 64, 2);
        assert_eq!(shrink_gif(&tiny).unwrap(), tiny);
        // Узкий, но меньше квадрата по одной стороне: обрезается до квадрата по меньшей стороне.
        assert_eq!(frames_of(&shrink_gif(&gif_of(300, 100, 1)).unwrap()), (100, 100, 1));
    }

    /// GIF, который от кадра к кадру меняется целиком (шум): без пропуска кадров в 1 МБ не уложить.
    fn noisy_gif(side: u32, frames: u32) -> Vec<u8> {
        use image::{Delay, Frame, RgbaImage, codecs::gif::GifEncoder};
        let mut seed = 0x1234_5678u32;
        let mut out = Vec::new();
        {
            let mut enc = GifEncoder::new_with_speed(&mut out, 30);
            for _ in 0..frames {
                let img = RgbaImage::from_fn(side, side, |_, _| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let b = seed.to_be_bytes();
                    image::Rgba([b[0], b[1], b[2], 255])
                });
                enc.encode_frame(Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(50, 1))).unwrap();
            }
        }
        out
    }

    #[test]
    fn busy_gif_drops_frames_to_fit() {
        let big = noisy_gif(300, 60);
        assert!(big.len() > AVATAR_MAX_BYTES, "исходный больше предела");
        // Отладочная сборка медленнее рабочей в десятки раз: срок здесь свой.
        let small = shrink_gif_within(&big, std::time::Duration::from_secs(900)).unwrap();
        assert!(small.len() <= AVATAR_MAX_BYTES, "{} байт", small.len());
        let (w, h, n) = frames_of(&small);
        assert!(w == h && w <= AVATAR_SIDE && n > 1 && n < 60, "{w}×{h}, {n} кадров");
    }

    #[test]
    fn calm_gif_keeps_every_frame() {
        // Неподвижный фон и бегущий квадрат: пишется только изменившаяся часть, все кадры на месте.
        use image::{Delay, Frame, RgbaImage, codecs::gif::GifEncoder};
        let mut src = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut src);
            for i in 0..60u32 {
                let img = RgbaImage::from_fn(400, 400, |x, y| {
                    if x / 20 == i % 20 && y < 60 {
                        image::Rgba([255, 0, 0, 255])
                    } else {
                        let c = if (x / 25 + y / 25) % 2 == 0 { 40 } else { 200 };
                        image::Rgba([c, c, 255 - c, 255])
                    }
                });
                enc.encode_frame(Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(40, 1))).unwrap();
            }
        }
        let small = shrink_gif(&src).unwrap();
        assert_eq!(frames_of(&small), (256, 256, 60));
        assert!(small.len() < 300 * 1024, "{} байт", small.len());
        // Одинаковые кадры склеиваются в один с общей задержкой.
        let still = gif_of(400, 400, 1);
        let mut twice = Vec::new();
        {
            use image::codecs::gif::GifEncoder;
            let mut enc = GifEncoder::new(&mut twice);
            let img = RgbaImage::from_pixel(400, 400, image::Rgba([9, 99, 199, 255]));
            for _ in 0..5 {
                enc.encode_frame(Frame::from_parts(img.clone(), 0, 0, Delay::from_numer_denom_ms(100, 1))).unwrap();
            }
        }
        assert_eq!(frames_of(&shrink_gif(&twice).unwrap()), (256, 256, 1));
        assert_eq!(frames_of(&shrink_gif(&still).unwrap()), (256, 256, 1));
    }

    #[test]
    fn transparent_gif_keeps_frames() {
        use image::{Delay, Frame, RgbaImage, codecs::gif::GifEncoder};
        let mut src = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut src);
            for i in 0..3u8 {
                let img = RgbaImage::from_fn(300, 300, |x, _| {
                    if x < 100 { image::Rgba([0, 0, 0, 0]) } else { image::Rgba([i * 60, 100, 200, 255]) }
                });
                enc.encode_frame(Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(100, 1))).unwrap();
            }
        }
        assert_eq!(frames_of(&shrink_gif(&src).unwrap()), (256, 256, 3));
    }

    #[test]
    fn bad_gifs_are_refused() {
        assert_eq!(shrink_gif(b"GIF89a\0\0garbage"), Err(GifError::Unreadable));
        assert_eq!(shrink_gif(&gif_of(2100, 10, 1)), Err(GifError::Unreadable), "больше 2048 по стороне");
        assert_eq!(shrink_gif(&gif_of(300, 300, 31)).map(|_| ()), Ok(()));
    }

    #[test]
    fn text_is_cleaned() {
        assert_eq!(clean("  привет\u{0007}\nмир  ", true), "привет\nмир");
        assert_eq!(clean("в\nотпуске\t", false), "вотпуске");
    }

    #[test]
    fn wall_modes() {
        assert!(can_post(WallMode::Everyone, false));
        assert!(!can_post(WallMode::Me, false));
        assert!(can_post(WallMode::Me, true));
        assert!(!can_post(WallMode::Off, true));
    }
}
