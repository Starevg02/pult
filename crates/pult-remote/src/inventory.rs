//! Оборудование и программы ПК из отчёта агента: приём, карточка ПК, поиск «где стоит программа».
//!
//! Агент присылает сведения при запуске и когда портал попросит (`want_inventory`: сведений нет
//! или им больше суток). Каждый раз они целиком заменяют прежние — история не копится.

use crate::perm;
use axum::{
    Json,
    extract::{Query, State},
};
use pult_api_types::{DeviceDisk, HardwareInventory, InstalledSoftware};
use pult_core::{ApiError, ApiResult, Core, CurrentUser};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction, types::Json as Jsonb};
use std::collections::HashSet;
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

const MAX_DISKS: usize = 64;
const MAX_SOFTWARE: usize = 5000;
/// Сведения старше — агент получает `want_inventory`.
const FRESH_HOURS: i32 = 24;
const MAX_HITS: i64 = 500;

/// Невидимые символы направления и нулевой ширины: в имени программы ими можно перевернуть текст
/// (U+202E перед «exe.txt» показывает «txt.exe») или сделать два одинаковых на вид имени.
fn invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{061C}' | '\u{FEFF}')
}

/// Строка от агента: без управляющих и невидимых символов, не длиннее `n`.
fn clip(s: Option<&str>, n: usize) -> Option<String> {
    s.map(|v| v.trim().chars().filter(|c| !c.is_control() && !invisible(*c)).take(n).collect::<String>())
        .filter(|v| !v.is_empty())
}

/// Заглушки производителя вместо модели и серийного номера («To be filled by O.E.M.», «0000…»).
pub fn placeholder(s: &str) -> bool {
    let l = s.trim().to_ascii_lowercase();
    l.chars().all(|c| matches!(c, '0' | ' ' | '-' | '.'))
        || matches!(
            l.as_str(),
            "to be filled by o.e.m."
                | "default string"
                | "system serial number"
                | "system product name"
                | "system manufacturer"
                | "chassis serial number"
                | "not specified"
                | "not applicable"
                | "none"
                | "n/a"
                | "na"
                | "oem"
                | "o.e.m."
                | "123456789"
                | "invalid"
                | "unknown"
        )
}

/// Проверенные сведения: строки обрезаны, заглушки убраны, программы без повторов.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Clean {
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub serial_number: Option<String>,
    pub bios_version: Option<String>,
    pub cpu: Option<String>,
    pub memory_mb: Option<i64>,
    pub disks: Vec<DeviceDisk>,
    pub software: Vec<InstalledSoftware>,
}

pub fn clean(r: &HardwareInventory) -> Clean {
    let text = |s: &Option<String>, n: usize| clip(s.as_deref(), n).filter(|v| !placeholder(v));
    let mut seen = HashSet::new();
    let software = r
        .software
        .iter()
        .filter_map(|s| {
            let name = clip(Some(&s.name), 300)?;
            let version = clip(s.version.as_deref(), 100);
            seen.insert((name.to_lowercase(), version.clone())).then(|| InstalledSoftware {
                name,
                version,
                publisher: clip(s.publisher.as_deref(), 200),
            })
        })
        .take(MAX_SOFTWARE)
        .collect();
    let disks = r
        .disks
        .iter()
        .filter_map(|d| {
            let size_gb = d.size_gb.min(1 << 20);
            Some(DeviceDisk { name: clip(Some(&d.name), 10)?, size_gb, free_gb: d.free_gb.min(size_gb) })
        })
        .take(MAX_DISKS)
        .collect();
    Clean {
        manufacturer: text(&r.manufacturer, 200),
        model: text(&r.model, 200),
        serial_number: text(&r.serial_number, 100),
        bios_version: text(&r.bios_version, 200),
        cpu: clip(r.cpu.as_deref(), 200).map(|c| c.split_whitespace().collect::<Vec<_>>().join(" ")),
        memory_mb: r.memory_mb.filter(|m| (1..=1 << 30).contains(m)).and_then(|m| i64::try_from(m).ok()),
        disks,
        software,
    }
}

