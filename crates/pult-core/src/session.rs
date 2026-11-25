//! Сеансы портала: случайный токен 256 бит в cookie, в БД — только SHA-256.
//! CSRF — double-submit: токен сеанса лежит в cookie `pult_csrf` (её читает фронтенд) и
//! должен прийти в заголовке `X-CSRF-Token` каждого изменяющего запроса.

use crate::{
    Core,
    audit::Actor,
    crypto::{constant_time_eq, random_token, sha256},
    error::ApiError,
    rbac::{Grants, load_grants},
    settings::{AdHealth, SecuritySettings, load},
};
use axum::{
    extract::{ConnectInfo, FromRef, FromRequestParts},
    http::{HeaderMap, Method, header, request::Parts},
};
use std::net::{IpAddr, SocketAddr};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

pub const CSRF_HEADER: &str = "x-csrf-token";

/// Имена и флаги cookie. С HTTPS — `__Host-` и `Secure` (браузер не пустит такую cookie
/// с другого поддомена и по HTTP); без HTTPS (разработка) — только явной настройкой сервера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CookiePolicy {
    pub secure: bool,
}

impl CookiePolicy {
    pub fn session_name(self) -> &'static str {
        if self.secure { "__Host-pult_session" } else { "pult_session" }
    }

    pub fn csrf_name(self) -> &'static str {
        if self.secure { "__Host-pult_csrf" } else { "pult_csrf" }
    }

    fn attrs(self) -> &'static str {
        if self.secure { "; Path=/; Secure; SameSite=Lax" } else { "; Path=/; SameSite=Lax" }
    }

    /// `Set-Cookie` для нового сеанса: токен — HttpOnly, CSRF — доступен скрипту.
    pub fn issue(self, token: &str, csrf: &str, max_age_secs: i64) -> [String; 2] {
        [
            format!("{}={token}; HttpOnly; Max-Age={max_age_secs}{}", self.session_name(), self.attrs()),
            format!("{}={csrf}; Max-Age={max_age_secs}{}", self.csrf_name(), self.attrs()),
        ]
    }

    pub fn clear(self) -> [String; 2] {
        [
            format!("{}=; HttpOnly; Max-Age=0{}", self.session_name(), self.attrs()),
            format!("{}=; Max-Age=0{}", self.csrf_name(), self.attrs()),
        ]
    }
}

/// Новый сеанс: токен для cookie и CSRF-токен.
pub struct NewSession {
    pub token: String,
    pub csrf: String,
    pub max_age_secs: i64,
}

/// Способ входа сеанса: пароль AD, пароль и код из приложения, билет Kerberos, провайдер OpenID Connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    Ldap,
    LdapTotp,
    Kerberos,
    Oidc,
}

impl AuthMethod {
    pub fn code(self) -> &'static str {
        match self {
            Self::Ldap => "ldap",
            Self::LdapTotp => "ldap+totp",
            Self::Kerberos => "kerberos",
            Self::Oidc => "oidc",
        }
    }
}

pub async fn create(core: &Core, user_id: Uuid, actor: &Actor, method: AuthMethod) -> Result<NewSession, ApiError> {
    let s: SecuritySettings = load(&core.db).await?;
    let token = random_token(32).map_err(ApiError::internal)?;
    let csrf = random_token(32).map_err(ApiError::internal)?;
    let now = OffsetDateTime::now_utc();
    let idle = now + Duration::hours(i64::from(s.session_idle_hours));
    let absolute = now + Duration::days(i64::from(s.session_absolute_days));
    sqlx::query(
        "INSERT INTO sessions (id, user_id, auth_method, csrf_token, ip, user_agent, idle_expires_at, absolute_expires_at)
         VALUES ($1, $2, $8, $3, $4, $5, $6, $7)",
    )
    .bind(sha256(token.as_bytes()))
    .bind(user_id)
    .bind(&csrf)
    .bind(actor.ip.map(sqlx::types::ipnetwork::IpNetwork::from))
    .bind(actor.user_agent.as_deref().map(|s| s.chars().take(500).collect::<String>()))
    .bind(idle)
    .bind(absolute)
    .bind(method.code())
    .execute(&core.db)
    .await?;
    Ok(NewSession { token, csrf, max_age_secs: (absolute - now).whole_seconds() })
}

