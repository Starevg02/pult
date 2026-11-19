//! Мастер первого запуска и сертификат портала из браузера.
//!
//! Пока домен не настроен, войти нельзя, поэтому настройка домена в админке недоступна. Мастер
//! закрывает этот разрыв: доступ — по одноразовому коду из журнала сервера (его видит только тот,
//! кто управляет сервером), домен проверяется до сохранения, первым суперадминистратором становится
//! тот, кто вошёл паролем AD в конце мастера. После этого маршруты мастера отвечают 404.

use crate::{api::ApiState, tls::TlsInfo};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, request::Parts},
    response::{IntoResponse, Response},
};
use pult_ad::{AdConfig, AdError, DcPool, TlsMode};
use pult_core::{
    ApiError, ApiResult, CurrentUser, Secret,
    audit::{self, Entry, Source},
    crypto::{constant_time_eq, random_token},
    rbac::{ROLE_SUPERADMIN, perm},
    session,
};
use pult_people::{admin::DcCheckView, auth::LoginRequest, domain};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

const SETUP_HEADER: &str = "x-setup-token";

/// Одноразовый код мастера: живёт в памяти процесса, при перезапуске меняется.
#[derive(Clone)]
pub struct SetupState {
    code: Arc<Secret>,
    /// Два завершения мастера не идут одновременно.
    finishing: Arc<tokio::sync::Mutex<()>>,
}

impl SetupState {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { code: Arc::new(Secret::new(random_token(16)?)), finishing: Arc::default() })
    }

    pub fn code(&self) -> &str {
        self.code.expose()
    }

    fn accepts(&self, headers: &HeaderMap) -> bool {
        headers.get(SETUP_HEADER).and_then(|v| v.to_str().ok()).is_some_and(|c| constant_time_eq(c.trim(), self.code()))
    }
}

/// Нужен ли мастер: `(нужен, домен уже настроен)`.
pub async fn needed(st: &ApiState) -> Result<(bool, bool), sqlx::Error> {
    let (has_domain, has_admin): (bool, bool) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM ad_domains WHERE enabled),
                EXISTS (SELECT 1 FROM user_roles ur JOIN roles r ON r.id = ur.role_id WHERE r.code = $1)",
    )
    .bind(ROLE_SUPERADMIN)
    .fetch_one(&st.core.db)
    .await?;
    // Домен есть, администратора нет, но он задан в конфиге — станет им при первом входе.
    Ok((!has_domain || (!has_admin && st.people.initial_superadmin.is_none()), has_domain))
}

/// Нужен ли мастер и пропустить ли шаг домена.
#[derive(Serialize, ToSchema)]
pub struct SetupStatus {
    pub needed: bool,
    pub has_domain: bool,
}

/// Состояние мастера первого запуска (без входа).
#[utoipa::path(get, path = "/api/setup/status", tag = "setup", responses((status = 200, body = SetupStatus)))]
async fn status(State(st): State<ApiState>) -> ApiResult<Json<SetupStatus>> {
    let (needed, has_domain) = needed(&st).await?;
    Ok(Json(SetupStatus { needed, has_domain }))
}

/// Мастер ещё нужен и код верный — иначе 404 или 403.
async fn guard(st: &ApiState, headers: &HeaderMap) -> ApiResult<bool> {
    let (is_needed, has_domain) = needed(st).await?;
    if !is_needed {
        return Err(ApiError::NotFound);
    }
    if !st.setup.accepts(headers) {
        return Err(ApiError::Forbidden);
    }
    Ok(has_domain)
}

/// Проверить код настройки.
#[utoipa::path(post, path = "/api/setup/verify", tag = "setup",
    responses((status = 204), (status = 403, body = pult_core::error::ErrorBody)))]
