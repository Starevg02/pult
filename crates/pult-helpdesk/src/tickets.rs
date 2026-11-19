//! Заявки: создание, лента, статусы, передача. HTTP — в `api.rs`.

use crate::{
    assignment,
    notify::{self, Ctx, Msg},
    perm,
    schedule::{self, AfterHours},
    settings::HelpdeskSettings,
};
use pult_core::l10n::tr;
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser,
    audit::{self, Actor, Entry, Source},
    crypto::{random_token, sha256},
    events::{self, Event},
    settings,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

/// Язык сотрудника — его учётки портала, иначе язык установки (для текстов фоновых задач).
pub async fn person_lang(core: &Core, person: Option<Uuid>) -> ApiResult<pult_core::l10n::Lang> {
    let locale: Option<String> = match person {
        Some(p) => {
            sqlx::query_scalar(
                "SELECT locale FROM users WHERE person_id = $1 ORDER BY last_login_at DESC NULLS LAST LIMIT 1",
            )
            .bind(p)
            .fetch_optional(&core.db)
            .await?
        }
        None => None,
    };
    Ok(match locale {
        Some(l) => pult_core::l10n::Lang::parse(&l),
        None => pult_core::l10n::default_lang(&core.db).await,
    })
}

/// Заявка, какой её видит логика.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct T {
    pub id: Uuid,
    pub number: i64,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub category_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub requester_id: Option<Uuid>,
    pub requester_user_id: Option<Uuid>,
    pub assignee_id: Option<Uuid>,
    pub line: String,
    pub parent_ticket_id: Option<Uuid>,
    pub is_major: bool,
    pub first_response_at: Option<OffsetDateTime>,
    pub taken_at: Option<OffsetDateTime>,
    pub version: i32,
}

pub const T_COLS: &str = "id, number, title, status, priority, category_id, site_id, requester_id, requester_user_id, \
    assignee_id, line, parent_ticket_id, is_major, first_response_at, taken_at, version";

/// Запрос заявки: список столбцов — константа, условие — литерал в коде.
pub fn select_t(condition: &str) -> String {
    format!("SELECT {T_COLS} FROM tickets {condition}") // SAFE: константа и литералы из кода
}

/// Кто действует.
#[derive(Debug, Clone)]
pub struct Who {
    pub user: Option<Uuid>,
    pub person: Option<Uuid>,
    pub engineer: bool,
    pub superadmin: bool,
    pub actor: Actor,
    pub source: Source,
}

impl Who {
    pub fn from_user(u: &CurrentUser) -> Self {
        Self {
            user: Some(u.user_id),
            person: u.person_id,
            engineer: u.grants.has(&perm::WORK),
            superadmin: u.grants.is_superadmin(),
            actor: u.actor.clone(),
            source: Source::Portal,
        }
    }

    pub fn system() -> Self {
        Self {
            user: None,
            person: None,
            engineer: false,
            superadmin: false,
            actor: Actor::system(),
            source: Source::Job,
        }
    }

    pub fn is_requester(&self, t: &T) -> bool {
        (self.user.is_some() && self.user == t.requester_user_id)
            || (self.person.is_some() && self.person == t.requester_id)
    }

    pub fn can_see(&self, t: &T) -> bool {
        self.engineer || self.is_requester(t)
    }
}

pub const OPEN_STATUSES: &[&str] = &["new", "assigned", "in_progress", "waiting", "waiting_approval", "on_hold"];

pub(crate) fn is_open(status: &str) -> bool {
    OPEN_STATUSES.contains(&status)
}

pub(crate) fn need_engineer(who: &Who) -> ApiResult<()> {
    if who.engineer { Ok(()) } else { Err(ApiError::Forbidden) }
}

pub(crate) fn bad_state(message: &str) -> ApiError {
    ApiError::bad_request("invalid_state", message)
}

/// Заявка под блокировкой. Недоступная выглядит как несуществующая.
pub async fn lock(tx: &mut Transaction<'_, Postgres>, number: i64, who: &Who) -> ApiResult<T> {
    let t: T = sqlx::query_as(&select_t("WHERE number = $1 FOR UPDATE"))
        .bind(number)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(ApiError::NotFound)?;
    if who.can_see(&t) || crate::card::is_watcher(tx, t.id, who).await? { Ok(t) } else { Err(ApiError::NotFound) }
}

pub(crate) async fn event(
    tx: &mut Transaction<'_, Postgres>,
    ticket: Uuid,
    actor: Option<Uuid>,
    kind: &str,
    from: Value,
    to: Value,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO ticket_events (ticket_id, actor_id, kind, from_val, to_val) VALUES ($1, $2, $3, $4, $5)")
        .bind(ticket)
        .bind(actor)
        .bind(kind)
        .bind(from)
        .bind(to)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub(crate) async fn audit_rec(
    tx: &mut Transaction<'_, Postgres>,
    who: &Who,
    action: &'static str,
    t: &T,
    d: Value,
) -> Result<(), sqlx::Error> {
    audit::record(&mut **tx, &who.actor, Entry::new(action, who.source).object("ticket", t.id).details(d)).await?;
    Ok(())
}

/// Браузерам: заявка изменилась. Видят инженеры и сам заявитель.
pub async fn ping(tx: &mut Transaction<'_, Postgres>, t: &T) -> Result<(), sqlx::Error> {
    let mut users: Vec<Uuid> = match t.requester_id {
        Some(p) => sqlx::query_scalar("SELECT id FROM users WHERE person_id = $1").bind(p).fetch_all(&mut **tx).await?,
        None => Vec::new(),
    };
    users.extend(t.requester_user_id);
    users.extend(crate::card::watcher_users(tx, t.id).await?);
    crate::integrations::enqueue(tx, t.id).await?;
    events::publish(
        &mut **tx,
        &Event { kind: "ticket".into(), ticket: Some(t.id), users, permission: Some(perm::WORK.code.into()) },
    )
    .await
}

pub(crate) async fn set_status(
    tx: &mut Transaction<'_, Postgres>,
    t: &mut T,
    who: &Who,
    new: &str,
) -> Result<(), sqlx::Error> {
    if t.status == new {
        return Ok(());
    }
    sqlx::query("UPDATE tickets SET status = $2, version = version + 1 WHERE id = $1")
        .bind(t.id)
        .bind(new)
        .execute(&mut **tx)
        .await?;
    event(tx, t.id, who.user, "status", json!(t.status), json!(new)).await?;
    audit_rec(tx, who, "ticket.status", t, json!({"before": t.status, "after": new})).await?;
    sqlx::query(
        "UPDATE tickets SET waiting_since = CASE WHEN $2 = 'waiting' THEN now() END,
                reminded_at = CASE WHEN $2 = 'waiting' THEN NULL ELSE reminded_at END WHERE id = $1",
    )
    .bind(t.id)
    .bind(new)
    .execute(&mut **tx)
    .await?;
    crate::sla::on_status(tx, t.id, &t.status, new).await?;
    t.status = new.to_string();
    Ok(())
}

/// «В работе» при первом действии исполнителя; с этого момента считается время реакции.
/// Заметка или правка диспетчера, коллеги не останавливает реакцию за исполнителя.
pub(crate) async fn start_work(tx: &mut Transaction<'_, Postgres>, t: &mut T, who: &Who) -> Result<(), sqlx::Error> {
    if who.user.is_none() || who.user != t.assignee_id {
        return Ok(());
    }
    if t.status == "assigned" {
        set_status(tx, t, who, "in_progress").await?;
    }
    if t.status == "in_progress" && t.taken_at.is_none() {
        let now = OffsetDateTime::now_utc();
        sqlx::query("UPDATE tickets SET taken_at = $2 WHERE id = $1").bind(t.id).bind(now).execute(&mut **tx).await?;
        t.taken_at = Some(now);
    }
    Ok(())
}

/// Если действовал не исполнитель — исполнитель узнаёт об этом.
pub(crate) async fn tell_assignee(tx: &mut Transaction<'_, Postgres>, t: &T, who: &Who) -> Result<(), sqlx::Error> {
    if let Some(a) = t.assignee_id
        && who.user != Some(a)
    {
        notify::bell(tx, a, "helpdesk.changed_by_other", t).await?;
    }
    Ok(())
}

pub(crate) async fn attach(
    tx: &mut Transaction<'_, Postgres>,
    ticket: Uuid,
    comment: Option<Uuid>,
    files: &[Uuid],
    uploader: Option<Uuid>,
) -> ApiResult<()> {
    if files.is_empty() {
        return Ok(());
    }
    // Прикрепить можно только свой ещё не использованный файл.
    let n = sqlx::query(
        "INSERT INTO ticket_attachments (ticket_id, comment_id, file_id)
         SELECT $1, $2, f.id FROM files f
          WHERE f.id = ANY($3) AND f.uploaded_by IS NOT DISTINCT FROM $4
            AND NOT EXISTS (SELECT 1 FROM ticket_attachments a WHERE a.file_id = f.id)",
    )
    .bind(ticket)
    .bind(comment)
    .bind(files)
    .bind(uploader)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if n as usize != files.len() {
        return Err(ApiError::bad_request("invalid_file", "Вложение не найдено или уже использовано."));
    }
    Ok(())
}

// ------------------------------------------------------------------ Создание

/// Поле формы категории.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FormField {
    pub key: String,
    pub label: String,
    /// `text`, `select` или `bool`.
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub options: Vec<String>,
}

