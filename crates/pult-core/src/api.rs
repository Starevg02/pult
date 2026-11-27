//! HTTP API ядра: уведомления, роли и пользователи, безопасность, журнал аудита.

use crate::{
    ApiError, ApiResult, Core, CurrentUser, Module, Permission,
    audit::{self, Entry, Source, VerifyResult},
    notify::Notification,
    rbac::{ROLE_SUPERADMIN, perm},
    settings::{self, SecuritySettings},
};
use axum::{
    Json,
    extract::{FromRef, Path, Query, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub struct CoreModule;

impl<S> Module<S> for CoreModule
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
{
    fn name(&self) -> &'static str {
        "core"
    }

    fn routes(&self) -> OpenApiRouter<S> {
        OpenApiRouter::new()
            .routes(routes!(list_notifications))
            .routes(routes!(events_stream))
            .routes(routes!(mark_read))
            .routes(routes!(get_security, put_security))
            .routes(routes!(get_locale, put_locale))
            .routes(routes!(list_roles))
            .routes(routes!(put_role_permissions))
            .routes(routes!(list_users))
            .routes(routes!(put_user_roles))
            .routes(routes!(list_audit))
            .routes(routes!(audit_csv))
            .routes(routes!(get_audit_verify, run_audit_verify))
            .routes(routes!(get_mail, put_mail))
            .routes(routes!(test_mail))
            .routes(routes!(list_outbox))
            .routes(routes!(crate::org::get_org))
            .routes(routes!(crate::org::put_org))
            .routes(routes!(crate::org::get_logo))
            .routes(routes!(crate::org::put_logo, crate::org::delete_logo))
    }

    fn permissions(&self) -> Vec<Permission> {
        perm::ALL.to_vec()
    }
}

// ------------------------------------------------------------------ Уведомления

#[derive(Serialize, ToSchema)]
pub struct NotificationList {
    pub unread: i64,
    pub items: Vec<Notification>,
}

/// Последние уведомления пользователя и число непрочитанных.
#[utoipa::path(get, path = "/api/notifications", tag = "notifications",
    responses((status = 200, body = NotificationList)))]
async fn list_notifications(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<NotificationList>> {
    let items: Vec<Notification> = sqlx::query_as(
        "SELECT id, kind, payload, created_at, read_at FROM notifications
          WHERE user_id = $1 ORDER BY created_at DESC LIMIT 50",
    )
    .bind(user.user_id)
    .fetch_all(&core.db)
    .await?;
    let unread: i64 = sqlx::query_scalar("SELECT count(*) FROM notifications WHERE user_id = $1 AND read_at IS NULL")
        .bind(user.user_id)
        .fetch_one(&core.db)
        .await?;
    Ok(Json(NotificationList { unread, items }))
}

#[derive(Deserialize, ToSchema)]
pub struct MarkRead {
    /// Пусто — отметить все.
    #[serde(default)]
    pub ids: Vec<Uuid>,
}

/// Отметить уведомления прочитанными.
#[utoipa::path(post, path = "/api/notifications/read", tag = "notifications", request_body = MarkRead,
    responses((status = 204)))]
async fn mark_read(
    State(core): State<Core>,
    user: CurrentUser,
    Json(body): Json<MarkRead>,
) -> ApiResult<axum::http::StatusCode> {
    sqlx::query(
        "UPDATE notifications SET read_at = now()
          WHERE user_id = $1 AND read_at IS NULL AND (cardinality($2::uuid[]) = 0 OR id = ANY($2))",
    )
    .bind(user.user_id)
    .bind(&body.ids)
    .execute(&core.db)
    .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Данные события для браузера: номер заявки или чат и автор действия — без содержимого.
fn sse_data(e: &crate::events::Event) -> String {
    match (e.ticket, e.chat) {
        (Some(t), _) => t.to_string(),
        (None, Some(c)) => serde_json::json!({"chat": c, "from": e.from}).to_string(),
        (None, None) => String::new(),
    }
}

/// Поток событий (SSE): что изменилось. Сами данные браузер читает через API.
/// Сеанс перепроверяется раз в минуту: закрытый сеанс обрывает поток.
#[utoipa::path(get, path = "/api/events", tag = "core",
    responses((status = 200, description = "Поток событий", content_type = "text/event-stream")))]
