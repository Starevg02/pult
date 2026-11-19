//! Журнал аудита. Хэш-цепочку и запрет изменений обеспечивает БД (миграция `core`):
//! приложение только вставляет записи — обычно в той же транзакции, что и само изменение.

use serde::Serialize;
use sqlx::PgExecutor;
use std::net::IpAddr;
use uuid::Uuid;

/// Откуда пришло действие.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Portal,
    Api,
    Email,
    Agent,
    Job,
    LegacyPhonebook,
    Cli,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Portal => "portal",
            Self::Api => "api",
            Self::Email => "email",
            Self::Agent => "agent",
            Self::Job => "job",
            Self::LegacyPhonebook => "legacy_phonebook",
            Self::Cli => "cli",
        }
    }
}

/// Кто действует: пользователь портала (или система, если `user_id = None`).
#[derive(Debug, Clone, Default)]
pub struct Actor {
    pub user_id: Option<Uuid>,
    pub on_behalf_of: Option<Uuid>,
    pub ip: Option<IpAddr>,
    pub user_agent: Option<String>,
}

impl Actor {
    pub fn system() -> Self {
        Self::default()
    }
}

/// Запись журнала. `details` — «было/стало» и прочие подробности; паролей и токенов здесь быть не должно.
#[derive(Debug, Clone)]
pub struct Entry {
    pub action: &'static str,
    pub object_type: Option<&'static str>,
    pub object_id: Option<String>,
    pub details: serde_json::Value,
    pub source: Source,
}

impl Entry {
    pub fn new(action: &'static str, source: Source) -> Self {
        Self { action, object_type: None, object_id: None, details: serde_json::json!({}), source }
    }

    pub fn object(mut self, kind: &'static str, id: impl ToString) -> Self {
        self.object_type = Some(kind);
        self.object_id = Some(id.to_string());
        self
    }

    pub fn details(mut self, d: serde_json::Value) -> Self {
        self.details = d;
        self
    }
}

pub async fn record(db: impl PgExecutor<'_>, actor: &Actor, e: Entry) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO audit_log (actor_id, on_behalf_of_id, action, object_type, object_id, details, ip, user_agent, source)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(actor.user_id)
    .bind(actor.on_behalf_of)
    .bind(e.action)
    .bind(e.object_type)
    .bind(e.object_id)
    .bind(e.details)
    .bind(actor.ip.map(sqlx::types::ipnetwork::IpNetwork::from))
    .bind(actor.user_agent.as_deref().map(|s| s.chars().take(500).collect::<String>()))
    .bind(e.source.as_str())
    .fetch_one(db)
    .await
}

/// «Было/стало» для изменённых полей: только поля, значение которых поменялось.
pub fn diff(before: &serde_json::Value, after: &serde_json::Value) -> serde_json::Value {
    let (Some(b), Some(a)) = (before.as_object(), after.as_object()) else {
        return serde_json::json!({ "before": before, "after": after });
    };
    let mut out = serde_json::Map::new();
    for (k, av) in a {
        let bv = b.get(k).unwrap_or(&serde_json::Value::Null);
        if bv != av {
            out.insert(k.clone(), serde_json::json!({ "before": bv, "after": av }));
        }
    }
    serde_json::Value::Object(out)
}

/// Результат проверки целостности цепочки.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct VerifyResult {
    pub checked_at: String,
    /// Номер первой записи с нарушенной цепочкой; `None` — цепочка цела.
    pub broken_at: Option<i64>,
    pub total: i64,
}

impl crate::settings::SettingsGroup for VerifyResult {
    const KEY: &'static str = "audit_verify";
}

/// Проверяет всю цепочку (задача `audit.verify`, ежедневно).
pub async fn verify(db: &sqlx::PgPool) -> Result<VerifyResult, sqlx::Error> {
    let broken_at: Option<i64> = sqlx::query_scalar("SELECT audit_verify(0)").fetch_one(db).await?;
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log").fetch_one(db).await?;
    let checked_at =
        time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339).unwrap_or_default();
    Ok(VerifyResult { checked_at, broken_at, total })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diff_keeps_only_changed_fields() {
        let d = diff(&json!({"a": 1, "b": "x", "c": null}), &json!({"a": 1, "b": "y", "c": "z"}));
        assert_eq!(d, json!({"b": {"before": "x", "after": "y"}, "c": {"before": null, "after": "z"}}));
        assert_eq!(diff(&json!({"a": 1}), &json!({"a": 1})), json!({}));
    }
}
