//! Перенос PhonebookDB в БД Pult (`pult import phonebook`).
//!
//! Всё — одной транзакцией. Идемпотентно: подразделения, должности и сотрудники сопоставляются
//! по `legacy_phonebook_id`, повторный запуск обновляет, а не дублирует; журнал Phonebook
//! переносится только новыми записями. `--dry-run` выполняет тот же перенос и откатывает
//! транзакцию — отчёт показывает ровно то, что произошло бы.

use crate::{
    PersonImport, Snapshot,
    persons::{FieldSource, field_sources},
};
use pult_core::audit::{self, Actor, Entry, Source};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::HashMap;
use uuid::Uuid;

/// Что изменилось в БД Pult.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Applied {
    pub departments_created: usize,
    pub departments_renamed: usize,
    pub positions_created: usize,
    pub positions_renamed: usize,
    pub persons_created: usize,
    pub persons_updated: usize,
    pub persons_unchanged: usize,
    pub persons_archived: usize,
    pub persons_restored: usize,
    /// Сотрудники, которых синхронизация AD создала до импорта и которые совпали по email с записью Phonebook.
    pub persons_merged: usize,
    pub audit_imported: usize,
}

/// Сверка с Phonebook: числа в источнике и в Pult после переноса.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub employees: usize,
    pub archived: usize,
    pub departments: usize,
    pub positions: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reconciliation {
    pub phonebook: Counts,
    pub pult: Counts,
    pub matches: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub dry_run: bool,
    pub source: crate::ImportReport,
    pub applied: Applied,
    pub reconciliation: Reconciliation,
}

/// Состояние переноса журнала между запусками.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct ImportState {
    last_audit_id: i64,
}

impl pult_core::settings::SettingsGroup for ImportState {
    const KEY: &'static str = "phonebook_import";
}

/// Длинный XML журнала Phonebook обрезается — запись аудита не должна весить мегабайты.
const MAX_XML: usize = 64 * 1024;

