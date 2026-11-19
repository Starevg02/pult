//! Админка: каталог AD и связывание учёток со справочником.

use crate::{
    PeopleState, computers,
    domain::{self, DomainInput, DomainView},
    perm,
    sync::{self, Kind, LinkSettings, Summary},
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use pult_ad::DcPool;
use pult_core::l10n::tr;
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Secret,
    audit::{self, Entry, Source},
    settings::{self, AdHealth},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct SyncRun {
    pub id: Uuid,
    pub kind: String,
    pub controller: Option<String>,
    pub status: String,
    pub added: i32,
    pub updated: i32,
    pub disabled: i32,
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub started_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub finished_at: Option<time::OffsetDateTime>,
}

#[derive(Serialize, ToSchema)]
pub struct AdOverview {
    pub domain: Option<DomainView>,
    pub health: AdHealth,
    pub runs: Vec<SyncRun>,
    /// Учёток в зеркале: всего и активных.
    pub accounts: i64,
    pub active_accounts: i64,
}

async fn runs(core: &Core) -> ApiResult<Vec<SyncRun>> {
    let mut rows: Vec<SyncRun> = sqlx::query_as(
        "SELECT id, kind, controller, status, added, updated, disabled, error, started_at, finished_at
           FROM ad_sync_runs ORDER BY started_at DESC LIMIT 30",
    )
    .fetch_all(&core.db)
    .await?;
    for r in &mut rows {
        r.error = r.error.take().map(|e| tr(&e).into_owned());
    }
    Ok(rows)
}

/// Домен, состояние связи и журнал синхронизаций.
#[utoipa::path(get, path = "/api/admin/ad", tag = "admin", responses((status = 200, body = AdOverview)))]
pub(crate) async fn get_ad(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<AdOverview>> {
    user.require(&perm::AD_MANAGE)?;
    let d = domain::current(&core.db).await?;
    let (accounts, active_accounts): (i64, i64) =
        sqlx::query_as("SELECT count(*), count(*) FILTER (WHERE is_enabled AND NOT is_missing) FROM ad_accounts")
            .fetch_one(&core.db)
            .await?;
    Ok(Json(AdOverview {
        domain: d.as_ref().map(DomainView::from),
        health: settings::load(&core.db).await?,
        runs: runs(&core).await?,
        accounts,
        active_accounts,
    }))
}

/// Создать или изменить домен. Пароль служебной учётки — только на запись.
#[utoipa::path(put, path = "/api/admin/ad", tag = "admin", request_body = DomainInput,
    responses((status = 200, body = DomainView), (status = 409)))]
