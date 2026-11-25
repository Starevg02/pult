//! Проблемы и изменения (по образцу GLPI). Это заявки другого типа: исполнитель, лента, чек-лист,
//! оборудование и наблюдатели — общие с обращениями, своё — причина и обходное решение проблемы,
//! план, откат, окно и согласование изменения. Создают и ведут только ИТ-специалисты.
//!
//! Согласование изменения — по правилам «категория + начиная с риска → согласующий». Подходят все
//! совпавшие правила; если ни одно не подошло, изменение согласовано сразу (стандартное изменение).
//! Отклонение одним согласующим возвращает изменение на доработку; правка согласованного или
//! ожидающего решения изменения начинает согласование заново: согласуют то, что будут выполнять.

use crate::{
    notify, perm,
    tickets::{self, NewTicket, Resolution, T, Who, bad_state, select_t},
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
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Инцидентов, решаемых вместе с проблемой за один раз.
const MAX_RESOLVE: i64 = 100;
const RISKS: &[&str] = &["low", "medium", "high"];
const LONG_TEXT: usize = 20_000;

const USER_NAME: &str =
    "coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name)), ''), a.display_name, a.sam_account_name)";

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(create_record))
        .routes(routes!(get_record, update_record))
        .routes(routes!(submit_change))
        .routes(routes!(link_incident))
        .routes(routes!(unlink_incident))
        .routes(routes!(resolve_incidents))
        .routes(routes!(my_change_approvals))
        .routes(routes!(decide_change))
        .routes(routes!(list_change_rules, create_change_rule))
        .routes(routes!(delete_change_rule))
}

fn bad(m: &str) -> ApiError {
    ApiError::bad_request("invalid_itil", m.to_string())
}

fn long(s: &str, what: &str) -> ApiResult<String> {
    let s = s.trim();
    if s.chars().count() > LONG_TEXT {
        return Err(bad(what));
    }
    Ok(s.to_string())
}

fn risk_ok(r: &str) -> ApiResult<()> {
    if RISKS.contains(&r) {
        Ok(())
    } else {
        Err(bad("Риск — низкий, средний или высокий."))
    }
}

/// Изменение выполняется только согласованным: решить его до согласования нельзя.
pub(crate) async fn ensure_approved(tx: &mut Transaction<'_, Postgres>, t: &T) -> ApiResult<()> {
    let a: Option<String> = sqlx::query_scalar("SELECT approval FROM ticket_changes WHERE ticket_id = $1")
        .bind(t.id)
        .fetch_optional(&mut **tx)
        .await?;
    match a.as_deref() {
        None | Some("approved") => Ok(()),
        Some(_) => Err(bad_state("Изменение не согласовано: выполнить его можно после согласования.")),
    }
}

/// Заявка проблемы или изменения под блокировкой; другого типа — как несуществующая.
async fn lock_kind(tx: &mut Transaction<'_, Postgres>, number: i64, who: &Who, kind: &str) -> ApiResult<T> {
    let t = tickets::lock(tx, number, who).await?;
    let k: String =
        sqlx::query_scalar("SELECT kind FROM tickets WHERE id = $1").bind(t.id).fetch_one(&mut **tx).await?;
    if k == kind { Ok(t) } else { Err(ApiError::NotFound) }
}

