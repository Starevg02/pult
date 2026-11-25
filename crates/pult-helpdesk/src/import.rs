//! Оборудование в реестр не вручную по одному: из CSV и из агента удалённой поддержки.
//!
//! CSV загружает инженер — это ручное действие: новые карточки создаются сразу, а строки с уже
//! известным инвентарным номером пропускаются (существующие карточки меняют только вручную).
//! Сначала — проверка без записи (`dry_run`), потом импорт.
//!
//! ПК с агентом попадают в реестр только после проверки инженером: «ожидают проверки» — активные
//! ПК без решения. Инженер принимает (выбирает тип, создаётся карточка) или отклоняет.

use crate::{
    objects::MANAGE_ASSETS,
    perm,
    tickets::{self, FormField},
};
use axum::{
    Json,
    extract::{FromRef, Path, Query, State},
    http::StatusCode,
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser,
    audit::{self, Entry, Source},
    l10n::{Lang, tr, tr_to},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(import_csv))
        .routes(routes!(list_discovered))
        .routes(routes!(accept_discovered))
        .routes(routes!(reject_discovered))
        .routes(routes!(restore_discovered))
}

// ------------------------------------------------------------------ CSV

/// Строки CSV. Разделитель — `;` или `,` (какого больше в первой строке), кавычки по RFC 4180
/// (`""` внутри кавычек — одна кавычка, перевод строки внутри кавычек — часть значения),
/// BOM в начале и пустые строки пропускаются.
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let first = text.lines().next().unwrap_or("");
    let delim = if first.matches(';').count() > first.matches(',').count() { ';' } else { ',' };
    let (mut rows, mut row, mut cell) = (Vec::new(), Vec::new(), String::new());
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut end_row = |row: &mut Vec<String>, cell: &mut String| {
        row.push(std::mem::take(cell));
        let r = std::mem::take(row);
        if !(r.len() == 1 && r[0].trim().is_empty()) {
            rows.push(r);
        }
    };
    while let Some(c) = chars.next() {
        // NUL и прочие управляющие символы база не примет — отбрасываем; табуляция и перевод строки остаются.
        if c.is_control() && !matches!(c, '\t' | '\n' | '\r') {
            continue;
        }
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cell.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                cell.push(c);
            }
        } else if c == '"' && cell.trim().is_empty() {
            cell.clear();
            quoted = true;
        } else if c == delim {
            row.push(std::mem::take(&mut cell));
        } else if c == '\n' || c == '\r' {
            if c == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
            }
            end_row(&mut row, &mut cell);
        } else {
            cell.push(c);
        }
    }
    if !cell.is_empty() || !row.is_empty() {
        end_row(&mut row, &mut cell);
    }
    rows
}

/// Колонки файла: ключ, подпись в интерфейсе (по-русски; английская и китайская — из каталога
/// переводов) и английские синонимы.
const COLUMNS: &[(&str, &str, &[&str])] = &[
    ("name", "Название", &[]),
    ("type", "Тип", &[]),
    ("inventory_number", "Инв. номер", &["inventory", "inventory number"]),
    ("serial_number", "Серийный номер", &["serial"]),
    ("site", "Площадка", &[]),
    ("room", "Кабинет", &[]),
    ("responsible", "Ответственный", &[]),
    ("status", "Состояние", &["state"]),
    ("notes", "Заметки", &[]),
];

fn column_of(header: &str) -> Option<&'static str> {
    let h = header.trim().to_lowercase();
    COLUMNS
        .iter()
        .find(|(key, label, alias)| {
            *key == h
                || alias.contains(&h.as_str())
                || [Lang::Ru, Lang::En, Lang::Zh].iter().any(|l| tr_to(*l, label).to_lowercase() == h)
        })
        .map(|(key, _, _)| *key)
}

/// Не больше строк за один импорт.
const MAX_ROWS: usize = 5000;

#[derive(Deserialize, ToSchema)]
pub struct AssetImportInput {
    /// Содержимое файла CSV (UTF-8).
    pub csv: String,
    /// Только проверить: ничего не записывать.
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Serialize, ToSchema)]
pub struct AssetImportRowError {
    /// Номер записи в файле, считая заголовок первой.
    pub row: usize,
    pub message: String,
}

