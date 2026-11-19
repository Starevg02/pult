//! Сотрудники справочника (`persons`): чтение, поиск, правка с оптимистичной
//! блокировкой, архив. Этими же функциями пользуется синхронизация AD (`pult-people`).

use crate::search;
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool, Postgres, Transaction};
use std::collections::BTreeMap;
use utoipa::ToSchema;
use uuid::Uuid;

/// Поля, у которых настраивается источник.
pub const FIELDS: &[&str] = &[
    "last_name",
    "first_name",
    "middle_name",
    "email",
    "department",
    "position",
    "work_phone",
    "mobile_phone",
    "site",
    "room",
];

/// Откуда берётся поле: из AD (в портале только чтение) или из портала.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FieldSource {
    Ad,
    Portal,
}

/// По умолчанию: ФИО и email — из AD, остальное — портал (в AD эти атрибуты обычно не ведутся).
pub fn default_source(field: &str) -> FieldSource {
    match field {
        "last_name" | "first_name" | "middle_name" | "email" => FieldSource::Ad,
        _ => FieldSource::Portal,
    }
}

pub async fn field_sources(db: impl PgExecutor<'_>) -> Result<BTreeMap<String, FieldSource>, sqlx::Error> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT field, source FROM person_field_sources").fetch_all(db).await?;
    let stored: BTreeMap<String, String> = rows.into_iter().collect();
    Ok(FIELDS
        .iter()
        .map(|f| {
            let s = match stored.get(*f).map(String::as_str) {
                Some("ad") => FieldSource::Ad,
                Some("portal") => FieldSource::Portal,
                _ => default_source(f),
            };
            (f.to_string(), s)
        })
        .collect())
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct Person {
    pub id: Uuid,
    pub last_name: String,
    pub first_name: String,
    pub middle_name: Option<String>,
    pub department_id: Option<Uuid>,
    pub department_name: Option<String>,
    pub position_id: Option<Uuid>,
    pub position_name: Option<String>,
    pub work_phone: Option<String>,
    pub internal_ext: Option<String>,
    pub mobile_phone: Option<String>,
    pub email: Option<String>,
    pub site_id: Option<Uuid>,
    pub site_name: Option<String>,
    pub room: Option<String>,
    pub is_archived: bool,
    pub source: String,
    /// Логин AD, если сотрудник связан с учёткой.
    pub ad_login: Option<String>,
    pub is_favorite: bool,
    pub version: i32,
}

impl Person {
    pub fn full_name(&self) -> String {
        [Some(self.last_name.as_str()), Some(self.first_name.as_str()), self.middle_name.as_deref()]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Значения полей для аудита «было/стало» и проверки источника.
    pub fn editable(&self) -> serde_json::Value {
        serde_json::json!({
            "last_name": self.last_name, "first_name": self.first_name, "middle_name": self.middle_name,
            "email": self.email, "department": self.department_id, "position": self.position_id,
            "work_phone": self.work_phone, "mobile_phone": self.mobile_phone, "site": self.site_id, "room": self.room,
        })
    }
}

const SELECT: &str = "SELECT p.id, p.last_name, p.first_name, p.middle_name,
        p.department_id, d.name AS department_name, p.position_id, pos.name AS position_name,
        p.work_phone, p.internal_ext, p.mobile_phone, p.email, p.site_id, s.name AS site_name, p.room,
        p.is_archived, p.source, a.sam_account_name AS ad_login,
        (f.person_id IS NOT NULL) AS is_favorite, p.version
   FROM persons p
   LEFT JOIN departments d ON d.id = p.department_id
   LEFT JOIN positions pos ON pos.id = p.position_id
   LEFT JOIN sites s ON s.id = p.site_id
   LEFT JOIN ad_accounts a ON a.id = p.ad_account_id
   LEFT JOIN person_favorites f ON f.person_id = p.id AND f.user_id = $1";

pub async fn get(db: impl PgExecutor<'_>, viewer: Uuid, id: Uuid) -> Result<Option<Person>, sqlx::Error> {
    sqlx::query_as(&format!("{SELECT} WHERE p.id = $2")).bind(viewer).bind(id).fetch_optional(db).await // SAFE: константа SELECT из кода
}

/// Фильтр списка.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub query: String,
    pub department_id: Option<Uuid>,
    pub favorites_only: bool,
    pub archived: bool,
    pub limit: i64,
}

/// Список с поиском. Сначала совпавшие по запросу как есть, затем — по исправленной раскладке.
pub async fn list(db: &PgPool, viewer: Uuid, f: &ListFilter) -> Result<Vec<Person>, sqlx::Error> {
    let vars = search::variants(&f.query);
    let arr = |i: usize| -> (Vec<String>, Vec<String>) {
        vars.get(i).map(|v| v.iter().map(|t| (t.like.clone(), t.digits.clone())).unzip()).unwrap_or_default()
    };
    let (l0, d0) = arr(0);
    let (l1, d1) = arr(1);
    let (l2, d2) = arr(2);
    // SAFE: подставляются только номера параметров ($N) и константа SELECT; значения — параметры.
    let m = |a: u8, b: u8| {
        format!(
            "(cardinality(${a}::text[]) > 0 AND NOT EXISTS (
                SELECT 1 FROM unnest(${a}::text[], ${b}::text[]) AS t(l, dg)
                 WHERE NOT (p.search_text LIKE '%' || t.l || '%'
                            OR (t.dg <> '' AND p.phone_digits LIKE '%' || t.dg || '%'))))"
        )
    };
    let sql = format!(
        "{SELECT}
          WHERE p.is_archived = $2
            AND ($3::uuid IS NULL OR p.department_id = $3)
            AND (NOT $4 OR f.person_id IS NOT NULL)
            AND (cardinality($5::text[]) = 0 OR {} OR {} OR {})
          ORDER BY CASE WHEN cardinality($5::text[]) = 0 OR {} THEN 0 ELSE 1 END,
                   lower(p.last_name), lower(p.first_name), lower(coalesce(p.middle_name, ''))
          LIMIT $11",
        m(5, 6),
        m(7, 8),
        m(9, 10),
        m(5, 6)
    );
    sqlx::query_as(&sql)
        .bind(viewer)
        .bind(f.archived)
        .bind(f.department_id)
        .bind(f.favorites_only)
        .bind(&l0)
        .bind(&d0)
        .bind(&l1)
        .bind(&d1)
        .bind(&l2)
        .bind(&d2)
        .bind(f.limit.clamp(1, 5000))
        .fetch_all(db)
        .await
}