/// Изменение отменено: незавершённые согласования снимаются, у согласующих они больше не висят.
pub(crate) async fn abandon(tx: &mut Transaction<'_, Postgres>, ticket: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE change_approvals SET status = 'skipped' WHERE ticket_id = $1 AND status = 'pending'")
        .bind(ticket)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE ticket_changes SET approval = 'draft' WHERE ticket_id = $1 AND approval = 'pending'")
        .bind(ticket)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Сменилась категория изменения: правила и согласующие по ней могли быть другими. Отправленное или
/// согласованное — снова черновик, незавершённые решения снимаются.
pub(crate) async fn reset_on_category(
    tx: &mut Transaction<'_, Postgres>,
    t: &mut T,
    who: &Who,
) -> Result<(), sqlx::Error> {
    let old: Option<String> = sqlx::query_scalar(
        "SELECT approval FROM ticket_changes WHERE ticket_id = $1 AND approval IN ('pending', 'approved') FOR UPDATE",
    )
    .bind(t.id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(old) = old else { return Ok(()) };
    sqlx::query("UPDATE ticket_changes SET approval = 'draft' WHERE ticket_id = $1")
        .bind(t.id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE change_approvals SET status = 'skipped' WHERE ticket_id = $1 AND status = 'pending'")
        .bind(t.id)
        .execute(&mut **tx)
        .await?;
    tickets::event(tx, t.id, who.user, "change_reset", json!(old), json!("draft")).await?;
    back_to_work(tx, t, who).await
}

/// После согласования (или отказа) — снова в работу.
async fn back_to_work(tx: &mut Transaction<'_, Postgres>, t: &mut T, who: &Who) -> Result<(), sqlx::Error> {
    if t.status == "waiting_approval" {
        let to = if t.assignee_id.is_some() { "in_progress" } else { "new" };
        tickets::set_status(tx, t, who, to).await?;
    }
    Ok(())
}

// ------------------------------------------------------------------ Создание

#[derive(Deserialize, ToSchema)]
pub struct NewRecord {
    /// `problem` или `change`.
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub category_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    /// `low`, `normal`, `high`, `critical`; пусто — по категории.
    pub priority: Option<String>,
    /// Изменение: `low`, `medium`, `high`.
    pub risk: Option<String>,
    /// Проблема, заведённая из инцидента: его номер.
    pub incident: Option<i64>,
    #[serde(default)]
    pub asset_ids: Vec<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedRecord {
    pub number: i64,
}

/// Завести проблему или изменение. Исполнитель — автор.
#[utoipa::path(post, path = "/api/helpdesk/itil", tag = "helpdesk", request_body = NewRecord,
    responses((status = 201, body = CreatedRecord), (status = 400, body = pult_core::error::ErrorBody)))]
async fn create_record(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<NewRecord>,
) -> ApiResult<(StatusCode, Json<CreatedRecord>)> {
    user.require(&perm::WORK)?;
    if !matches!(b.kind.as_str(), "problem" | "change") {
        return Err(bad("Тип — проблема или изменение."));
    }
    let risk = b.risk.clone().unwrap_or_else(|| "low".into());
    risk_ok(&risk)?;
    if let Some(p) = &b.priority
        && !matches!(p.as_str(), "low" | "normal" | "high" | "critical")
    {
        return Err(bad("Нет такого приоритета."));
    }
    let who = Who::from_user(&user);
    // Инцидент проверяется до создания: иначе ошибка связи пришла бы после коммита, и повтор запроса
    // завёл бы вторую проблему.
    if let (Some(incident), "problem") = (b.incident, b.kind.as_str()) {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM tickets WHERE number = $1 AND kind IN ('incident', 'request'))",
        )
        .bind(incident)
        .fetch_one(&core.db)
        .await?;
        if !ok {
            return Err(bad("Инцидент не найден."));
        }
    }
    let created = tickets::create(
        &core,
        &who,
        NewTicket {
            title: b.title,
            description: b.description,
            category_id: b.category_id,
            site_id: b.site_id,
            channel: "portal".into(),
            form_answers: json!({}),
            prefer_assignee: Some(user.user_id),
            asset_ids: b.asset_ids,
            kind: Some(b.kind.clone()),
            risk: (b.kind == "change").then_some(risk),
            priority: b.priority,
            ..Default::default()
        },
    )
    .await?;
    // Запись уже создана: связь с инцидентом — вдогонку, её сбой (инцидент удалили за эти мгновения)
    // не превращает созданную проблему в ошибку.
    if let (Some(incident), "problem") = (b.incident, b.kind.as_str())
        && let Err(e) = link(&core, &who, created.number, incident).await
    {
        tracing::warn!(problem = created.number, incident, error = ?e, "проблема создана, но инцидент не связан");
    }
    Ok((StatusCode::CREATED, Json(CreatedRecord { number: created.number })))
}

// ------------------------------------------------------------------ Просмотр и правка

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct LinkedIncident {
    pub number: i64,
    pub title: String,
    pub status: String,
}

#[derive(Serialize, ToSchema)]
pub struct ProblemView {
    pub cause: String,
    pub workaround: String,
    pub known_error: bool,
    pub incidents: Vec<LinkedIncident>,
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ApprovalView {
    pub id: Uuid,
    pub round: i32,
    pub approver_name: Option<String>,
    /// `pending`, `approved`, `rejected`, `skipped`.
    pub status: String,
    pub comment: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub decided_at: Option<OffsetDateTime>,
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ChangeView {
    pub risk: String,
    pub plan: String,
    pub rollback: String,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub planned_start: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub planned_end: Option<OffsetDateTime>,
    /// `draft`, `pending`, `approved`, `rejected`.
    pub approval: String,
    pub round: i32,
    #[sqlx(skip)]
    pub approvals: Vec<ApprovalView>,
}

#[derive(Serialize, ToSchema)]
pub struct ItilRecord {
    pub kind: String,
    pub problem: Option<ProblemView>,
    pub change: Option<ChangeView>,
}

/// Сведения проблемы или изменения.
#[utoipa::path(get, path = "/api/helpdesk/itil/{number}", tag = "helpdesk", params(("number" = i64, Path)),
    responses((status = 200, body = ItilRecord)))]