async fn verify(State(st): State<ApiState>, headers: HeaderMap) -> ApiResult<StatusCode> {
    guard(&st, &headers).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Домен в мастере: то, что знает администратор, остальное выводится.
#[derive(Deserialize, ToSchema)]
pub struct SetupDomain {
    /// DNS-имя домена, например `corp.example.local`.
    pub dns_name: String,
    /// Контроллеры без схемы: `dc1.corp.example.local` или с портом.
    pub controllers: Vec<String>,
    /// `ldaps` или `starttls`.
    pub tls_mode: String,
    /// Служебная учётка с правом чтения: `svc-pult@corp.example.local` или DN.
    pub bind_dn: String,
    #[schema(value_type = String, format = Password)]
    pub bind_password: Secret,
    /// Корневой сертификат (PEM), если контроллеры подписаны внутренним CA.
    pub ca_cert_pem: Option<String>,
}

/// `corp.example.local` → `DC=corp,DC=example,DC=local`.
fn base_dn(dns: &str) -> Option<String> {
    let dns = dns.trim().trim_end_matches('.').to_lowercase();
    let ok = !dns.is_empty()
        && dns.split('.').all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    ok.then(|| dns.split('.').map(|l| format!("DC={l}")).collect::<Vec<_>>().join(","))
}

fn to_config(d: &SetupDomain) -> ApiResult<AdConfig> {
    let bad = |m: &str| ApiError::bad_request("invalid_domain", m);
    let base_dn = base_dn(&d.dns_name).ok_or_else(|| bad("Укажите DNS-имя домена, например corp.example.local."))?;
    if d.bind_dn.trim().is_empty() || d.bind_password.is_empty() {
        return Err(bad("Укажите служебную учётку и её пароль."));
    }
    let tls = match d.tls_mode.as_str() {
        "ldaps" => TlsMode::Ldaps,
        "starttls" => TlsMode::Starttls,
        _ => return Err(bad("Режим TLS — ldaps или starttls.")),
    };
    Ok(AdConfig {
        controllers: d.controllers.iter().map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect(),
        tls,
        base_dn,
        bind_dn: d.bind_dn.trim().to_string(),
        bind_password: d.bind_password.clone(),
        ca_cert: None,
        ca_cert_pem: d.ca_cert_pem.clone().filter(|p| !p.trim().is_empty()),
        user_filter: "(&(objectCategory=person)(objectClass=user))".into(),
        timeout_secs: 5,
    })
}

fn pool(cfg: AdConfig) -> ApiResult<DcPool> {
    DcPool::new(cfg).map_err(|e| ApiError::bad_request("invalid_domain", e.to_string()))
}

/// Проверить подключение к каждому контроллеру, ничего не сохраняя.
#[utoipa::path(post, path = "/api/setup/test", tag = "setup", request_body = SetupDomain,
    responses((status = 200, body = Vec<DcCheckView>)))]
async fn test(
    State(st): State<ApiState>,
    headers: HeaderMap,
    Json(d): Json<SetupDomain>,
) -> ApiResult<Json<Vec<DcCheckView>>> {
    guard(&st, &headers).await?;
    let checks = pool(to_config(&d)?)?.check_controllers().await;
    Ok(Json(checks.into_iter().map(DcCheckView::from).collect()))
}

/// Завершение мастера.
#[derive(Deserialize, ToSchema)]
pub struct SetupComplete {
    /// Домен; не нужен, если он уже настроен.
    pub domain: Option<SetupDomain>,
    /// Логин будущего суперадминистратора.
    pub admin_login: String,
    #[schema(value_type = String, format = Password)]
    pub admin_password: Secret,
}

pub(crate) struct RequestParts(Parts);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for RequestParts {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(parts.clone()))
    }
}

/// Сохранить домен, войти администратором и запустить первую синхронизацию.
/// Успех — 204 и cookie сеанса. Неверный домен или пароль — ничего не сохраняется.
#[utoipa::path(post, path = "/api/setup/complete", tag = "setup", request_body = SetupComplete,
    responses((status = 204, description = "Настройка завершена, cookie сеанса"),
              (status = 400, body = pult_core::error::ErrorBody),
              (status = 403, body = pult_core::error::ErrorBody)))]
