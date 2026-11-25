//! Раздел «Компьютеры»: ПК с агентом, их сессии, «где сейчас залогинен», карточка ПК
//! (сессии, история подключений, последние заявки), избранное, скачивание helper и MSI.

use crate::{
    RemoteSettings,
    connect::{TOKEN_SECS, rules},
    perm,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::IntoResponse,
};
use pult_core::{ApiError, ApiResult, Core, CurrentUser, settings};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

const MAX_QUERY_CHARS: usize = 100;

fn needle(q: Option<&str>) -> String {
    q.unwrap_or("").trim().chars().take(MAX_QUERY_CHARS).collect()
}

async fn online_minutes(core: &Core) -> ApiResult<i32> {
    let cfg: RemoteSettings = settings::load(&core.db).await?;
    Ok(cfg.online_minutes.clamp(1, 60) as i32)
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct DeviceRow {
    pub id: Uuid,
    pub hostname: String,
    pub fqdn: Option<String>,
    pub os_version: Option<String>,
    pub agent_version: Option<String>,
    pub ip_addrs: Vec<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_seen_at: Option<OffsetDateTime>,
    pub online: bool,
    pub is_active: bool,
    /// Пользователи в сессиях (логины).
    pub users: Vec<String>,
    /// Псевдоним из избранного инженера (`None` — не в избранном).
    pub alias: Option<String>,
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct DevicesQuery {
    /// Часть имени ПК, DNS-имени, логина в сессии или IP.
    pub q: Option<String>,
    /// Только в сети.
    pub online: Option<bool>,
    pub limit: Option<i64>,
}

const DEVICE_COLS: &str = "d.id, d.hostname, d.fqdn, d.os_version, d.agent_version,
    ARRAY(SELECT host(a) FROM unnest(d.ip_addrs) a ORDER BY 1) AS ip_addrs, d.last_seen_at,
    coalesce(d.last_seen_at > now() - make_interval(mins => $2), false) AS online, d.is_active,
    ARRAY(SELECT s.username FROM device_sessions s WHERE s.device_id = d.id ORDER BY s.session_id) AS users,
    f.alias";

/// Компьютеры с агентом: сначала избранные, затем в сети, по имени.
#[utoipa::path(get, path = "/api/remote/devices", tag = "remote", params(DevicesQuery),
    responses((status = 200, body = Vec<DeviceRow>)))]
pub async fn list_devices(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<DevicesQuery>,
) -> ApiResult<Json<Vec<DeviceRow>>> {
    user.require(&perm::DEVICE_VIEW)?;
    let n = needle(q.q.as_deref());
    let rows = sqlx::query_as::<_, DeviceRow>(&format!(
        "SELECT {DEVICE_COLS}
           FROM devices d LEFT JOIN remote_favorites f ON f.device_id = d.id AND f.user_id = $1
          WHERE ($3 = '' OR strpos(lower(d.hostname), lower($3)) > 0 OR strpos(lower(coalesce(d.fqdn, '')), lower($3)) > 0
                 OR strpos(lower(coalesce(f.alias, '')), lower($3)) > 0
                 OR EXISTS (SELECT 1 FROM device_sessions s WHERE s.device_id = d.id
                             AND strpos(lower(s.username), lower($3)) > 0)
                 OR EXISTS (SELECT 1 FROM unnest(d.ip_addrs) a WHERE host(a) = $3))
            AND (NOT $4 OR d.last_seen_at > now() - make_interval(mins => $2))
          ORDER BY (f.user_id IS NOT NULL) DESC, d.is_active DESC,
                   coalesce(d.last_seen_at > now() - make_interval(mins => $2), false) DESC, d.hostname
          LIMIT $5" // SAFE: константа
    ))
    .bind(user.user_id)
    .bind(online_minutes(&core).await?)
    .bind(&n)
    .bind(q.online.unwrap_or(false))
    .bind(q.limit.unwrap_or(100).clamp(1, 500))
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct SessionRow {
    pub device_id: Uuid,
    pub hostname: String,
    pub online: bool,
    pub session_id: i32,
    pub username: String,
    pub user_domain: Option<String>,
    /// Имя из AD, если учётка найдена.
    pub display_name: Option<String>,
    pub person_id: Option<Uuid>,
    /// `active` | `disconnected` | `idle`.
    pub state: String,
    pub client_name: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub logon_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

const SESSION_SELECT: &str = "SELECT s.device_id, d.hostname,
        coalesce(d.last_seen_at > now() - make_interval(mins => $1), false) AS online,
        s.session_id, s.username, s.user_domain, a.display_name, p.id AS person_id, s.state, s.client_name,
        s.logon_at, s.updated_at
   FROM device_sessions s JOIN devices d ON d.id = s.device_id AND d.is_active
   LEFT JOIN ad_accounts a ON a.id = s.account_id
   LEFT JOIN persons p ON p.ad_account_id = s.account_id";

#[derive(Debug, Deserialize, IntoParams)]
pub struct WhereQuery {
    /// Логин, ФИО или часть имени.
    pub q: Option<String>,
}

/// «Где сейчас залогинен»: сессии по логину или имени сотрудника — по данным агентов, мгновенно.
#[utoipa::path(get, path = "/api/remote/sessions", tag = "remote", params(WhereQuery),
    responses((status = 200, body = Vec<SessionRow>)))]
