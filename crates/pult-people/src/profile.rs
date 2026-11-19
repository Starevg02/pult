//! Профиль пользователя и личная настройка интерфейса, как в GLPI: тема, плотность, формат даты и времени,
//! порядок ФИО, строк на странице, стартовая страница; свои сеансы — список и выход на других устройствах.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser,
    audit::{self, Entry, Source},
    crypto::sha256,
    session,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Как в системе (prefers-color-scheme).
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Density {
    #[default]
    Comfortable,
    Compact,
}

#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DateFormat {
    /// По языку интерфейса.
    #[default]
    Auto,
    /// 31.12.2026
    Dmy,
    /// 2026-12-31
    Ymd,
    /// 12/31/2026
    Mdy,
}

#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Clock {
    #[default]
    H24,
    H12,
}

#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NameOrder {
    /// Иванов Иван Иванович
    #[default]
    LastFirst,
    /// Иван Иванович Иванов
    FirstLast,
}

#[derive(Serialize, Deserialize, ToSchema, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StartPage {
    #[default]
    Home,
    Tickets,
    Desk,
    Phonebook,
    Kb,
    Notifications,
}

/// Строк на странице в списках.
pub const PAGE_SIZES: [u16; 4] = [25, 50, 100, 200];

/// Личные предпочтения. Хранятся в `users.settings → prefs`; неизвестное или устаревшее значение
/// при чтении заменяется значением по умолчанию — настройка не может сломать вход.
#[derive(Serialize, Deserialize, ToSchema, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct UserPrefs {
    pub theme: Theme,
    pub density: Density,
    pub date_format: DateFormat,
    pub clock: Clock,
    pub name_order: NameOrder,
    /// 25, 50, 100 или 200.
    pub page_size: u16,
    pub start_page: StartPage,
}

impl Default for UserPrefs {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            density: Density::default(),
            date_format: DateFormat::default(),
            clock: Clock::default(),
            name_order: NameOrder::default(),
            page_size: 50,
            start_page: StartPage::default(),
        }
    }
}

impl UserPrefs {
    /// Из сохранённого JSON: битое поле — по умолчанию, остальные сохраняются.
    pub fn from_stored(v: Option<serde_json::Value>) -> Self {
        let Some(serde_json::Value::Object(map)) = v else { return Self::default() };
        let mut p = Self::default();
        let field = |k: &str| map.get(k).cloned().unwrap_or(serde_json::Value::Null);
        if let Ok(x) = serde_json::from_value(field("theme")) {
            p.theme = x;
        }
        if let Ok(x) = serde_json::from_value(field("density")) {
            p.density = x;
        }
        if let Ok(x) = serde_json::from_value(field("date_format")) {
            p.date_format = x;
        }
        if let Ok(x) = serde_json::from_value(field("clock")) {
            p.clock = x;
        }
        if let Ok(x) = serde_json::from_value(field("name_order")) {
            p.name_order = x;
        }
        if let Ok(x) = serde_json::from_value::<u16>(field("page_size"))
            && PAGE_SIZES.contains(&x)
        {
            p.page_size = x;
        }
        if let Ok(x) = serde_json::from_value(field("start_page")) {
            p.start_page = x;
        }
        p
    }
}

pub async fn load_prefs(db: &sqlx::PgPool, user_id: Uuid) -> Result<UserPrefs, sqlx::Error> {
    let v: Option<serde_json::Value> = sqlx::query_scalar("SELECT settings->'prefs' FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(db)
        .await?
        .flatten();
    Ok(UserPrefs::from_stored(v))
}

/// Сохранить предпочтения (целиком). Неизвестное значение — 400, а не молчаливая замена.
#[utoipa::path(put, path = "/api/me/prefs", tag = "auth", request_body = UserPrefs,
    responses((status = 200, body = UserPrefs), (status = 400, body = pult_core::error::ErrorBody)))]
pub(crate) async fn put_my_prefs(
    State(core): State<Core>,
    user: CurrentUser,
    Json(raw): Json<serde_json::Value>,
) -> ApiResult<Json<UserPrefs>> {
    let invalid = || ApiError::bad_request("invalid_prefs", "Недопустимое значение настройки.");
    let prefs: UserPrefs = serde_json::from_value(raw).map_err(|_| invalid())?;
    if !PAGE_SIZES.contains(&prefs.page_size) {
        return Err(invalid());
    }
    let v = serde_json::to_value(&prefs).map_err(ApiError::internal)?;
    sqlx::query("UPDATE users SET settings = jsonb_set(settings, '{prefs}', $2) WHERE id = $1")
        .bind(user.user_id)
        .bind(&v)
        .execute(&core.db)
        .await?;
    Ok(Json(prefs))
}

#[derive(Serialize, ToSchema)]
pub struct ProfileRole {
    pub code: String,
    pub name: String,
    /// Площадка, если роль ограничена ею.
    pub site: Option<String>,
}