async fn events_stream(State(core): State<Core>, user: CurrentUser, headers: HeaderMap) -> ApiResult<Response> {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use tokio::sync::broadcast::error::RecvError;

    let token =
        crate::session::cookie(&headers, core.cookies.session_name()).ok_or(ApiError::Unauthorized)?.to_string();
    struct St {
        rx: tokio::sync::broadcast::Receiver<std::sync::Arc<crate::events::Event>>,
        stopped: tokio::sync::watch::Receiver<bool>,
        tick: tokio::time::Interval,
        grants: crate::rbac::Grants,
        core: Core,
        token: String,
        user: Uuid,
    }
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    tick.reset();
    let st = St {
        rx: core.events.subscribe(),
        stopped: core.events.stopped(),
        tick,
        grants: user.grants.clone(),
        token,
        user: user.user_id,
        core,
    };
    let stream = futures_util::stream::unfold(st, |mut s| async move {
        loop {
            let event = tokio::select! {
                r = s.stopped.changed() => {
                    if r.is_err() || *s.stopped.borrow() {
                        return None;
                    }
                    continue;
                }
                _ = s.tick.tick() => {
                    match crate::session::current_grants(&s.core, &s.token, s.user).await {
                        Ok(Some(g)) => s.grants = g,
                        Ok(None) => return None,
                        Err(e) => tracing::warn!(error = %e, "проверка сеанса SSE"),
                    }
                    continue;
                }
                r = s.rx.recv() => r,
            };
            let out = match event {
                Ok(e) if e.visible_to(s.user, &s.grants) => {
                    SseEvent::default().event(e.kind.clone()).data(sse_data(&e))
                }
                Ok(_) => continue,
                // Отстали — пусть браузер перечитает всё.
                Err(RecvError::Lagged(_)) => SseEvent::default().event("refresh").data(""),
                Err(RecvError::Closed) => return None,
            };
            return Some((Ok::<_, std::convert::Infallible>(out), s));
        }
    });
    let mut resp = Sse::new(stream).keep_alive(KeepAlive::default()).into_response();
    resp.headers_mut().insert("x-accel-buffering", axum::http::HeaderValue::from_static("no"));
    Ok(resp)
}

// ------------------------------------------------------------------ Безопасность

/// Настройки сеансов и входа.
#[utoipa::path(get, path = "/api/admin/security", tag = "admin",
    responses((status = 200, body = SecuritySettings)))]
async fn get_security(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<SecuritySettings>> {
    user.require(&perm::SETTINGS_MANAGE)?;
    Ok(Json(settings::load(&core.db).await?))
}

#[utoipa::path(put, path = "/api/admin/security", tag = "admin", request_body = SecuritySettings,
    responses((status = 200, body = SecuritySettings)))]
async fn put_security(
    State(core): State<Core>,
    user: CurrentUser,
    Json(body): Json<SecuritySettings>,
) -> ApiResult<Json<SecuritySettings>> {
    user.require(&perm::SETTINGS_MANAGE)?;
    body.validate().map_err(|m| ApiError::bad_request("invalid_settings", m))?;
    let mut tx = core.db.begin().await?;
    let before: SecuritySettings = settings::load(&mut *tx).await?;
    settings::save(&mut *tx, &body, Some(user.user_id)).await?;
    let d = audit::diff(&json!(before), &json!(body));
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("settings.update", Source::Portal).object("settings", "security").details(d),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(body))
}

// ------------------------------------------------------------------ Язык установки

/// Язык по умолчанию: новые пользователи, письма на адреса без учётки, фоновые тексты.
#[utoipa::path(get, path = "/api/admin/locale", tag = "admin", responses((status = 200, body = settings::LocaleSettings)))]
async fn get_locale(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<settings::LocaleSettings>> {
    user.require(&perm::SETTINGS_MANAGE)?;
    Ok(Json(settings::load(&core.db).await?))
}

#[utoipa::path(put, path = "/api/admin/locale", tag = "admin", request_body = settings::LocaleSettings,
    responses((status = 200, body = settings::LocaleSettings)))]
