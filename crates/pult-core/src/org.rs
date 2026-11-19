//! Организация: название, логотип и включённые модули (мастер установки).
//!
//! Выключенный модуль не виден: его API отвечает 404 ([`gate`]), интерфейс прячет разделы
//! по `GET /api/org`. Данные модуля остаются — включение возвращает всё как было.

use crate::{
    ApiError, ApiResult, Core, CurrentUser,
    audit::{self, Entry, Source},
    rbac::perm,
    settings::{self, SettingsGroup},
};
use axum::{
    Json,
    extract::{Request, State},
    http::header,
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};
use utoipa::ToSchema;

/// Модули, которые можно выключить. Ядро и учётки (вход, AD) работают всегда.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct Modules {
    /// Заявки, база знаний, оборудование.
    pub helpdesk: bool,
    /// Справочник сотрудников.
    pub phonebook: bool,
    /// Изменения в AD из портала (включение учёток, группы, сброс пароля).
    pub directory: bool,
    /// Удалённая поддержка (агент на ПК).
    pub remote: bool,
    /// Телефония: журнал звонков и звонок из справочника (нужен справочник).
    pub telephony: bool,
}

impl Default for Modules {
    fn default() -> Self {
        Self { helpdesk: true, phonebook: true, directory: true, remote: true, telephony: true }
    }
}

/// Модуль, к которому относится путь API, если его можно выключить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gated {
    Helpdesk,
    Phonebook,
    Directory,
    Remote,
    Telephony,
}

/// Префиксы путей по модулям; более длинные — раньше (звонки — часть справочника по пути).
/// Поиск сотрудников, отделы и должности (`/api/phonebook/persons|departments|positions`) — общие:
/// без справочника ими пользуются заявки (заявитель, ответственный), поэтому они не выключаются.
const PREFIXES: &[(&str, Gated)] = &[
    ("/api/phonebook/calls", Gated::Telephony),
    ("/api/admin/telephony", Gated::Telephony),
    ("/api/phonebook/config", Gated::Phonebook),
    ("/api/phonebook/favorites", Gated::Phonebook),
    ("/api/phonebook/duplicates", Gated::Phonebook),
    ("/api/admin/phonebook", Gated::Phonebook),
    ("/api/helpdesk", Gated::Helpdesk),
    ("/api/admin/helpdesk", Gated::Helpdesk),
    ("/api/admin/webhooks", Gated::Helpdesk),
    ("/api/admin/api-tokens", Gated::Helpdesk),
    ("/api/kb", Gated::Helpdesk),
    ("/api/public/login-help", Gated::Helpdesk),
    ("/api/public/ticket-response", Gated::Helpdesk),
    ("/api/directory", Gated::Directory),
    ("/api/admin/directory", Gated::Directory),
    ("/api/remote", Gated::Remote),
    ("/api/admin/remote", Gated::Remote),
    ("/api/agent", Gated::Remote),
];

/// Модуль пути: префикс совпадает целым сегментом (`/api/kbx` — не база знаний).
pub fn gated(path: &str) -> Option<Gated> {
    PREFIXES.iter().find_map(|(p, m)| {
        let rest = path.strip_prefix(p)?;
        (rest.is_empty() || rest.starts_with('/') || rest.starts_with('.')).then_some(*m)
    })
}

impl Modules {
    pub fn enabled(&self, m: Gated) -> bool {
        match m {
            Gated::Helpdesk => self.helpdesk,
            Gated::Phonebook => self.phonebook,
            Gated::Directory => self.directory,
            Gated::Remote => self.remote,
            Gated::Telephony => self.telephony,
        }
    }

    /// Зависимости между модулями.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.telephony && !self.phonebook {
            return Err("Телефония работает только вместе со справочником.");
        }
        Ok(())
    }
}

/// Организация (ключ `org`). Логотип хранится отдельно — [`OrgLogo`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct OrgSettings {
    /// Название в шапке портала и в письмах; пусто — «pult».
    pub name: String,
    pub modules: Modules,
    /// Мастер настройки организации пройден (до этого суперадминистратору показывается мастер).
    pub setup_done: bool,
    /// Версия логотипа (часть адреса `/api/org/logo?v=…`); `None` — логотипа нет.
    pub logo: Option<String>,
}

impl SettingsGroup for OrgSettings {
    const KEY: &'static str = "org";
}

/// Логотип: PNG, JPEG или WebP (SVG нельзя — в нём может быть скрипт).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OrgLogo {
    pub content_type: String,
    /// Base64.
    pub data: String,
}

impl SettingsGroup for OrgLogo {
    const KEY: &'static str = "org_logo";
}

/// Наибольший размер логотипа.
pub const LOGO_MAX_BYTES: usize = 256 * 1024;