pub async fn destroy(core: &Core, token: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM sessions WHERE id = $1").bind(sha256(token.as_bytes())).execute(&core.db).await?;
    Ok(())
}

/// Закрывает все сеансы пользователя (учётка отключена в AD, снята роль и т. п.).
pub async fn destroy_all(db: impl sqlx::PgExecutor<'_>, user_id: Uuid) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM sessions WHERE user_id = $1").bind(user_id).execute(db).await?.rows_affected())
}

/// Удаляет истёкшие сеансы (задача по расписанию).
pub async fn prune(db: &sqlx::PgPool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM sessions WHERE idle_expires_at < now() OR absolute_expires_at < now()")
        .execute(db)
        .await?
        .rows_affected())
}

/// Вошедший пользователь. Извлекается из cookie в каждом запросе; для изменяющих запросов
/// заодно проверяется CSRF-токен — обработчик, который требует пользователя, защищён сам.
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub user_id: Uuid,
    pub person_id: Option<Uuid>,
    pub ad_account_id: Uuid,
    pub login: String,
    pub display_name: String,
    pub locale: String,
    pub grants: Grants,
    pub actor: Actor,
}

impl CurrentUser {
    pub fn require(&self, p: &crate::Permission) -> Result<(), ApiError> {
        if self.grants.has(p) { Ok(()) } else { Err(ApiError::Forbidden) }
    }

    pub fn require_superadmin(&self) -> Result<(), ApiError> {
        if self.grants.is_superadmin() { Ok(()) } else { Err(ApiError::Forbidden) }
    }
}

/// Сеанс всё ещё действует, и его разрешения — для долгих соединений (SSE).
pub async fn current_grants(core: &Core, token: &str, user_id: Uuid) -> Result<Option<Grants>, sqlx::Error> {
    let alive: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM sessions s JOIN users u ON u.id = s.user_id
                          JOIN ad_accounts a ON a.id = u.ad_account_id
                         WHERE s.id = $1 AND s.idle_expires_at > now() AND s.absolute_expires_at > now()
                           AND u.is_active AND a.is_enabled AND NOT a.is_missing)",
    )
    .bind(sha256(token.as_bytes()))
    .fetch_one(&core.db)
    .await?;
    if !alive {
        return Ok(None);
    }
    Ok(Some(load_grants(&core.db, user_id).await?))
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    user_id: Uuid,
    csrf_token: String,
    last_seen_at: OffsetDateTime,
    person_id: Option<Uuid>,
    ad_account_id: Uuid,
    sam_account_name: String,
    display_name: Option<String>,
    locale: String,
}

/// Значение cookie по имени.
pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

/// Имя сервера из запроса: в HTTP/2 его нет в заголовке `Host`, оно приходит как `:authority`.
pub fn request_host(headers: &HeaderMap, uri: &axum::http::Uri) -> Option<String> {
    uri.authority()
        .map(|a| a.as_str().to_string())
        .or_else(|| headers.get(header::HOST).and_then(|v| v.to_str().ok()).map(str::to_string))
}

/// IP клиента. За обратным прокси — адрес прокси (доверенные заголовки — отдельной настройкой).
pub fn client_ip(parts: &Parts) -> Option<IpAddr> {
    parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip())
}

/// Метка запроса: пришёл через прокси, которому сервер не доверяет (`PULT_TRUSTED_PROXIES`).
/// Настоящий адрес клиента неизвестен — виден адрес прокси, обычно внутренний.
#[derive(Debug, Clone, Copy)]
pub struct UnverifiedForwarding;

/// Запрос из-за пределов сети организации: вход только с кодом из приложения. Адрес неизвестен или
/// скрыт недоверенным прокси — тоже «снаружи»: иначе прокси на этом же сервере открыл бы вход без кода всем.
pub fn is_external(ip: Option<IpAddr>, unverified_forwarding: bool, sec: &SecuritySettings) -> bool {
    match ip {
        Some(ip) if !unverified_forwarding => !sec.is_internal(ip),
        _ => true,
    }
}