async fn put_locale(
    State(core): State<Core>,
    user: CurrentUser,
    Json(body): Json<settings::LocaleSettings>,
) -> ApiResult<Json<settings::LocaleSettings>> {
    user.require(&perm::SETTINGS_MANAGE)?;
    if !crate::l10n::Lang::is_code(&body.default_locale) {
        return Err(ApiError::bad_request("invalid_settings", "Язык — ru, en или zh."));
    }
    let mut tx = core.db.begin().await?;
    let before: settings::LocaleSettings = settings::load(&mut *tx).await?;
    settings::save(&mut *tx, &body, Some(user.user_id)).await?;
    let d = audit::diff(&json!(before), &json!(body));
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("settings.update", Source::Portal).object("settings", "locale").details(d),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(body))
}

// ------------------------------------------------------------------ Роли

#[derive(Serialize, ToSchema)]
pub struct PermissionInfo {
    pub code: String,
    pub description: String,
}

#[derive(Serialize, ToSchema)]
pub struct RoleInfo {
    pub code: String,
    pub name: String,
    pub is_system: bool,
    pub permissions: Vec<String>,
    pub users: i64,
}

#[derive(Serialize, ToSchema)]
pub struct RolesMatrix {
    pub permissions: Vec<PermissionInfo>,
    pub roles: Vec<RoleInfo>,
}

/// Роли, их разрешения и все разрешения модулей.
#[utoipa::path(get, path = "/api/admin/roles", tag = "admin", responses((status = 200, body = RolesMatrix)))]
async fn list_roles(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<RolesMatrix>> {
    user.require(&perm::ROLES_MANAGE)?;
    let rows: Vec<(String, String, bool, Vec<String>, i64)> = sqlx::query_as(
        "SELECT r.code, r.name, r.is_system,
                coalesce(array_agg(DISTINCT rp.permission) FILTER (WHERE rp.permission IS NOT NULL), '{}'),
                (SELECT count(DISTINCT ur.user_id) FROM user_roles ur WHERE ur.role_id = r.id)
           FROM roles r LEFT JOIN role_permissions rp ON rp.role_id = r.id
          GROUP BY r.id ORDER BY r.created_at",
    )
    .fetch_all(&core.db)
    .await?;
    let mut permissions: Vec<PermissionInfo> = core
        .permissions
        .iter()
        .map(|p| PermissionInfo { code: p.code.into(), description: crate::l10n::tr(p.description).into() })
        .collect();
    permissions.sort_by(|a, b| a.code.cmp(&b.code));
    let roles = rows
        .into_iter()
        .map(|(code, name, is_system, permissions, users)| {
            let name = role_display_name(&code, name, is_system);
            RoleInfo { code, name, is_system, permissions, users }
        })
        .collect();
    Ok(Json(RolesMatrix { permissions, roles }))
}

/// Название роли для показа: системная роль с исходным названием — на языке запроса,
/// переименованная администратором — как есть.
pub fn role_display_name(code: &str, name: String, is_system: bool) -> String {
    match system_role_name(code) {
        Some(ru) if is_system && name == ru => crate::l10n::tr(ru).into_owned(),
        _ => name,
    }
}

/// Исходные названия системных ролей (миграция `core`): показываются на языке запроса.
fn system_role_name(code: &str) -> Option<&'static str> {
    Some(match code {
        "employee" => "Сотрудник",
        "engineer" => "Инженер",
        "phonebook_editor" => "Редактор справочника",
        "superadmin" => "Суперадминистратор",
        "auditor" => "Аудитор",
        _ => return None,
    })
}

#[derive(Deserialize, ToSchema)]
pub struct RolePermissions {
    pub permissions: Vec<String>,
}

/// Заменить разрешения роли. У суперадминистратора разрешения не настраиваются — у него есть все.
#[utoipa::path(put, path = "/api/admin/roles/{code}/permissions", tag = "admin", request_body = RolePermissions,
    params(("code" = String, Path)), responses((status = 204)))]