/// Поля сотрудника при создании и правке.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PersonInput {
    pub last_name: String,
    pub first_name: String,
    pub middle_name: Option<String>,
    pub department_id: Option<Uuid>,
    pub position_id: Option<Uuid>,
    pub work_phone: Option<String>,
    pub mobile_phone: Option<String>,
    pub email: Option<String>,
    pub site_id: Option<Uuid>,
    pub room: Option<String>,
}

fn clean(s: &Option<String>) -> Option<String> {
    s.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(|v| v.chars().take(200).collect())
}

impl PersonInput {
    /// Обрезает пробелы, пустое → NULL, email — в нижнем регистре. Ошибка — текст для пользователя.
    pub fn normalized(&self) -> Result<Self, String> {
        let p = Self {
            last_name: self.last_name.trim().chars().take(100).collect(),
            first_name: self.first_name.trim().chars().take(100).collect(),
            middle_name: clean(&self.middle_name),
            department_id: self.department_id,
            position_id: self.position_id,
            work_phone: clean(&self.work_phone),
            mobile_phone: clean(&self.mobile_phone),
            email: clean(&self.email).map(|e| e.to_lowercase()),
            site_id: self.site_id,
            room: clean(&self.room),
        };
        if p.last_name.is_empty() && p.first_name.is_empty() {
            return Err("Укажите фамилию или имя.".into());
        }
        if let Some(e) = &p.email
            && !crate::looks_like_email(e)
        {
            return Err(format!("«{e}» не похоже на адрес почты."));
        }
        Ok(p)
    }

    pub fn as_json(&self) -> serde_json::Value {
        serde_json::json!({
            "last_name": self.last_name, "first_name": self.first_name, "middle_name": self.middle_name,
            "email": self.email, "department": self.department_id, "position": self.position_id,
            "work_phone": self.work_phone, "mobile_phone": self.mobile_phone, "site": self.site_id, "room": self.room,
        })
    }
}

pub async fn create(
    tx: &mut Transaction<'_, Postgres>,
    p: &PersonInput,
    source: &str,
    ad_account_id: Option<Uuid>,
    legacy_id: Option<i32>,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO persons (last_name, first_name, middle_name, department_id, position_id, work_phone, internal_ext,
                              mobile_phone, email, site_id, room, source, ad_account_id, legacy_phonebook_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) RETURNING id",
    )
    .bind(&p.last_name)
    .bind(&p.first_name)
    .bind(&p.middle_name)
    .bind(p.department_id)
    .bind(p.position_id)
    .bind(&p.work_phone)
    .bind(p.work_phone.as_deref().and_then(crate::internal_ext))
    .bind(&p.mobile_phone)
    .bind(&p.email)
    .bind(p.site_id)
    .bind(&p.room)
    .bind(source)
    .bind(ad_account_id)
    .bind(legacy_id)
    .fetch_one(&mut **tx)
    .await
}