/// Проверяет ответы по схеме категории; посторонние поля отбрасываются.
pub fn validate_answers(schema: &[FormField], answers: &Value) -> Result<Value, String> {
    let mut out = serde_json::Map::new();
    for f in schema {
        let v = answers.get(&f.key).filter(|v| !v.is_null());
        let empty = match v {
            None => true,
            Some(Value::String(s)) => s.trim().is_empty(),
            Some(_) => false,
        };
        if empty {
            if f.required {
                return Err(format!("Заполните поле «{}».", f.label));
            }
            continue;
        }
        let v = v.cloned().unwrap_or(Value::Null);
        match (f.kind.as_str(), &v) {
            ("text", Value::String(s)) if s.chars().count() <= 2000 => {}
            ("select", Value::String(s)) if f.options.contains(s) => {}
            ("bool", Value::Bool(_)) => {}
            _ => return Err(format!("Неверное значение поля «{}».", f.label)),
        }
        out.insert(f.key.clone(), v);
    }
    Ok(Value::Object(out))
}

#[derive(Debug, Clone, Default)]
pub struct NewTicket {
    pub title: String,
    pub description: String,
    pub category_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub requester_id: Option<Uuid>,
    pub requester_user_id: Option<Uuid>,
    pub unverified_contact: Option<Value>,
    pub channel: String,
    pub contact_phone: Option<String>,
    pub contact_room: Option<String>,
    pub computer_name: Option<String>,
    pub form_answers: Value,
    pub parent_ticket_id: Option<Uuid>,
    pub follow_up_of: Option<Uuid>,
    pub files: Vec<Uuid>,
    /// Кому принадлежат вложения: `None` — файлы из письма.
    pub files_owner: Option<Uuid>,
    /// Назначить сразу этому инженеру, если он активен (повторное обращение).
    pub prefer_assignee: Option<Uuid>,
    /// Шаблон: тема, описание и чек-лист по умолчанию.
    pub template_id: Option<Uuid>,
    /// Регламентная заявка: от чьего имени.
    pub recurring_id: Option<Uuid>,
    /// Категория «Запрос доступа»: к какому ресурсу.
    pub resource_id: Option<Uuid>,
    /// Оборудование из реестра, выбранное в форме (`objects::attach_assets`).
    pub asset_ids: Vec<Uuid>,
    /// Заявку создаёт сама система (повторное обращение, регламент): обязательные поля формы не
    /// проверяются, выключенные категория и площадка заменяются пустыми, запрос доступа не открывается —
    /// иначе ответ заявителя или запуск регламента терялся бы из-за правки справочников.
    pub system: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Created {
    pub number: i64,
    pub status: String,
    pub assignee_name: Option<String>,
    pub after_hours: Option<AfterHours>,
}

pub(crate) fn clean_opt(s: Option<String>, max: usize) -> Option<String> {
    s.map(|v| v.trim().chars().take(max).collect::<String>()).filter(|v| !v.is_empty())
}

/// Короткое имя ПК из введённого в заявке (короткого или FQDN).
fn short_name(name: &str) -> &str {
    name.trim().split('.').next().unwrap_or_default()
}

pub async fn user_name(tx: &mut Transaction<'_, Postgres>, user: Uuid) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name)), ''), a.display_name, a.sam_account_name)
           FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id LEFT JOIN persons p ON p.id = u.person_id WHERE u.id = $1",
    )
    .bind(user)
    .fetch_optional(&mut **tx)
    .await
}

/// Правила 1–5 раздела 10; без кандидатов — очередь и уведомление суперадминистраторам.
pub(crate) async fn assign_or_queue(
    tx: &mut Transaction<'_, Postgres>,
    t: &mut T,
    who: &Who,
    c: &Ctx,
    prefer: Option<Uuid>,
) -> ApiResult<()> {
    let prefer = match prefer {
        Some(u) if assignment::is_active(tx, u).await? => Some(u),
        _ => None,
    };
    let group: Option<Uuid> = sqlx::query_scalar(
        "SELECT g.id FROM tickets t JOIN engineer_groups g ON g.id = t.group_id AND g.auto_assign WHERE t.id = $1",
    )
    .bind(t.id)
    .fetch_optional(&mut **tx)
    .await?;
    let picked = match (prefer, group, t.site_id) {
        (Some(u), _, _) => Some(u),
        (None, Some(g), _) => assignment::pick_in_group(tx, g).await?,
        (None, None, Some(site)) => assignment::pick(tx, site, &t.line).await?,
        (None, None, None) => None,
    };
    match picked {
        Some(u) => set_assignee(tx, t, who, Some(u), "assigned", c).await,
        None => {
            if t.status != "new" {
                set_status(tx, t, who, "new").await?;
                sqlx::query("UPDATE tickets SET assignee_id = NULL WHERE id = $1")
                    .bind(t.id)
                    .execute(&mut **tx)
                    .await?;
                t.assignee_id = None;
            }
            notify::unassigned_alert(tx, t, c).await?;
            Ok(())
        }
    }
}

pub(crate) async fn set_assignee(
    tx: &mut Transaction<'_, Postgres>,
    t: &mut T,
    who: &Who,
    assignee: Option<Uuid>,
    status: &str,
    c: &Ctx,
) -> ApiResult<()> {
    let before = t.assignee_id;
    sqlx::query("UPDATE tickets SET assignee_id = $2, assigned_at = now(), version = version + 1 WHERE id = $1")
        .bind(t.id)
        .bind(assignee)
        .execute(&mut **tx)
        .await?;
    t.assignee_id = assignee;
    event(tx, t.id, who.user, "assigned", json!(before), json!(assignee)).await?;
    audit_rec(tx, who, "ticket.assign", t, json!({"before": before, "after": assignee})).await?;
    // Передача не снимает ожидание ответа, отложенность и согласование: таймеры и напоминания идут дальше.
    let paused = matches!(t.status.as_str(), "waiting" | "on_hold" | "waiting_approval");
    if !(paused && assignee.is_some() && status == "assigned") {
        set_status(tx, t, who, status).await?;
    }
    if let Some(u) = assignee.filter(|u| who.user != Some(*u)) {
        notify::bell(tx, u, "helpdesk.assigned", t).await?;
        notify::to_user(tx, u, "helpdesk.assigned", t, &Msg::Assigned, c).await?;
    }
    Ok(())
}

/// Для заявки от системы: выключенная или удалённая категория, категория «Запрос доступа» и выключенная
/// площадка заменяются пустыми — заявку разберёт инженер.
async fn system_fallbacks(tx: &mut Transaction<'_, Postgres>, mut n: NewTicket) -> ApiResult<NewTicket> {
    if let Some(id) = n.category_id {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM ticket_categories WHERE id = $1 AND is_active AND code IS DISTINCT FROM $2)",
        )
        .bind(id)
        .bind(crate::access::CATEGORY)
        .fetch_one(&mut **tx)
        .await?;
        if !ok {
            n.category_id = None;
            n.form_answers = json!({});
        }
    }
    if let Some(site) = n.site_id {
        let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sites WHERE id = $1 AND is_active)")
            .bind(site)
            .fetch_one(&mut **tx)
            .await?;
        if !ok {
            n.site_id = None;
        }
    }
    Ok(n)
}