async fn put_role_permissions(
    State(core): State<Core>,
    user: CurrentUser,
    Path(code): Path<String>,
    Json(body): Json<RolePermissions>,
) -> ApiResult<axum::http::StatusCode> {
    user.require(&perm::ROLES_MANAGE)?;
    if code == ROLE_SUPERADMIN {
        return Err(ApiError::bad_request("superadmin_fixed", "У суперадминистратора есть все разрешения."));
    }
    if let Some(bad) = body.permissions.iter().find(|p| !core.permissions.iter().any(|k| k.code == p.as_str())) {
        return Err(ApiError::bad_request("unknown_permission", format!("Неизвестное разрешение: {bad}")));
    }
    let mut tx = core.db.begin().await?;
    let role_id: Uuid =
        sqlx::query_scalar("SELECT id FROM roles WHERE code = $1").bind(&code).fetch_one(&mut *tx).await?;
    let before: Vec<String> =
        sqlx::query_scalar("SELECT permission FROM role_permissions WHERE role_id = $1 ORDER BY permission")
            .bind(role_id)
            .fetch_all(&mut *tx)
            .await?;
    // Не суперадминистратор меняет только те разрешения, что есть у него самого: иначе
    // `core.roles.manage` дал бы любые права (например, `people.ad.manage` — а с ним и вход за любого).
    if !user.grants.is_superadmin() {
        let changed = before
            .iter()
            .filter(|p| !body.permissions.contains(p))
            .chain(body.permissions.iter().filter(|p| !before.contains(p)));
        if let Some(p) = changed.into_iter().find(|p| !user.grants.permissions.contains(p.as_str())) {
            return Err(ApiError::bad_request(
                "escalation",
                format!("Нельзя выдать или снять разрешение, которого нет у вас: {p}"),
            ));
        }
    }
    sqlx::query("DELETE FROM role_permissions WHERE role_id = $1").bind(role_id).execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO role_permissions (role_id, permission) SELECT $1, unnest($2::text[]) ON CONFLICT DO NOTHING",
    )
    .bind(role_id)
    .bind(&body.permissions)
    .execute(&mut *tx)
    .await?;
    let mut after = body.permissions.clone();
    after.sort();
    after.dedup();
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("role.permissions", Source::Portal)
            .object("role", &code)
            .details(json!({"before": before, "after": after})),
    )
    .await?;
    tx.commit().await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Пользователи

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct UserInfo {
    pub id: Uuid,
    pub login: String,
    pub name: String,
    pub email: Option<String>,
    pub is_active: bool,
    pub roles: Vec<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_login_at: Option<OffsetDateTime>,
    /// Подключён второй фактор (вход из интернета).
    pub totp: bool,
}

#[derive(Deserialize, IntoParams)]
pub struct UserQuery {
    /// Поиск по логину или имени.
    pub q: Option<String>,
    /// Только с этой ролью.
    pub role: Option<String>,
}

/// Пользователи портала с ролями.
#[utoipa::path(get, path = "/api/admin/users", tag = "admin", params(UserQuery),
    responses((status = 200, body = Vec<UserInfo>)))]
async fn list_users(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<UserQuery>,
) -> ApiResult<Json<Vec<UserInfo>>> {
    user.require(&perm::ROLES_MANAGE)?;
    let like = q.q.as_deref().map(|s| format!("%{}%", s.trim().to_lowercase().replace(['%', '_'], "")));
    let rows: Vec<UserInfo> = sqlx::query_as(
        "SELECT u.id, a.sam_account_name AS login,
                coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name, p.middle_name)), ''), a.display_name, a.sam_account_name) AS name,
                coalesce(p.email, a.mail) AS email, u.is_active,
                coalesce(array_agg(DISTINCT r.code) FILTER (WHERE r.code IS NOT NULL), '{}') AS roles,
                u.last_login_at, u.totp_enabled_at IS NOT NULL AS totp
           FROM users u
           JOIN ad_accounts a ON a.id = u.ad_account_id
           LEFT JOIN persons p ON p.id = u.person_id
           LEFT JOIN user_roles ur ON ur.user_id = u.id
           LEFT JOIN roles r ON r.id = ur.role_id
          WHERE ($1::text IS NULL OR lower(a.sam_account_name) LIKE $1 OR lower(coalesce(a.display_name, '')) LIKE $1
                 OR lower(concat_ws(' ', p.last_name, p.first_name)) LIKE $1)
          GROUP BY u.id, a.id, p.id
         HAVING $2::text IS NULL OR $2 = ANY(array_agg(r.code))
          ORDER BY name LIMIT 500",
    )
    .bind(like)
    .bind(q.role)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize, ToSchema)]