#[derive(Serialize, ToSchema)]
pub struct AssetImportResult {
    pub dry_run: bool,
    /// Создано (при проверке — будет создано) карточек.
    pub created: usize,
    /// Пропущено: инвентарный номер уже есть в реестре или повторяется в файле.
    pub skipped: usize,
    pub errors: Vec<AssetImportRowError>,
}

/// Поиск по имени без учёта регистра: тип, площадка.
async fn names(core: &Core, sql: &str) -> ApiResult<HashMap<String, (Uuid, Value)>> {
    let rows: Vec<(Uuid, String, Value)> = sqlx::query_as(sql).fetch_all(&core.db).await?;
    Ok(rows.into_iter().map(|(id, n, f)| (n.trim().to_lowercase(), (id, f))).collect())
}

/// Импорт оборудования из CSV. Обязательные колонки — название и тип; ответственный — по e-mail или
/// логину AD; остальные колонки, совпавшие с полями типа (ключ или подпись), — в доп. поля.
#[utoipa::path(post, path = "/api/helpdesk/assets/import", tag = "helpdesk", request_body = AssetImportInput,
    responses((status = 200, body = AssetImportResult)))]
async fn import_csv(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<AssetImportInput>,
) -> ApiResult<Json<AssetImportResult>> {
    user.require(&MANAGE_ASSETS)?;
    let bad = |m: &str| ApiError::bad_request("invalid_import", m.to_string());
    let rows = parse_csv(&b.csv);
    let Some((header, data)) = rows.split_first() else { return Err(bad("Файл пустой.")) };
    if data.len() > MAX_ROWS {
        return Err(bad("Не больше 5000 строк за один импорт."));
    }
    let cols: Vec<Option<&str>> = header.iter().map(|h| column_of(h)).collect();
    if !cols.contains(&Some("name")) || !cols.contains(&Some("type")) {
        return Err(bad("Нужны колонки name (название) и type (тип)."));
    }
    let mut keys = HashSet::new();
    if cols.iter().flatten().any(|k| !keys.insert(*k)) {
        return Err(bad("Две колонки означают одно и то же (например, «name» и «Название») — оставьте одну."));
    }
    let types = names(&core, "SELECT id, name, fields FROM asset_types WHERE is_active").await?;
    let sites = names(&core, "SELECT id, name, '{}'::jsonb FROM sites WHERE is_active").await?;
    let mut tx = core.db.begin().await?;
    let mut res = AssetImportResult { dry_run: b.dry_run, created: 0, skipped: 0, errors: Vec::new() };
    let mut seen = HashSet::new();
    for (i, row) in data.iter().enumerate() {
        let n = i + 2;
        let mut err = |m: String| res.errors.push(AssetImportRowError { row: n, message: m });
        let mut v: HashMap<&str, String> = HashMap::new();
        let mut extra: Vec<(&str, String)> = Vec::new();
        for (j, cell) in row.iter().enumerate() {
            let cell = cell.trim().to_string();
            match cols.get(j).copied().flatten() {
                Some(k) => {
                    v.insert(k, cell);
                }
                None => {
                    if let Some(h) = header.get(j) {
                        extra.push((h.as_str(), cell));
                    }
                }
            }
        }
        let get = |k: &str| v.get(k).map(String::as_str).filter(|s| !s.is_empty());
        let Some(name) = get("name") else {
            err(tr("Нет названия.").into_owned());
            continue;
        };
        let Some((type_id, fields)) = get("type").and_then(|t| types.get(&t.to_lowercase())) else {
            err(tr(&format!("Тип «{}» не найден.", get("type").unwrap_or(""))).into_owned());
            continue;
        };
        let site = match get("site") {
            None => None,
            Some(s) => match sites.get(&s.to_lowercase()) {
                Some((id, _)) => Some(*id),
                None => {
                    err(tr(&format!("Площадка «{s}» не найдена.")).into_owned());
                    continue;
                }
            },
        };
        let responsible: Option<Uuid> = match get("responsible") {
            None => None,
            Some(r) => {
                let found: Option<Uuid> = sqlx::query_scalar(
                    "SELECT p.id FROM persons p LEFT JOIN ad_accounts a ON a.id = p.ad_account_id
                      WHERE NOT p.is_archived AND (lower(p.email) = lower($1) OR lower(a.sam_account_name) = lower($1))
                      ORDER BY p.id LIMIT 1",
                )
                .bind(r)
                .fetch_optional(&mut *tx)
                .await?;
                match found {
                    Some(p) => Some(p),
                    None => {
                        err(tr(&format!("Сотрудник «{r}» не найден (нужен e-mail или логин AD).")).into_owned());
                        continue;
                    }
                }
            }
        };
        let status = get("status")
            .map(str::to_lowercase)
            .unwrap_or_else(|| if responsible.is_some() { "in_use".into() } else { "in_stock".into() });
        if !matches!(status.as_str(), "in_use" | "in_stock" | "repair" | "retired") {
            err(tr(&format!("Состояние «{status}»: нужно in_use, in_stock, repair или retired.")).into_owned());
            continue;
        }
        let allowed: bool = sqlx::query_scalar("SELECT (lifecycle -> 'statuses') ? $2 FROM asset_types WHERE id = $1")
            .bind(type_id)
            .bind(&status)
            .fetch_one(&mut *tx)
            .await?;
        if !allowed {
            err(tr(&format!("Состояния «{status}» нет в жизненном цикле этого типа.")).into_owned());
            continue;
        }
        // Доп. поля: колонка совпала с ключом или подписью поля типа.
        let fields: Vec<FormField> = serde_json::from_value(fields.clone()).unwrap_or_default();
        let mut attrs = Map::new();
        for (h, cell) in &extra {
            let h = h.trim().to_lowercase();
            if let Some(f) = fields.iter().find(|f| f.key.to_lowercase() == h || f.label.to_lowercase() == h)
                && !cell.is_empty()
            {
                // Да/нет в таблице — словами или цифрой; непонятное значение отклонит проверка полей.
                let v = match (f.kind.as_str(), cell.to_lowercase().as_str()) {
                    ("bool", "1" | "true" | "yes" | "да" | "是") => Value::Bool(true),
                    ("bool", "0" | "false" | "no" | "нет" | "否") => Value::Bool(false),
                    _ => Value::String(cell.clone()),
                };
                attrs.insert(f.key.clone(), v);
            }
        }
        let attrs = match tickets::validate_answers(&fields, &Value::Object(attrs)) {
            Ok(a) => a,
            Err(m) => {
                err(tr(&m).into_owned());
                continue;
            }
        };
        let inventory = get("inventory_number").map(|s| s.chars().take(200).collect::<String>());
        if let Some(inv) = &inventory {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM assets WHERE lower(inventory_number) = lower($1))")
                    .bind(inv)
                    .fetch_one(&mut *tx)
                    .await?;
            if exists || !seen.insert(inv.to_lowercase()) {
                res.skipped += 1;
                continue;
            }
        }
        let clip = |k: &str| get(k).map(|s| s.chars().take(200).collect::<String>());
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO assets (type_id, name, inventory_number, serial_number, site_id, room, responsible_person_id, status, notes, attributes)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
        )
        .bind(type_id)
        .bind(name.chars().take(200).collect::<String>())
        .bind(&inventory)
        .bind(clip("serial_number"))
        .bind(site)
        .bind(clip("room"))
        .bind(responsible)
        .bind(&status)
        .bind(clip("notes"))
        .bind(attrs)
        .fetch_one(&mut *tx)
        .await
        // Тот же номер только что внёс параллельный импорт.
        .map_err(crate::objects::dup)?;
        sqlx::query(
            "INSERT INTO asset_events (asset_id, kind, to_person_id, to_name, comment, actor_id)
             VALUES ($1, 'created', $2,
                     (SELECT nullif(trim(concat_ws(' ', last_name, first_name, middle_name)), '') FROM persons WHERE id = $2),
                     'CSV', $3)",
        )
        .bind(id)
        .bind(responsible)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
        res.created += 1;
    }
    if b.dry_run {
        tx.rollback().await?;
    } else {
        let details = json!({"created": res.created, "skipped": res.skipped, "errors": res.errors.len()});
        audit::record(&mut *tx, &user.actor, Entry::new("asset.import", Source::Portal).details(details)).await?;
        tx.commit().await?;
    }
    Ok(Json(res))
}

