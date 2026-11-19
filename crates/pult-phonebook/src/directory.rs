//! Подразделения и должности: используемое не удаляется — только в архив.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

/// Справочник: подразделения или должности. Имя таблицы — только из этого перечисления.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dict {
    Departments,
    Positions,
}

impl Dict {
    pub fn table(self) -> &'static str {
        match self {
            Self::Departments => "departments",
            Self::Positions => "positions",
        }
    }

    pub fn person_column(self) -> &'static str {
        match self {
            Self::Departments => "department_id",
            Self::Positions => "position_id",
        }
    }

    pub fn object_type(self) -> &'static str {
        match self {
            Self::Departments => "department",
            Self::Positions => "position",
        }
    }
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct DictItem {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
    pub is_archived: bool,
    /// Сотрудников не в архиве.
    pub persons: i64,
    pub version: i32,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DictInput {
    pub name: String,
    /// Только для подразделений.
    pub parent_id: Option<Uuid>,
}

pub async fn list(db: &PgPool, d: Dict, with_archived: bool) -> Result<Vec<DictItem>, sqlx::Error> {
    let parent = if d == Dict::Departments { "t.parent_id" } else { "NULL::uuid" };
    // SAFE: таблица и столбец — константы из `Dict`, `parent` — одна из двух констант выше.
    let sql = format!(
        "SELECT t.id, t.name, {parent} AS parent_id, t.is_archived, t.version,
                (SELECT count(*) FROM persons p WHERE p.{col} = t.id AND NOT p.is_archived) AS persons
           FROM {table} t
          WHERE $1 OR NOT t.is_archived
          ORDER BY lower(t.name)",
        col = d.person_column(),
        table = d.table()
    );
    sqlx::query_as(&sql).bind(with_archived).fetch_all(db).await
}

pub async fn get(db: &PgPool, d: Dict, id: Uuid) -> Result<Option<DictItem>, sqlx::Error> {
    Ok(list(db, d, true).await?.into_iter().find(|i| i.id == id))
}

/// Имя, уже занятое другой активной записью (подсказка против дублей).
pub async fn name_taken(db: &PgPool, d: Dict, name: &str, except: Option<Uuid>) -> Result<bool, sqlx::Error> {
    // SAFE: таблица — константа из `Dict`, значения — параметры.
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM {} WHERE pult_norm(name) = pult_norm($1) AND NOT is_archived AND ($2::uuid IS NULL OR id <> $2))",
        d.table()
    );
    sqlx::query_scalar(&sql).bind(name).bind(except).fetch_one(db).await
}

pub fn clean_name(s: &str) -> Result<String, String> {
    let n: String = s.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(200).collect();
    if n.is_empty() { Err("Укажите название.".into()) } else { Ok(n) }
}
