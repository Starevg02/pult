//! Инвентарь из NetBox: устройства (сетевое оборудование, серверы, СХД) и виртуальные машины.
//! Портал только читает NetBox по API-токену и показывает найденное в «Оборудовании»; в каталог
//! объект попадает после проверки инженером, а принятая карточка потом не обновляется сама.

use crate::{
    objects::MANAGE_ASSETS,
    tickets::{self, FormField},
};
use axum::{
    Json,
    extract::{FromRef, Path, Query, State},
    http::StatusCode,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Secret,
    audit::{self, Entry, Source},
    l10n,
    settings::{self, SettingsGroup},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::time::Duration;
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub const SYNC_JOB: &str = "helpdesk.netbox_sync";

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(get_netbox, put_netbox))
        .routes(routes!(sync_netbox))
        .routes(routes!(list_external))
        .routes(routes!(accept_external))
        .routes(routes!(reject_external))
        .routes(routes!(restore_external))
}

// ------------------------------------------------------------------ Настройки

/// Подключение к NetBox. Токен — отдельно и зашифрованным.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct NetboxSettings {
    /// Синхронизировать раз в сутки.
    pub enabled: bool,
    /// Адрес NetBox, например `https://netbox.example.local`.
    pub base_url: String,
    /// Брать и виртуальные машины.
    pub import_vms: bool,
}

impl Default for NetboxSettings {
    fn default() -> Self {
        Self { enabled: false, base_url: String::new(), import_vms: true }
    }
}

impl SettingsGroup for NetboxSettings {
    const KEY: &'static str = "netbox";
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct NetboxSecret {
    sealed: Option<String>,
}

impl SettingsGroup for NetboxSecret {
    const KEY: &'static str = "netbox_secret";
}

/// Итог последней синхронизации.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct NetboxState {
    /// RFC 3339.
    pub last_sync_at: Option<String>,
    pub last_error: Option<String>,
    /// Получено объектов.
    pub fetched: i64,
    /// Из них новых.
    pub added: i64,
    /// Пропало из NetBox с прошлого раза.
    pub missing: i64,
}

impl SettingsGroup for NetboxState {
    const KEY: &'static str = "netbox_state";
}

#[derive(Serialize, ToSchema)]
pub struct NetboxView {
    pub settings: NetboxSettings,
    pub has_token: bool,
    pub state: NetboxState,
}

#[derive(Deserialize, ToSchema)]
pub struct NetboxUpdate {
    pub settings: NetboxSettings,
    /// Новый токен API; пустая строка — удалить; не передан — оставить прежний.
    pub token: Option<String>,
}

const URL_HINT: &str = "Адрес NetBox — http:// или https:// без логина, пароля и параметров.";

fn base_url(raw: &str) -> Result<reqwest::Url, &'static str> {
    let u = reqwest::Url::parse(raw.trim()).map_err(|_| URL_HINT)?;
    let ok = matches!(u.scheme(), "http" | "https")
        && u.host_str().is_some()
        && u.username().is_empty()
        && u.password().is_none()
        && u.query().is_none()
        && u.fragment().is_none();
    if !ok {
        return Err(URL_HINT);
    }
    if crate::egress::literal_forbidden(&u) {
        Err("Служебный адрес link-local не подходит для NetBox.")
    } else {
        Ok(u)
    }
}

async fn view(core: &Core) -> ApiResult<NetboxView> {
    let settings: NetboxSettings = settings::load(&core.db).await?;
    let secret: NetboxSecret = settings::load(&core.db).await?;
    let state: NetboxState = settings::load(&core.db).await?;
    Ok(NetboxView { settings, has_token: secret.sealed.is_some(), state })
}

/// Подключение к NetBox. Только суперадминистратор: токен открывает чтение всей инфраструктуры.
#[utoipa::path(get, path = "/api/admin/inventory/netbox", tag = "admin", responses((status = 200, body = NetboxView)))]
async fn get_netbox(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<NetboxView>> {
    user.require_superadmin()?;
    Ok(Json(view(&core).await?))
}

#[utoipa::path(put, path = "/api/admin/inventory/netbox", tag = "admin", request_body = NetboxUpdate,
    responses((status = 200, body = NetboxView), (status = 400, body = pult_core::error::ErrorBody)))]