/// Заменить сведения ПК. Вызывается в транзакции отчёта: строка `devices` уже заблокирована, два
/// отчёта одного ПК не смешают списки программ. Чаще раза в 10 минут не заменяются (ключ агента не
/// превратить в способ нагружать базу); пустой список программ прежний не стирает — это сбой чтения
/// реестра, а не ПК без программ.
pub async fn store(tx: &mut Transaction<'_, Postgres>, device: Uuid, c: &Clean) -> ApiResult<()> {
    let n = sqlx::query(
        "INSERT INTO device_inventory (device_id, manufacturer, model, serial_number, bios_version, cpu, memory_mb, disks)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         ON CONFLICT (device_id) DO UPDATE
            SET manufacturer = EXCLUDED.manufacturer, model = EXCLUDED.model, serial_number = EXCLUDED.serial_number,
                bios_version = EXCLUDED.bios_version, cpu = EXCLUDED.cpu, memory_mb = EXCLUDED.memory_mb,
                disks = EXCLUDED.disks, collected_at = now()
          WHERE device_inventory.collected_at < now() - interval '10 minutes'",
    )
    .bind(device)
    .bind(&c.manufacturer)
    .bind(&c.model)
    .bind(&c.serial_number)
    .bind(&c.bios_version)
    .bind(&c.cpu)
    .bind(c.memory_mb)
    .bind(Jsonb(&c.disks))
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if n == 0 || c.software.is_empty() {
        return Ok(());
    }
    sqlx::query("DELETE FROM device_software WHERE device_id = $1").bind(device).execute(&mut **tx).await?;
    sqlx::query(
        "INSERT INTO device_software (device_id, name, version, publisher)
         SELECT $1, s.n, s.v, s.p FROM unnest($2::text[], $3::text[], $4::text[]) AS s(n, v, p)",
    )
    .bind(device)
    .bind(c.software.iter().map(|s| s.name.clone()).collect::<Vec<_>>())
    .bind(c.software.iter().map(|s| s.version.clone()).collect::<Vec<_>>())
    .bind(c.software.iter().map(|s| s.publisher.clone()).collect::<Vec<_>>())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Нужны ли порталу свежие сведения: их нет или им больше суток.
pub async fn wanted(core: &Core, device: Uuid) -> ApiResult<bool> {
    let fresh: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM device_inventory
                         WHERE device_id = $1 AND collected_at > now() - make_interval(hours => $2))",
    )
    .bind(device)
    .bind(FRESH_HOURS)
    .fetch_one(&core.db)
    .await?;
    Ok(!fresh)
}

/// Оборудование и программы ПК (карточка ПК).
#[derive(Debug, Serialize, ToSchema)]
pub struct DeviceInventory {
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub serial_number: Option<String>,
    pub bios_version: Option<String>,
    pub cpu: Option<String>,
    /// Оперативная память, МБ.
    pub memory_mb: Option<i64>,
    pub disks: Vec<DeviceDisk>,
    pub software: Vec<InstalledSoftware>,
    /// Когда агент собрал сведения.
    #[serde(with = "time::serde::rfc3339")]
    pub collected_at: OffsetDateTime,
}

type InventoryRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
    Jsonb<Vec<DeviceDisk>>,
    OffsetDateTime,
);