/// Тип картинки по первым байтам; остальное не принимается.
pub fn image_type(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Логотип в памяти: тело и тип.
type LogoBytes = Option<(axum::body::Bytes, &'static str)>;

/// Кэш для [`gate`] и логотипа: таблица `settings` читается не чаще раза в несколько секунд
/// (логотип отдаётся без входа — каждый запрос не должен идти в базу).
#[derive(Clone, Default)]
pub struct ModuleCache {
    modules: Arc<RwLock<Option<(Instant, Modules)>>>,
    logo: Arc<RwLock<Option<(Instant, LogoBytes)>>>,
}

const CACHE_TTL: Duration = Duration::from_secs(5);
const LOGO_TTL: Duration = Duration::from_secs(60);

impl ModuleCache {
    pub async fn get(&self, db: &sqlx::PgPool) -> Result<Modules, sqlx::Error> {
        if let Ok(g) = self.modules.read()
            && let Some((at, m)) = *g
            && at.elapsed() < CACHE_TTL
        {
            return Ok(m);
        }
        let m = settings::load::<OrgSettings>(db).await?.modules;
        if let Ok(mut g) = self.modules.write() {
            *g = Some((Instant::now(), m));
        }
        Ok(m)
    }

    /// Логотип (декодирован и проверен) или `None`.
    pub async fn logo(&self, db: &sqlx::PgPool) -> Result<LogoBytes, sqlx::Error> {
        if let Ok(g) = self.logo.read()
            && let Some((at, l)) = &*g
            && at.elapsed() < LOGO_TTL
        {
            return Ok(l.clone());
        }
        let stored: OrgLogo = settings::load(db).await?;
        let l = base64::engine::general_purpose::STANDARD
            .decode(&stored.data)
            .ok()
            .and_then(|b| image_type(&b).map(|ct| (axum::body::Bytes::from(b), ct)));
        if let Ok(mut g) = self.logo.write() {
            *g = Some((Instant::now(), l.clone()));
        }
        Ok(l)
    }

    /// Сбросить после изменения настроек.
    pub fn invalidate(&self) {
        if let Ok(mut g) = self.modules.write() {
            *g = None;
        }
        if let Ok(mut g) = self.logo.write() {
            *g = None;
        }
    }
}

/// Настройки организации под блокировкой строки: параллельные изменения названия и логотипа
/// не затирают друг друга.
async fn lock_org(tx: &mut sqlx::PgConnection) -> Result<OrgSettings, sqlx::Error> {
    sqlx::query("INSERT INTO settings (key, value) VALUES ($1, '{}') ON CONFLICT (key) DO NOTHING")
        .bind(OrgSettings::KEY)
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT 1 FROM settings WHERE key = $1 FOR UPDATE").bind(OrgSettings::KEY).execute(&mut *tx).await?;
    settings::load(&mut *tx).await
}

/// Middleware: API выключенного модуля отвечает 404, как будто его нет.
/// Если базу прочитать не удалось — пропускает: права проверяет сам обработчик.
pub async fn gate(State(core): State<Core>, req: Request, next: Next) -> Response {
    if let Some(m) = gated(req.uri().path()) {
        match core.org.get(&core.db).await {
            Ok(mods) if !mods.enabled(m) => return ApiError::NotFound.into_response(),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "настройки модулей не прочитаны — запрос пропущен"),
        }
    }
    next.run(req).await
}

// ------------------------------------------------------------------ API

/// Название, логотип и модули — для шапки и меню, в том числе до входа.
#[utoipa::path(get, path = "/api/org", tag = "core", responses((status = 200, body = OrgSettings)))]
pub async fn get_org(State(core): State<Core>) -> ApiResult<Json<OrgSettings>> {
    Ok(Json(settings::load(&core.db).await?))
}

/// Логотип организации.
#[utoipa::path(get, path = "/api/org/logo", tag = "core",
    responses((status = 200, content_type = "image/png"), (status = 404)))]
pub async fn get_logo(State(core): State<Core>) -> ApiResult<Response> {
    let Some((bytes, ct)) = core.org.logo(&core.db).await? else { return Err(ApiError::NotFound) };
    // Адрес меняется вместе с логотипом (`?v=`), поэтому кэшировать можно долго.
    Ok(([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "public, max-age=86400")], bytes).into_response())
}

/// Изменяемая часть настроек организации.
#[derive(Debug, Deserialize, ToSchema)]
pub struct OrgUpdate {
    pub name: String,
    pub modules: Modules,
    pub setup_done: bool,
}

#[utoipa::path(put, path = "/api/admin/org", tag = "admin", request_body = OrgUpdate,
    responses((status = 200, body = OrgSettings), (status = 400, body = crate::error::ErrorBody)))]
pub async fn put_org(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<OrgUpdate>,
) -> ApiResult<Json<OrgSettings>> {
    user.require(&perm::SETTINGS_MANAGE)?;
    let name = b.name.trim().to_string();
    if name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(ApiError::bad_request("invalid_settings", "Название — до 100 символов, в одну строку."));
    }
    b.modules.validate().map_err(|m| ApiError::bad_request("invalid_settings", m))?;
    let mut tx = core.db.begin().await?;
    let before = lock_org(&mut tx).await?;
    let after = OrgSettings { name, modules: b.modules, setup_done: b.setup_done, logo: before.logo.clone() };
    settings::save(&mut *tx, &after, Some(user.user_id)).await?;
    let d = audit::diff(&json!(before), &json!(after));
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("settings.update", Source::Portal).object("settings", "org").details(d),
    )
    .await?;
    tx.commit().await?;
    core.org.invalidate();
    Ok(Json(after))
}