async fn put_netbox(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<NetboxUpdate>,
) -> ApiResult<Json<NetboxView>> {
    user.require_superadmin()?;
    let bad = |m: &str| ApiError::bad_request("invalid_settings", m.to_owned());
    let mut new = b.settings;
    new.base_url = new.base_url.trim().trim_end_matches('/').to_owned();
    if new.enabled || !new.base_url.is_empty() {
        base_url(&new.base_url).map_err(bad)?;
    }
    if b.token.as_deref().is_some_and(|k| k.len() > 4096 || k.chars().any(char::is_control)) {
        return Err(bad("Токен — одна строка до 4096 символов."));
    }
    let mut tx = core.db.begin().await?;
    let before: NetboxSettings = settings::load(&mut *tx).await?;
    let stored: NetboxSecret = settings::load(&mut *tx).await?;
    let url = |s: &str| s.trim().trim_end_matches('/').to_lowercase();
    // Сохранённый токен не уходит на другой адрес.
    if stored.sealed.is_some() && b.token.is_none() && url(&before.base_url) != url(&new.base_url) {
        return Err(ApiError::bad_request("key_required", "Адрес изменился — введите токен заново."));
    }
    let token_after = match &b.token {
        Some(k) => !k.is_empty(),
        None => stored.sealed.is_some(),
    };
    if new.enabled && !token_after {
        return Err(bad("Укажите токен API NetBox."));
    }
    settings::save(&mut *tx, &new, Some(user.user_id)).await?;
    let mut details = audit::diff(&json!(before), &json!(new));
    if let Some(k) = &b.token {
        let sealed = if k.is_empty() {
            None
        } else {
            let raw = core.secrets.encrypt(&Secret::new(k.clone())).map_err(ApiError::internal)?;
            Some(STANDARD.encode(raw))
        };
        settings::save(&mut *tx, &NetboxSecret { sealed }, Some(user.user_id)).await?;
        if let Some(o) = details.as_object_mut() {
            o.insert("token".into(), json!(if k.is_empty() { "удалён" } else { "изменён" }));
        }
    }
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("settings.update", Source::Portal).object("settings", "netbox").details(details),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(view(&core).await?))
}

// ------------------------------------------------------------------ Синхронизация

/// Объект NetBox после разбора.
#[derive(Debug, PartialEq)]
struct Found {
    external_id: String,
    kind: &'static str,
    name: String,
    manufacturer: Option<String>,
    model: Option<String>,
    serial: Option<String>,
    ip: Option<String>,
    site: Option<String>,
    role: Option<String>,
    platform: Option<String>,
}

fn text_at(v: &Value, path: &[&str], max: usize) -> Option<String> {
    let mut x = v;
    for k in path {
        x = x.get(k)?;
    }
    x.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().filter(|c| !c.is_control()).take(max).collect())
}

/// Устройство или ВМ из ответа API NetBox (3.x и 4.x). Без id и имени — пропускается.
fn parse(kind: &'static str, v: &Value) -> Option<Found> {
    let id = v.get("id")?.as_i64()?;
    let name = text_at(v, &["name"], 300).or_else(|| text_at(v, &["display"], 300))?;
    Some(Found {
        external_id: format!("{kind}:{id}"),
        kind,
        name,
        manufacturer: text_at(v, &["device_type", "manufacturer", "name"], 200),
        model: text_at(v, &["device_type", "model"], 200),
        serial: text_at(v, &["serial"], 200),
        // «10.0.0.5/24» → «10.0.0.5».
        ip: text_at(v, &["primary_ip", "address"], 100).map(|a| a.split('/').next().unwrap_or_default().to_owned()),
        site: text_at(v, &["site", "name"], 200),
        role: text_at(v, &["role", "name"], 200).or_else(|| text_at(v, &["device_role", "name"], 200)),
        platform: text_at(v, &["platform", "name"], 200),
    })
}

/// Сколько объектов берётся за синхронизацию и сколько байт — в одном ответе.
const MAX_OBJECTS: usize = 50_000;
const MAX_PAGE_BYTES: usize = 16 * 1024 * 1024;