async fn get_record(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
) -> ApiResult<Json<ItilRecord>> {
    user.require(&perm::WORK)?;
    let (id, kind): (Uuid, String) = sqlx::query_as("SELECT id, kind FROM tickets WHERE number = $1")
        .bind(number)
        .fetch_optional(&core.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut rec = ItilRecord { kind: kind.clone(), problem: None, change: None };
    match kind.as_str() {
        "problem" => {
            let (cause, workaround, known_error): (String, String, bool) =
                sqlx::query_as("SELECT cause, workaround, known_error FROM ticket_problems WHERE ticket_id = $1")
                    .bind(id)
                    .fetch_optional(&core.db)
                    .await?
                    .unwrap_or_default();
            let incidents = sqlx::query_as(
                "SELECT number, title, status FROM tickets WHERE problem_id = $1 ORDER BY number DESC LIMIT 1000",
            )
            .bind(id)
            .fetch_all(&core.db)
            .await?;
            rec.problem = Some(ProblemView { cause, workaround, known_error, incidents });
        }
        "change" => {
            let mut c: ChangeView = sqlx::query_as(
                "SELECT risk, plan, rollback, planned_start, planned_end, approval, round FROM ticket_changes WHERE ticket_id = $1",
            )
            .bind(id)
            .fetch_one(&core.db)
            .await?;
            c.approvals = sqlx::query_as(&format!(
                "SELECT ca.id, ca.round, {USER_NAME} AS approver_name, ca.status, ca.comment, ca.decided_at
                   FROM change_approvals ca
                   JOIN users u ON u.id = ca.approver_id JOIN ad_accounts a ON a.id = u.ad_account_id
                   LEFT JOIN persons p ON p.id = u.person_id
                  WHERE ca.ticket_id = $1 ORDER BY ca.round DESC, ca.created_at" // SAFE: константа
            ))
            .bind(id)
            .fetch_all(&core.db)
            .await?;
            rec.change = Some(c);
        }
        _ => {}
    }
    Ok(Json(rec))
}

/// Полная замена своих полей: у проблемы — причина, обходное решение, «известная ошибка»;
/// у изменения — риск, план, откат и окно работ.
#[derive(Deserialize, ToSchema, Default)]
#[serde(default)]
pub struct RecordUpdate {
    pub cause: String,
    pub workaround: String,
    pub known_error: bool,
    pub risk: String,
    pub plan: String,
    pub rollback: String,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub planned_start: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub planned_end: Option<OffsetDateTime>,
}

#[utoipa::path(put, path = "/api/helpdesk/itil/{number}", tag = "helpdesk", params(("number" = i64, Path)),
    request_body = RecordUpdate, responses((status = 204), (status = 400, body = pult_core::error::ErrorBody)))]