pub struct UserRoles {
    pub roles: Vec<String>,
}

/// Заменить роли пользователя. Роль суперадминистратора выдаёт и снимает только суперадминистратор;
/// последнего снять нельзя.
#[utoipa::path(put, path = "/api/admin/users/{id}/roles", tag = "admin", request_body = UserRoles,
    params(("id" = Uuid, Path)), responses((status = 204)))]
async fn put_user_roles(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UserRoles>,
) -> ApiResult<axum::http::StatusCode> {
    user.require(&perm::ROLES_MANAGE)?;
    let mut tx = core.db.begin().await?;
    // Последовательно: проверка «последний суперадминистратор» не должна гоняться сама с собой.
    sqlx::query("SELECT pg_advisory_xact_lock(7231003)").execute(&mut *tx).await?;
    let before: Vec<String> = sqlx::query_scalar(
        "SELECT r.code FROM user_roles ur JOIN roles r ON r.id = ur.role_id WHERE ur.user_id = $1 ORDER BY r.code",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let mut after: Vec<String> =
        body.roles.iter().filter(|r| r.as_str() != crate::rbac::ROLE_EMPLOYEE).cloned().collect();
    after.sort();
    after.dedup();
    let touches_super = before.iter().any(|r| r == ROLE_SUPERADMIN) != after.iter().any(|r| r == ROLE_SUPERADMIN);
    if touches_super && !user.grants.is_superadmin() {
        return Err(ApiError::Forbidden);
    }
    if before.iter().any(|r| r == ROLE_SUPERADMIN) && !after.iter().any(|r| r == ROLE_SUPERADMIN) {
        let others: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM user_roles ur JOIN roles r ON r.id = ur.role_id JOIN users u ON u.id = ur.user_id
              WHERE r.code = $1 AND ur.user_id <> $2 AND u.is_active",
        )
        .bind(ROLE_SUPERADMIN)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if others == 0 {
            return Err(ApiError::bad_request(
                "last_superadmin",
                "Нельзя снять роль с последнего суперадминистратора.",
            ));
        }
    }
    let known: i64 =
        sqlx::query_scalar("SELECT count(*) FROM roles WHERE code = ANY($1)").bind(&after).fetch_one(&mut *tx).await?;
    if known != after.len() as i64 {
        return Err(ApiError::bad_request("unknown_role", "Неизвестная роль."));
    }
    // Не суперадминистратор выдаёт и снимает только роли, все разрешения которых есть у него самого.
    if !user.grants.is_superadmin() {
        let changed: Vec<String> = before
            .iter()
            .filter(|r| !after.contains(r))
            .chain(after.iter().filter(|r| !before.contains(r)))
            .cloned()
            .collect();
        let missing: Option<String> = sqlx::query_scalar(
            "SELECT rp.permission FROM role_permissions rp JOIN roles r ON r.id = rp.role_id
              WHERE r.code = ANY($1) AND NOT (rp.permission = ANY($2)) LIMIT 1",
        )
        .bind(&changed)
        .bind(user.grants.permissions.iter().cloned().collect::<Vec<String>>())
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(p) = missing {
            return Err(ApiError::bad_request(
                "escalation",
                format!("Нельзя выдать или снять роль с разрешением, которого нет у вас: {p}"),
            ));
        }
    }
    sqlx::query("DELETE FROM user_roles WHERE user_id = $1 AND site_id IS NULL").bind(id).execute(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO user_roles (user_id, role_id, granted_by)
         SELECT $1, r.id, $3 FROM roles r WHERE r.code = ANY($2) ON CONFLICT DO NOTHING",
    )
    .bind(id)
    .bind(&after)
    .bind(user.user_id)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("user.roles", Source::Portal).object("user", id).details(json!({"before": before, "after": after})),
    )
    .await?;
    tx.commit().await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Аудит