/// Правка при совпадении версии. `false` — версия устарела (кто-то успел изменить запись).
pub async fn update(
    tx: &mut Transaction<'_, Postgres>,
    id: Uuid,
    version: i32,
    p: &PersonInput,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE persons SET last_name = $3, first_name = $4, middle_name = $5, department_id = $6, position_id = $7,
                work_phone = $8, internal_ext = $9, mobile_phone = $10, email = $11, site_id = $12, room = $13,
                version = version + 1
          WHERE id = $1 AND version = $2",
    )
    .bind(id)
    .bind(version)
    .bind(&p.last_name)
    .bind(&p.first_name)
    .bind(&p.middle_name)
    .bind(p.department_id)
    .bind(p.position_id)
    .bind(&p.work_phone)
    .bind(p.work_phone.as_deref().and_then(crate::internal_ext))
    .bind(&p.mobile_phone)
    .bind(&p.email)
    .bind(p.site_id)
    .bind(&p.room)
    .execute(&mut **tx)
    .await?
    .rows_affected()
        == 1)
}

/// Архив / восстановление с проверкой версии.
pub async fn set_archived(
    db: impl PgExecutor<'_>,
    id: Uuid,
    version: Option<i32>,
    archived: bool,
    reason: &str,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE persons SET is_archived = $3,
                archived_at = CASE WHEN $3 THEN now() END,
                archived_reason = CASE WHEN $3 THEN $4 END,
                version = version + 1
          WHERE id = $1 AND ($2::int IS NULL OR version = $2) AND is_archived <> $3",
    )
    .bind(id)
    .bind(version)
    .bind(archived)
    .bind(reason)
    .execute(db)
    .await?
    .rows_affected()
        == 1)
}

/// Возможный дубль (предупреждение, не запрет — тёзки допустимы).
#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct Duplicate {
    pub id: Uuid,
    pub name: String,
    pub department_name: Option<String>,
    /// Что совпало: `name`, `work_phone`, `mobile_phone`.
    pub reason: String,
}

pub async fn duplicates(db: &PgPool, p: &PersonInput, exclude: Option<Uuid>) -> Result<Vec<Duplicate>, sqlx::Error> {
    let digits = |s: &Option<String>| s.as_deref().map(|v| v.chars().filter(char::is_ascii_digit).collect::<String>());
    sqlx::query_as(
        "SELECT p.id, concat_ws(' ', p.last_name, p.first_name, p.middle_name) AS name, d.name AS department_name,
                CASE WHEN pult_norm(p.last_name) = pult_norm($1) AND pult_norm(p.first_name) = pult_norm($2) THEN 'name'
                     WHEN length($3) >= 3 AND pult_digits(p.work_phone) = $3 THEN 'work_phone'
                     ELSE 'mobile_phone' END AS reason
           FROM persons p LEFT JOIN departments d ON d.id = p.department_id
          WHERE NOT p.is_archived AND ($5::uuid IS NULL OR p.id <> $5)
            AND ((pult_norm(p.last_name) = pult_norm($1) AND pult_norm(p.first_name) = pult_norm($2) AND $1 <> '')
                 OR (length($3) >= 3 AND pult_digits(p.work_phone) = $3)
                 OR (length($4) >= 5 AND right(pult_digits(p.mobile_phone), 10) = right($4, 10)))
          LIMIT 10",
    )
    .bind(&p.last_name)
    .bind(&p.first_name)
    .bind(digits(&p.work_phone).unwrap_or_default())
    .bind(digits(&p.mobile_phone).unwrap_or_default())
    .bind(exclude)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_normalization() {
        let p = PersonInput {
            last_name: "  Иванов ".into(),
            first_name: "Иван".into(),
            email: Some(" Ivanov@Example.Test ".into()),
            room: Some("  ".into()),
            ..Default::default()
        }
        .normalized()
        .unwrap();
        assert_eq!((p.last_name.as_str(), p.email.as_deref(), p.room), ("Иванов", Some("ivanov@example.test"), None));
        assert!(PersonInput::default().normalized().is_err());
        assert!(
            PersonInput { last_name: "X".into(), email: Some("bad".into()), ..Default::default() }
                .normalized()
                .is_err()
        );
    }

    #[test]
    fn defaults_for_sources() {
        assert_eq!(default_source("email"), FieldSource::Ad);
        assert_eq!(default_source("work_phone"), FieldSource::Portal);
        assert!(FIELDS.iter().all(|f| matches!(default_source(f), FieldSource::Ad | FieldSource::Portal)));
    }
}