async fn update_record(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<RecordUpdate>,
) -> ApiResult<StatusCode> {
    user.require(&perm::WORK)?;
    let who = Who::from_user(&user);
    let mut tx = core.db.begin().await?;
    let mut t = tickets::lock(&mut tx, number, &who).await?;
    if !tickets::is_open(&t.status) {
        return Err(bad_state("Заявка уже решена или закрыта."));
    }
    let kind: String =
        sqlx::query_scalar("SELECT kind FROM tickets WHERE id = $1").bind(t.id).fetch_one(&mut *tx).await?;
    match kind.as_str() {
        "problem" => {
            let cause = long(&b.cause, "Причина слишком длинная.")?;
            let workaround = long(&b.workaround, "Обходное решение слишком длинное.")?;
            if b.known_error && workaround.is_empty() {
                return Err(bad("Известная ошибка — с обходным решением: опишите его."));
            }
            sqlx::query(
                "UPDATE ticket_problems SET cause = $2, workaround = $3, known_error = $4 WHERE ticket_id = $1",
            )
            .bind(t.id)
            .bind(&cause)
            .bind(&workaround)
            .bind(b.known_error)
            .execute(&mut *tx)
            .await?;
            tickets::audit_rec(&mut tx, &who, "itil.problem.update", &t, json!({"known_error": b.known_error})).await?;
        }
        "change" => {
            risk_ok(&b.risk)?;
            let plan = long(&b.plan, "План слишком длинный.")?;
            let rollback = long(&b.rollback, "План отката слишком длинный.")?;
            if let (Some(s), Some(e)) = (b.planned_start, b.planned_end)
                && e < s
            {
                return Err(bad("Окончание работ — не раньше начала."));
            }
            type Old = (String, String, String, Option<OffsetDateTime>, Option<OffsetDateTime>, String, i32);
            let old: Old = sqlx::query_as(
                "SELECT risk, plan, rollback, planned_start, planned_end, approval, round FROM ticket_changes
                  WHERE ticket_id = $1 FOR UPDATE",
            )
            .bind(t.id)
            .fetch_one(&mut *tx)
            .await?;
            let same =
                (&old.0, &old.1, &old.2, old.3, old.4) == (&b.risk, &plan, &rollback, b.planned_start, b.planned_end);
            if same {
                return Ok(StatusCode::NO_CONTENT);
            }
            // Согласуют конкретный план: правка на согласовании или после — согласование заново.
            let reset = matches!(old.5.as_str(), "pending" | "approved");
            sqlx::query(
                "UPDATE ticket_changes SET risk = $2, plan = $3, rollback = $4, planned_start = $5, planned_end = $6,
                        approval = CASE WHEN $7 THEN 'draft' ELSE approval END WHERE ticket_id = $1",
            )
            .bind(t.id)
            .bind(&b.risk)
            .bind(&plan)
            .bind(&rollback)
            .bind(b.planned_start)
            .bind(b.planned_end)
            .bind(reset)
            .execute(&mut *tx)
            .await?;
            if reset {
                sqlx::query(
                    "UPDATE change_approvals SET status = 'skipped' WHERE ticket_id = $1 AND status = 'pending'",
                )
                .bind(t.id)
                .execute(&mut *tx)
                .await?;
                tickets::event(&mut tx, t.id, who.user, "change_reset", json!(old.5), json!("draft")).await?;
                back_to_work(&mut tx, &mut t, &who).await?;
            }
            tickets::audit_rec(
                &mut tx,
                &who,
                "itil.change.update",
                &t,
                json!({"risk": {"before": old.0, "after": b.risk}, "reset": reset}),
            )
            .await?;
        }
        _ => return Err(ApiError::NotFound),
    }
    tickets::ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Согласование изменения

#[derive(Serialize, ToSchema)]
pub struct ChangeSubmitted {
    /// `pending` — ждёт согласующих; `approved` — правил нет, согласовано сразу.
    pub approval: String,
    pub approvers: usize,
}

/// Отправить изменение на согласование.
#[utoipa::path(post, path = "/api/helpdesk/itil/{number}/submit", tag = "helpdesk", params(("number" = i64, Path)),
    responses((status = 200, body = ChangeSubmitted), (status = 400, body = pult_core::error::ErrorBody)))]