async fn read_limited(mut r: reqwest::Response) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    while let Some(chunk) = r.chunk().await.map_err(|e| e.to_string())? {
        if buf.len() + chunk.len() > MAX_PAGE_BYTES {
            return Err(l10n::tr("Ответ NetBox слишком большой.").into_owned());
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Все страницы списка. Ссылка «next» принимается только на тот же сервер.
async fn fetch_all(
    client: &reqwest::Client,
    base: &reqwest::Url,
    token: &str,
    path: &str,
    kind: &'static str,
    out: &mut Vec<Found>,
) -> Result<(), String> {
    let mut url = reqwest::Url::parse(&format!("{}/{path}?limit=250", base.as_str().trim_end_matches('/')))
        .map_err(|e| e.to_string())?;
    loop {
        let r = client
            .get(url.clone())
            .header("Authorization", format!("Token {token}"))
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = r.status();
        if !status.is_success() {
            let code = status.as_u16();
            return Err(l10n::tr(&format!("NetBox ответил {code}.")).into_owned());
        }
        let body: Value = serde_json::from_slice(&read_limited(r).await?)
            .map_err(|_| l10n::tr("NetBox вернул не JSON.").into_owned())?;
        let results = body
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| l10n::tr("NetBox вернул не JSON.").into_owned())?;
        out.extend(results.iter().filter_map(|v| parse(kind, v)));
        if out.len() > MAX_OBJECTS {
            return Err(l10n::tr("В NetBox больше 50 000 объектов: синхронизация остановлена.").into_owned());
        }
        let Some(next) = body.get("next").and_then(Value::as_str) else {
            return Ok(());
        };
        let next = reqwest::Url::parse(next).map_err(|e| e.to_string())?;
        if next.scheme() != base.scheme()
            || next.host_str() != base.host_str()
            || next.port_or_known_default() != base.port_or_known_default()
        {
            return Err(l10n::tr("NetBox прислал ссылку на другой сервер.").into_owned());
        }
        url = next;
    }
}

/// Синхронизация с NetBox сохранёнными настройками. Итог (и ошибка) — в состоянии.
pub async fn sync(core: &Core) -> ApiResult<NetboxState> {
    let s: NetboxSettings = settings::load(&core.db).await?;
    let secret: NetboxSecret = settings::load(&core.db).await?;
    let started = OffsetDateTime::now_utc();
    let result = run(core, &s, &secret).await;
    let mut state: NetboxState = settings::load(&core.db).await?;
    state.last_sync_at = started.format(&time::format_description::well_known::Rfc3339).ok();
    match result {
        Ok((fetched, added, missing)) => {
            state.last_error = None;
            state.fetched = fetched;
            state.added = added;
            state.missing = missing;
        }
        Err(e) => {
            tracing::warn!(error = %e, "netbox sync failed");
            state.last_error = Some(e);
        }
    }
    settings::save(&core.db, &state, None).await?;
    Ok(state)
}

async fn run(core: &Core, s: &NetboxSettings, secret: &NetboxSecret) -> Result<(i64, i64, i64), String> {
    let base = base_url(&s.base_url).map_err(|m| l10n::tr(m).into_owned())?;
    let token = match &secret.sealed {
        Some(b) => {
            let raw = STANDARD.decode(b).map_err(|e| e.to_string())?;
            core.secrets.decrypt(&raw).map_err(|e| e.to_string())?
        }
        None => return Err(l10n::tr("Укажите токен API NetBox.").into_owned()),
    };
    let client = crate::egress::client()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| e.to_string())?;
    let mut found = Vec::new();
    fetch_all(&client, &base, token.expose(), "api/dcim/devices/", "device", &mut found).await?;
    if s.import_vms {
        fetch_all(&client, &base, token.expose(), "api/virtualization/virtual-machines/", "vm", &mut found).await?;
    }
    let started: OffsetDateTime =
        sqlx::query_scalar("SELECT now()").fetch_one(&core.db).await.map_err(|e| e.to_string())?;
    let mut tx = core.db.begin().await.map_err(|e| e.to_string())?;
    let mut added = 0;
    for f in &found {
        let inserted: bool = sqlx::query_scalar(
            "INSERT INTO external_inventory (source, external_id, kind, name, manufacturer, model, serial_number, ip, site, role, platform)
             VALUES ('netbox', $1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (source, external_id) DO UPDATE SET kind = EXCLUDED.kind, name = EXCLUDED.name,
                 manufacturer = EXCLUDED.manufacturer, model = EXCLUDED.model, serial_number = EXCLUDED.serial_number,
                 ip = EXCLUDED.ip, site = EXCLUDED.site, role = EXCLUDED.role, platform = EXCLUDED.platform,
                 seen_at = now(), missing = false
             RETURNING (xmax = 0)",
        )
        .bind(&f.external_id)
        .bind(f.kind)
        .bind(&f.name)
        .bind(&f.manufacturer)
        .bind(&f.model)
        .bind(&f.serial)
        .bind(&f.ip)
        .bind(&f.site)
        .bind(&f.role)
        .bind(&f.platform)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        added += i64::from(inserted);
    }
    let missing = sqlx::query(
        "UPDATE external_inventory SET missing = true WHERE source = 'netbox' AND seen_at < $1 AND NOT missing",
    )
    .bind(started)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?
    .rows_affected();
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok((found.len() as i64, added, i64::try_from(missing).unwrap_or(i64::MAX)))
}

#[utoipa::path(post, path = "/api/admin/inventory/netbox/sync", tag = "admin", responses((status = 200, body = NetboxState)))]
async fn sync_netbox(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<NetboxState>> {
    user.require_superadmin()?;
    let state = sync(&core).await?;
    audit::record(&core.db, &user.actor, Entry::new("inventory.netbox.sync", Source::Portal)).await?;
    Ok(Json(state))
}

// ------------------------------------------------------------------ Проверка инженером

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ExternalItem {
    pub id: Uuid,
    pub source: String,
    /// `device` или `vm`.
    pub kind: String,
    pub name: String,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub serial_number: Option<String>,
    pub ip: Option<String>,
    pub site: Option<String>,
    pub role: Option<String>,
    pub platform: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub seen_at: OffsetDateTime,
}

#[derive(Deserialize, IntoParams)]
pub struct ExternalQuery {
    /// Отклонённые вместо ожидающих.
    #[serde(default)]
    pub rejected: bool,
}

#[utoipa::path(get, path = "/api/helpdesk/inventory/external", tag = "helpdesk", params(ExternalQuery),
    responses((status = 200, body = Vec<ExternalItem>)))]