/// [`is_external`] для запроса.
pub fn request_is_external(parts: &Parts, sec: &SecuritySettings) -> bool {
    is_external(client_ip(parts), parts.extensions.get::<UnverifiedForwarding>().is_some(), sec)
}

pub fn actor_from(parts: &Parts, user_id: Option<Uuid>) -> Actor {
    Actor {
        user_id,
        on_behalf_of: None,
        ip: client_ip(parts),
        user_agent: parts.headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).map(str::to_string),
    }
}

/// Токен API: `Authorization: Bearer pult_…`.
fn bearer(h: &HeaderMap) -> Option<&str> {
    h.get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| t.starts_with("pult_"))
}

/// Запрос пришёл с токеном API, а не из браузера: такой вход видит только то, что разрешено токену.
pub fn via_api_token(h: &HeaderMap) -> bool {
    bearer(h).is_some()
}

#[derive(sqlx::FromRow)]
struct TokenRow {
    id: Uuid,
    user_id: Uuid,
    permissions: Vec<String>,
    last_used_at: Option<OffsetDateTime>,
    person_id: Option<Uuid>,
    ad_account_id: Uuid,
    sam_account_name: String,
    display_name: Option<String>,
    locale: String,
}

/// Пользователь по токену API. Токен — только чтение (создавать заявки через API
/// нельзя) и только с правами, выданными токену и всё ещё есть у владельца.
async fn api_token_user(core: &Core, parts: &Parts, token: &str) -> Result<CurrentUser, ApiError> {
    if !is_safe(&parts.method) {
        return Err(ApiError::Forbidden);
    }
    let row: TokenRow = sqlx::query_as(
        "SELECT t.id, t.owner_user_id AS user_id, t.permissions, t.last_used_at, u.person_id, u.ad_account_id,
                a.sam_account_name, coalesce(a.display_name, a.sam_account_name) AS display_name, u.locale
           FROM api_tokens t JOIN users u ON u.id = t.owner_user_id JOIN ad_accounts a ON a.id = u.ad_account_id
          WHERE t.token_hash = $1 AND t.revoked_at IS NULL AND (t.expires_at IS NULL OR t.expires_at > now())
            AND u.is_active AND a.is_enabled AND NOT a.is_missing",
    )
    .bind(sha256(token.as_bytes()))
    .fetch_optional(&core.db)
    .await?
    .ok_or(ApiError::Unauthorized)?;
    if row.last_used_at.is_none_or(|t| OffsetDateTime::now_utc() - t > Duration::minutes(1)) {
        sqlx::query("UPDATE api_tokens SET last_used_at = now() WHERE id = $1").bind(row.id).execute(&core.db).await?;
    }
    let owner = load_grants(&core.db, row.user_id).await?;
    let mut grants = Grants::default();
    grants.roles.insert(crate::rbac::ROLE_EMPLOYEE.to_string());
    for p in row.permissions {
        let held = owner.is_superadmin() || owner.permissions.contains(&p);
        if held {
            grants.permissions.insert(p);
        }
    }
    Ok(CurrentUser {
        user_id: row.user_id,
        person_id: row.person_id,
        ad_account_id: row.ad_account_id,
        login: row.sam_account_name,
        display_name: row.display_name.unwrap_or_default(),
        locale: row.locale,
        grants,
        actor: actor_from(parts, Some(row.user_id)),
    })
}

fn is_safe(m: &Method) -> bool {
    matches!(*m, Method::GET | Method::HEAD | Method::OPTIONS)
}