pub async fn create(core: &Core, who: &Who, n: NewTicket) -> ApiResult<Created> {
    if n.description.chars().count() > 100_000 {
        return Err(ApiError::bad_request("invalid_ticket", "Описание слишком длинное."));
    }
    let cfg: HelpdeskSettings = settings::load(&core.db).await?;
    let after = schedule::after_hours(&core.db, n.site_id, &cfg).await?;
    let c = notify::ctx(core).await?;

    let mut tx = core.db.begin().await?;
    let tpl = match n.template_id {
        Some(id) => Some(crate::templates::load(&mut tx, id, who.engineer).await?),
        None => None,
    };
    let n = match &tpl {
        Some(tp) => NewTicket {
            title: if n.title.trim().is_empty() { tp.title.clone() } else { n.title },
            description: if n.description.trim().is_empty() { tp.description.clone() } else { n.description },
            category_id: n.category_id.or(tp.category_id),
            ..n
        },
        None => n,
    };
    let n = if n.system { system_fallbacks(&mut tx, n).await? } else { n };
    let title: String = n.title.trim().chars().take(300).collect();
    if title.is_empty() {
        return Err(ApiError::bad_request("invalid_ticket", "Укажите тему."));
    }
    let mut assets = n.asset_ids.clone();
    assets.sort_unstable();
    assets.dedup();
    if assets.len() > crate::objects::MAX_TICKET_ASSETS {
        return Err(ApiError::bad_request("invalid_ticket", "Не больше 10 единиц оборудования."));
    }
    let (kind, priority, answers, category_code): (String, String, Value, Option<String>) = match n.category_id {
        Some(id) => {
            let row: Option<(String, String, Value, Option<String>)> = sqlx::query_as(
                "SELECT kind, default_priority, form_schema, code FROM ticket_categories WHERE id = $1 AND is_active",
            )
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
            let (k, p, schema, code) =
                row.ok_or_else(|| ApiError::bad_request("invalid_ticket", "Категория не найдена."))?;
            let schema: Vec<FormField> = serde_json::from_value(schema).unwrap_or_default();
            let a = if n.system {
                if n.form_answers.is_object() { n.form_answers.clone() } else { json!({}) }
            } else {
                validate_answers(&schema, &n.form_answers).map_err(|m| ApiError::bad_request("invalid_ticket", m))?
            };
            (k, p, a, code)
        }
        None => ("incident".into(), "normal".into(), json!({}), None),
    };
    // «Запрос доступа»: одна заявка — один ресурс из каталога.
    let resource = if category_code.as_deref() == Some(crate::access::CATEGORY) {
        Some(crate::access::check_resource(&mut tx, n.resource_id).await?)
    } else {
        None
    };
    if let Some(site) = n.site_id {
        let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sites WHERE id = $1 AND is_active)")
            .bind(site)
            .fetch_one(&mut *tx)
            .await?;
        if !ok {
            return Err(ApiError::bad_request("invalid_ticket", "Площадка не найдена."));
        }
    }
    if let Some(parent) = n.parent_ticket_id {
        let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM tickets WHERE id = $1 AND is_major AND status <> ALL('{resolved,closed,canceled}'))")
            .bind(parent)
            .fetch_one(&mut *tx)
            .await?;
        if !ok {
            return Err(ApiError::bad_request("invalid_ticket", "Массовый сбой уже решён."));
        }
    }
    // Компьютер: указанный в форме или, если не указан, ПК, где у заявителя сейчас активная сессия
    // (данные агента). Связь с устройством — основание для действий инженера из заявки
    // (C$, «Управление компьютером», PowerShell), поэтому имя, введённое самим заявителем,
    // связывает заявку с устройством, только если агент видит заявителя на этом ПК. Имя,
    // указанное инженером (заявка за сотрудника, шаблон), — как есть.
    let mut computer = clean_opt(n.computer_name.clone(), 100);
    let by_requester = (who.user.is_some() && who.user == n.requester_user_id)
        || (who.person.is_some() && who.person == n.requester_id);
    let mut device_id = None;
    if computer.is_none() || by_requester {
        let account: Option<Uuid> = sqlx::query_scalar(
            "SELECT coalesce((SELECT ad_account_id FROM persons WHERE id = $1),
                             (SELECT ad_account_id FROM users WHERE id = $2))",
        )
        .bind(n.requester_id)
        .bind(n.requester_user_id)
        .fetch_one(&mut *tx)
        .await?;
        // Адрес запроса говорит о ПК заявителя, только если заявку создаёт он сам.
        let ip = if by_requester { who.actor.ip } else { None };
        if let Some((d, name)) = pult_people::computers::of_session(&core.db, account, ip).await? {
            match &computer {
                None => {
                    computer = Some(name);
                    device_id = Some(d);
                }
                Some(typed) if short_name(typed).eq_ignore_ascii_case(&name) => device_id = Some(d),
                Some(_) => {}
            }
        }
    } else {
        device_id = pult_people::computers::device_by_name(&core.db, computer.as_deref()).await?;
    }
    let mut t: T = sqlx::query_as(&format!(
        "INSERT INTO tickets (kind, title, description, priority, category_id, form_answers, site_id, requester_id,
                              requester_user_id, unverified_contact, created_by, channel, contact_phone, contact_room,
                              computer_name, parent_ticket_id, follow_up_of, template_id, recurring_id, device_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)
         RETURNING {T_COLS}" // SAFE: константа
    ))
    .bind(kind)
    .bind(&title)
    .bind(&n.description)
    .bind(priority)
    .bind(n.category_id)
    .bind(answers)
    .bind(n.site_id)
    .bind(n.requester_id)
    .bind(n.requester_user_id)
    .bind(&n.unverified_contact)
    .bind(who.user)
    .bind(&n.channel)
    .bind(clean_opt(n.contact_phone.clone(), 50))
    .bind(clean_opt(n.contact_room.clone(), 50))
    .bind(&computer)
    .bind(n.parent_ticket_id)
    .bind(n.follow_up_of)
    .bind(n.template_id)
    .bind(n.recurring_id)
    .bind(device_id)
    .fetch_one(&mut *tx)
    .await?;
    attach(&mut tx, t.id, None, &n.files, n.files_owner).await?;
    event(&mut tx, t.id, who.user, "created", Value::Null, json!({"channel": n.channel})).await?;
    audit_rec(&mut tx, who, "ticket.create", &t, json!({"number": t.number, "channel": n.channel})).await?;
    // VIP: приоритет на ступень выше.
    let vip: bool = sqlx::query_scalar("SELECT coalesce((SELECT is_vip FROM persons WHERE id = $1), false)")
        .bind(t.requester_id)
        .fetch_one(&mut *tx)
        .await?;
    if vip && cfg.vip_bump {
        let p = crate::sla::bump(&t.priority);
        sqlx::query("UPDATE tickets SET priority = $2 WHERE id = $1").bind(t.id).bind(&p).execute(&mut *tx).await?;
        t.priority = p;
    }
    if let Some(resource) = resource {
        crate::access::open(&mut tx, &t, who, resource).await?;
    }
    if let Some(tp) = &tpl {
        crate::checklist::insert(&mut tx, &t, who, &tp.checklist, true).await?;
        if let Some(g) = tp.group_id {
            sqlx::query("UPDATE tickets SET group_id = $2 WHERE id = $1").bind(t.id).bind(g).execute(&mut *tx).await?;
        }
    }
    // Объект «Компьютер» по имени ПК — как в GLPI «связанные элементы».
    crate::objects::attach_computer_by_name(&mut tx, &t, who).await?;
    crate::objects::attach_assets(&mut tx, &t, who, &assets, n.requester_id).await?;
    crate::rules::fire(&mut tx, &t, crate::rules::Trigger::Created).await?;
    let mut t: T = sqlx::query_as(&select_t("WHERE id = $1")).bind(t.id).fetch_one(&mut *tx).await?;
    if t.assignee_id.is_none() {
        assign_or_queue(&mut tx, &mut t, who, &c, n.prefer_assignee).await?;
    }
    crate::sla::apply(&mut tx, t.id).await?;
    let assignee_name = match t.assignee_id {
        Some(u) => user_name(&mut tx, u).await?,
        None => None,
    };
    notify::to_requester(&mut tx, &t, &Msg::Created { assignee: assignee_name.as_deref(), after: after.as_ref() }, &c)
        .await?;
    if n.channel == "login_help" {
        // «Не могу войти» видят все инженеры: личность не подтверждена, площадка неизвестна.
        let engineers: Vec<Uuid> =
            sqlx::query_scalar("SELECT DISTINCT user_id FROM site_engineers").fetch_all(&mut *tx).await?;
        for e in engineers {
            notify::bell(&mut tx, e, "helpdesk.login_help", &t).await?;
        }
    }
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(Created { number: t.number, status: t.status, assignee_name, after_hours: after })
}

// ------------------------------------------------------------------ Ответы