fn cut(s: &str) -> String {
    if s.len() <= MAX_XML {
        return s.to_string();
    }
    let mut end = MAX_XML;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

async fn upsert_dict(
    tx: &mut Transaction<'_, Postgres>,
    table: &str,
    items: &[(i32, String)],
    created: &mut usize,
    renamed: &mut usize,
) -> Result<HashMap<i32, Uuid>, sqlx::Error> {
    let mut map = HashMap::new();
    for (legacy, name) in items {
        let name = if name.trim().is_empty() {
            format!("Без названия ({legacy})")
        } else {
            name.trim().to_string()
        };
        // Имя таблицы — константа из кода (departments / positions).
        let existing: Option<(Uuid, String)> =
            sqlx::query_as(&format!("SELECT id, name FROM {table} WHERE legacy_phonebook_id = $1")) // SAFE: имя таблицы — константа
                .bind(legacy)
                .fetch_optional(&mut **tx)
                .await?;
        let id = match existing {
            Some((id, old)) => {
                if old != name {
                    sqlx::query(&format!("UPDATE {table} SET name = $2, version = version + 1 WHERE id = $1")) // SAFE: константа
                        .bind(id)
                        .bind(&name)
                        .execute(&mut **tx)
                        .await?;
                    *renamed += 1;
                }
                id
            }
            None => {
                *created += 1;
                sqlx::query_scalar(&format!(
                    "INSERT INTO {table} (name, legacy_phonebook_id) VALUES ($1, $2) RETURNING id"
                )) // SAFE: константа
                .bind(&name)
                .bind(legacy)
                .fetch_one(&mut **tx)
                .await?
            }
        };
        map.insert(*legacy, id);
    }
    Ok(map)
}

#[derive(sqlx::FromRow)]
struct Existing {
    id: Uuid,
    last_name: String,
    first_name: String,
    middle_name: Option<String>,
    department_id: Option<Uuid>,
    position_id: Option<Uuid>,
    work_phone: Option<String>,
    internal_ext: Option<String>,
    mobile_phone: Option<String>,
    email: Option<String>,
    is_archived: bool,
    archived_reason: Option<String>,
    linked: bool,
}

pub async fn apply(
    db: &PgPool,
    snap: &Snapshot,
    dry_run: bool,
    actor: &Actor,
    source_name: &str,
) -> Result<Outcome, sqlx::Error> {
    let (people, report) = snap.plan();
    let mut a = Applied::default();
    let mut tx = db.begin().await?;
    // Два импорта одновременно не нужны: второй подождёт.
    sqlx::query("SELECT pg_advisory_xact_lock(7231010)").execute(&mut *tx).await?;

    let deps =
        upsert_dict(&mut tx, "departments", &snap.departments, &mut a.departments_created, &mut a.departments_renamed)
            .await?;
    let pos =
        upsert_dict(&mut tx, "positions", &snap.positions, &mut a.positions_created, &mut a.positions_renamed).await?;
    let sources = field_sources(&mut *tx).await?;
    let from_ad = |f: &str| sources.get(f) == Some(&FieldSource::Ad);

    for p in &people {
        apply_person(&mut tx, p, &deps, &pos, &from_ad, &mut a).await?;
    }
    a.persons_merged = merge_ad_created(&mut tx).await?;

    // Журнал Phonebook — только новые записи (после последнего перенесённого номера).
    let mut state: ImportState = pult_core::settings::load(&mut *tx).await?;
    let after = state.last_audit_id;
    for r in snap.audit.iter().filter(|r| r.id > after) {
        audit::record(
            &mut *tx,
            &Actor::system(),
            Entry::new("phonebook.legacy", Source::LegacyPhonebook).object("legacy_audit", r.id).details(
                serde_json::json!({
                    "occurred_at": r.occurred_at, "action": r.action, "entity": r.entity, "subject": r.subject,
                    "login": r.login, "application": r.application,
                    "before_xml": cut(&r.before_xml), "after_xml": cut(&r.after_xml),
                }),
            ),
        )
        .await?;
        a.audit_imported += 1;
        state.last_audit_id = state.last_audit_id.max(r.id);
    }
    pult_core::settings::save(&mut *tx, &state, actor.user_id).await?;

    let pult = pult_counts(&mut tx).await?;
    let phonebook = Counts {
        employees: report.employees,
        archived: report.archived,
        departments: report.departments,
        positions: report.positions,
    };
    let reconciliation = Reconciliation { matches: phonebook == pult, phonebook, pult };
    let outcome = Outcome { dry_run, source: report, applied: a, reconciliation };

    let summary = serde_json::json!({
        "source": source_name, "dry_run": dry_run, "applied": outcome.applied, "reconciliation": outcome.reconciliation,
    });
    sqlx::query("INSERT INTO phonebook_import_runs (source, dry_run, counts, started_by, finished_at) VALUES ($1, $2, $3, $4, now())")
        .bind(source_name)
        .bind(dry_run)
        .bind(&summary)
        .bind(actor.user_id)
        .execute(&mut *tx)
        .await?;
    audit::record(&mut *tx, actor, Entry::new("phonebook.import", Source::Cli).details(summary)).await?;
    if dry_run {
        tx.rollback().await?;
    } else {
        tx.commit().await?;
    }
    Ok(outcome)
}

async fn apply_person(
    tx: &mut Transaction<'_, Postgres>,
    p: &PersonImport,
    deps: &HashMap<i32, Uuid>,
    pos: &HashMap<i32, Uuid>,
    from_ad: &dyn Fn(&str) -> bool,
    a: &mut Applied,
) -> Result<(), sqlx::Error> {
    let department_id = p.legacy_department_id.and_then(|d| deps.get(&d).copied());
    let position_id = p.legacy_position_id.and_then(|d| pos.get(&d).copied());
    let existing: Option<Existing> = sqlx::query_as(
        "SELECT id, last_name, first_name, middle_name, department_id, position_id, work_phone, internal_ext, mobile_phone,
                email, is_archived, archived_reason, ad_account_id IS NOT NULL AS linked
           FROM persons WHERE legacy_phonebook_id = $1",
    )
    .bind(p.legacy_phonebook_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(e) = existing else {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO persons (last_name, first_name, middle_name, department_id, position_id, work_phone, internal_ext,
                                  mobile_phone, email, source, legacy_phonebook_id, is_archived, archived_at, archived_reason)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'phonebook_import', $10, $11,
                     CASE WHEN $11 THEN now() END, CASE WHEN $11 THEN 'legacy' END)
             RETURNING id",
        )
        .bind(&p.last_name)
        .bind(&p.first_name)
        .bind(&p.middle_name)
        .bind(department_id)
        .bind(position_id)
        .bind(&p.work_phone)
        .bind(&p.internal_ext)
        .bind(&p.mobile_phone)
        .bind(&p.email)
        .bind(p.legacy_phonebook_id)
        .bind(p.is_archived)
        .fetch_one(&mut **tx)
        .await?;
        let _ = id;
        a.persons_created += 1;
        return Ok(());
    };
    // У связанного с AD сотрудника поля с источником «AD» ведёт синхронизация — импорт их не трогает.
    let keep = |field: &str| e.linked && from_ad(field);
    let pick =
        |field: &str, old: &Option<String>, new: &Option<String>| if keep(field) { old.clone() } else { new.clone() };
    let last_name = if keep("last_name") { e.last_name.clone() } else { p.last_name.clone() };
    let first_name = if keep("first_name") { e.first_name.clone() } else { p.first_name.clone() };
    let middle_name = pick("middle_name", &e.middle_name, &p.middle_name);
    let email = pick("email", &e.email, &p.email);
    let department_id = if keep("department") { e.department_id } else { department_id };
    let position_id = if keep("position") { e.position_id } else { position_id };
    let work_phone = pick("work_phone", &e.work_phone, &p.work_phone);
    let internal_ext = if keep("work_phone") { e.internal_ext.clone() } else { p.internal_ext.clone() };
    let mobile_phone = pick("mobile_phone", &e.mobile_phone, &p.mobile_phone);
    let changed = (
        &last_name,
        &first_name,
        &middle_name,
        &email,
        department_id,
        position_id,
        &work_phone,
        &internal_ext,
        &mobile_phone,
    ) != (
        &e.last_name,
        &e.first_name,
        &e.middle_name,
        &e.email,
        e.department_id,
        e.position_id,
        &e.work_phone,
        &e.internal_ext,
        &e.mobile_phone,
    );
    if changed {
        sqlx::query(
            "UPDATE persons SET last_name = $2, first_name = $3, middle_name = $4, email = $5, department_id = $6,
                    position_id = $7, work_phone = $8, internal_ext = $9, mobile_phone = $10, version = version + 1
              WHERE id = $1",
        )
        .bind(e.id)
        .bind(&last_name)
        .bind(&first_name)
        .bind(&middle_name)
        .bind(&email)
        .bind(department_id)
        .bind(position_id)
        .bind(&work_phone)
        .bind(&internal_ext)
        .bind(&mobile_phone)
        .execute(&mut **tx)
        .await?;
        a.persons_updated += 1;
    } else {
        a.persons_unchanged += 1;
    }
    // Архив: в Phonebook архивный — в архив; вернули из архива в Phonebook — возвращаем,
    // но только если в архив отправил импорт, а не редактор или синхронизация AD.
    if p.is_archived && !e.is_archived {
        crate::persons::set_archived(&mut **tx, e.id, None, true, "legacy").await?;
        a.persons_archived += 1;
    } else if !p.is_archived && e.is_archived && e.archived_reason.as_deref() == Some("legacy") {
        crate::persons::set_archived(&mut **tx, e.id, None, false, "legacy").await?;
        a.persons_restored += 1;
    }
    Ok(())
}