// ------------------------------------------------------------------ ПК из агента

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct DiscoveredDevice {
    pub device_id: Uuid,
    pub hostname: String,
    pub fqdn: Option<String>,
    pub os: Option<String>,
    pub os_version: Option<String>,
    pub ip_addrs: Vec<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_seen_at: Option<OffsetDateTime>,
    /// Производитель и модель из сведений агента — при приёме идут в карточку.
    pub model: Option<String>,
    pub serial_number: Option<String>,
}

#[derive(Deserialize, IntoParams)]
pub struct DiscoveredQuery {
    /// `true` — отклонённые ПК (их можно вернуть в список).
    #[serde(default)]
    pub rejected: bool,
}

/// ПК с агентом, которых нет в реестре: по умолчанию — ожидающие проверки, с `rejected=true` — отклонённые.
#[utoipa::path(get, path = "/api/helpdesk/assets/discovered", tag = "helpdesk", params(DiscoveredQuery),
    responses((status = 200, body = Vec<DiscoveredDevice>)))]
async fn list_discovered(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<DiscoveredQuery>,
) -> ApiResult<Json<Vec<DiscoveredDevice>>> {
    user.require(&perm::WORK)?;
    let rows = sqlx::query_as(
        "SELECT d.id AS device_id, d.hostname, d.fqdn, d.os, d.os_version,
                array(SELECT host(x) FROM unnest(d.ip_addrs) x) AS ip_addrs, d.last_seen_at,
                nullif(concat_ws(' ', i.manufacturer, i.model), '') AS model, i.serial_number
           FROM devices d
           LEFT JOIN asset_discovery r ON r.device_id = d.id
           LEFT JOIN device_inventory i ON i.device_id = d.id
          WHERE d.is_active AND CASE WHEN $1 THEN r.decision = 'rejected' ELSE r.device_id IS NULL END
          ORDER BY lower(d.hostname) LIMIT 1000",
    )
    .bind(q.rejected)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize, ToSchema)]