/// Карточка сотрудника из справочника — то, что видят коллеги.
#[derive(Serialize, ToSchema)]
pub struct ProfilePerson {
    pub id: Uuid,
    pub last_name: String,
    pub first_name: String,
    pub middle_name: Option<String>,
    pub department: Option<String>,
    pub position: Option<String>,
    pub site: Option<String>,
    pub room: Option<String>,
    pub work_phone: Option<String>,
    pub internal_ext: Option<String>,
    pub mobile_phone: Option<String>,
    pub email: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct Profile {
    pub login: String,
    pub display_name: String,
    /// `ivanov@corp.example` из AD.
    pub upn: Option<String>,
    pub locale: String,
    pub person: Option<ProfilePerson>,
    pub roles: Vec<ProfileRole>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_login_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    pub prefs: UserPrefs,
}

type PersonRow = (
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

/// Мой профиль: учётка, карточка справочника, роли, последний вход и предпочтения.
#[utoipa::path(get, path = "/api/me/profile", tag = "auth", responses((status = 200, body = Profile), (status = 401), (status = 403)))]
pub(crate) async fn get_my_profile(
    State(core): State<Core>,
    user: CurrentUser,
    headers: HeaderMap,
) -> ApiResult<Json<Profile>> {
    browser_only(&headers)?;
    let (upn, last_login_at, created_at): (Option<String>, Option<OffsetDateTime>, OffsetDateTime) = sqlx::query_as(
        "SELECT a.upn, u.last_login_at, u.created_at
           FROM users u LEFT JOIN ad_accounts a ON a.id = u.ad_account_id
          WHERE u.id = $1",
    )
    .bind(user.user_id)
    .fetch_one(&core.db)
    .await?;
    let person = match user.person_id {
        Some(pid) => sqlx::query_as::<_, PersonRow>(
            "SELECT p.id, p.last_name, p.first_name, p.middle_name, d.name, pos.name, s.name, p.room,
                    p.work_phone, p.internal_ext, p.mobile_phone, p.email
               FROM persons p
               LEFT JOIN departments d ON d.id = p.department_id
               LEFT JOIN positions pos ON pos.id = p.position_id
               LEFT JOIN sites s ON s.id = p.site_id
              WHERE p.id = $1",
        )
        .bind(pid)
        .fetch_optional(&core.db)
        .await?
        .map(|r| ProfilePerson {
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
    let roles: Vec<(String, String, bool, Option<String>)> = sqlx::query_as(
        // Роли — из прав сеанса (там и неявная «Сотрудник»), площадка — из назначения.
        "SELECT r.code, r.name, r.is_system, s.name
           FROM roles r
           LEFT JOIN user_roles ur ON ur.role_id = r.id AND ur.user_id = $1
           LEFT JOIN sites s ON s.id = ur.site_id
          WHERE r.code = ANY($2)
          ORDER BY r.is_system DESC, r.name, s.name NULLS FIRST",
    )
    .bind(user.user_id)
    .bind(user.grants.roles.iter().cloned().collect::<Vec<_>>())
    .fetch_all(&core.db)
    .await?;
    let roles = roles
        .into_iter()
        .map(|(code, name, is_system, site)| ProfileRole {
            name: pult_core::api::role_display_name(&code, name, is_system),
            code,
            site,
        })
        .collect();
    Ok(Json(Profile {
        login: user.login.clone(),
        display_name: user.display_name.clone(),
        upn,
        locale: user.locale.clone(),
        person,
        roles,
        last_login_at,
        created_at,
        prefs: load_prefs(&core.db, user.user_id).await?,
    }))
}

#[derive(Serialize, ToSchema)]
pub struct MySession {
    /// Ссылка на сеанс для закрытия; это не токен и не его хеш.
    pub handle: String,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub last_seen_at: OffsetDateTime,
    /// Сеанс этого браузера.
    pub current: bool,
}

/// handle, адрес, браузер, вход, активность, «этот браузер».
type SessionRow = (String, Option<String>, Option<String>, OffsetDateTime, OffsetDateTime, Option<bool>);

/// Профиль и сеансы — только из браузера: токен API (даже без прав) не раскрывает адреса и браузеры владельца.
fn browser_only(headers: &HeaderMap) -> ApiResult<()> {
    if session::via_api_token(headers) { Err(ApiError::Forbidden) } else { Ok(()) }
}

/// Ключ строки `sessions` этого браузера (хеш токена из cookie).
fn current_id(core: &Core, headers: &HeaderMap) -> Option<Vec<u8>> {
    session::cookie(headers, core.cookies.session_name()).map(|t| sha256(t.as_bytes()))
}

/// Мои открытые сеансы, последние — сверху.
#[utoipa::path(get, path = "/api/me/sessions", tag = "auth", responses((status = 200, body = Vec<MySession>), (status = 403)))]
pub(crate) async fn list_my_sessions(
    State(core): State<Core>,
    user: CurrentUser,
    headers: HeaderMap,
) -> ApiResult<Json<Vec<MySession>>> {
    browser_only(&headers)?;
    // Ключ строки — хеш токена; наружу отдаётся хеш от него, чтобы ключ не покидал сервер.
    let sql = "SELECT substr(encode(sha256(id), 'hex'), 1, 32), host(ip), user_agent, created_at, last_seen_at, id = $2
           FROM sessions
          WHERE user_id = $1 AND idle_expires_at > now() AND absolute_expires_at > now()
          ORDER BY last_seen_at DESC
          LIMIT 50";
    let rows: Vec<SessionRow> =
        sqlx::query_as(sql).bind(user.user_id).bind(current_id(&core, &headers)).fetch_all(&core.db).await?;
    Ok(Json(
        rows.into_iter()
            .map(|(handle, ip, user_agent, created_at, last_seen_at, current)| MySession {
                handle,
                ip,
                user_agent: user_agent.map(|u| u.chars().take(300).collect()),
                created_at,
                last_seen_at,
                current: current.unwrap_or(false),
            })
            .collect(),
    ))
}

/// Закрыть свой сеанс на другом устройстве. Текущий закрывается выходом.
#[utoipa::path(delete, path = "/api/me/sessions/{handle}", tag = "auth", params(("handle" = String, Path)),
    responses((status = 204), (status = 400, body = pult_core::error::ErrorBody), (status = 404)))]
pub(crate) async fn revoke_my_session(
    State(core): State<Core>,
    user: CurrentUser,
    headers: HeaderMap,
    Path(handle): Path<String>,
) -> ApiResult<StatusCode> {
    if handle.len() != 32 || !handle.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError::NotFound);
    }
    let mut tx = core.db.begin().await?;
    let sql = "SELECT id = $3, host(ip), left(user_agent, 300) FROM sessions
          WHERE user_id = $1 AND substr(encode(sha256(id), 'hex'), 1, 32) = lower($2) FOR UPDATE";
    let found: Option<(Option<bool>, Option<String>, Option<String>)> = sqlx::query_as(sql)
        .bind(user.user_id)
        .bind(&handle)
        .bind(current_id(&core, &headers))
        .fetch_optional(&mut *tx)
        .await?;
    let (ip, user_agent) = match found {
        None => return Err(ApiError::NotFound),
        Some((Some(true), ..)) => {
            return Err(ApiError::bad_request("current_session", "Текущий сеанс закрывается кнопкой «Выйти»."));
        }
        Some((_, ip, ua)) => (ip, ua),
    };
    let sql = "DELETE FROM sessions WHERE user_id = $1 AND substr(encode(sha256(id), 'hex'), 1, 32) = lower($2)";
    sqlx::query(sql).bind(user.user_id).bind(&handle).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        &user.actor,
        // Какой сеанс закрыт: ссылка (не токен), адрес и браузер — чтобы разобрать взлом учётки.
        Entry::new("auth.session_revoked", Source::Portal)
            .object("user", user.user_id)
            .details(json!({"handle": handle.to_ascii_lowercase(), "ip": ip, "user_agent": user_agent})),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
pub struct Revoked {
    pub closed: u64,
}

/// Выйти на всех других устройствах: остаётся только сеанс этого браузера.
#[utoipa::path(post, path = "/api/me/sessions/revoke-others", tag = "auth", responses((status = 200, body = Revoked)))]
pub(crate) async fn revoke_other_sessions(
    State(core): State<Core>,
    user: CurrentUser,
    headers: HeaderMap,
) -> ApiResult<Json<Revoked>> {
    let mut tx = core.db.begin().await?;
    let closed = sqlx::query("DELETE FROM sessions WHERE user_id = $1 AND id IS DISTINCT FROM $2")
        .bind(user.user_id)
        .bind(current_id(&core, &headers))
        .execute(&mut *tx)
        .await?
        .rows_affected();
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("auth.sessions_revoked", Source::Portal)
            .object("user", user.user_id)
            .details(json!({"closed": closed})),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(Revoked { closed }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_prefs_are_tolerant() {
        assert_eq!(UserPrefs::from_stored(None), UserPrefs::default());
        let p =
            UserPrefs::from_stored(Some(json!({"theme": "dark", "density": "huge", "page_size": 7, "clock": "h12"})));
        assert_eq!(p.theme, Theme::Dark);
        assert_eq!(p.density, Density::Comfortable, "неизвестное — по умолчанию");
        assert_eq!(p.page_size, 50);
        assert_eq!(p.clock, Clock::H12);
        assert_eq!(UserPrefs::from_stored(Some(json!("dark"))), UserPrefs::default());
    }

    #[test]
    fn prefs_json_shape() {
        let v = serde_json::to_value(UserPrefs::default()).unwrap_or_default();
        assert_eq!(
            v,
            json!({"theme": "system", "density": "comfortable", "date_format": "auto", "clock": "h24",
                   "name_order": "last_first", "page_size": 50, "start_page": "home"})
        );
    }
}