pub async fn load(core: &Core, device: Uuid) -> ApiResult<Option<DeviceInventory>> {
    let row: Option<InventoryRow> = sqlx::query_as(
        "SELECT manufacturer, model, serial_number, bios_version, cpu, memory_mb, disks, collected_at
           FROM device_inventory WHERE device_id = $1",
    )
    .bind(device)
    .fetch_optional(&core.db)
    .await?;
    let Some((manufacturer, model, serial_number, bios_version, cpu, memory_mb, Jsonb(disks), collected_at)) = row
    else {
        return Ok(None);
    };
    let software: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT name, version, publisher FROM device_software WHERE device_id = $1 ORDER BY lower(name), version",
    )
    .bind(device)
    .fetch_all(&core.db)
    .await?;
    Ok(Some(DeviceInventory {
        manufacturer,
        model,
        serial_number,
        bios_version,
        cpu,
        memory_mb,
        disks,
        software: software
            .into_iter()
            .map(|(name, version, publisher)| InstalledSoftware { name, version, publisher })
            .collect(),
        collected_at,
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
pub struct SoftwareQuery {
    /// Часть названия программы, от 3 символов.
    pub q: String,
}

#[derive(Debug, Serialize, ToSchema, sqlx::FromRow)]
pub struct SoftwareHit {
    pub device_id: Uuid,
    pub hostname: String,
    pub name: String,
    pub version: Option<String>,
    pub publisher: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_seen_at: Option<OffsetDateTime>,
}

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// На каких ПК стоит программа: поиск по части названия среди включённых ПК (до 500 строк).
#[utoipa::path(get, path = "/api/remote/software", tag = "remote", params(SoftwareQuery),
    responses((status = 200, body = Vec<SoftwareHit>), (status = 400, description = "Слишком короткий запрос")))]
pub async fn search_software(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<SoftwareQuery>,
) -> ApiResult<Json<Vec<SoftwareHit>>> {
    user.require(&perm::DEVICE_VIEW)?;
    let q: String = q.q.trim().chars().filter(|c| !c.is_control()).take(100).collect();
    // От трёх символов: так поиск идёт по триграммному индексу, а не перебором всех программ.
    if q.chars().count() < 3 {
        return Err(ApiError::bad_request("query_too_short", "Введите хотя бы 3 символа.".to_string()));
    }
    let rows = sqlx::query_as::<_, SoftwareHit>(
        "SELECT d.id AS device_id, d.hostname, s.name, s.version, s.publisher, d.last_seen_at
           FROM device_software s JOIN devices d ON d.id = s.device_id
          WHERE d.is_active AND lower(s.name) LIKE '%' || $1 || '%'
          ORDER BY lower(s.name), s.version, lower(d.hostname) LIMIT $2",
    )
    .bind(escape_like(&q.to_lowercase()))
    .bind(MAX_HITS)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders() {
        for s in ["To be filled by O.E.M.", "Default string", "0", "00000000", " ", "N/A", "System Serial Number"] {
            assert!(placeholder(s), "{s}");
        }
        for s in ["5CG1234XYZ", "OptiPlex 7090", "Dell Inc."] {
            assert!(!placeholder(s), "{s}");
        }
    }

    #[test]
    fn invisible_characters_are_removed() {
        assert_eq!(clip(Some("Evil\u{202E}exe.txt\u{200B}"), 100).as_deref(), Some("Evilexe.txt"));
        assert_eq!(clip(Some("\u{FEFF}\u{2066}"), 100), None);
        assert_eq!(clip(Some("Компас-3D v21"), 100).as_deref(), Some("Компас-3D v21"));
    }

    #[test]
    fn cleaning() {
        let sw = |n: &str, v: Option<&str>| InstalledSoftware {
            name: n.into(),
            version: v.map(Into::into),
            publisher: None,
        };
        let r = HardwareInventory {
            manufacturer: Some("  Dell Inc. ".into()),
            model: Some("To be filled by O.E.M.".into()),
            serial_number: Some("00000000".into()),
            cpu: Some("Intel(R) Core(TM) i5-10500   CPU @ 3.10GHz".into()),
            memory_mb: Some(0),
            disks: vec![DeviceDisk { name: "C:".into(), size_gb: 100, free_gb: 500 }],
            software: vec![
                sw("7-Zip", Some("24.08")),
                sw("7-zip", Some("24.08")),
                sw("7-Zip", Some("23.01")),
                sw(" ", None),
            ],
            ..Default::default()
        };
        let c = clean(&r);
        assert_eq!(c.manufacturer.as_deref(), Some("Dell Inc."));
        assert_eq!((c.model, c.serial_number, c.memory_mb), (None, None, None));
        assert_eq!(c.cpu.as_deref(), Some("Intel(R) Core(TM) i5-10500 CPU @ 3.10GHz"));
        assert_eq!(c.disks[0].free_gb, 100, "свободно не больше размера");
        assert_eq!(c.software.len(), 2, "повтор и пустое имя отброшены");
    }

    #[test]
    fn like_escaping() {
        assert_eq!(escape_like("100%_a\\b"), "100\\%\\_a\\\\b");
    }
}