async fn pult_counts(tx: &mut Transaction<'_, Postgres>) -> Result<Counts, sqlx::Error> {
    let (employees, archived): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE is_archived) FROM persons WHERE legacy_phonebook_id IS NOT NULL",
    )
    .fetch_one(&mut **tx)
    .await?;
    let departments: i64 = sqlx::query_scalar("SELECT count(*) FROM departments WHERE legacy_phonebook_id IS NOT NULL")
        .fetch_one(&mut **tx)
        .await?;
    let positions: i64 = sqlx::query_scalar("SELECT count(*) FROM positions WHERE legacy_phonebook_id IS NOT NULL")
        .fetch_one(&mut **tx)
        .await?;
    let n = |v: i64| usize::try_from(v).unwrap_or(0);
    Ok(Counts { employees: n(employees), archived: n(archived), departments: n(departments), positions: n(positions) })
}

/// Синхронизация AD прошла раньше импорта и сама создала сотрудников. Если такой сотрудник
/// однозначно совпадает по email с записью Phonebook, учётка переходит к записи Phonebook,
/// а созданная синхронизацией запись удаляется (ссылки на неё переносятся).
async fn merge_ad_created(tx: &mut Transaction<'_, Postgres>) -> Result<usize, sqlx::Error> {
    let pairs: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "SELECT a.id, l.id, a.ad_account_id
           FROM persons a
           JOIN persons l ON lower(l.email) = lower(a.email)
          WHERE a.source = 'ad' AND a.legacy_phonebook_id IS NULL AND a.ad_account_id IS NOT NULL AND a.email IS NOT NULL
            AND l.legacy_phonebook_id IS NOT NULL AND l.ad_account_id IS NULL
            AND (SELECT count(*) FROM persons x WHERE lower(x.email) = lower(a.email) AND x.legacy_phonebook_id IS NOT NULL) = 1",
    )
    .fetch_all(&mut **tx)
    .await?;
    for (from, to, account) in &pairs {
        sqlx::query("UPDATE persons SET ad_account_id = NULL WHERE id = $1").bind(from).execute(&mut **tx).await?;
        sqlx::query("UPDATE persons SET ad_account_id = $2, version = version + 1 WHERE id = $1")
            .bind(to)
            .bind(account)
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE users SET person_id = $2 WHERE person_id = $1")
            .bind(from)
            .bind(to)
            .execute(&mut **tx)
            .await?;
        sqlx::query("INSERT INTO person_favorites (user_id, person_id) SELECT user_id, $2 FROM person_favorites WHERE person_id = $1 ON CONFLICT DO NOTHING")
            .bind(from)
            .bind(to)
            .execute(&mut **tx)
            .await?;
        sqlx::query("DELETE FROM person_link_candidates WHERE person_id = $1").bind(from).execute(&mut **tx).await?;
        sqlx::query("DELETE FROM persons WHERE id = $1").bind(from).execute(&mut **tx).await?;
        audit::record(
            &mut **tx,
            &Actor::system(),
            Entry::new("person.merge", Source::Cli)
                .object("person", to)
                .details(serde_json::json!({"merged": from, "ad_account": account})),
        )
        .await?;
    }
    Ok(pairs.len())
}

#[cfg(test)]
mod tests {
    use super::cut;

    #[test]
    fn cut_respects_char_boundaries() {
        let s = "я".repeat(40_000); // 80 000 байт
        let c = cut(&s);
        assert!(c.len() <= super::MAX_XML + "…".len());
        assert!(c.ends_with('…'));
        assert_eq!(cut("abc"), "abc");
    }
}