async fn submit_change(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
) -> ApiResult<Json<ChangeSubmitted>> {
    user.require(&perm::WORK)?;
    let who = Who::from_user(&user);
    let mut tx = core.db.begin().await?;
    let mut t = lock_kind(&mut tx, number, &who, "change").await?;
    if !tickets::is_open(&t.status) {
        return Err(bad_state("Заявка уже решена или закрыта."));
    }
    // После согласования изменение возвращается «в работу»: отложенное или ждущее ответа потеряло бы
    // своё состояние и напоминания.
    if !matches!(t.status.as_str(), "new" | "assigned" | "in_progress") {
        return Err(bad_state("Отправить на согласование можно изменение в работе: сначала верните его в работу."));
    }
    let (risk, plan, rollback, approval, round): (String, String, String, String, i32) = sqlx::query_as(
        "SELECT risk, plan, rollback, approval, round FROM ticket_changes WHERE ticket_id = $1 FOR UPDATE",
    )
    .bind(t.id)
    .fetch_one(&mut *tx)
    .await?;
    if !matches!(approval.as_str(), "draft" | "rejected") {
        return Err(bad_state("Изменение уже на согласовании или согласовано."));
    }
    if plan.is_empty() {
        return Err(bad("Опишите план работ."));
    }
    if risk != "low" && rollback.is_empty() {
        return Err(bad("Для среднего и высокого риска опишите план отката."));
    }
    let (category, created_by, requester, assignee): (Option<Uuid>, Option<Uuid>, Option<Uuid>, Option<Uuid>) =
        sqlx::query_as("SELECT category_id, created_by, requester_user_id, assignee_id FROM tickets WHERE id = $1")
            .bind(t.id)
            .fetch_one(&mut *tx)
            .await?;
    // Своё изменение не согласует никто из причастных: отправивший, зарегистрировавший, заявитель и исполнитель.
    // Иначе автор попросил бы коллегу нажать «Отправить» и согласовал бы сам.
    let own = |a: &Uuid| [who.user, created_by, requester, assignee].contains(&Some(*a));
    // Все подходящие правила.
    // Отключённые согласующие тоже считаются: правило подошло, и без решения человека изменение не пройдёт.
    let rules: Vec<(Uuid, bool)> = sqlx::query_as(
        "SELECT DISTINCT r.approver_id, u.is_active FROM change_approval_rules r JOIN users u ON u.id = r.approver_id
          WHERE (r.category_id IS NULL OR r.category_id = $1)
            AND array_position(ARRAY['low','medium','high'], r.min_risk) <= array_position(ARRAY['low','medium','high'], $2)",
    )
    .bind(category)
    .bind(&risk)
    .fetch_all(&mut *tx)
    .await?;
    let approvers: Vec<Uuid> = rules.iter().filter(|(a, active)| *active && !own(a)).map(|(a, _)| *a).collect();
    if !rules.is_empty() && approvers.is_empty() {
        let only_author = rules.iter().any(|(a, active)| *active && own(a));
        return Err(bad(if only_author {
            "Согласовать своё изменение нельзя: попросите администратора добавить другого согласующего."
        } else {
            "Согласующий по правилу недоступен (учётка отключена): попросите администратора назначить другого."
        }));
    }
    let round = round + 1;
    let state = if approvers.is_empty() { "approved" } else { "pending" };
    sqlx::query("UPDATE ticket_changes SET approval = $2, round = $3 WHERE ticket_id = $1")
        .bind(t.id)
        .bind(state)
        .bind(round)
        .execute(&mut *tx)
        .await?;
    for a in &approvers {
        sqlx::query("INSERT INTO change_approvals (ticket_id, round, approver_id) VALUES ($1, $2, $3)")
            .bind(t.id)
            .bind(round)
            .bind(a)
            .execute(&mut *tx)
            .await?;
        notify::bell(&mut tx, *a, "helpdesk.change_approval", &t).await?;
    }
    tickets::event(&mut tx, t.id, who.user, "change_submitted", json!(approval), json!(state)).await?;
    if state == "pending" {
        tickets::set_status(&mut tx, &mut t, &who, "waiting_approval").await?;
    }
    tickets::audit_rec(&mut tx, &who, "itil.change.submit", &t, json!({"round": round, "approvers": approvers.len()}))
        .await?;
    tickets::ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(Json(ChangeSubmitted { approval: state.into(), approvers: approvers.len() }))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ApprovalItem {
    pub id: Uuid,
    /// `pending`, `approved`, `rejected`, `skipped`.
    pub status: String,
    pub comment: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub decided_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    pub number: i64,
    pub title: String,
    pub description: String,
    pub category_name: Option<String>,
    pub risk: String,
    pub plan: String,
    pub rollback: String,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub planned_start: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub planned_end: Option<OffsetDateTime>,
    /// Кто ведёт изменение.
    pub assignee_name: Option<String>,
}

/// Мои согласования изменений: ожидающие и решённые за 30 дней. Согласующему не нужен доступ к заявкам.
#[utoipa::path(get, path = "/api/helpdesk/change-approvals", tag = "helpdesk",
    responses((status = 200, body = Vec<ApprovalItem>)))]
async fn my_change_approvals(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<ApprovalItem>>> {
    let rows = sqlx::query_as(&format!(
        "SELECT ca.id, ca.status, ca.comment, ca.decided_at, ca.created_at, t.number, t.title, t.description,
                cat.name AS category_name, c.risk, c.plan, c.rollback, c.planned_start, c.planned_end,
                (SELECT {USER_NAME} FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id
                   LEFT JOIN persons p ON p.id = u.person_id WHERE u.id = t.assignee_id) AS assignee_name
           FROM change_approvals ca
           JOIN tickets t ON t.id = ca.ticket_id
           JOIN ticket_changes c ON c.ticket_id = ca.ticket_id
           LEFT JOIN ticket_categories cat ON cat.id = t.category_id
          WHERE ca.approver_id = $1 AND ca.status <> 'skipped'
            AND (ca.status = 'pending' OR ca.decided_at > now() - interval '30 days')
          ORDER BY ca.status = 'pending' DESC, ca.created_at DESC LIMIT 200" // SAFE: константа
    ))
    .bind(user.user_id)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize, ToSchema)]