pub(crate) async fn put_ad(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<DomainInput>,
) -> ApiResult<Json<DomainView>> {
    user.require(&perm::AD_MANAGE)?;
    b.validate().map_err(|m| ApiError::bad_request("invalid_domain", m))?;
    let ca = match b.ca_cert_pem.as_deref() {
        Some(p) if p.trim().is_empty() => Some(None),
        Some(p) => {
            if !p.contains("-----BEGIN CERTIFICATE-----") || p.len() > 64 * 1024 {
                return Err(ApiError::bad_request("invalid_ca", "Сертификат CA — в формате PEM."));
            }
            Some(Some(p.to_string()))
        }
        None => None,
    };
    let sealed = match b.bind_password.as_deref().filter(|p| !p.is_empty()) {
        Some(p) => Some(core.secrets.encrypt(&Secret::new(p)).map_err(ApiError::internal)?),
        None => None,
    };
    let mut tx = core.db.begin().await?;
    let existing = domain::current(&core.db).await?;
    let mut writer_reset = false;
    let id = match &existing {
        None => {
            let sealed =
                sealed.ok_or_else(|| ApiError::bad_request("invalid_domain", "Укажите пароль служебной учётки."))?;
            sqlx::query_scalar(
                "INSERT INTO ad_domains (dns_name, netbios_name, base_dn, controllers, tls_mode, bind_dn, bind_secret, ca_cert_pem,
                                         user_filter, timeout_secs)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
            )
            .bind(domain::dns_from_base_dn(&b.base_dn))
            .bind(&b.netbios_name)
            .bind(&b.base_dn)
            .bind(&b.controllers)
            .bind(&b.tls_mode)
            .bind(b.bind_dn.trim())
            .bind(sealed)
            .bind(ca.clone().flatten())
            .bind(&b.user_filter)
            .bind(b.timeout_secs)
            .fetch_one(&mut *tx)
            .await?
        }
        Some(d) => {
            // Сохранённый пароль служебной учётки не уходит на новый адрес: иначе правка списка
            // контроллеров (или CA) позволила бы выманить его на свой LDAP-сервер.
            let ca_changed = ca.as_ref().is_some_and(|c| c.as_deref() != d.ca_cert_pem.as_deref());
            // Куда уходят пароли: сервер (контроллеры, TLS, CA). Учётка чтения тоже требует пароль заново.
            let server_changed = d.controllers != b.controllers || d.tls_mode != b.tls_mode || ca_changed;
            let endpoint_changed = server_changed || d.bind_dn != b.bind_dn.trim();
            writer_reset = server_changed;
            if endpoint_changed && sealed.is_none() {
                return Err(ApiError::bad_request(
                    "secret_required",
                    "Контроллеры, режим TLS, CA или служебная учётка изменились — введите пароль служебной учётки заново.",
                ));
            }
            let n = sqlx::query(
                "UPDATE ad_domains SET netbios_name = $3, base_dn = $4, controllers = $5, tls_mode = $6, bind_dn = $7,
                        bind_secret = coalesce($8, bind_secret),
                        ca_cert_pem = CASE WHEN $9 THEN $10 ELSE ca_cert_pem END,
                        user_filter = $11, timeout_secs = $12, version = version + 1,
                        dns_name = $13,
                        writer_secret = CASE WHEN $14 THEN NULL ELSE writer_secret END
                  WHERE id = $1 AND version = $2",
            )
            .bind(d.id)
            .bind(b.version)
            .bind(&b.netbios_name)
            .bind(&b.base_dn)
            .bind(&b.controllers)
            .bind(&b.tls_mode)
            .bind(b.bind_dn.trim())
            .bind(sealed.as_deref())
            .bind(ca.is_some())
            .bind(ca.clone().flatten())
            .bind(&b.user_filter)
            .bind(b.timeout_secs)
            .bind(domain::dns_from_base_dn(&b.base_dn))
            // Пароль учётки записи (каталог AD) тоже не уходит на новый сервер: его вводят заново.
            .bind(server_changed)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if n == 0 {
                return Err(ApiError::Conflict { current: json!(DomainView::from(d)) });
            }
            d.id
        }
    };
    let before = existing.as_ref().map(|d| json!(DomainView::from(d))).unwrap_or(json!({}));
    let after = json!({
        "netbios_name": b.netbios_name, "base_dn": b.base_dn, "controllers": b.controllers, "tls_mode": b.tls_mode,
        "bind_dn": b.bind_dn, "user_filter": b.user_filter, "timeout_secs": b.timeout_secs,
    });
    let mut details = audit::diff(&before, &after);
    if let Some(o) = details.as_object_mut() {
        if b.bind_password.as_deref().is_some_and(|p| !p.is_empty()) {
            o.insert("bind_password".into(), json!("изменён"));
        }
        if ca.is_some() {
            o.insert("ca_cert".into(), json!("изменён"));
        }
        if writer_reset {
            o.insert("writer_password".into(), json!("сброшен"));
        }
    }
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("ad.domain.update", Source::Portal).object("ad_domain", id).details(details),
    )
    .await?;
    tx.commit().await?;
    let d = domain::current(&core.db).await?.ok_or(ApiError::NotFound)?;
    Ok(Json(DomainView::from(&d)))
}

#[derive(Serialize, ToSchema)]
pub struct DcCheckView {
    pub controller: String,
    pub ok: bool,
    pub error: Option<String>,
    /// Что с этим делать.
    pub hint: Option<String>,
}

impl From<pult_ad::DcCheck> for DcCheckView {
    fn from(c: pult_ad::DcCheck) -> Self {
        let t = |s: Option<String>| s.map(|s| tr(&s).into_owned());
        Self { controller: c.controller, ok: c.ok, error: t(c.error), hint: t(c.hint) }
    }
}

/// Проверить подключение к каждому контроллеру (TLS + вход служебной учёткой).
#[utoipa::path(post, path = "/api/admin/ad/test", tag = "admin", responses((status = 200, body = Vec<DcCheckView>)))]
pub(crate) async fn test_ad(
    State(core): State<Core>,
    State(people): State<PeopleState>,
    user: CurrentUser,
) -> ApiResult<Json<Vec<DcCheckView>>> {
    user.require(&perm::AD_MANAGE)?;
    let Some((_, pool)) = people.ad.pool(&core).await? else { return Err(ApiError::NotFound) };
    let pool: &DcPool = &pool;
    Ok(Json(pool.check_controllers().await.into_iter().map(DcCheckView::from).collect()))
}

#[derive(Deserialize, ToSchema)]
pub struct SyncRequest {
    pub kind: Kind,
    /// Подтверждение: синхронизация может отключить много учёток (иначе — отказ `ad_sync_mass_disable`).
    #[serde(default)]
    pub allow_mass_disable: bool,
}

/// Запустить синхронизацию сейчас.
#[utoipa::path(post, path = "/api/admin/ad/sync", tag = "admin", request_body = SyncRequest, responses((status = 200, body = Summary)))]
pub(crate) async fn run_sync(
    State(core): State<Core>,
    State(people): State<PeopleState>,
    user: CurrentUser,
    Json(b): Json<SyncRequest>,
) -> ApiResult<Json<Summary>> {
    user.require(&perm::AD_MANAGE)?;
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("ad.sync.requested", Source::Portal)
            .details(json!({"kind": b.kind, "allow_mass_disable": b.allow_mass_disable})),
    )
    .await?;
    let summary = sync::run(&core, &people.ad, b.kind, b.allow_mass_disable).await?;
    // Полная синхронизация по кнопке обновляет и компьютеры; их сбой учётки не отменяет.
    if b.kind == Kind::Full
        && let Err(e) = computers::run(&core, &people.ad).await
    {
        tracing::warn!(error = %e, "компьютеры домена не обновлены");
    }
    Ok(Json(summary))
}

