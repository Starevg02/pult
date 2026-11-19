//! Правила автоматизации: «событие → условия → действия», без исполнения кода.
//! Правила не обходят права: действуют от имени системы, только над полями заявки, и каждое
//! срабатывание пишется в историю заявки. Действия правил других правил не запускают.

use crate::{
    HelpdeskState, assignment, notify, perm, sla,
    tickets::{NewComment, T, Who, audit_rec, event, insert_comment, select_t, set_assignee},
};
use axum::{
    Json,
    extract::{FromRef, Path, State},
    http::StatusCode,
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser,
    audit::{self, Entry, Source},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Connection, Postgres, Transaction};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Created,
    Updated,
    Commented,
    SlaWarning,
    SlaBreached,
}

impl Trigger {
    pub fn code(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Updated => "updated",
            Self::Commented => "commented",
            Self::SlaWarning => "sla_warning",
            Self::SlaBreached => "sla_breached",
        }
    }
}

const TRIGGERS: &[&str] = &["created", "updated", "commented", "sla_warning", "sla_breached"];
const FIELDS: &[&str] = &[
    "title",
    "description",
    "text",
    "comment",
    "category_id",
    "site_id",
    "priority",
    "status",
    "channel",
    "line",
    "requester_vip",
    "requester_department",
    "assignee_id",
    "group_id",
];
const OPS: &[&str] = &["contains", "not_contains", "equals", "not_equals", "is_empty", "not_empty"];
const ACTIONS: &[&str] = &[
    "set_category",
    "set_priority",
    "set_site",
    "set_line",
    "set_group",
    "assign",
    "add_note",
    "notify",
    "add_watcher",
];

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct Condition {
    /// `title`, `description`, `text` (тема или описание), `comment` (текст нового комментария),
    /// `category_id`, `site_id`, `priority`, `status`, `channel`, `line`, `requester_vip`,
    /// `requester_department`, `assignee_id`, `group_id`.
    pub field: String,
    /// `contains`, `not_contains`, `equals`, `not_equals`, `is_empty`, `not_empty`.
    pub op: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct Action {
    /// `set_category`, `set_priority`, `set_site`, `set_line`, `set_group`, `assign`, `add_note`,
    /// `notify` (пользователь; `assignee` — исполнитель; `superadmins`), `add_watcher` (сотрудник).
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub value: String,
}

/// Данные заявки, по которым проверяются условия.
#[derive(sqlx::FromRow)]
struct Facts {
    title: String,
    description: String,
    category_id: Option<Uuid>,
    site_id: Option<Uuid>,
    priority: String,
    status: String,
    channel: String,
    line: String,
    assignee_id: Option<Uuid>,
    group_id: Option<Uuid>,
    requester_vip: bool,
    requester_department: Option<String>,
}

fn norm(s: &str) -> String {
    s.to_lowercase().replace('ё', "е")
}

fn check(c: &Condition, f: &Facts, comment: Option<&str>) -> bool {
    let uuid = |u: Option<Uuid>| u.map(|u| u.to_string()).unwrap_or_default();
    let value: String = match c.field.as_str() {
        "title" => f.title.clone(),
        "description" => f.description.clone(),
        "text" => format!("{}\n{}", f.title, f.description),
        "comment" => comment.unwrap_or_default().to_string(),
        "category_id" => uuid(f.category_id),
        "site_id" => uuid(f.site_id),
        "priority" => f.priority.clone(),
        "status" => f.status.clone(),
        "channel" => f.channel.clone(),
        "line" => f.line.clone(),
        "assignee_id" => uuid(f.assignee_id),
        "group_id" => uuid(f.group_id),
        "requester_vip" => f.requester_vip.to_string(),
        "requester_department" => f.requester_department.clone().unwrap_or_default(),
        _ => return false,
    };
    let (v, want) = (norm(&value), norm(c.value.trim()));
    match c.op.as_str() {
        "contains" => !want.is_empty() && v.contains(&want),
        "not_contains" => want.is_empty() || !v.contains(&want),
        "equals" => v.trim() == want,
        "not_equals" => v.trim() != want,
        "is_empty" => v.trim().is_empty(),
        "not_empty" => !v.trim().is_empty(),
        _ => false,
    }
}

#[derive(sqlx::FromRow)]
struct RuleRow {
    id: Uuid,
    name: String,
    conditions: Value,
    actions: Value,
    stop: bool,
}

/// Проверить правила события. `comment` — текст нового комментария (для `commented`).
pub async fn fire_with(
    tx: &mut Transaction<'_, Postgres>,
    t: &T,
    trigger: Trigger,
    comment: Option<&str>,
) -> ApiResult<()> {
    let rules: Vec<RuleRow> = sqlx::query_as(
        "SELECT id, name, conditions, actions, stop FROM automation_rules WHERE is_active AND trigger = $1 ORDER BY sort, name",
    )
    .bind(trigger.code())
    .fetch_all(&mut **tx)
    .await?;
    if rules.is_empty() {
        return Ok(());
    }
    let who = Who { source: Source::Job, ..Who::system() };
    for r in rules {
        let facts: Facts = sqlx::query_as(
            "SELECT t.title, t.description, t.category_id, t.site_id, t.priority, t.status, t.channel, t.line, t.assignee_id,
                    t.group_id, coalesce(p.is_vip, false) AS requester_vip, d.name AS requester_department
               FROM tickets t LEFT JOIN persons p ON p.id = t.requester_id LEFT JOIN departments d ON d.id = p.department_id
              WHERE t.id = $1",
        )
        .bind(t.id)
        .fetch_one(&mut **tx)
        .await?;
        // Нечитаемые условия не должны превращаться в «пустой список — срабатывает всегда».
        let parsed: Result<(Vec<Condition>, Vec<Action>), _> =
            serde_json::from_value(r.conditions).and_then(|c| serde_json::from_value(r.actions).map(|a| (c, a)));
        let Ok((conditions, actions)) = parsed else {
            tracing::error!(rule = %r.name, "правило пропущено: условия или действия не читаются");
            continue;
        };
        if !conditions.iter().all(|c| check(c, &facts, comment)) {
            continue;
        }
        let mut done = Vec::new();
        let mut recompute = false;
        for a in &actions {
            // Каждое действие — в своей точке сохранения: ошибка SQL (например, группа уже удалена)
            // иначе оборвала бы транзакцию, и заявка не создалась бы вовсе.
            let mut sp = (**tx).begin().await?;
            match run(&mut sp, t, &who, a).await {
                Ok(changed) => {
                    sp.commit().await?;
                    recompute |= changed;
                    done.push(a.kind.clone());
                }
                Err(e) => {
                    sp.rollback().await?;
                    tracing::warn!(rule = %r.name, action = %a.kind, error = %e, "действие правила не выполнено");
                }
            }
        }
        event(tx, t.id, None, "rule", json!(r.name), json!(done)).await?;
        audit_rec(tx, &who, "ticket.rule", t, json!({"rule": r.id, "actions": done})).await?;
        if recompute {
            sla::apply(tx, t.id).await?;
        }
        if r.stop {
            break;
        }
    }
    Ok(())
}

pub async fn fire(tx: &mut Transaction<'_, Postgres>, t: &T, trigger: Trigger) -> ApiResult<()> {
    fire_with(tx, t, trigger, None).await
}

fn id_of(v: &str) -> ApiResult<Uuid> {
    Uuid::parse_str(v.trim()).map_err(|_| ApiError::bad_request("invalid_rule", "Нужен идентификатор."))
}

/// Выполнить действие. `true` — изменилось поле, влияющее на SLA.
async fn run(tx: &mut Transaction<'_, Postgres>, t: &T, who: &Who, a: &Action) -> ApiResult<bool> {
    let set = |col: &'static str| {
        format!("UPDATE tickets SET {col} = $2, version = version + 1 WHERE id = $1") // SAFE: имя столбца — из литералов ниже
    };
    match a.kind.as_str() {
        "set_category" => {
            let id = id_of(&a.value)?;
            sqlx::query(&set("category_id")).bind(t.id).bind(id).execute(&mut **tx).await?;
            event(tx, t.id, None, "category", json!(t.category_id), json!(id)).await?;
            Ok(true)
        }
        "set_site" => {
            let id = id_of(&a.value)?;
            sqlx::query(&set("site_id")).bind(t.id).bind(id).execute(&mut **tx).await?;
            event(tx, t.id, None, "site", json!(t.site_id), json!(id)).await?;
            Ok(true)
        }
        "set_group" => {
            let id = id_of(&a.value)?;
            sqlx::query(&set("group_id")).bind(t.id).bind(id).execute(&mut **tx).await?;
            event(tx, t.id, None, "group", Value::Null, json!(id)).await?;
            Ok(false)
        }
        "set_priority" if sla::PRIORITIES.contains(&a.value.as_str()) => {
            sqlx::query(&set("priority")).bind(t.id).bind(&a.value).execute(&mut **tx).await?;
            event(tx, t.id, None, "priority", json!(t.priority), json!(a.value)).await?;
            Ok(true)
        }
        "set_line" if matches!(a.value.as_str(), "l1" | "l2") => {
            sqlx::query(&set("line")).bind(t.id).bind(&a.value).execute(&mut **tx).await?;
            event(tx, t.id, None, "line", json!(t.line), json!(a.value)).await?;
            Ok(false)
        }
        "add_note" if !a.value.trim().is_empty() => {
            let note = NewComment {
                body: a.value.trim().to_string(),
                internal: true,
                source: "system".into(),
                ..Default::default()
            };
            insert_comment(tx, t.id, who, &note).await?;
            Ok(false)
        }
        "assign" => {
            let u = id_of(&a.value)?;
            if !assignment::is_engineer(tx, u).await? || !assignment::is_active(tx, u).await? {
                return Err(ApiError::bad_request("invalid_rule", "Инженер неактивен."));
            }
            let mut cur: T = sqlx::query_as(&select_t("WHERE id = $1")).bind(t.id).fetch_one(&mut **tx).await?;
            if cur.assignee_id != Some(u) {
                let c = notify::ctx_tx(tx).await?;
                let status = if matches!(cur.status.as_str(), "new" | "assigned") {
                    "assigned".to_string()
                } else {
                    cur.status.clone()
                };
                set_assignee(tx, &mut cur, who, Some(u), &status, &c).await?;
            }
            Ok(false)
        }
        "notify" => {
            let users: Vec<Uuid> = match a.value.as_str() {
                "assignee" => {
                    sqlx::query_scalar("SELECT assignee_id FROM tickets WHERE id = $1 AND assignee_id IS NOT NULL")
                        .bind(t.id)
                        .fetch_all(&mut **tx)
                        .await?
                }
                "superadmins" => {
                    pult_core::notify::notify_superadmins(
                        &mut **tx,
                        "helpdesk.rule",
                        json!({"number": t.number, "title": t.title}),
                    )
                    .await?;
                    Vec::new()
                }
                v => vec![id_of(v)?],
            };
            for u in users {
                notify::bell(tx, u, "helpdesk.rule", t).await?;
            }
            Ok(false)
        }
        "add_watcher" => {
            let p = id_of(&a.value)?;
            sqlx::query("INSERT INTO ticket_watchers (ticket_id, person_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
                .bind(t.id)
                .bind(p)
                .execute(&mut **tx)
                .await?;
            event(tx, t.id, None, "watcher_added", Value::Null, json!(p)).await?;
            Ok(false)
        }
        _ => Err(ApiError::bad_request("invalid_rule", "Неизвестное действие.")),
    }
}

// ------------------------------------------------------------------ Админка

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
    HelpdeskState: FromRef<S>,
{
    OpenApiRouter::new().routes(routes!(list_rules, create_rule)).routes(routes!(update_rule, delete_rule))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct Rule {
    pub id: Uuid,
    pub name: String,
    pub trigger: String,
    #[schema(value_type = Vec<Condition>)]
    pub conditions: Value,
    #[schema(value_type = Vec<Action>)]
    pub actions: Value,
    pub is_active: bool,
    pub stop: bool,
    pub sort: i32,
}

#[derive(Deserialize, ToSchema)]
pub struct RuleInput {
    pub name: String,
    /// `created`, `updated`, `commented`, `sla_warning`, `sla_breached`.
    pub trigger: String,
    #[serde(default)]
    pub conditions: Vec<Condition>,
    pub actions: Vec<Action>,
    #[serde(default = "yes")]
    pub is_active: bool,
    /// Остановить проверку следующих правил.
    #[serde(default)]
    pub stop: bool,
    #[serde(default = "hundred")]
    pub sort: i32,
}

fn yes() -> bool {
    true
}
fn hundred() -> i32 {
    100
}

pub fn validate(b: &RuleInput) -> Result<(), String> {
    if b.name.trim().is_empty() {
        return Err("Укажите название правила.".into());
    }
    if !TRIGGERS.contains(&b.trigger.as_str()) {
        return Err("Неизвестное событие.".into());
    }
    for c in &b.conditions {
        if !FIELDS.contains(&c.field.as_str()) || !OPS.contains(&c.op.as_str()) {
            return Err(format!("Неверное условие «{} {}».", c.field, c.op));
        }
    }
    if b.actions.is_empty() {
        return Err("Добавьте хотя бы одно действие.".into());
    }
    for a in &b.actions {
        if !ACTIONS.contains(&a.kind.as_str()) {
            return Err(format!("Неизвестное действие «{}».", a.kind));
        }
        let needs_id = matches!(a.kind.as_str(), "set_category" | "set_site" | "set_group" | "assign" | "add_watcher");
        if needs_id && Uuid::parse_str(a.value.trim()).is_err() {
            return Err(format!("Действию «{}» нужен выбор из списка.", a.kind));
        }
        if a.kind == "set_priority" && !sla::PRIORITIES.contains(&a.value.as_str()) {
            return Err("Неверный приоритет.".into());
        }
        if a.kind == "set_line" && !matches!(a.value.as_str(), "l1" | "l2") {
            return Err("Линия — l1 или l2.".into());
        }
        if a.kind == "add_note" && a.value.trim().is_empty() {
            return Err("Текст заметки пуст.".into());
        }
    }
    Ok(())
}

#[utoipa::path(get, path = "/api/admin/helpdesk/rules", tag = "helpdesk-admin", responses((status = 200, body = Vec<Rule>)))]
async fn list_rules(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<Rule>>> {
    user.require(&perm::ADMIN)?;
    let rows = sqlx::query_as(
        "SELECT id, name, trigger, conditions, actions, is_active, stop, sort FROM automation_rules ORDER BY sort, name",
    )
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[utoipa::path(post, path = "/api/admin/helpdesk/rules", tag = "helpdesk-admin", request_body = RuleInput, responses((status = 201, body = Object)))]
async fn create_rule(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<RuleInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&perm::ADMIN)?;
    validate(&b).map_err(|m| ApiError::bad_request("invalid_rule", m))?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO automation_rules (name, trigger, conditions, actions, is_active, stop, sort)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(b.name.trim())
    .bind(&b.trigger)
    .bind(json!(b.conditions))
    .bind(json!(b.actions))
    .bind(b.is_active)
    .bind(b.stop)
    .bind(b.sort)
    .fetch_one(&core.db)
    .await?;
    audit::record(&core.db, &user.actor, Entry::new("helpdesk.rule.create", Source::Portal).object("rule", id)).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[utoipa::path(put, path = "/api/admin/helpdesk/rules/{id}", tag = "helpdesk-admin", params(("id" = Uuid, Path)), request_body = RuleInput, responses((status = 204)))]
async fn update_rule(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<RuleInput>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    validate(&b).map_err(|m| ApiError::bad_request("invalid_rule", m))?;
    let n = sqlx::query(
        "UPDATE automation_rules SET name = $2, trigger = $3, conditions = $4, actions = $5, is_active = $6, stop = $7,
                sort = $8, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(b.name.trim())
    .bind(&b.trigger)
    .bind(json!(b.conditions))
    .bind(json!(b.actions))
    .bind(b.is_active)
    .bind(b.stop)
    .bind(b.sort)
    .execute(&core.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("helpdesk.rule.update", Source::Portal).object("rule", id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/admin/helpdesk/rules/{id}", tag = "helpdesk-admin", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn delete_rule(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let n = sqlx::query("DELETE FROM automation_rules WHERE id = $1").bind(id).execute(&core.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("helpdesk.rule.delete", Source::Portal).object("rule", id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            title: "Не печатает ПРИНТЁР".into(),
            description: "".into(),
            category_id: None,
            site_id: None,
            priority: "normal".into(),
            status: "new".into(),
            channel: "portal".into(),
            line: "l1".into(),
            assignee_id: None,
            group_id: None,
            requester_vip: false,
            requester_department: None,
        }
    }

    #[test]
    fn conditions() {
        let c =
            |field: &str, op: &str, value: &str| Condition { field: field.into(), op: op.into(), value: value.into() };
        assert!(check(&c("title", "contains", "принтер"), &facts(), None), "регистр и ё не важны");
        assert!(!check(&c("title", "contains", ""), &facts(), None), "пустое «содержит» не срабатывает");
        assert!(check(&c("priority", "equals", "normal"), &facts(), None));
        assert!(check(&c("category_id", "is_empty", ""), &facts(), None));
        assert!(check(&c("comment", "contains", "срочно"), &facts(), Some("Очень СРОЧНО")));
        assert!(!check(&c("unknown", "equals", ""), &facts(), None));
    }

    #[test]
    fn validation() {
        let rule = |actions: Vec<Action>| RuleInput {
            name: "r".into(),
            trigger: "created".into(),
            conditions: vec![],
            actions,
            is_active: true,
            stop: false,
            sort: 1,
        };
        let a = |k: &str, v: &str| Action { kind: k.into(), value: v.into() };
        assert!(validate(&rule(vec![a("set_priority", "high")])).is_ok());
        assert!(validate(&rule(vec![])).is_err());
        assert!(validate(&rule(vec![a("set_category", "принтеры")])).is_err());
        assert!(validate(&rule(vec![a("run_script", "rm -rf")])).is_err());
    }
}