pub struct ChangeDecision {
    pub approve: bool,
    /// Обязателен при отказе.
    pub comment: Option<String>,
}

/// Решение согласующего.
#[utoipa::path(post, path = "/api/helpdesk/change-approvals/{id}", tag = "helpdesk", params(("id" = Uuid, Path)),
    request_body = ChangeDecision, responses((status = 204), (status = 400, body = pult_core::error::ErrorBody)))]
async fn decide_change(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ChangeDecision>,
) -> ApiResult<StatusCode> {
    let comment: Option<String> =
        b.comment.as_deref().map(str::trim).filter(|c| !c.is_empty()).map(|c| c.chars().take(4000).collect());
    if !b.approve && comment.is_none() {
        return Err(bad("Напишите, почему изменение не согласовано."));
    }
    let ticket: Uuid = sqlx::query_scalar("SELECT ticket_id FROM change_approvals WHERE id = $1 AND approver_id = $2")
        .bind(id)
        .bind(user.user_id)
        .fetch_optional(&core.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let who = Who::from_user(&user);
    let mut tx = core.db.begin().await?;
    // Порядок блокировок — как у правки и отправки: заявка, изменение, решения.
    let mut t: T = sqlx::query_as(&select_t("WHERE id = $1 FOR UPDATE")).bind(ticket).fetch_one(&mut *tx).await?;
    let (approval, round): (String, i32) =
        sqlx::query_as("SELECT approval, round FROM ticket_changes WHERE ticket_id = $1 FOR UPDATE")
            .bind(ticket)
            .fetch_one(&mut *tx)
            .await?;
    let (status, my_round): (String, i32) =
        sqlx::query_as("SELECT status, round FROM change_approvals WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if status != "pending" || approval != "pending" || my_round != round || !tickets::is_open(&t.status) {
        return Err(bad_state("Это согласование уже завершено."));
    }
    sqlx::query("UPDATE change_approvals SET status = $2, comment = $3, decided_at = now() WHERE id = $1")
        .bind(id)
        .bind(if b.approve { "approved" } else { "rejected" })
        .bind(&comment)
        .execute(&mut *tx)
        .await?;
    let finished = if b.approve {
        let left: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM change_approvals WHERE ticket_id = $1 AND round = $2 AND status = 'pending'",
        )
        .bind(ticket)
        .bind(round)
        .fetch_one(&mut *tx)
        .await?;
        (left == 0).then_some("approved")
    } else {
        sqlx::query(
            "UPDATE change_approvals SET status = 'skipped' WHERE ticket_id = $1 AND round = $2 AND status = 'pending'",
        )
        .bind(ticket)
        .bind(round)
        .execute(&mut *tx)
        .await?;
        Some("rejected")
    };
    let mut d = json!({"approve": b.approve});
    if let Some(c) = &comment {
        d["comment"] = json!(c);
    }
    tickets::event(&mut tx, ticket, who.user, "change_decision", Value::Null, d).await?;
    if let Some(state) = finished {
        sqlx::query("UPDATE ticket_changes SET approval = $2 WHERE ticket_id = $1")
            .bind(ticket)
            .bind(state)
            .execute(&mut *tx)
            .await?;
        tickets::event(&mut tx, ticket, who.user, "change_submitted", json!("pending"), json!(state)).await?;
        back_to_work(&mut tx, &mut t, &who).await?;
        if let Some(a) = t.assignee_id {
            notify::bell(&mut tx, a, "helpdesk.change_decided", &t).await?;
        }
    }
    tickets::audit_rec(&mut tx, &who, "itil.change.decide", &t, json!({"approve": b.approve, "round": round})).await?;
    tickets::ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Инциденты проблемы

#[derive(Deserialize, ToSchema)]
pub struct IncidentRef {
    pub number: i64,
}

async fn link(core: &Core, who: &Who, problem: i64, incident: i64) -> ApiResult<()> {
    let mut tx = core.db.begin().await?;
    let p = lock_kind(&mut tx, problem, who, "problem").await?;
    if matches!(p.status.as_str(), "closed" | "canceled") {
        return Err(bad_state("Проблема закрыта."));
    }
    let row: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT id, problem_id FROM tickets WHERE number = $1 AND kind IN ('incident', 'request') FOR UPDATE",
    )
    .bind(incident)
    .fetch_optional(&mut *tx)
    .await?;
    let (id, was) = row.ok_or_else(|| bad("Инцидент не найден."))?;
    if was == Some(p.id) {
        return Ok(());
    }
    sqlx::query("UPDATE tickets SET problem_id = $2 WHERE id = $1").bind(id).bind(p.id).execute(&mut *tx).await?;
    let before: Option<i64> = match was {
        Some(w) => {
            sqlx::query_scalar("SELECT number FROM tickets WHERE id = $1").bind(w).fetch_optional(&mut *tx).await?
        }
        None => None,
    };
    if let Some(n) = before {
        tickets::event(&mut tx, id, who.user, "problem_unlinked", json!(n), Value::Null).await?;
    }
    tickets::event(&mut tx, id, who.user, "problem_linked", Value::Null, json!(p.number)).await?;
    tickets::audit_rec(&mut tx, who, "itil.problem.link", &p, json!({"incident": incident, "before": before})).await?;
    tickets::ping(&mut tx, &p).await?;
    let inc: T = sqlx::query_as(&select_t("WHERE id = $1")).bind(id).fetch_one(&mut *tx).await?;
    tickets::ping(&mut tx, &inc).await?;
    tx.commit().await?;
    Ok(())
}