pub struct AcceptInput {
    pub type_id: Uuid,
    /// Название карточки; пусто — имя ПК.
    pub name: Option<String>,
    pub inventory_number: Option<String>,
    pub site_id: Option<Uuid>,
    pub responsible_person_id: Option<Uuid>,
}

/// Принять ПК в реестр: создаётся карточка выбранного типа; ОС, IP и модель — в доп. поля, если они у типа
/// есть, серийный номер — из сведений агента.
#[utoipa::path(post, path = "/api/helpdesk/assets/discovered/{id}/accept", tag = "helpdesk", params(("id" = Uuid, Path)),
    request_body = AcceptInput, responses((status = 201, body = Object)))]
async fn accept_discovered(
    State(core): State<Core>,
    user: CurrentUser,
    Path(device): Path<Uuid>,
    Json(b): Json<AcceptInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&MANAGE_ASSETS)?;
    let bad = |m: &str| ApiError::bad_request("invalid_asset", m.to_string());
    let mut tx = core.db.begin().await?;
    // Решение — первым: второй одновременный запрос по тому же ПК дождётся первого и получит 404.
    let claimed = sqlx::query(
        "INSERT INTO asset_discovery (device_id, decision, decided_by)
         SELECT id, 'accepted', $2 FROM devices WHERE id = $1 AND is_active
         ON CONFLICT (device_id) DO NOTHING",
    )
    .bind(device)
    .bind(user.user_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if claimed == 0 {
        return Err(ApiError::NotFound);
    }
    type DeviceFacts = (String, String, Option<String>, Option<String>, Option<String>);
    let (hostname, os, ip, model, serial): DeviceFacts = sqlx::query_as(
        "SELECT d.hostname, concat_ws(' ', d.os, d.os_version), host(coalesce(d.ip_addrs[1], d.last_ip)),
                nullif(concat_ws(' ', i.manufacturer, i.model), ''), i.serial_number
           FROM devices d LEFT JOIN device_inventory i ON i.device_id = d.id WHERE d.id = $1",
    )
    .bind(device)
    .fetch_one(&mut *tx)
    .await?;
    let fields: Option<Value> = sqlx::query_scalar("SELECT fields FROM asset_types WHERE id = $1 AND is_active")
        .bind(b.type_id)
        .fetch_optional(&mut *tx)
        .await?;
    let fields: Vec<FormField> =
        serde_json::from_value(fields.ok_or_else(|| bad("Тип не найден."))?).unwrap_or_default();
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
    for (key, val) in [("os", Some(os)), ("ip", ip), ("model", model)] {
        if let Some(val) = val.filter(|v| !v.trim().is_empty())
            && fields.iter().any(|f| f.key == key)
        {
            attrs.insert(key.into(), Value::String(val));
        }
    }
    let attrs = tickets::validate_answers(&fields, &Value::Object(attrs)).map_err(|m| bad(&m))?;
    let name = b.name.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(&hostname);
    let inventory = b.inventory_number.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let status = if b.responsible_person_id.is_some() { "in_use" } else { "in_stock" };
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO assets (type_id, name, inventory_number, serial_number, site_id, responsible_person_id, status, attributes)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(b.type_id)
    .bind(name.chars().take(200).collect::<String>())
    .bind(inventory.map(|s| s.chars().take(200).collect::<String>()))
    .bind(serial)
    .bind(b.site_id)
    .bind(b.responsible_person_id)
    .bind(status)
    .bind(attrs)
    .fetch_one(&mut *tx)
    .await
    .map_err(crate::objects::dup)?;
    sqlx::query(
        "INSERT INTO asset_events (asset_id, kind, to_person_id, to_name, comment, actor_id)
         VALUES ($1, 'created', $2,
                 (SELECT nullif(trim(concat_ws(' ', last_name, first_name, middle_name)), '') FROM persons WHERE id = $2),
                 $3, $4)",
    )
    .bind(id)
    .bind(b.responsible_person_id)
    .bind(&hostname)
    .bind(user.user_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE asset_discovery SET asset_id = $2 WHERE device_id = $1")
        .bind(device)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let details = json!({"device": device, "hostname": hostname});
    let entry = Entry::new("asset.discovery.accept", Source::Portal).object("asset", id).details(details);
    audit::record(&mut *tx, &user.actor, entry).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

/// Не вносить ПК в реестр: он больше не предлагается.
#[utoipa::path(post, path = "/api/helpdesk/assets/discovered/{id}/reject", tag = "helpdesk", params(("id" = Uuid, Path)),
    responses((status = 204)))]