async fn list_external(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<ExternalQuery>,
) -> ApiResult<Json<Vec<ExternalItem>>> {
    user.require(&MANAGE_ASSETS)?;
    Ok(Json(
        sqlx::query_as(
            "SELECT id, source, kind, name, manufacturer, model, serial_number, ip, site, role, platform, seen_at
               FROM external_inventory
              WHERE NOT missing AND (CASE WHEN $1 THEN decision = 'rejected' ELSE decision IS NULL END)
              ORDER BY kind, lower(name) LIMIT 1000",
        )
        .bind(q.rejected)
        .fetch_all(&core.db)
        .await?,
    ))
}

#[derive(Deserialize, ToSchema)]
pub struct ExternalAcceptInput {
    pub type_id: Uuid,
    /// Название карточки; пусто — имя из NetBox.
    pub name: Option<String>,
    pub inventory_number: Option<String>,
    pub site_id: Option<Uuid>,
    pub responsible_person_id: Option<Uuid>,
    /// Состояние; по умолчанию `in_use`.
    pub status: Option<String>,
}

/// Принять в каталог: карточка выбранного типа; модель, IP и платформа — в доп. поля `model`, `ip`
/// и `os`, если они у типа есть; серийный номер — из NetBox.
#[utoipa::path(post, path = "/api/helpdesk/inventory/external/{id}/accept", tag = "helpdesk", params(("id" = Uuid, Path)),
    request_body = ExternalAcceptInput, responses((status = 201, body = Object)))]
async fn accept_external(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ExternalAcceptInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&MANAGE_ASSETS)?;
    let bad = |m: &str| ApiError::bad_request("invalid_asset", m.to_owned());
    let status = b.status.clone().unwrap_or_else(|| "in_use".into());
    let mut tx = core.db.begin().await?;
    type Row = (String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>);
    // Решение — первым: второй одновременный запрос получит 404.
    let row: Option<Row> = sqlx::query_as(
        "UPDATE external_inventory SET decision = 'accepted', decided_by = $2, decided_at = now()
          WHERE id = $1 AND decision IS NULL AND NOT missing
          RETURNING name, nullif(concat_ws(' ', manufacturer, model), ''), serial_number, ip, platform, external_id",
    )
    .bind(id)
    .bind(user.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (ext_name, model, serial, ip, platform, external_id) = row.ok_or(ApiError::NotFound)?;
    let fields: Option<Value> = sqlx::query_scalar("SELECT fields FROM asset_types WHERE id = $1 AND is_active")
        .bind(b.type_id)
        .fetch_optional(&mut *tx)
        .await?;
    let fields: Vec<FormField> =
        serde_json::from_value(fields.ok_or_else(|| bad("Тип не найден."))?).unwrap_or_default();
    if let Some(l) = crate::cmdb::lifecycle_of(&mut *tx, b.type_id).await? {
        crate::cmdb::check_status(&l, None, &status)?;
    }
    if let Some(site) = b.site_id {
        let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sites WHERE id = $1 AND is_active)")
            .bind(site)
            .fetch_one(&mut *tx)
            .await?;
        if !ok {
            return Err(bad("Площадка не найдена."));
        }
    }
    if let Some(p) = b.responsible_person_id {
        let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM persons WHERE id = $1 AND NOT is_archived)")
            .bind(p)
            .fetch_one(&mut *tx)
            .await?;
        if !ok {
            return Err(bad("Сотрудник не найден."));
        }
    }
    let mut attrs = Map::new();
    for (key, val) in [("model", model), ("ip", ip), ("os", platform)] {
        if let Some(val) = val
            && fields.iter().any(|f| f.key == key)
        {
            attrs.insert(key.into(), Value::String(val));
        }
    }
    let attrs = tickets::validate_answers(&fields, &Value::Object(attrs)).map_err(|m| bad(&m))?;
    let name = b.name.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(&ext_name);
    let inventory = b.inventory_number.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let asset: Uuid = sqlx::query_scalar(
        "INSERT INTO assets (type_id, name, inventory_number, serial_number, site_id, responsible_person_id, status, attributes)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(b.type_id)
    .bind(name.chars().take(200).collect::<String>())
    .bind(inventory.map(|s| s.chars().take(200).collect::<String>()))
    .bind(serial)
    .bind(b.site_id)
    .bind(b.responsible_person_id)
    .bind(&status)
    .bind(attrs)
    .fetch_one(&mut *tx)
    .await
    .map_err(crate::objects::dup)?;
    sqlx::query(
        "INSERT INTO asset_events (asset_id, kind, to_person_id, to_name, comment, actor_id)
         VALUES ($1, 'created', $2,
                 (SELECT nullif(trim(concat_ws(' ', last_name, first_name, middle_name)), '') FROM persons WHERE id = $2),
                 'NetBox', $3)",
    )
    .bind(asset)
    .bind(b.responsible_person_id)
    .bind(user.user_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE external_inventory SET asset_id = $2 WHERE id = $1")
        .bind(id)
        .bind(asset)
        .execute(&mut *tx)
        .await?;
    let details = json!({"source": "netbox", "external_id": external_id});
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("asset.discovery.accept", Source::Portal).object("asset", asset).details(details),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"id": asset}))))
}