async fn complete(
    State(st): State<ApiState>,
    RequestParts(parts): RequestParts,
    Json(b): Json<SetupComplete>,
) -> Result<Response, ApiError> {
    let setup = st.setup.clone();
    let _one_at_a_time = setup.finishing.lock().await;
    let has_domain = guard(&st, &parts.headers).await?;
    // Домен, который сохраним, если администратор войдёт.
    let new_domain = match (has_domain, &b.domain) {
        (true, _) => None,
        (false, Some(d)) => Some(to_config(d)?),
        (false, None) => return Err(ApiError::bad_request("invalid_domain", "Укажите настройки домена.")),
    };
    let authed = match &new_domain {
        Some(cfg) => {
            let p = pool(cfg.clone())?;
            if !p.check_controllers().await.iter().any(|c| c.ok) {
                return Err(ApiError::bad_request(
                    "ad_check_failed",
                    "Ни один контроллер не принял служебную учётку — проверьте подключение.",
                ));
            }
            p.authenticate(&b.admin_login, &b.admin_password).await
        }
        None => {
            let (_, p) = st.people.ad.pool(&st.core).await?.ok_or(ApiError::NotFound)?;
            p.authenticate(&b.admin_login, &b.admin_password).await
        }
    };
    let authed = match authed {
        Ok(u) => u,
        Err(AdError::Bind(f)) => return Err(ApiError::bad_request("admin_rejected", f.user_message())),
        Err(AdError::Unavailable(_)) => {
            return Err(ApiError::Unavailable {
                code: "ad_unavailable", message: "Нет связи с доменом.".into()
            });
        }
        Err(e) => return Err(ApiError::internal(e)),
    };
    let actor = session::actor_from(&parts, None);
    if let Some(cfg) = &new_domain {
        domain::store(&st.core, cfg, None, false).await?;
        audit::record(
            &st.core.db,
            &actor,
            Entry::new("ad.domain.update", Source::Portal).details(
                json!({"base_dn": cfg.base_dn, "controllers": cfg.controllers, "bind_dn": cfg.bind_dn, "setup": true}),
            ),
        )
        .await?;
    }
    // Обычный вход: заводит пользователя и, раз суперадминистраторов нет, выдаёт ему роль.
    let req = LoginRequest { login: b.admin_login.clone(), password: b.admin_password.clone(), code: None };
    let resp = pult_people::auth::login(&st.core, &st.people.ad, Some(&authed.sam_account_name), &parts, req).await?;
    remember_portal(&st, &parts).await;
    for kind in [pult_people::jobs::SYNC_FULL, pult_people::jobs::COMPUTERS] {
        if let Err(e) = pult_jobs::enqueue(&st.core.db, &pult_jobs::NewJob::new(kind, json!({}))).await {
            tracing::warn!(error = %e, job = kind, "первая синхронизация не поставлена — пойдёт по расписанию");
        }
    }
    tracing::info!(user = %authed.sam_account_name, "мастер первого запуска завершён");
    Ok(resp.into_response())
}

/// Адрес, по которому открыт мастер, — база для ссылок в письмах.
async fn remember_portal(st: &ApiState, parts: &Parts) {
    use pult_helpdesk::settings::HelpdeskSettings;
    let Some(host) = session::request_host(&parts.headers, &parts.uri) else { return };
    let Ok(mut cfg) = pult_core::settings::load::<HelpdeskSettings>(&st.core.db).await else { return };
    cfg.portal_url = format!("https://{host}");
    if let Err(e) = pult_core::settings::save(&st.core.db, &cfg, None).await {
        tracing::warn!(error = %e, "адрес портала не сохранён");
    }
}

// ------------------------------------------------------------------ Сертификат портала

/// Сертификат портала.
#[derive(Serialize, ToSchema)]
pub struct TlsStatus {
    /// `true` — сертификатом управляет портал, его можно заменить здесь.
    pub managed: bool,
    pub info: Option<TlsInfo>,
}

fn managed_status(st: &ApiState) -> ApiResult<TlsStatus> {
    Ok(match &st.tls {
        Some(m) => TlsStatus { managed: true, info: Some(m.info().map_err(ApiError::internal)?) },
        None => TlsStatus { managed: false, info: None },
    })
}

#[utoipa::path(get, path = "/api/admin/tls", tag = "admin", responses((status = 200, body = TlsStatus)))]
async fn get_tls(State(st): State<ApiState>, user: CurrentUser) -> ApiResult<Json<TlsStatus>> {
    user.require(&perm::SETTINGS_MANAGE)?;
    Ok(Json(managed_status(&st)?))
}

/// Новый сертификат портала.
#[derive(Deserialize, ToSchema)]
pub struct TlsUpload {
    /// Сертификат и промежуточные, PEM.
    pub cert_pem: String,
    /// Закрытый ключ, PEM.
    #[schema(value_type = String, format = Password)]
    pub key_pem: Secret,
}

/// Заменить сертификат портала. Применяется сразу, без перезапуска. Только суперадминистратор.
#[utoipa::path(put, path = "/api/admin/tls", tag = "admin", request_body = TlsUpload,
    responses((status = 200, body = TlsStatus), (status = 400, body = pult_core::error::ErrorBody)))]