// ------------------------------------------------------------------ Связывание

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct LinkCandidate {
    pub id: Uuid,
    pub reason: String,
    pub ad_login: String,
    pub ad_name: Option<String>,
    pub ad_mail: Option<String>,
    pub person_id: Uuid,
    pub person_name: String,
    pub person_department: Option<String>,
    pub person_email: Option<String>,
}

/// Спорные пары «учётка AD — сотрудник» на подтверждение.
#[utoipa::path(get, path = "/api/admin/people/link-candidates", tag = "admin", responses((status = 200, body = Vec<LinkCandidate>)))]
pub(crate) async fn list_candidates(
    State(core): State<Core>,
    user: CurrentUser,
) -> ApiResult<Json<Vec<LinkCandidate>>> {
    user.require(&perm::LINK_MANAGE)?;
    let mut rows: Vec<LinkCandidate> = sqlx::query_as(
        "SELECT c.id, c.reason, a.sam_account_name AS ad_login, a.display_name AS ad_name, a.mail AS ad_mail,
                    p.id AS person_id, concat_ws(' ', p.last_name, p.first_name, p.middle_name) AS person_name,
                    d.name AS person_department, p.email AS person_email
               FROM person_link_candidates c
               JOIN ad_accounts a ON a.id = c.ad_account_id
               JOIN persons p ON p.id = c.person_id
               LEFT JOIN departments d ON d.id = p.department_id
              WHERE c.status = 'pending'
              ORDER BY a.sam_account_name",
    )
    .fetch_all(&core.db)
    .await?;
    for r in &mut rows {
        r.reason = tr(&r.reason).into_owned();
    }
    Ok(Json(rows))
}

#[derive(Deserialize, ToSchema)]
pub struct CandidateDecision {
    pub confirm: bool,
}

/// Подтвердить (связать) или отклонить пару.
#[utoipa::path(post, path = "/api/admin/people/link-candidates/{id}", tag = "admin", params(("id" = Uuid, Path)),
    request_body = CandidateDecision, responses((status = 204)))]
pub(crate) async fn decide_candidate(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<CandidateDecision>,
) -> ApiResult<StatusCode> {
    user.require(&perm::LINK_MANAGE)?;
    let mut tx = core.db.begin().await?;
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "UPDATE person_link_candidates SET status = $2, decided_by = $3, decided_at = now()
          WHERE id = $1 AND status = 'pending' RETURNING ad_account_id, person_id",
    )
    .bind(id)
    .bind(if b.confirm { "confirmed" } else { "rejected" })
    .bind(user.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (account, person) = row.ok_or(ApiError::NotFound)?;
    if b.confirm {
        let n = sqlx::query(
            "UPDATE persons SET ad_account_id = $2, version = version + 1 WHERE id = $1 AND ad_account_id IS NULL",
        )
        .bind(person)
        .bind(account)
        .execute(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                ApiError::bad_request("already_linked", "Эта учётка уже связана с другим сотрудником.")
            }
            _ => e.into(),
        })?
        .rows_affected();
        if n == 0 {
            return Err(ApiError::bad_request("already_linked", "Сотрудник уже связан с другой учёткой."));
        }
        // Остальные пары этой учётки и этого сотрудника больше не нужны.
        sqlx::query(
            "UPDATE person_link_candidates SET status = 'rejected', decided_by = $3, decided_at = now()
              WHERE status = 'pending' AND (ad_account_id = $1 OR person_id = $2)",
        )
        .bind(account)
        .bind(person)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
        sync::sync_user_person(&mut tx, account).await?;
    }
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new(if b.confirm { "person.link" } else { "person.link_rejected" }, Source::Portal)
            .object("person", person)
            .details(json!({"ad_account": account, "by": "admin"})),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Настройки связывания.
#[utoipa::path(get, path = "/api/admin/people/link-settings", tag = "admin", responses((status = 200, body = LinkSettings)))]
pub(crate) async fn get_link_settings(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<LinkSettings>> {
    user.require(&perm::LINK_MANAGE)?;
    Ok(Json(settings::load(&core.db).await?))
}

#[utoipa::path(put, path = "/api/admin/people/link-settings", tag = "admin", request_body = LinkSettings,
    responses((status = 200, body = LinkSettings)))]
pub(crate) async fn put_link_settings(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<LinkSettings>,
) -> ApiResult<Json<LinkSettings>> {
    user.require(&perm::LINK_MANAGE)?;
    let mut tx = core.db.begin().await?;
    let before: LinkSettings = settings::load(&mut *tx).await?;
    settings::save(&mut *tx, &b, Some(user.user_id)).await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("settings.update", Source::Portal)
            .object("settings", "people_link")
            .details(audit::diff(&json!(before), &json!(b))),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(b))
}