#[derive(Deserialize, IntoParams)]
pub struct AuditQuery {
    pub actor_id: Option<Uuid>,
    pub action: Option<String>,
    pub object_type: Option<String>,
    pub object_id: Option<String>,
    /// С (RFC 3339).
    pub from: Option<String>,
    /// По (RFC 3339).
    pub to: Option<String>,
    /// Записи с номером меньше (постранично).
    pub before_id: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct AuditRow {
    pub id: i64,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub ts: OffsetDateTime,
    pub actor_id: Option<Uuid>,
    pub actor: Option<String>,
    pub action: String,
    pub object_type: Option<String>,
    pub object_id: Option<String>,
    #[schema(value_type = Object)]
    pub details: serde_json::Value,
    pub ip: Option<String>,
    pub source: String,
}

fn parse_ts(s: &Option<String>) -> ApiResult<Option<OffsetDateTime>> {
    s.as_deref()
        .map(|v| {
            OffsetDateTime::parse(v, &time::format_description::well_known::Rfc3339)
                .map_err(|_| ApiError::bad_request("bad_date", "Дата — в формате RFC 3339"))
        })
        .transpose()
}

async fn fetch_audit(core: &Core, q: &AuditQuery, limit: i64) -> ApiResult<Vec<AuditRow>> {
    Ok(sqlx::query_as(
        "SELECT l.id, l.ts, l.actor_id, a.sam_account_name AS actor, l.action, l.object_type, l.object_id, l.details,
                host(l.ip) AS ip, l.source
           FROM audit_log l
           LEFT JOIN users u ON u.id = l.actor_id
           LEFT JOIN ad_accounts a ON a.id = u.ad_account_id
          WHERE ($1::uuid IS NULL OR l.actor_id = $1)
            AND ($2::text IS NULL OR l.action = $2 OR l.action LIKE $2 || '.%')
            AND ($3::text IS NULL OR l.object_type = $3)
            AND ($4::text IS NULL OR l.object_id = $4)
            AND ($5::timestamptz IS NULL OR l.ts >= $5)
            AND ($6::timestamptz IS NULL OR l.ts < $6)
            AND ($7::bigint IS NULL OR l.id < $7)
          ORDER BY l.id DESC LIMIT $8",
    )
    .bind(q.actor_id)
    .bind(&q.action)
    .bind(&q.object_type)
    .bind(&q.object_id)
    .bind(parse_ts(&q.from)?)
    .bind(parse_ts(&q.to)?)
    .bind(q.before_id)
    .bind(limit)
    .fetch_all(&core.db)
    .await?)
}

/// Журнал аудита с фильтрами, новые сверху.
#[utoipa::path(get, path = "/api/admin/audit", tag = "admin", params(AuditQuery),
    responses((status = 200, body = Vec<AuditRow>)))]
async fn list_audit(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<Vec<AuditRow>>> {
    user.require(&perm::AUDIT_VIEW)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let mut rows = fetch_audit(&core, &q, limit).await?;
    // В журнале — служебные слова («изменён», «удалён») на русском; на экране — на языке интерфейса.
    // Выгрузка CSV остаётся как записано.
    for r in &mut rows {
        crate::l10n::tr_values(&mut r.details);
    }
    Ok(Json(rows))
}

/// Выгрузка журнала в CSV (до 50 000 записей по фильтру). Сама выгрузка тоже попадает в журнал.
#[utoipa::path(get, path = "/api/admin/audit.csv", tag = "admin", params(AuditQuery),
    responses((status = 200, content_type = "text/csv")))]