async fn put_tls(
    State(st): State<ApiState>,
    user: CurrentUser,
    Json(b): Json<TlsUpload>,
) -> ApiResult<Json<TlsStatus>> {
    user.require_superadmin()?;
    let Some(managed) = &st.tls else {
        return Err(ApiError::bad_request("tls_external", "HTTPS настроен вне портала — сертификат меняется там же."));
    };
    let info = managed
        .replace(b.cert_pem.as_bytes(), b.key_pem.expose().as_bytes())
        .await
        .map_err(|e| ApiError::bad_request("invalid_certificate", e.to_string()))?;
    audit::record(
        &st.core.db,
        &user.actor,
        Entry::new("settings.update", Source::Portal)
            .object("settings", "tls")
            .details(json!({"subject": info.subject, "issuer": info.issuer, "not_after": info.not_after})),
    )
    .await?;
    Ok(Json(TlsStatus { managed: true, info: Some(info) }))
}

/// Последняя резервная копия.
#[utoipa::path(get, path = "/api/admin/backup", tag = "admin", responses((status = 200, body = crate::backup::BackupStatus)))]
async fn get_backup(State(st): State<ApiState>, user: CurrentUser) -> ApiResult<Json<crate::backup::BackupStatus>> {
    user.require(&perm::SETTINGS_MANAGE)?;
    Ok(Json(pult_core::settings::load(&st.core.db).await?))
}

/// Язык установки, выбранный в мастере.
#[derive(Deserialize, ToSchema)]
pub struct InstallLanguage {
    /// `ru`, `en` или `zh`.
    pub locale: String,
}

/// Итог смены языка установки.
#[derive(Serialize, ToSchema)]
pub struct InstallLanguageResult {
    pub locale: String,
    /// Сколько стартовых записей (категории, шаблоны, типы оборудования) переведено.
    pub renamed: u64,
}

/// Язык установки: язык по умолчанию и стартовые данные helpdesk на этом языке.
/// Записи, которые администратор уже переименовал, не трогаются. Только суперадминистратор.
#[utoipa::path(put, path = "/api/admin/org/language", tag = "admin", request_body = InstallLanguage,
    responses((status = 200, body = InstallLanguageResult), (status = 400, body = pult_core::error::ErrorBody)))]
async fn put_install_language(
    State(st): State<ApiState>,
    user: CurrentUser,
    Json(b): Json<InstallLanguage>,
) -> ApiResult<Json<InstallLanguageResult>> {
    user.require_superadmin()?;
    if !pult_core::l10n::Lang::is_code(&b.locale) {
        return Err(ApiError::bad_request("invalid_settings", "Язык — ru, en или zh."));
    }
    let lang = pult_core::l10n::Lang::parse(&b.locale);
    // Язык и перевод стартовых данных — одной транзакцией: либо всё, либо ничего.
    let mut tx = st.core.db.begin().await?;
    let before: pult_core::settings::LocaleSettings = pult_core::settings::load(&mut *tx).await?;
    let after = pult_core::settings::LocaleSettings { default_locale: lang.code().to_string() };
    pult_core::settings::save(&mut *tx, &after, Some(user.user_id)).await?;
    let renamed = pult_helpdesk::starter::localize(&mut tx, lang).await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("settings.update", Source::Portal).object("settings", "locale").details(json!({
            "default_locale": {"old": before.default_locale, "new": after.default_locale},
            "starter_renamed": renamed,
        })),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(InstallLanguageResult { locale: after.default_locale, renamed }))
}

pub fn routes() -> OpenApiRouter<ApiState> {
    OpenApiRouter::new()
        .routes(routes!(put_install_language))
        .routes(routes!(get_backup))
        .routes(routes!(status))
        .routes(routes!(verify))
        .routes(routes!(test))
        .routes(routes!(complete))
        .routes(routes!(get_tls, put_tls))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_dn_from_dns_name() {
        assert_eq!(base_dn(" Corp.Example.LOCAL. ").as_deref(), Some("DC=corp,DC=example,DC=local"));
        assert_eq!(base_dn("corp").as_deref(), Some("DC=corp"));
        for bad in ["", "corp..local", "corp,DC=evil", "cn=a b"] {
            assert_eq!(base_dn(bad), None, "{bad}");
        }
    }

    #[test]
    fn code_is_checked_whole() {
        let s = SetupState::new().unwrap();
        let with = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(SETUP_HEADER, v.parse().unwrap());
            h
        };
        assert!(s.accepts(&with(s.code())));
        assert!(s.accepts(&with(&format!(" {} ", s.code()))), "пробелы при вставке — не ошибка");
        assert!(!s.accepts(&with("wrong")));
        assert!(!s.accepts(&with(&s.code()[1..])));
        assert!(!s.accepts(&HeaderMap::new()));
    }
}