async fn reject_discovered(
    State(core): State<Core>,
    user: CurrentUser,
    Path(device): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require(&MANAGE_ASSETS)?;
    let n = sqlx::query(
        "INSERT INTO asset_discovery (device_id, decision, decided_by)
         SELECT id, 'rejected', $2 FROM devices WHERE id = $1 AND is_active ON CONFLICT (device_id) DO NOTHING",
    )
    .bind(device)
    .bind(user.user_id)
    .execute(&core.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("asset.discovery.reject", Source::Portal).object("device", device))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Вернуть отклонённый ПК в список ожидающих проверки.
#[utoipa::path(post, path = "/api/helpdesk/assets/discovered/{id}/restore", tag = "helpdesk", params(("id" = Uuid, Path)),
    responses((status = 204)))]
async fn restore_discovered(
    State(core): State<Core>,
    user: CurrentUser,
    Path(device): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require(&MANAGE_ASSETS)?;
    let n = sqlx::query("DELETE FROM asset_discovery WHERE device_id = $1 AND decision = 'rejected'")
        .bind(device)
        .execute(&core.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("asset.discovery.restore", Source::Portal).object("device", device),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::{column_of, parse_csv};

    #[test]
    fn csv_delimiters_quotes_and_bom() {
        let rows = parse_csv("\u{feff}name;type;notes\r\nМФУ;Принтер;\"a; \"\"b\"\"\nc\"\r\n\r\nНоутбук;Ноутбук;\n");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], ["name", "type", "notes"]);
        assert_eq!(rows[1], ["МФУ", "Принтер", "a; \"b\"\nc"]);
        assert_eq!(rows[2], ["Ноутбук", "Ноутбук", ""]);
        let rows = parse_csv("name,type\nx,y");
        assert_eq!(rows, [vec!["name", "type"], vec!["x", "y"]]);
    }

    #[test]
    fn headers_in_any_language() {
        assert_eq!(column_of(" Название "), Some("name"));
        assert_eq!(column_of("Инв. номер"), Some("inventory_number"));
        assert_eq!(column_of("负责人"), Some("responsible"));
        assert_eq!(column_of("model"), None);
    }
}