/// Связать инцидент с проблемой (прежняя связь инцидента заменяется).
#[utoipa::path(post, path = "/api/helpdesk/itil/{number}/incidents", tag = "helpdesk", params(("number" = i64, Path)),
    request_body = IncidentRef, responses((status = 204), (status = 400, body = pult_core::error::ErrorBody)))]
async fn link_incident(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<IncidentRef>,
) -> ApiResult<StatusCode> {
    user.require(&perm::WORK)?;
    link(&core, &Who::from_user(&user), number, b.number).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/helpdesk/itil/{number}/incidents/{incident}", tag = "helpdesk",
    params(("number" = i64, Path), ("incident" = i64, Path)), responses((status = 204)))]
async fn unlink_incident(
    State(core): State<Core>,
    user: CurrentUser,
    Path((number, incident)): Path<(i64, i64)>,
) -> ApiResult<StatusCode> {
    user.require(&perm::WORK)?;
    let who = Who::from_user(&user);
    let mut tx = core.db.begin().await?;
    let p = lock_kind(&mut tx, number, &who, "problem").await?;
    let id: Uuid =
        sqlx::query_scalar("UPDATE tickets SET problem_id = NULL WHERE number = $1 AND problem_id = $2 RETURNING id")
            .bind(incident)
            .bind(p.id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApiError::NotFound)?;
    tickets::event(&mut tx, id, who.user, "problem_unlinked", json!(p.number), Value::Null).await?;
    tickets::audit_rec(&mut tx, &who, "itil.problem.unlink", &p, json!({"incident": incident})).await?;
    tickets::ping(&mut tx, &p).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct ResolveAll {
    /// Ответ заявителям инцидентов.
    pub body: String,
    #[serde(default)]
    pub closure_code: String,
}

#[derive(Serialize, ToSchema)]
pub struct ResolvedAll {
    pub resolved: usize,
    /// Номера, которые решить не удалось (например, не закрыт чек-лист).
    pub failed: Vec<i64>,
}

/// Решить все открытые инциденты проблемы одним ответом — каждый как обычное решение.
#[utoipa::path(post, path = "/api/helpdesk/itil/{number}/resolve-incidents", tag = "helpdesk",
    params(("number" = i64, Path)), request_body = ResolveAll, responses((status = 200, body = ResolvedAll)))]