async fn decide(core: &Core, user: &CurrentUser, id: Uuid, reject: bool) -> ApiResult<StatusCode> {
    user.require(&MANAGE_ASSETS)?;
    let sql = if reject {
        "UPDATE external_inventory SET decision = 'rejected', decided_by = $2, decided_at = now()
          WHERE id = $1 AND decision IS NULL AND NOT missing"
    } else {
        "UPDATE external_inventory SET decision = NULL, decided_by = $2, decided_at = now()
          WHERE id = $1 AND decision = 'rejected'"
    };
    let n = sqlx::query(sql).bind(id).bind(user.user_id).execute(&core.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    let action = if reject { "asset.discovery.reject" } else { "asset.discovery.restore" };
    audit::record(&core.db, &user.actor, Entry::new(action, Source::Portal).object("external_inventory", id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Не вносить в каталог: объект больше не предлагается.
#[utoipa::path(post, path = "/api/helpdesk/inventory/external/{id}/reject", tag = "helpdesk", params(("id" = Uuid, Path)),
    responses((status = 204)))]
async fn reject_external(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    decide(&core, &user, id, true).await
}

/// Вернуть отклонённое в список ожидающих проверки.
#[utoipa::path(post, path = "/api/helpdesk/inventory/external/{id}/restore", tag = "helpdesk", params(("id" = Uuid, Path)),
    responses((status = 204)))]
async fn restore_external(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    decide(&core, &user, id, false).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netbox_objects() {
        let d = json!({"id": 7, "name": "core-sw-01", "device_type": {"model": "C9300", "manufacturer": {"name": "Cisco"}},
                       "serial": " FOC123 ", "primary_ip": {"address": "10.0.0.2/24"}, "site": {"name": "HQ"},
                       "role": {"name": "Access switch"}, "platform": null});
        let f = parse("device", &d).unwrap();
        assert_eq!(f.external_id, "device:7");
        assert_eq!(f.model.as_deref(), Some("C9300"));
        assert_eq!(f.manufacturer.as_deref(), Some("Cisco"));
        assert_eq!(f.serial.as_deref(), Some("FOC123"));
        assert_eq!(f.ip.as_deref(), Some("10.0.0.2"));
        assert_eq!(f.role.as_deref(), Some("Access switch"));
        assert_eq!(f.platform, None);
        // NetBox 3.x: device_role; без имени — display.
        let d = json!({"id": 8, "name": null, "display": "Unnamed", "device_role": {"name": "Server"}});
        let f = parse("device", &d).unwrap();
        assert_eq!((f.name.as_str(), f.role.as_deref()), ("Unnamed", Some("Server")));
        assert!(parse("vm", &json!({"name": "no-id"})).is_none());
        assert!(base_url("https://netbox.local").is_ok());
        assert!(base_url("https://u:p@netbox.local").is_err());
        assert!(base_url("http://169.254.169.254").is_err());
        assert!(base_url("ftp://netbox.local").is_err());
    }
}