/// Новый логотип, base64.
#[derive(Deserialize, ToSchema)]
pub struct LogoUpload {
    pub data: String,
}

#[utoipa::path(put, path = "/api/admin/org/logo", tag = "admin", request_body = LogoUpload,
    responses((status = 200, body = OrgSettings), (status = 400, body = crate::error::ErrorBody)))]
pub async fn put_logo(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<LogoUpload>,
) -> ApiResult<Json<OrgSettings>> {
    user.require(&perm::SETTINGS_MANAGE)?;
    let bad = || ApiError::bad_request("invalid_logo", "Логотип — PNG, JPEG или WebP до 256 КБ.");
    let bytes = base64::engine::general_purpose::STANDARD.decode(b.data.trim()).map_err(|_| bad())?;
    if bytes.is_empty() || bytes.len() > LOGO_MAX_BYTES {
        return Err(bad());
    }
    let ct = image_type(&bytes).ok_or_else(bad)?;
    let digest = ring::digest::digest(&ring::digest::SHA256, &bytes);
    let version: String = digest.as_ref()[..6].iter().map(|x| format!("{x:02x}")).collect();
    let mut tx = core.db.begin().await?;
    let mut org = lock_org(&mut tx).await?;
    settings::save(
        &mut *tx,
        &OrgLogo { content_type: ct.into(), data: base64::engine::general_purpose::STANDARD.encode(&bytes) },
        Some(user.user_id),
    )
    .await?;
    org.logo = Some(version.clone());
    settings::save(&mut *tx, &org, Some(user.user_id)).await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("settings.update", Source::Portal)
            .object("settings", "org_logo")
            .details(json!({"content_type": ct, "bytes": bytes.len(), "version": version})),
    )
    .await?;
    tx.commit().await?;
    core.org.invalidate();
    Ok(Json(org))
}

#[utoipa::path(delete, path = "/api/admin/org/logo", tag = "admin", responses((status = 200, body = OrgSettings)))]
pub async fn delete_logo(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<OrgSettings>> {
    user.require(&perm::SETTINGS_MANAGE)?;
    let mut tx = core.db.begin().await?;
    let mut org = lock_org(&mut tx).await?;
    sqlx::query("DELETE FROM settings WHERE key = $1").bind(OrgLogo::KEY).execute(&mut *tx).await?;
    org.logo = None;
    settings::save(&mut *tx, &org, Some(user.user_id)).await?;
    audit::record(&mut *tx, &user.actor, Entry::new("settings.update", Source::Portal).object("settings", "org_logo"))
        .await?;
    tx.commit().await?;
    core.org.invalidate();
    Ok(Json(org))
}

/// Включён ли модуль — для фоновых задач (при ошибке чтения считается включённым).
pub async fn module_enabled(core: &Core, m: Gated) -> bool {
    core.org.get(&core.db).await.map(|mods| mods.enabled(m)).unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_map_to_modules() {
        assert_eq!(gated("/api/phonebook/calls"), Some(Gated::Telephony));
        assert_eq!(gated("/api/phonebook/calls/recent"), Some(Gated::Telephony));
        assert_eq!(gated("/api/phonebook/favorites"), Some(Gated::Phonebook));
        assert_eq!(gated("/api/phonebook/persons"), None, "поиск сотрудников нужен заявкам и без справочника");
        assert_eq!(gated("/api/admin/webhooks/1"), Some(Gated::Helpdesk));
        assert_eq!(gated("/api/helpdesk/tickets"), Some(Gated::Helpdesk));
        assert_eq!(gated("/api/kb/articles"), Some(Gated::Helpdesk));
        assert_eq!(gated("/api/agent/v1/report"), Some(Gated::Remote));
        assert_eq!(gated("/api/directory/operations"), Some(Gated::Directory));
        assert_eq!(gated("/api/kbx"), None, "префикс — целый сегмент");
        for always in
            ["/api/auth/login", "/api/me", "/api/org", "/api/admin/org", "/api/setup/status", "/api/admin/sites"]
        {
            assert_eq!(gated(always), None, "{always}");
        }
    }

    #[test]
    fn telephony_needs_phonebook() {
        let m = Modules { phonebook: false, ..Modules::default() };
        assert!(m.validate().is_err());
        assert!(Modules { telephony: false, ..m }.validate().is_ok());
    }

    #[test]
    fn only_raster_images_are_logos() {
        assert_eq!(image_type(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(image_type(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(image_type(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(image_type(b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script/></svg>"), None);
        assert_eq!(image_type(b"GIF89a"), None);
    }

    #[test]
    fn missing_settings_mean_everything_on_and_setup_pending() {
        let o: OrgSettings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(o.modules, Modules::default());
        assert!(!o.setup_done);
        let o: OrgSettings = serde_json::from_value(json!({"modules": {"remote": false}})).unwrap();
        assert!(o.modules.helpdesk && !o.modules.remote);
    }
}