async fn audit_csv(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<AuditQuery>,
) -> ApiResult<impl IntoResponse> {
    user.require(&perm::AUDIT_VIEW)?;
    let rows = fetch_audit(&core, &q, 50_000).await?;
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("audit.export", Source::Portal).details(json!({"rows": rows.len()})),
    )
    .await?;
    let mut out = String::from("\u{feff}id;ts;actor;action;object_type;object_id;ip;source;details\r\n");
    for r in rows {
        let ts = r.ts.format(&time::format_description::well_known::Rfc3339).unwrap_or_default();
        let cells = [
            r.id.to_string(),
            ts,
            r.actor.unwrap_or_default(),
            r.action,
            r.object_type.unwrap_or_default(),
            r.object_id.unwrap_or_default(),
            r.ip.unwrap_or_default(),
            r.source,
            r.details.to_string(),
        ];
        out.push_str(&cells.iter().map(|c| csv_cell(c)).collect::<Vec<_>>().join(";"));
        out.push_str("\r\n");
    }
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (header::CONTENT_DISPOSITION, "attachment; filename=\"pult-audit.csv\""),
        ],
        out,
    ))
}

/// Ячейка CSV: кавычки по необходимости и защита от формул в Excel (`=`, `+`, `-`, `@`).
fn csv_cell(s: &str) -> String {
    let s = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) { format!("'{s}") } else { s.to_string() };
    if s.contains([';', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s }
}