impl<S> FromRequestParts<S> for CurrentUser
where
    S: Send + Sync,
    Core: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if let Some(u) = parts.extensions.get::<CurrentUser>() {
            return Ok(u.clone());
        }
        let core = Core::from_ref(state);
        if let Some(bearer) = bearer(&parts.headers) {
            let user = api_token_user(&core, parts, bearer).await?;
            parts.extensions.insert(user.clone());
            return Ok(user);
        }
        let token = cookie(&parts.headers, core.cookies.session_name()).ok_or(ApiError::Unauthorized)?;
        let row: Option<SessionRow> = sqlx::query_as(
            "SELECT s.user_id, s.csrf_token, s.last_seen_at, u.person_id, u.ad_account_id, a.sam_account_name,
                    coalesce(a.display_name, a.sam_account_name) AS display_name, u.locale
               FROM sessions s
               JOIN users u ON u.id = s.user_id
               JOIN ad_accounts a ON a.id = u.ad_account_id
              WHERE s.id = $1 AND s.idle_expires_at > now() AND s.absolute_expires_at > now()
                AND u.is_active AND a.is_enabled AND NOT a.is_missing",
        )
        .bind(sha256(token.as_bytes()))
        .fetch_optional(&core.db)
        .await?;
        let row = row.ok_or(ApiError::Unauthorized)?;
        if !is_safe(&parts.method) {
            let sent = parts.headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
            if sent.is_empty() || !constant_time_eq(sent, &row.csrf_token) {
                return Err(ApiError::Csrf);
            }
        }
        // Нет связи с доменом и политика требует приостановить сеансы.
        let health: AdHealth = load(&core.db).await?;
        if health.suspended {
            return Err(ApiError::Unavailable {
                code: "ad_unavailable",
                message: "Нет связи с доменом. Работа продолжится, когда связь восстановится.".into(),
            });
        }
        // Продлеваем сеанс не чаще раза в минуту — не пишем в БД на каждый запрос.
        if OffsetDateTime::now_utc() - row.last_seen_at > Duration::minutes(1) {
            let s: SecuritySettings = load(&core.db).await?;
            sqlx::query(
                "UPDATE sessions SET last_seen_at = now(),
                        idle_expires_at = least(now() + make_interval(hours => $2), absolute_expires_at)
                  WHERE id = $1",
            )
            .bind(sha256(token.as_bytes()))
            .bind(i32::try_from(s.session_idle_hours).unwrap_or(8))
            .execute(&core.db)
            .await?;
        }
        let grants = load_grants(&core.db, row.user_id).await?;
        let user = CurrentUser {
            user_id: row.user_id,
            person_id: row.person_id,
            ad_account_id: row.ad_account_id,
            login: row.sam_account_name,
            display_name: row.display_name.unwrap_or_default(),
            locale: row.locale,
            grants,
            actor: actor_from(parts, Some(row.user_id)),
        };
        parts.extensions.insert(user.clone());
        Ok(user)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn host_from_http1_and_http2() {
        let mut h = HeaderMap::new();
        h.insert(header::HOST, HeaderValue::from_static("pult.example.test"));
        assert_eq!(request_host(&h, &"/api/x".parse().unwrap()).as_deref(), Some("pult.example.test"), "HTTP/1.1");
        let uri: axum::http::Uri = "https://pult.example.test:8443/api/x".parse().unwrap();
        assert_eq!(request_host(&HeaderMap::new(), &uri).as_deref(), Some("pult.example.test:8443"), "HTTP/2");
        assert_eq!(request_host(&HeaderMap::new(), &"/api/x".parse().unwrap()), None);
    }

    #[test]
    fn cookie_parsing() {
        let mut h = HeaderMap::new();
        h.append(header::COOKIE, HeaderValue::from_static("a=1; __Host-pult_session=tok; b=2"));
        h.append(header::COOKIE, HeaderValue::from_static("pult_csrf=c"));
        assert_eq!(cookie(&h, "__Host-pult_session"), Some("tok"));
        assert_eq!(cookie(&h, "pult_csrf"), Some("c"));
        assert_eq!(cookie(&h, "pult_session"), None);
    }

    #[test]
    fn cookie_flags() {
        let [s, c] = CookiePolicy { secure: true }.issue("t", "x", 60);
        assert!(s.starts_with("__Host-pult_session=t;") && s.contains("HttpOnly") && s.contains("Secure"));
        assert!(s.contains("SameSite=Lax") && s.contains("Path=/") && !s.contains("Domain"));
        assert!(c.starts_with("__Host-pult_csrf=x;") && !c.contains("HttpOnly"));
        let [s, _] = CookiePolicy { secure: false }.issue("t", "x", 60);
        assert!(s.starts_with("pult_session=") && !s.contains("Secure"));
    }
}