async fn resolve_incidents(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
    Json(b): Json<ResolveAll>,
) -> ApiResult<Json<ResolvedAll>> {
    user.require(&perm::WORK)?;
    if b.body.trim().is_empty() {
        return Err(ApiError::bad_request(
            "comment_required",
            "Напишите, как решены инциденты: заявители получат это письмом.",
        ));
    }
    let who = Who::from_user(&user);
    {
        let mut tx = core.db.begin().await?;
        lock_kind(&mut tx, number, &who, "problem").await?;
        tx.rollback().await?;
    }
    let numbers: Vec<i64> = sqlx::query_scalar(
        "SELECT i.number FROM tickets i JOIN tickets p ON p.id = i.problem_id
          WHERE p.number = $1 AND p.kind = 'problem' AND i.status = ANY($2) ORDER BY i.number LIMIT $3",
    )
    .bind(number)
    .bind(tickets::OPEN_STATUSES)
    .bind(MAX_RESOLVE)
    .fetch_all(&core.db)
    .await?;
    let mut res = ResolvedAll { resolved: 0, failed: Vec::new() };
    for n in numbers {
        let r = Resolution { body: &b.body, files: &[], closure_code: &b.closure_code, minutes: None };
        match tickets::resolve(&core, &who, n, r).await {
            Ok(()) => res.resolved += 1,
            // Часть инцидентов уже решена и письма ушли: сбой одного не отменяет ответ о прочих.
            Err(ApiError::Internal(e)) => {
                tracing::error!(problem = number, incident = n, error = %e, "инцидент проблемы не решён");
                res.failed.push(n);
            }
            Err(_) => res.failed.push(n),
        }
    }
    Ok(Json(res))
}

// ------------------------------------------------------------------ Правила согласования

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ApprovalRule {
    pub id: Uuid,
    pub category_id: Option<Uuid>,
    pub category_name: Option<String>,
    pub min_risk: String,
    pub approver_id: Uuid,
    pub approver_name: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct ApprovalRuleInput {
    /// Пусто — любая категория.
    pub category_id: Option<Uuid>,
    /// Правило действует начиная с этого риска.
    pub min_risk: String,
    /// Согласующий — сотрудник из справочника с учёткой портала (руководитель, владелец сервиса).
    pub person_id: Uuid,
}

#[utoipa::path(get, path = "/api/helpdesk/change-rules", tag = "helpdesk", responses((status = 200, body = Vec<ApprovalRule>)))]
async fn list_change_rules(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<ApprovalRule>>> {
    user.require(&perm::ADMIN)?;
    let rows = sqlx::query_as(&format!(
        "SELECT r.id, r.category_id, c.name AS category_name, r.min_risk, r.approver_id, {USER_NAME} AS approver_name
           FROM change_approval_rules r
           LEFT JOIN ticket_categories c ON c.id = r.category_id
           JOIN users u ON u.id = r.approver_id JOIN ad_accounts a ON a.id = u.ad_account_id
           LEFT JOIN persons p ON p.id = u.person_id
          ORDER BY c.name NULLS FIRST, array_position(ARRAY['low','medium','high'], r.min_risk), approver_name" // SAFE: константа
    ))
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[utoipa::path(post, path = "/api/helpdesk/change-rules", tag = "helpdesk", request_body = ApprovalRuleInput,
    responses((status = 201, body = Object), (status = 400, body = pult_core::error::ErrorBody)))]
async fn create_change_rule(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<ApprovalRuleInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&perm::ADMIN)?;
    risk_ok(&b.min_risk)?;
    let approver: Uuid = sqlx::query_scalar(
        "SELECT id FROM users WHERE person_id = $1 AND is_active ORDER BY last_login_at DESC NULLS LAST LIMIT 1",
    )
    .bind(b.person_id)
    .fetch_optional(&core.db)
    .await?
    .ok_or_else(|| bad("У сотрудника нет учётки в портале: согласовать он не сможет."))?;
    if let Some(c) = b.category_id {
        let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ticket_categories WHERE id = $1)")
            .bind(c)
            .fetch_one(&core.db)
            .await?;
        if !ok {
            return Err(bad("Категория не найдена."));
        }
    }
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO change_approval_rules (category_id, min_risk, approver_id, created_by) VALUES ($1, $2, $3, $4)
         ON CONFLICT DO NOTHING RETURNING id",
    )
    .bind(b.category_id)
    .bind(&b.min_risk)
    .bind(approver)
    .bind(user.user_id)
    .fetch_optional(&core.db)
    .await?;
    let id = id.ok_or_else(|| bad("Такое правило уже есть."))?;
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("itil.rule.create", Source::Portal)
            .object("change_rule", id)
            .details(json!({"category_id": b.category_id, "min_risk": b.min_risk, "approver_id": approver})),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[utoipa::path(delete, path = "/api/helpdesk/change-rules/{id}", tag = "helpdesk", params(("id" = Uuid, Path)),
    responses((status = 204)))]
async fn delete_change_rule(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let n = sqlx::query("DELETE FROM change_approval_rules WHERE id = $1")
        .bind(id)
        .execute(&core.db)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("itil.rule.delete", Source::Portal).object("change_rule", id))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