pub async fn where_is(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<WhereQuery>,
) -> ApiResult<Json<Vec<SessionRow>>> {
    user.require(&perm::DEVICE_VIEW)?;
    let n = needle(q.q.as_deref());
    if n.chars().count() < 2 {
        return Ok(Json(Vec::new()));
    }
    let rows = sqlx::query_as::<_, SessionRow>(&format!(
        "{SESSION_SELECT}
          WHERE strpos(lower(s.username), lower($2)) > 0
             OR strpos(lower(coalesce(a.display_name, '')), lower($2)) > 0
             OR strpos(lower(p.last_name || ' ' || p.first_name || coalesce(' ' || p.middle_name, '')), lower($2)) > 0
          ORDER BY (lower(s.username) = lower($2)) DESC, (s.state = 'active') DESC, s.updated_at DESC
          LIMIT 50" // SAFE: константа
    ))
    .bind(online_minutes(&core).await?)
    .bind(&n)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

/// Сессии сотрудника (карточка сотрудника: «Где сейчас залогинен»).
#[utoipa::path(get, path = "/api/remote/persons/{id}/sessions", tag = "remote", params(("id" = Uuid, Path)),
    responses((status = 200, body = Vec<SessionRow>)))]
pub async fn person_sessions(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<SessionRow>>> {
    user.require(&perm::DEVICE_VIEW)?;
    let rows = sqlx::query_as::<_, SessionRow>(&format!(
        "{SESSION_SELECT}
          WHERE p.id = $2
          ORDER BY (s.state = 'active') DESC, s.updated_at DESC" // SAFE: константа
    ))
    .bind(online_minutes(&core).await?)
    .bind(id)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct RemoteConnectionRow {
    pub id: Uuid,
    pub operator: String,
    pub action: String,
    pub target_user: Option<String>,
    pub mode: Option<String>,
    pub consent: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub ticket_number: Option<i64>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct DeviceTicket {
    pub number: i64,
    pub title: String,
    pub status: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeviceCard {
    pub device: DeviceRow,
    pub domain_name: Option<String>,
    pub os: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub enrolled_at: OffsetDateTime,
    /// Описание компьютера в AD.
    pub description: Option<String>,
    /// Правила политики: `deny`, `consent_required`.
    pub rules: Vec<String>,
    pub sessions: Vec<SessionRow>,
    pub connections: Vec<RemoteConnectionRow>,
    /// Последние заявки с этого ПК (видны тем, кто работает с заявками).
    pub tickets: Vec<DeviceTicket>,
    /// Оборудование и программы из отчёта агента; агент старой версии их не присылает.
    pub inventory: Option<crate::inventory::DeviceInventory>,
}

/// Карточка ПК.
#[utoipa::path(get, path = "/api/remote/devices/{id}", tag = "remote", params(("id" = Uuid, Path)),
    responses((status = 200, body = DeviceCard), (status = 404, description = "Нет такого")))]
pub async fn get_device(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<DeviceCard>> {
    user.require(&perm::DEVICE_VIEW)?;
    let mins = online_minutes(&core).await?;
    let device = sqlx::query_as::<_, DeviceRow>(&format!(
        "SELECT {DEVICE_COLS}
           FROM devices d LEFT JOIN remote_favorites f ON f.device_id = d.id AND f.user_id = $1
          WHERE d.id = $3" // SAFE: константа
    ))
    .bind(user.user_id)
    .bind(mins)
    .bind(id)
    .fetch_optional(&core.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    type Extra = (Option<String>, Option<String>, OffsetDateTime, Option<String>, Option<String>);
    let (domain_name, os, enrolled_at, description, ad_domain): Extra = sqlx::query_as(
        "SELECT d.domain_name, d.os, d.enrolled_at, c.description, dom.dns_name
           FROM devices d LEFT JOIN ad_computers c ON c.id = d.ad_computer_id
           LEFT JOIN ad_domains dom ON dom.id = c.domain_id
          WHERE d.id = $1",
    )
    .bind(id)
    .fetch_one(&core.db)
    .await?;
    // Те же правила, что при подключении: по имени из AD и домену.
    let target = crate::policy::connect_target(&device.hostname, ad_domain.as_deref());
    let rules = rules(&core, &device.hostname, Some(&target)).await?;
    let sessions = sqlx::query_as::<_, SessionRow>(&format!(
        "{SESSION_SELECT} WHERE s.device_id = $2 ORDER BY s.session_id" // SAFE: константа
    ))
    .bind(mins)
    .bind(id)
    .fetch_all(&core.db)
    .await?;
    let connections = sqlx::query_as::<_, RemoteConnectionRow>(
        "SELECT c.id, coalesce(a.display_name, a.sam_account_name) AS operator, c.action, c.target_user, c.mode,
                c.consent, CASE WHEN c.status = 'issued' AND c.created_at < now() - make_interval(secs => $2)
                                THEN 'expired' ELSE c.status END AS status,
                c.error, t.number AS ticket_number, c.created_at
           FROM remote_connections c JOIN users u ON u.id = c.operator_id JOIN ad_accounts a ON a.id = u.ad_account_id
           LEFT JOIN tickets t ON t.id = c.ticket_id
          WHERE c.device_id = $1 ORDER BY c.created_at DESC LIMIT 20",
    )
    .bind(id)
    .bind(TOKEN_SECS as f64)
    .fetch_all(&core.db)
    .await?;
    let tickets = if user.grants.has(&pult_helpdesk::perm::WORK) {
        sqlx::query_as::<_, DeviceTicket>(
            "SELECT number, title, status, created_at FROM tickets
              WHERE device_id = $1 OR lower(computer_name) = lower($2)
              ORDER BY created_at DESC LIMIT 10",
        )
        .bind(id)
        .bind(&device.hostname)
        .fetch_all(&core.db)
        .await?
    } else {
        Vec::new()
    };
    let inventory = crate::inventory::load(&core, id).await?;
    Ok(Json(DeviceCard {
        inventory,
        device,
        domain_name,
        os,
        enrolled_at,
        description,
        rules: rules.iter().map(|r| r.as_str().to_string()).collect(),
        sessions,
        connections,
        tickets,
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct FavoriteDevice {
    #[serde(default)]
    pub alias: String,
}

/// Добавить ПК в избранное (с псевдонимом) — личная настройка инженера.
#[utoipa::path(put, path = "/api/remote/devices/{id}/favorite", tag = "remote", params(("id" = Uuid, Path)),
    request_body = FavoriteDevice, responses((status = 204, description = "Сохранено")))]
pub async fn put_favorite(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<FavoriteDevice>,
) -> ApiResult<StatusCode> {
    user.require(&perm::DEVICE_VIEW)?;
    let alias: String = b.alias.trim().chars().filter(|c| !c.is_control()).take(100).collect();
    let n = sqlx::query(
        "INSERT INTO remote_favorites (user_id, device_id, alias) SELECT $1, id, $3 FROM devices WHERE id = $2
         ON CONFLICT (user_id, device_id) DO UPDATE SET alias = EXCLUDED.alias",
    )
    .bind(user.user_id)
    .bind(id)
    .bind(alias)
    .execute(&core.db)
    .await?
    .rows_affected();
    if n == 0 { Err(ApiError::NotFound) } else { Ok(StatusCode::NO_CONTENT) }
}

/// Убрать ПК из избранного.
#[utoipa::path(delete, path = "/api/remote/devices/{id}/favorite", tag = "remote", params(("id" = Uuid, Path)),
    responses((status = 204, description = "Убрано")))]
pub async fn delete_favorite(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require(&perm::DEVICE_VIEW)?;
    sqlx::query("DELETE FROM remote_favorites WHERE user_id = $1 AND device_id = $2")
        .bind(user.user_id)
        .bind(id)
        .execute(&core.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Имя файла для `Content-Disposition`: только безопасные символы.
pub fn safe_file_name(s: &str) -> String {
    let n: String = s.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).take(100).collect();
    if n.is_empty() || n.starts_with('.') { "package.bin".into() } else { n }
}

/// Скачать текущий пакет: helper — инженерам с правом подключения, MSI агента — администратору.
#[utoipa::path(get, path = "/api/remote/packages/{kind}/download", tag = "remote",
    params(("kind" = String, Path, description = "`helper` или `agent_msi`")),
    responses((status = 200, description = "Файл", content_type = "application/octet-stream"),
              (status = 404, description = "Пакет не загружен")))]
pub async fn download_package(
    State(core): State<Core>,
    user: CurrentUser,
    Path(kind): Path<String>,
) -> ApiResult<impl IntoResponse> {
    match kind.as_str() {
        "helper" if user.grants.has(&perm::CONNECT_VIEW) || user.grants.has(&perm::CONNECT_CONTROL) => {}
        "agent_msi" => user.require(&perm::MANAGE)?,
        "helper" => return Err(ApiError::Forbidden),
        _ => return Err(ApiError::NotFound),
    }
    let (file_name, data): (String, Vec<u8>) =
        sqlx::query_as("SELECT file_name, data FROM agent_packages WHERE kind = $1 AND is_current")
            .bind(&kind)
            .fetch_optional(&core.db)
            .await?
            .ok_or(ApiError::NotFound)?;
    let disposition = format!("attachment; filename=\"{}\"", safe_file_name(&file_name));
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_string()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        data,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names() {
        assert_eq!(safe_file_name("pult-helper-0.5.0.exe"), "pult-helper-0.5.0.exe");
        assert_eq!(safe_file_name("a\"; b.exe\r\n"), "ab.exe");
        assert_eq!(safe_file_name("..\\..\\x"), "package.bin");
        assert_eq!(safe_file_name(""), "package.bin");
    }
}