#[derive(Debug, Clone, Default)]
pub struct NewComment {
    pub body: String,
    pub internal: bool,
    pub files: Vec<Uuid>,
    pub files_owner: Option<Uuid>,
    pub broadcast: bool,
    pub source: String,
    pub author_name: Option<String>,
    /// Затраченное время инженера, минут.
    pub minutes: Option<i32>,
}

pub(crate) async fn insert_comment(
    tx: &mut Transaction<'_, Postgres>,
    ticket: Uuid,
    who: &Who,
    c: &NewComment,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO ticket_comments (ticket_id, author_id, author_name, visibility, body, source)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(ticket)
    .bind(who.user)
    .bind(&c.author_name)
    .bind(if c.internal { "internal" } else { "public" })
    .bind(&c.body)
    .bind(if c.source.is_empty() { "portal" } else { c.source.as_str() })
    .fetch_one(&mut **tx)
    .await
}

/// Вернуть в работу: тому же исполнителю, если он активен, иначе — по правилам назначения.
pub(crate) async fn reopen(
    tx: &mut Transaction<'_, Postgres>,
    t: &mut T,
    who: &Who,
    c: &Ctx,
    body: Option<&str>,
) -> ApiResult<()> {
    sqlx::query("UPDATE tickets SET reopened_count = reopened_count + 1, resolved_at = NULL, auto_close_at = NULL, closed_at = NULL, closed_by = NULL, closure_code = NULL WHERE id = $1")
        .bind(t.id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE ticket_tokens SET used_at = now() WHERE ticket_id = $1 AND used_at IS NULL")
        .bind(t.id)
        .execute(&mut **tx)
        .await?;
    event(tx, t.id, who.user, "reopened", Value::Null, Value::Null).await?;
    audit_rec(tx, who, "ticket.reopen", t, json!({})).await?;
    let active = match t.assignee_id {
        Some(u) => assignment::is_active(tx, u).await?,
        None => false,
    };
    if active {
        set_status(tx, t, who, "in_progress").await?;
        if let Some(a) = t.assignee_id {
            notify::bell(tx, a, "helpdesk.reopened", t).await?;
            notify::to_user(tx, a, "helpdesk.reopened", t, &Msg::Reopened { body }, c).await?;
        }
    } else {
        assign_or_queue(tx, t, who, c, None).await?;
    }
    Ok(())
}

/// Что получилось: комментарий в этой заявке или новая связанная.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CommentResult {
    pub number: i64,
    /// Заявка была закрыта — создана новая связанная.
    pub follow_up: bool,
}

pub async fn comment(core: &Core, who: &Who, number: i64, input: NewComment) -> ApiResult<CommentResult> {
    let body = input.body.trim();
    if body.is_empty() && input.files.is_empty() {
        return Err(ApiError::bad_request("invalid_comment", "Напишите текст ответа."));
    }
    if body.chars().count() > 100_000 {
        return Err(ApiError::bad_request("invalid_comment", "Текст слишком длинный."));
    }
    if input.internal && !who.engineer {
        return Err(ApiError::Forbidden);
    }
    let c = notify::ctx(core).await?;
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    let requester = who.is_requester(&t);
    if matches!(t.status.as_str(), "closed" | "canceled") {
        if who.engineer {
            return Err(bad_state("Заявка закрыта. Открыть её заново может суперадминистратор."));
        }
        if !requester {
            return Err(bad_state("Заявка закрыта."));
        }
        // Ответ заявителя на закрытую — новая связанная заявка.
        if t.status == "canceled" {
            return Err(bad_state("Заявка отменена."));
        }
        let (requester_id, requester_user_id, site_id, category_id, assignee) =
            (t.requester_id, t.requester_user_id, t.site_id, t.category_id, t.assignee_id);
        let (id, old_number) = (t.id, t.number);
        // Поля формы исходной заявки переходят в повторную: обязательные уже заполнены.
        let form_answers: Value =
            sqlx::query_scalar("SELECT form_answers FROM tickets WHERE id = $1").bind(id).fetch_one(&mut *tx).await?;
        // Повторное обращение уже открыто — ответ идёт туда, а не в ещё одну новую заявку.
        let open: Option<i64> = sqlx::query_scalar(
            "SELECT number FROM tickets WHERE follow_up_of = $1 AND status = ANY($2) ORDER BY created_at DESC LIMIT 1",
        )
        .bind(id)
        .bind(OPEN_STATUSES)
        .fetch_optional(&mut *tx)
        .await?;
        drop(tx);
        if let Some(n) = open {
            let r = Box::pin(comment(core, who, n, input)).await?;
            return Ok(CommentResult { follow_up: true, ..r });
        }
        let created = create(
            core,
            who,
            NewTicket {
                title: tr(&format!("Повторное обращение по #{old_number}")).into_owned(),
                description: body.to_string(),
                category_id,
                site_id,
                requester_id,
                requester_user_id,
                channel: "portal".into(),
                form_answers,
                follow_up_of: Some(id),
                system: true,
                files: input.files.clone(),
                files_owner: input.files_owner,
                prefer_assignee: assignee,
                ..Default::default()
            },
        )
        .await?;
        return Ok(CommentResult { number: created.number, follow_up: true });
    }
    let input = NewComment { body: body.to_string(), ..input };
    let comment_id = insert_comment(&mut tx, t.id, who, &input).await?;
    crate::rules::fire_with(&mut tx, &t, crate::rules::Trigger::Commented, Some(&input.body)).await?;
    attach(&mut tx, t.id, Some(comment_id), &input.files, input.files_owner).await?;
    crate::card::log_work(&mut tx, &t, who, input.minutes, Some(comment_id)).await?;
    let action = if input.internal { "ticket.note" } else { "ticket.reply" };
    audit_rec(&mut tx, who, action, &t, json!({"comment": comment_id})).await?;
    sqlx::query("UPDATE tickets SET version = version + 1 WHERE id = $1").bind(t.id).execute(&mut *tx).await?;
    if who.engineer {
        start_work(&mut tx, &mut t, who).await?;
        if !input.internal {
            if t.first_response_at.is_none() {
                sqlx::query("UPDATE tickets SET first_response_at = now() WHERE id = $1")
                    .bind(t.id)
                    .execute(&mut *tx)
                    .await?;
            }
            notify::to_requester(&mut tx, &t, &Msg::Reply { body: &input.body }, &c).await?;
            if input.broadcast && t.is_major {
                broadcast(&mut tx, &t, who, &input, &c).await?;
            }
        }
        tell_assignee(&mut tx, &t, who).await?;
    } else {
        match t.status.as_str() {
            // Без исполнителя — обратно в очередь, а не «В работе» ни у кого.
            "waiting" if requester => {
                let next = if t.assignee_id.is_some() { "in_progress" } else { "new" };
                set_status(&mut tx, &mut t, who, next).await?
            }
            "resolved" if requester => reopen(&mut tx, &mut t, who, &c, Some(&input.body)).await?,
            _ => {}
        }
        if !requester {
            notify::to_requester(&mut tx, &t, &Msg::Reply { body: &input.body }, &c).await?;
        }
        if let Some(a) = t.assignee_id {
            notify::bell(&mut tx, a, "helpdesk.requester_replied", &t).await?;
            notify::to_user(
                &mut tx,
                a,
                "helpdesk.requester_replied",
                &t,
                &Msg::RequesterReplied { body: &input.body },
                &c,
            )
            .await?;
        }
    }
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(CommentResult { number: t.number, follow_up: false })
}

/// Публичный ответ главной заявки — во все привязанные.
async fn broadcast(
    tx: &mut Transaction<'_, Postgres>,
    major: &T,
    who: &Who,
    input: &NewComment,
    c: &Ctx,
) -> ApiResult<()> {
    let children: Vec<T> = sqlx::query_as(&format!( // SAFE: константа
        "SELECT {T_COLS} FROM tickets WHERE parent_ticket_id = $1 AND status <> ALL('{{resolved,closed,canceled}}') FOR UPDATE"
    ))
    .bind(major.id)
    .fetch_all(&mut **tx)
    .await?;
    for ch in children {
        let copy = NewComment { files: Vec::new(), ..input.clone() };
        insert_comment(tx, ch.id, who, &copy).await?;
        notify::to_requester(tx, &ch, &Msg::Reply { body: &input.body }, c).await?;
        ping(tx, &ch).await?;
    }
    Ok(())
}

// ------------------------------------------------------------------ Статусы