/// Результат последней проверки целостности журнала.
#[utoipa::path(get, path = "/api/admin/audit/verify", tag = "admin", responses((status = 200, body = VerifyResult)))]
async fn get_audit_verify(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<VerifyResult>> {
    user.require(&perm::AUDIT_VIEW)?;
    Ok(Json(settings::load(&core.db).await?))
}

/// Проверить целостность журнала сейчас.
#[utoipa::path(post, path = "/api/admin/audit/verify", tag = "admin", responses((status = 200, body = VerifyResult)))]
async fn run_audit_verify(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<VerifyResult>> {
    user.require(&perm::AUDIT_VIEW)?;
    Ok(Json(verify_and_store(&core.db).await?))
}

// ------------------------------------------------------------------ Почта

#[derive(Serialize, Deserialize, ToSchema)]
pub struct MailView {
    pub settings: crate::mail::MailSettings,
    /// Пароль SMTP задан (значение не показывается).
    pub has_password: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct MailUpdate {
    pub settings: crate::mail::MailSettings,
    /// Новый пароль; не передан — прежний; пустая строка — удалить.
    pub password: Option<String>,
}

async fn mail_view(core: &Core) -> ApiResult<MailView> {
    let s: crate::mail::MailSettings = settings::load(&core.db).await?;
    let p: crate::mail::MailSecret = settings::load(&core.db).await?;
    Ok(MailView { settings: s, has_password: p.sealed.is_some() })
}

/// Отправка почты (SMTP).
#[utoipa::path(get, path = "/api/admin/mail", tag = "admin", responses((status = 200, body = MailView)))]
async fn get_mail(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<MailView>> {
    user.require(&perm::MAIL_MANAGE)?;
    Ok(Json(mail_view(&core).await?))
}

#[utoipa::path(put, path = "/api/admin/mail", tag = "admin", request_body = MailUpdate, responses((status = 200, body = MailView)))]
async fn put_mail(State(core): State<Core>, user: CurrentUser, Json(b): Json<MailUpdate>) -> ApiResult<Json<MailView>> {
    use base64::Engine;
    user.require(&perm::MAIL_MANAGE)?;
    b.settings.validate().map_err(|m| ApiError::bad_request("invalid_settings", m))?;
    if b.settings.security == crate::mail::SmtpSecurity::None && !core.insecure_mail {
        return Err(ApiError::bad_request("invalid_settings", "Почта без шифрования запрещена."));
    }
    let mut tx = core.db.begin().await?;
    let before: crate::mail::MailSettings = settings::load(&mut *tx).await?;
    // Сохранённый пароль SMTP не уходит на другой сервер или под другим логином.
    let stored: crate::mail::MailSecret = settings::load(&mut *tx).await?;
    if stored.sealed.is_some() && b.password.is_none() && before.endpoint_changed(&b.settings) {
        return Err(ApiError::bad_request(
            "secret_required",
            "Сервер, порт, шифрование или логин SMTP изменились — введите пароль заново.",
        ));
    }
    settings::save(&mut *tx, &b.settings, Some(user.user_id)).await?;
    let mut details = audit::diff(&json!(before), &json!(b.settings));
    if let Some(p) = &b.password {
        let sealed = if p.is_empty() {
            None
        } else {
            let raw = core.secrets.encrypt(&crate::Secret::new(p.clone())).map_err(ApiError::internal)?;
            Some(base64::engine::general_purpose::STANDARD.encode(raw))
        };
        settings::save(&mut *tx, &crate::mail::MailSecret { sealed }, Some(user.user_id)).await?;
        if let Some(o) = details.as_object_mut() {
            o.insert("password".into(), json!(if p.is_empty() { "удалён" } else { "изменён" }));
        }
    }
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("settings.update", Source::Portal).object("settings", "mail").details(details),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(mail_view(&core).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct MailTest {
    pub to: String,
}

#[derive(Serialize, ToSchema)]
pub struct MailTestResult {
    pub ok: bool,
    pub error: Option<String>,
}

/// Отправить проверочное письмо сразу, минуя очередь.
#[utoipa::path(post, path = "/api/admin/mail/test", tag = "admin", request_body = MailTest, responses((status = 200, body = MailTestResult)))]
async fn test_mail(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<MailTest>,
) -> ApiResult<Json<MailTestResult>> {
    user.require(&perm::MAIL_MANAGE)?;
    let s: crate::mail::MailSettings = settings::load(&core.db).await?;
    let pw = crate::mail::password(&core.db, &core.secrets).await.map_err(ApiError::internal)?;
    let r = crate::mail::send_now(
        &s,
        pw,
        &[b.to.clone()],
        &crate::l10n::tr("Pult: проверка почты"),
        &crate::l10n::tr("Это проверочное письмо портала Pult. Отвечать на него не нужно."),
    )
    .await;
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("mail.test", Source::Portal).details(json!({"to": b.to, "ok": r.is_ok()})),
    )
    .await?;
    Ok(Json(MailTestResult { ok: r.is_ok(), error: r.err() }))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct OutboxRow {
    pub id: Uuid,
    pub to_addrs: Vec<String>,
    pub subject: String,
    pub status: String,
    pub attempts: i32,
    pub last_error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub sent_at: Option<OffsetDateTime>,
}

/// Последние письма очереди.
#[utoipa::path(get, path = "/api/admin/mail/outbox", tag = "admin", responses((status = 200, body = Vec<OutboxRow>)))]
async fn list_outbox(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<OutboxRow>>> {
    user.require(&perm::MAIL_MANAGE)?;
    Ok(Json(
        sqlx::query_as(
            "SELECT id, to_addrs, subject, status, attempts, last_error, created_at, sent_at
               FROM outbox_emails ORDER BY created_at DESC LIMIT 50",
        )
        .fetch_all(&core.db)
        .await?,
    ))
}

/// Проверка цепочки с сохранением результата и уведомлением при нарушении (и для задачи по расписанию).
pub async fn verify_and_store(db: &sqlx::PgPool) -> Result<VerifyResult, sqlx::Error> {
    let r = audit::verify(db).await?;
    settings::save(db, &r, None).await?;
    if let Some(id) = r.broken_at {
        tracing::error!(record = id, "журнал аудита: нарушена цепочка хэшей");
        crate::notify::notify_superadmins(db, crate::notify::kind::AUDIT_CHAIN_BROKEN, json!({"record": id})).await?;
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::csv_cell;

    #[test]
    fn csv_escaping() {
        assert_eq!(csv_cell("abc"), "abc");
        assert_eq!(csv_cell("a;b"), "\"a;b\"");
        assert_eq!(csv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_cell("a\"b;"), "\"a\"\"b;\"");
        assert_eq!(csv_cell("=1+1"), "'=1+1");
    }
}