pub async fn take(core: &Core, who: &Who, number: i64) -> ApiResult<()> {
    need_engineer(who)?;
    let me = who.user.ok_or(ApiError::Forbidden)?;
    let c = notify::ctx(core).await?;
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    if !is_open(&t.status) {
        return Err(bad_state("Заявка уже решена или закрыта."));
    }
    let other = t.assignee_id.filter(|a| *a != me);
    if t.assignee_id != Some(me) {
        set_assignee(&mut tx, &mut t, who, Some(me), "in_progress", &c).await?;
    } else {
        set_status(&mut tx, &mut t, who, "in_progress").await?;
    }
    start_work(&mut tx, &mut t, who).await?;
    if let Some(o) = other {
        notify::bell(&mut tx, o, "helpdesk.changed_by_other", &t).await?;
    }
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn waiting(core: &Core, who: &Who, number: i64, note: Option<String>) -> ApiResult<()> {
    need_engineer(who)?;
    let c = notify::ctx(core).await?;
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    if !is_open(&t.status) {
        return Err(bad_state("Заявка уже решена или закрыта."));
    }
    let body = note.as_deref().map(str::trim).filter(|b| !b.is_empty());
    if let Some(b) = body {
        let cm = NewComment { body: b.to_string(), ..Default::default() };
        insert_comment(&mut tx, t.id, who, &cm).await?;
        if t.first_response_at.is_none() {
            sqlx::query("UPDATE tickets SET first_response_at = now() WHERE id = $1")
                .bind(t.id)
                .execute(&mut *tx)
                .await?;
        }
    }
    start_work(&mut tx, &mut t, who).await?;
    set_status(&mut tx, &mut t, who, "waiting").await?;
    notify::to_requester(&mut tx, &t, &Msg::Waiting { body }, &c).await?;
    tell_assignee(&mut tx, &t, who).await?;
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

/// Решить заявку; главная решает и все привязанные с тем же комментарием.
#[derive(Debug, Clone, Default)]
pub struct Resolution<'a> {
    pub body: &'a str,
    pub files: &'a [Uuid],
    pub closure_code: &'a str,
    pub minutes: Option<i32>,
}

pub async fn resolve(core: &Core, who: &Who, number: i64, r: Resolution<'_>) -> ApiResult<()> {
    need_engineer(who)?;
    let (body, files) = (r.body, r.files);
    let body = body.trim();
    if body.is_empty() {
        return Err(ApiError::bad_request(
            "comment_required",
            "Напишите, как решена заявка: заявитель получит это письмом.",
        ));
    }
    let cfg: HelpdeskSettings = settings::load(&core.db).await?;
    let code = crate::card::check_close_code(&cfg.close_codes, r.closure_code)?;
    let c = notify::ctx(core).await?;
    let site: Option<Uuid> = sqlx::query_scalar("SELECT site_id FROM tickets WHERE number = $1")
        .bind(number)
        .fetch_optional(&core.db)
        .await?
        .flatten();
    let close_at = schedule::auto_close_at(&core.db, site, &cfg).await?;
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    if !is_open(&t.status) {
        return Err(bad_state("Заявка уже решена или закрыта."));
    }
    let children: Vec<T> = if t.is_major {
        sqlx::query_as(&format!(
            // SAFE: константа
            "SELECT {T_COLS} FROM tickets WHERE parent_ticket_id = $1 AND status = ANY($2) FOR UPDATE"
        ))
        .bind(t.id)
        .bind(OPEN_STATUSES)
        .fetch_all(&mut *tx)
        .await?
    } else {
        Vec::new()
    };
    crate::checklist::ensure_done(&mut tx, &t).await?;
    let id = resolve_one(&mut tx, &mut t, who, body, files, close_at, &code, &c).await?;
    crate::card::log_work(&mut tx, &t, who, r.minutes, Some(id)).await?;
    for mut ch in children {
        resolve_one(&mut tx, &mut ch, who, body, &[], close_at, &code, &c).await?;
    }
    if t.is_major {
        sqlx::query(
            "UPDATE announcements SET ends_at = now() WHERE ticket_id = $1 AND (ends_at IS NULL OR ends_at > now())",
        )
        .bind(t.id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn resolve_one(
    tx: &mut Transaction<'_, Postgres>,
    t: &mut T,
    who: &Who,
    body: &str,
    files: &[Uuid],
    close_at: OffsetDateTime,
    code: &str,
    c: &Ctx,
) -> ApiResult<Uuid> {
    let cm = NewComment { body: body.to_string(), ..Default::default() };
    let id = insert_comment(tx, t.id, who, &cm).await?;
    attach(tx, t.id, Some(id), files, who.user).await?;
    sqlx::query(
        "UPDATE tickets SET resolved_at = now(), auto_close_at = $2, first_response_at = coalesce(first_response_at, now()),
                closure_code = nullif($3, '') WHERE id = $1",
    )
    .bind(t.id)
    .bind(close_at)
    .bind(code)
    .execute(&mut **tx)
    .await?;
    start_work(tx, t, who).await?;
    set_status(tx, t, who, "resolved").await?;
    let token = random_token(32).map_err(ApiError::internal)?;
    sqlx::query(
        "INSERT INTO ticket_tokens (ticket_id, purpose, token_hash, expires_at) VALUES ($1, 'resolve_confirm', $2, $3)",
    )
    .bind(t.id)
    .bind(sha256(token.as_bytes()))
    .bind(close_at)
    .execute(&mut **tx)
    .await?;
    notify::to_requester(tx, t, &Msg::Resolved { body, token: &token }, c).await?;
    tell_assignee(tx, t, who).await?;
    ping(tx, t).await?;
    Ok(id)
}

pub(crate) async fn close(tx: &mut Transaction<'_, Postgres>, t: &mut T, who: &Who, c: &Ctx) -> ApiResult<()> {
    sqlx::query("UPDATE tickets SET closed_at = now(), closed_by = $2, auto_close_at = NULL WHERE id = $1")
        .bind(t.id)
        .bind(who.user)
        .execute(&mut **tx)
        .await?;
    sqlx::query("UPDATE ticket_tokens SET used_at = now() WHERE ticket_id = $1 AND used_at IS NULL")
        .bind(t.id)
        .execute(&mut **tx)
        .await?;
    set_status(tx, t, who, "closed").await?;
    notify::to_requester(tx, t, &Msg::Closed, c).await?;
    ping(tx, t).await?;
    Ok(())
}

/// Заявитель в портале: «Да, решена».
pub async fn confirm(core: &Core, who: &Who, number: i64) -> ApiResult<()> {
    let c = notify::ctx(core).await?;
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    if t.status != "resolved" || !who.is_requester(&t) {
        return Err(bad_state("Подтвердить решение может заявитель решённой заявки."));
    }
    close(&mut tx, &mut t, who, &c).await?;
    tx.commit().await?;
    Ok(())
}

/// Заявитель в портале: «Нет, не решена».
pub async fn not_resolved(core: &Core, who: &Who, number: i64, body: &str) -> ApiResult<()> {
    let body = body.trim();
    if body.is_empty() {
        return Err(ApiError::bad_request("comment_required", "Напишите, что не так."));
    }
    {
        let mut tx = core.db.begin().await?;
        let t = lock(&mut tx, number, who).await?;
        if t.status != "resolved" || !who.is_requester(&t) {
            return Err(bad_state("Вернуть в работу может заявитель решённой заявки."));
        }
    }
    comment(core, who, number, NewComment { body: body.to_string(), ..Default::default() }).await.map(|_| ())
}

/// Отменить: инженер — всегда, заявитель — пока инженер не ответил.
pub async fn cancel(core: &Core, who: &Who, number: i64, reason: Option<String>) -> ApiResult<()> {
    let c = notify::ctx(core).await?;
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    if !is_open(&t.status) {
        return Err(bad_state("Заявка уже решена или закрыта."));
    }
    if !who.engineer && !who.is_requester(&t) {
        return Err(ApiError::Forbidden);
    }
    if !who.engineer && t.first_response_at.is_some() {
        return Err(bad_state("Инженер уже ответил — отменить заявку может только он."));
    }
    if let Some(b) = reason.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        insert_comment(&mut tx, t.id, who, &NewComment { body: b.to_string(), ..Default::default() }).await?;
    }
    set_status(&mut tx, &mut t, who, "canceled").await?;
    sqlx::query("UPDATE tickets SET closed_at = now(), closed_by = $2 WHERE id = $1")
        .bind(t.id)
        .bind(who.user)
        .execute(&mut *tx)
        .await?;
    if t.is_major {
        // Сбой оказался не массовым: объявление снимается, привязанные заявки живут дальше сами по себе.
        sqlx::query(
            "UPDATE announcements SET ends_at = now() WHERE ticket_id = $1 AND (ends_at IS NULL OR ends_at > now())",
        )
        .bind(t.id)
        .execute(&mut *tx)
        .await?;
        let children: Vec<Uuid> = sqlx::query_scalar(
            "UPDATE tickets SET parent_ticket_id = NULL, version = version + 1
             WHERE parent_ticket_id = $1 AND status = ANY($2) RETURNING id",
        )
        .bind(t.id)
        .bind(OPEN_STATUSES)
        .fetch_all(&mut *tx)
        .await?;
        for ch in children {
            event(&mut tx, ch, who.user, "linked", json!(t.number), Value::Null).await?;
        }
    }
    if who.engineer {
        notify::to_requester(&mut tx, &t, &Msg::Closed, &c).await?;
        tell_assignee(&mut tx, &t, who).await?;
    } else if let Some(a) = t.assignee_id {
        notify::bell(&mut tx, a, "helpdesk.changed_by_other", &t).await?;
    }
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

/// Суперадминистратор открывает закрытую заявку заново.
pub async fn reopen_closed(core: &Core, who: &Who, number: i64) -> ApiResult<()> {
    if !who.superadmin {
        return Err(ApiError::Forbidden);
    }
    let c = notify::ctx(core).await?;
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    if t.status != "closed" {
        return Err(bad_state("Заявка не закрыта."));
    }
    reopen(&mut tx, &mut t, who, &c, None).await?;
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

/// Передать другому инженеру; на L2 — с обязательной причиной (правила 6–7).
pub async fn transfer(core: &Core, who: &Who, number: i64, assignee: Uuid, line: &str, reason: &str) -> ApiResult<()> {
    need_engineer(who)?;
    let reason = reason.trim();
    if !matches!(line, "l1" | "l2") {
        return Err(ApiError::bad_request("invalid_ticket", "Линия — l1 или l2."));
    }
    if reason.is_empty() {
        return Err(ApiError::bad_request("comment_required", "Укажите причину передачи."));
    }
    let c = notify::ctx(core).await?;
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    if !is_open(&t.status) {
        return Err(bad_state("Заявка уже решена или закрыта."));
    }
    let is_engineer: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM site_engineers WHERE user_id = $1)")
        .bind(assignee)
        .fetch_one(&mut *tx)
        .await?;
    if !is_engineer || !assignment::is_active(&mut tx, assignee).await? {
        return Err(ApiError::bad_request("invalid_ticket", "Такого активного инженера нет."));
    }
    let previous = t.assignee_id;
    insert_comment(&mut tx, t.id, who, &NewComment { body: reason.to_string(), internal: true, ..Default::default() })
        .await?;
    if t.line != line {
        sqlx::query("UPDATE tickets SET line = $2 WHERE id = $1").bind(t.id).bind(line).execute(&mut *tx).await?;
        event(&mut tx, t.id, who.user, "line", json!(t.line), json!(line)).await?;
        t.line = line.to_string();
    }
    set_assignee(&mut tx, &mut t, who, Some(assignee), "assigned", &c).await?;
    if let Some(p) = previous.filter(|p| *p != assignee && who.user != Some(*p)) {
        notify::bell(&mut tx, p, "helpdesk.changed_by_other", &t).await?;
    }
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

/// Правка полей инженером. Устаревшая версия — 409.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TicketUpdate {
    pub version: i32,
    pub priority: Option<String>,
    pub category_id: Option<Uuid>,
    pub site_id: Option<Uuid>,
    pub title: Option<String>,
    pub computer_name: Option<String>,
    /// `person`, `department`, `organization`.
    pub impact: Option<String>,
    /// `normal`, `hinders`, `blocked`.
    pub urgency: Option<String>,
    /// Группа исполнителей; `clear_group` — убрать.
    pub group_id: Option<Uuid>,
    #[serde(default)]
    pub clear_group: bool,
}

pub async fn update(core: &Core, who: &Who, number: i64, mut b: TicketUpdate) -> ApiResult<()> {
    need_engineer(who)?;
    if b.priority.as_deref().is_some_and(|p| !matches!(p, "low" | "normal" | "high" | "critical")) {
        return Err(ApiError::bad_request("invalid_ticket", "Неверный приоритет."));
    }
    if b.impact.as_deref().is_some_and(|p| !crate::sla::IMPACTS.contains(&p))
        || b.urgency.as_deref().is_some_and(|p| !crate::sla::URGENCIES.contains(&p))
    {
        return Err(ApiError::bad_request("invalid_ticket", "Неверное влияние или срочность."));
    }
    let cfg: HelpdeskSettings = settings::load(&core.db).await?;
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    if t.version != b.version {
        return Err(ApiError::Conflict {
            current: json!({"version": t.version, "status": t.status, "priority": t.priority}),
        });
    }
    if !is_open(&t.status) && t.status != "resolved" {
        return Err(bad_state("Заявка закрыта."));
    }
    let (impact, urgency, vip): (String, String, bool) = sqlx::query_as(
        "SELECT t.impact, t.urgency, coalesce(p.is_vip, false) FROM tickets t LEFT JOIN persons p ON p.id = t.requester_id WHERE t.id = $1",
    )
    .bind(t.id)
    .fetch_one(&mut *tx)
    .await?;
    let matrix_changed =
        b.impact.as_deref().is_some_and(|v| v != impact) || b.urgency.as_deref().is_some_and(|v| v != urgency);
    if matrix_changed && b.priority.is_none() {
        let (i, u) = (b.impact.clone().unwrap_or(impact.clone()), b.urgency.clone().unwrap_or(urgency.clone()));
        b.priority = Some(crate::sla::matrix_priority(&cfg, &i, &u, vip));
    }
    if matrix_changed {
        event(&mut tx, t.id, who.user, "impact", json!([impact, urgency]), json!([b.impact, b.urgency])).await?;
    }
    if let Some(g) = b.group_id {
        let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM engineer_groups WHERE id = $1)")
            .bind(g)
            .fetch_one(&mut *tx)
            .await?;
        if !ok {
            return Err(ApiError::bad_request("invalid_ticket", "Группа не найдена."));
        }
    }
    // Новая категория и площадка — только действующие, как при создании (иначе 500 на внешнем ключе).
    if let Some(c) = b.category_id.filter(|c| Some(*c) != t.category_id) {
        let ok: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ticket_categories WHERE id = $1 AND is_active)")
                .bind(c)
                .fetch_one(&mut *tx)
                .await?;
        if !ok {
            return Err(ApiError::bad_request("invalid_ticket", "Категория не найдена."));
        }
    }
    if let Some(s) = b.site_id.filter(|s| Some(*s) != t.site_id) {
        let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sites WHERE id = $1 AND is_active)")
            .bind(s)
            .fetch_one(&mut *tx)
            .await?;
        if !ok {
            return Err(ApiError::bad_request("invalid_ticket", "Площадка не найдена."));
        }
    }
    let group_changed = b.clear_group || b.group_id.is_some();
    if group_changed {
        let g = if b.clear_group { None } else { b.group_id };
        sqlx::query("UPDATE tickets SET group_id = $2 WHERE id = $1").bind(t.id).bind(g).execute(&mut *tx).await?;
        event(&mut tx, t.id, who.user, "group", Value::Null, json!(g)).await?;
    }
    let sla_changed = b.priority.as_deref().is_some_and(|p| p != t.priority)
        || b.category_id.is_some_and(|c| Some(c) != t.category_id)
        || b.site_id.is_some_and(|s| Some(s) != t.site_id);
    let title = clean_opt(b.title.clone(), 300);
    let computer = clean_opt(b.computer_name.clone(), 100);
    for (kind, changed, from, to) in [
        (
            "priority",
            b.priority.is_some() && b.priority.as_deref() != Some(t.priority.as_str()),
            json!(t.priority),
            json!(b.priority),
        ),
        (
            "category",
            b.category_id.is_some() && b.category_id != t.category_id,
            json!(t.category_id),
            json!(b.category_id),
        ),
        ("site", b.site_id.is_some() && b.site_id != t.site_id, json!(t.site_id), json!(b.site_id)),
        ("title", title.is_some() && title.as_deref() != Some(t.title.as_str()), json!(t.title), json!(title)),
    ] {
        if changed {
            event(&mut tx, t.id, who.user, kind, from, to).await?;
        }
    }
    sqlx::query(
        "UPDATE tickets SET priority = coalesce($2, priority), category_id = coalesce($3, category_id), site_id = coalesce($4, site_id),
                title = coalesce($5, title), computer_name = coalesce($6, computer_name), impact = coalesce($7, impact),
                device_id = CASE WHEN $6::text IS NULL THEN device_id
                                 ELSE (SELECT d.id FROM devices d WHERE lower(d.hostname) = lower(split_part($6, '.', 1))
                                          AND d.is_active) END,
                urgency = coalesce($8, urgency), version = version + 1
          WHERE id = $1",
    )
    .bind(t.id)
    .bind(&b.priority)
    .bind(b.category_id)
    .bind(b.site_id)
    .bind(&title)
    .bind(&computer)
    .bind(&b.impact)
    .bind(&b.urgency)
    .execute(&mut *tx)
    .await?;
    // У решённой сроки уже итоговые: пересчёт задним числом менял бы «просрочено» в отчётах.
    if sla_changed && t.status != "resolved" {
        crate::sla::apply(&mut tx, t.id).await?;
    }
    if computer.is_some() {
        crate::objects::attach_computer_by_name(&mut tx, &t, who).await?;
    }
    // Заявку без исполнителя перевели в группу с автоназначением — её берёт наименее загруженный участник.
    if let Some(g) = b.group_id.filter(|_| !b.clear_group && t.assignee_id.is_none() && is_open(&t.status)) {
        let auto: bool = sqlx::query_scalar("SELECT auto_assign FROM engineer_groups WHERE id = $1")
            .bind(g)
            .fetch_one(&mut *tx)
            .await?;
        let picked = if auto { assignment::pick_in_group(&mut tx, g).await? } else { None };
        if let Some(u) = picked {
            let c = notify::ctx(core).await?;
            set_assignee(&mut tx, &mut t, who, Some(u), "assigned", &c).await?;
        }
    }
    audit_rec(
        &mut tx,
        who,
        "ticket.update",
        &t,
        json!({"priority": b.priority, "category": b.category_id, "site": b.site_id}),
    )
    .await?;
    // Правка диспетчера (группа, приоритет) — не реакция исполнителя: «В работе» ставит только он сам.
    if who.user.is_some() && who.user == t.assignee_id {
        start_work(&mut tx, &mut t, who).await?;
    }
    crate::rules::fire(&mut tx, &t, crate::rules::Trigger::Updated).await?;
    tell_assignee(&mut tx, &t, who).await?;
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

/// Инженер подтвердил личность обратившегося через «Не могу войти» и связал с сотрудником.
pub async fn verify_identity(core: &Core, who: &Who, number: i64, person: Uuid) -> ApiResult<()> {
    need_engineer(who)?;
    let c = notify::ctx(core).await?;
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    if !is_open(&t.status) {
        return Err(bad_state("Заявка уже решена или закрыта."));
    }
    if t.requester_id.is_some() || t.requester_user_id.is_some() {
        return Err(bad_state("Заявитель уже указан."));
    }
    let row: Option<(Option<Uuid>, Option<Uuid>)> = sqlx::query_as(
        "SELECT p.site_id, (SELECT id FROM users u WHERE u.person_id = p.id LIMIT 1) FROM persons p WHERE p.id = $1 AND NOT p.is_archived",
    )
    .bind(person)
    .fetch_optional(&mut *tx)
    .await?;
    let (site, user) = row.ok_or_else(|| ApiError::bad_request("invalid_ticket", "Сотрудник не найден."))?;
    sqlx::query("UPDATE tickets SET requester_id = $2, requester_user_id = $3, site_id = coalesce(site_id, $4), version = version + 1 WHERE id = $1")
        .bind(t.id)
        .bind(person)
        .bind(user)
        .bind(site)
        .execute(&mut *tx)
        .await?;
    t.requester_id = Some(person);
    t.requester_user_id = user;
    t.site_id = t.site_id.or(site);
    event(&mut tx, t.id, who.user, "identity_verified", Value::Null, json!(person)).await?;
    audit_rec(&mut tx, who, "ticket.identity_verified", &t, json!({"person": person})).await?;
    if t.assignee_id.is_none() {
        assign_or_queue(&mut tx, &mut t, who, &c, None).await?;
    }
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

// ------------------------------------------------------------------ Массовые сбои

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MajorInput {
    pub title: String,
    pub body: String,
    /// Пусто — все площадки.
    #[serde(default)]
    pub site_ids: Vec<Uuid>,
}

/// Сделать заявку главной и объявить о сбое.
pub async fn make_major(core: &Core, who: &Who, number: i64, b: MajorInput) -> ApiResult<()> {
    need_engineer(who)?;
    if b.title.trim().is_empty() {
        return Err(ApiError::bad_request("invalid_ticket", "Напишите, что не работает."));
    }
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    if !is_open(&t.status) {
        return Err(bad_state("Заявка уже решена или закрыта."));
    }
    if t.parent_ticket_id.is_some() {
        return Err(bad_state("Заявка уже привязана к другой главной."));
    }
    sqlx::query("UPDATE tickets SET is_major = true, version = version + 1 WHERE id = $1")
        .bind(t.id)
        .execute(&mut *tx)
        .await?;
    t.is_major = true;
    sqlx::query(
        "UPDATE announcements SET ends_at = now() WHERE ticket_id = $1 AND (ends_at IS NULL OR ends_at > now())",
    )
    .bind(t.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO announcements (ticket_id, title, body, site_ids, created_by) VALUES ($1, $2, $3, $4, $5)")
        .bind(t.id)
        .bind(b.title.trim())
        .bind(b.body.trim())
        .bind(&b.site_ids)
        .bind(who.user)
        .execute(&mut *tx)
        .await?;
    event(&mut tx, t.id, who.user, "major", Value::Null, json!(b.title.trim())).await?;
    audit_rec(&mut tx, who, "ticket.major", &t, json!({"title": b.title.trim(), "sites": b.site_ids})).await?;
    start_work(&mut tx, &mut t, who).await?;
    events::publish(
        &mut *tx,
        &Event { kind: "announcement".into(), permission: Some("*".into()), ..Default::default() },
    )
    .await?;
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

/// Привязать заявку к главной или отвязать (`parent` = `None`).
pub async fn link(core: &Core, who: &Who, number: i64, parent: Option<i64>) -> ApiResult<()> {
    need_engineer(who)?;
    let mut tx = core.db.begin().await?;
    let mut t = lock(&mut tx, number, who).await?;
    let parent_id = match parent {
        Some(p) => {
            let row: Option<(Uuid, bool, String)> =
                sqlx::query_as("SELECT id, is_major, status FROM tickets WHERE number = $1")
                    .bind(p)
                    .fetch_optional(&mut *tx)
                    .await?;
            let (id, major, status) =
                row.ok_or_else(|| ApiError::bad_request("invalid_ticket", "Главная заявка не найдена."))?;
            if !is_open(&status) {
                return Err(bad_state("Главная заявка уже решена или закрыта."));
            }
            if !major || id == t.id {
                return Err(ApiError::bad_request("invalid_ticket", "Привязать можно только к другой главной заявке."));
            }
            if t.is_major {
                return Err(bad_state("Главную заявку нельзя привязать к другой."));
            }
            Some(id)
        }
        None => None,
    };
    sqlx::query("UPDATE tickets SET parent_ticket_id = $2, version = version + 1 WHERE id = $1")
        .bind(t.id)
        .bind(parent_id)
        .execute(&mut *tx)
        .await?;
    event(&mut tx, t.id, who.user, "linked", json!(t.parent_ticket_id), json!(parent)).await?;
    audit_rec(&mut tx, who, "ticket.link", &t, json!({"parent": parent})).await?;
    t.parent_ticket_id = parent_id;
    start_work(&mut tx, &mut t, who).await?;
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(())
}

// ------------------------------------------------------------------ Фоновые правила

/// Решённые, у которых вышел срок, закрываются сами.
/// Ошибка по одной заявке не останавливает остальные: пишется в журнал, обход продолжается.
pub async fn auto_close(core: &Core) -> ApiResult<usize> {
    let c = notify::ctx(core).await?;
    let who = Who::system();
    let numbers: Vec<i64> = sqlx::query_scalar(
        "SELECT number FROM tickets WHERE status = 'resolved' AND auto_close_at <= now() ORDER BY auto_close_at LIMIT 500",
    )
    .fetch_all(&core.db)
    .await?;
    let mut n = 0;
    for number in numbers {
        match close_due(core, &who, &c, number).await {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(e) => tracing::warn!(ticket = number, error = %e, "автозакрытие не выполнено"),
        }
    }
    Ok(n)
}

async fn close_due(core: &Core, who: &Who, c: &Ctx, number: i64) -> ApiResult<bool> {
    let mut tx = core.db.begin().await?;
    let t: Option<T> = sqlx::query_as(&format!(
        // SAFE: константа
        "SELECT {T_COLS} FROM tickets WHERE number = $1 AND status = 'resolved' AND auto_close_at <= now() FOR UPDATE"
    ))
    .bind(number)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(mut t) = t else { return Ok(false) };
    close(&mut tx, &mut t, who, c).await?;
    tx.commit().await?;
    Ok(true)
}

/// Инженер отключён или архивирован: его незакрытые заявки — в очередь (правило 8).
pub async fn release_orphans(core: &Core) -> ApiResult<usize> {
    let c = notify::ctx(core).await?;
    let who = Who::system();
    let numbers: Vec<i64> = sqlx::query_scalar(&format!(
        // SAFE: константа
        "SELECT t.number FROM tickets t {ORPHAN} ORDER BY t.number LIMIT 500"
    ))
    .bind(OPEN_STATUSES)
    .fetch_all(&core.db)
    .await?;
    let mut n = 0;
    for number in numbers {
        match release_one(core, &who, &c, number).await {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(ticket = number, error = %e, "заявка отключённого инженера не возвращена в очередь")
            }
        }
    }
    if n >= MANY_ORPHANS {
        // Обычно это одна-две заявки уволенного; много сразу — похоже, у роли сняли право работы с заявками.
        tracing::warn!(released = n, "в очередь возвращено много заявок: проверьте право helpdesk.ticket.work у ролей");
    }
    Ok(n)
}

/// С какого числа возвращённых за один запуск заявок писать предупреждение в журнал.
const MANY_ORPHANS: usize = 20;

/// Заявка открыта, а исполнитель отключён или больше не может работать с заявками (сняли роль) —
/// условие проверяется ещё раз под блокировкой.
const ORPHAN: &str = concat!(
    "JOIN users u ON u.id = t.assignee_id JOIN ad_accounts a ON a.id = u.ad_account_id
     WHERE t.status = ANY($1) AND (NOT u.is_active OR NOT a.is_enabled OR a.is_missing OR NOT (true",
    crate::can_work_sql!(),
    "))"
);

async fn release_one(core: &Core, who: &Who, c: &Ctx, number: i64) -> ApiResult<bool> {
    let mut tx = core.db.begin().await?;
    // Пока шёл обход, заявку могли переназначить или решить — тогда её не трогаем.
    let cols = T_COLS.split(", ").map(|col| format!("t.{col}")).collect::<Vec<_>>().join(", ");
    let t: Option<T> = sqlx::query_as(&format!(
        // SAFE: константы
        "SELECT {cols} FROM tickets t {ORPHAN} AND t.number = $2 FOR UPDATE OF t"
    ))
    .bind(OPEN_STATUSES)
    .bind(number)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(mut t) = t else { return Ok(false) };
    let before = t.assignee_id;
    sqlx::query("UPDATE tickets SET assignee_id = NULL, version = version + 1 WHERE id = $1")
        .bind(t.id)
        .execute(&mut *tx)
        .await?;
    t.assignee_id = None;
    event(&mut tx, t.id, None, "assigned", json!(before), Value::Null).await?;
    audit_rec(
        &mut tx,
        who,
        "ticket.assign",
        &t,
        json!({"before": before, "after": null, "reason": "assignee_disabled"}),
    )
    .await?;
    set_status(&mut tx, &mut t, who, "new").await?;
    notify::unassigned_alert(&mut tx, &t, c).await?;
    ping(&mut tx, &t).await?;
    tx.commit().await?;
    Ok(true)
}

/// Токен из письма: `(заявка, запись токена)` под блокировкой, если он действует.
/// Сначала блокируется заявка, потом токен — в том же порядке, что и при закрытии из портала.
pub async fn lock_token(tx: &mut Transaction<'_, Postgres>, token: &str) -> ApiResult<Option<(T, Uuid)>> {
    let hash = sha256(token.as_bytes());
    let ticket: Option<Uuid> =
        sqlx::query_scalar("SELECT ticket_id FROM ticket_tokens WHERE token_hash = $1 AND purpose = 'resolve_confirm'")
            .bind(&hash)
            .fetch_optional(&mut **tx)
            .await?;
    let Some(ticket) = ticket else { return Ok(None) };
    let t: T = sqlx::query_as(&select_t("WHERE id = $1 FOR UPDATE")).bind(ticket).fetch_one(&mut **tx).await?;
    let token_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM ticket_tokens
          WHERE token_hash = $1 AND purpose = 'resolve_confirm' AND used_at IS NULL AND expires_at > now() FOR UPDATE",
    )
    .bind(&hash)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(token_id.filter(|_| t.status == "resolved").map(|id| (t, id)))
}

/// Ответ заявителя по ссылке из письма. Токен одноразовый.
pub async fn respond_by_token(core: &Core, token: &str, accepted: bool, comment_text: &str) -> ApiResult<i64> {
    let c = notify::ctx(core).await?;
    let mut tx = core.db.begin().await?;
    let Some((mut t, token_id)) = lock_token(&mut tx, token).await? else {
        return Err(ApiError::bad_request("token_invalid", "Ссылка уже использована или срок её действия истёк."));
    };
    let who = Who { source: Source::Email, ..Who::system() };
    let comment_text = comment_text.trim();
    if !accepted && comment_text.is_empty() {
        return Err(ApiError::bad_request("comment_required", "Напишите, что не так."));
    }
    sqlx::query("UPDATE ticket_tokens SET used_at = now() WHERE id = $1").bind(token_id).execute(&mut *tx).await?;
    if accepted {
        close(&mut tx, &mut t, &who, &c).await?;
    } else {
        let name: Option<String> = sqlx::query_scalar(
            "SELECT nullif(trim(concat_ws(' ', last_name, first_name)), '') FROM persons WHERE id = $1",
        )
        .bind(t.requester_id)
        .fetch_optional(&mut *tx)
        .await?
        .flatten();
        let cm = NewComment {
            body: comment_text.to_string(),
            source: "email".into(),
            author_name: name,
            ..Default::default()
        };
        insert_comment(&mut tx, t.id, &who, &cm).await?;
        reopen(&mut tx, &mut t, &who, &c, Some(comment_text)).await?;
        ping(&mut tx, &t).await?;
    }
    tx.commit().await?;
    Ok(t.number)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_computer_name_drops_domain() {
        assert_eq!(short_name(" ws-emp.pult.test "), "ws-emp");
        assert_eq!(short_name("WS-EMP"), "WS-EMP");
        assert_eq!(short_name(""), "");
    }

    fn field(key: &str, kind: &str, required: bool) -> FormField {
        FormField {
            key: key.into(),
            label: key.into(),
            kind: kind.into(),
            required,
            options: vec!["A".into(), "B".into()],
        }
    }

    #[test]
    fn required_fields_are_enforced() {
        let schema = [field("model", "text", true)];
        assert!(validate_answers(&schema, &json!({})).is_err());
        assert!(validate_answers(&schema, &json!({"model": "  "})).is_err());
        assert_eq!(validate_answers(&schema, &json!({"model": "HP"})).unwrap(), json!({"model": "HP"}));
    }

    #[test]
    fn values_are_checked_and_extra_keys_dropped() {
        let schema = [field("s", "select", false), field("b", "bool", false)];
        assert!(validate_answers(&schema, &json!({"s": "C"})).is_err(), "нет в списке");
        assert!(validate_answers(&schema, &json!({"b": "да"})).is_err(), "не логическое");
        assert_eq!(
            validate_answers(&schema, &json!({"s": "A", "b": true, "x": 1})).unwrap(),
            json!({"s": "A", "b": true})
        );
    }

    #[test]
    fn requester_and_engineer_visibility() {
        let (me, person) = (Uuid::now_v7(), Uuid::now_v7());
        let who = Who { user: Some(me), person: Some(person), ..Who::system() };
        let t = T {
            id: Uuid::now_v7(),
            number: 1,
            title: "x".into(),
            status: "new".into(),
            priority: "normal".into(),
            category_id: None,
            site_id: None,
            requester_id: None,
            requester_user_id: None,
            assignee_id: None,
            line: "l1".into(),
            parent_ticket_id: None,
            is_major: false,
            first_response_at: None,
            taken_at: None,
            version: 1,
        };
        assert!(!who.can_see(&t), "чужая заявка");
        assert!(who.can_see(&T { requester_id: Some(person), ..t.clone() }));
        assert!(who.can_see(&T { requester_user_id: Some(me), ..t.clone() }));
        assert!(Who { engineer: true, ..who.clone() }.can_see(&t));
        // Система без пользователя не становится заявителем заявки без заявителя.
        assert!(!Who::system().can_see(&t));
    }
}
