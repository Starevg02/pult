//! Уведомления по заявке: письма через outbox и колокольчик в портале.
//! Внутренние заметки сюда не попадают по построению: `Msg` несёт только публичный текст.

use crate::{schedule::AfterHours, settings::HelpdeskSettings, tickets::T};
use pult_core::{
    ApiResult, Core,
    mail::{MailSettings, Outgoing, ThreadHeaders, enqueue_message},
    notify,
    rbac::ROLE_SUPERADMIN,
    settings,
};
use serde_json::json;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Адрес портала и домен для заголовков.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub portal: String,
    pub domain: String,
}

pub async fn ctx(core: &Core) -> ApiResult<Ctx> {
    let mut tx = core.db.begin().await?;
    let c = ctx_tx(&mut tx).await?;
    tx.commit().await?;
    Ok(c)
}

/// То же внутри транзакции.
pub async fn ctx_tx(tx: &mut Transaction<'_, Postgres>) -> Result<Ctx, sqlx::Error> {
    let h: HelpdeskSettings = settings::load(&mut **tx).await?;
    let m: MailSettings = settings::load(&mut **tx).await?;
    let domain = m.from_address.rsplit_once('@').map(|(_, d)| d.to_string()).unwrap_or_else(|| "pult.local".into());
    Ok(Ctx { portal: h.portal_url.trim_end_matches('/').to_string(), domain })
}

/// Письмо на произвольный адрес (отчёт).
pub async fn to_address(
    tx: &mut Transaction<'_, Postgres>,
    to: &str,
    subject: &str,
    m: &Msg<'_>,
    c: &Ctx,
) -> Result<(), sqlx::Error> {
    let (_, text, html) = render("ru", m, 0, subject, c);
    enqueue_message(
        &mut **tx,
        &Outgoing {
            to: vec![to.to_string()],
            subject: subject.to_string(),
            text,
            html: Some(html),
            headers: ThreadHeaders::default(),
        },
    )
    .await?;
    Ok(())
}

/// Что сообщаем.
#[derive(Clone, Copy)]
pub enum Msg<'a> {
    Created {
        assignee: Option<&'a str>,
        after: Option<&'a AfterHours>,
    },
    Reply {
        body: &'a str,
    },
    Waiting {
        body: Option<&'a str>,
    },
    Resolved {
        body: &'a str,
        token: &'a str,
    },
    Closed,
    Assigned,
    RequesterReplied {
        body: &'a str,
    },
    Reopened {
        body: Option<&'a str>,
    },
    OnHold {
        body: &'a str,
    },
    /// Напоминание по «Ждёт ответа».
    Reminder,
    SlaWarning,
    SlaBreached,
    BadRating {
        comment: Option<&'a str>,
    },
    /// Текст отчёта — для еженедельной рассылки.
    Report {
        body: &'a str,
    },
    /// Владельцу ресурса: запрос доступа ждёт согласования.
    ApprovalRequested {
        resource: &'a str,
        person: &'a str,
    },
    /// Исполнителю: владелец решил.
    ApprovalDecided {
        approved: bool,
        comment: Option<&'a str>,
    },
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn para(s: &str) -> String {
    format!("<p style=\"white-space:pre-wrap\">{}</p>", esc(s))
}

fn button(url: &str, text: &str, color: &str) -> String {
    format!(
        "<a href=\"{}\" style=\"display:inline-block;padding:10px 18px;margin-right:8px;border-radius:8px;background:{color};color:#fff;text-decoration:none\">{}</a>",
        esc(url),
        esc(text)
    )
}

/// `(тема, текст, HTML)`. Тема всегда содержит `[#номер]` — по нему ответ вернётся в заявку.
pub fn render(lang: &str, m: &Msg<'_>, number: i64, title: &str, c: &Ctx) -> (String, String, String) {
    // Английский — прямо здесь, китайский — по каталогу `l10n/zh.tsv` (тест следит, чтобы строки были в нём).
    let lang = pult_core::l10n::Lang::parse(lang);
    let l = |ru: &'static str, en_text: &'static str| -> std::borrow::Cow<'static, str> {
        match lang {
            pult_core::l10n::Lang::En => en_text.into(),
            pult_core::l10n::Lang::Zh => pult_core::l10n::tr_to(lang, ru),
            pult_core::l10n::Lang::Ru => ru.into(),
        }
    };
    let link = if c.portal.is_empty() { String::new() } else { format!("{}/tickets/{number}", c.portal) };
    let (head, body): (String, String) = match m {
        Msg::Created { assignee, after } => {
            let mut b = match assignee {
                Some(a) => format!(
                    "{} {a}.",
                    l("Заявка принята. Исполнитель:", "Your request has been received. Assigned to:")
                ),
                None => l(
                    "Заявка принята, исполнитель скоро будет назначен.",
                    "Your request has been received and will be assigned soon.",
                )
                .to_string(),
            };
            if let Some(a) = after {
                b.push_str("\n\n");
                b.push_str(&format!(
                    "{} {}.",
                    l(
                        "Сейчас нерабочее время, заявка будет взята в",
                        "It is outside working hours, the request will be picked up at"
                    ),
                    a.next_open
                ));
                if let Some(p) = &a.emergency_phone {
                    b.push_str(&format!(" {} {p}.", l("Если авария — звоните", "In an emergency call")));
                }
            }
            (l("Заявка принята", "Request received").to_string(), b)
        }
        Msg::Reply { body } => (l("Ответ по заявке", "Reply to your request").to_string(), (*body).to_string()),
        Msg::Waiting { body } => (
            l("Нужна информация по заявке", "Information needed").to_string(),
            body.map(str::to_string).unwrap_or_else(|| {
                l(
                    "Исполнителю нужна дополнительная информация — ответьте в заявке на портале.",
                    "Please add more information to the request on the portal.",
                )
                .to_string()
            }),
        ),
        Msg::Resolved { body, .. } => (l("Заявка решена", "Request resolved").to_string(), (*body).to_string()),
        Msg::Closed => (
            l("Заявка закрыта", "Request closed").to_string(),
            l("Заявка закрыта.", "The request has been closed.").to_string(),
        ),
        Msg::Assigned => (l("Вам назначена заявка", "A request was assigned to you").to_string(), title.to_string()),
        Msg::RequesterReplied { body } => {
            (l("Заявитель ответил", "The requester replied").to_string(), (*body).to_string())
        }
        Msg::OnHold { body } => (l("Заявка отложена", "Request on hold").to_string(), (*body).to_string()),
        Msg::Reminder => (
            l("Ждём вашего ответа", "We are waiting for your reply").to_string(),
            l(
                "Исполнитель ждёт от вас информацию по заявке. Если ответа не будет, заявка будет решена без неё.",
                "The engineer is waiting for your reply. Without it the request will be resolved as is.",
            )
            .to_string(),
        ),
        Msg::SlaWarning => (
            l("Скоро срок по заявке", "Request is nearly due").to_string(),
            l("Осталось меньше 20 % срока SLA.", "Less than 20% of the SLA time is left.").to_string(),
        ),
        Msg::SlaBreached => (
            l("Срок по заявке нарушен", "Request is overdue").to_string(),
            l("Срок SLA истёк.", "The SLA time is over.").to_string(),
        ),
        Msg::BadRating { comment } => (
            l("Заявитель оценил решение: плохо", "The requester rated the resolution: bad").to_string(),
            comment.map(str::to_string).unwrap_or_else(|| l("Без комментария.", "No comment.").to_string()),
        ),
        Msg::Report { body } => (l("Отчёт Helpdesk", "Helpdesk report").to_string(), (*body).to_string()),
        Msg::ApprovalRequested { resource, person } => (
            l("Нужно ваше согласование доступа", "Your approval is needed").to_string(),
            format!(
                "{} {person}: «{resource}». {}",
                l("Запрос доступа от", "Access request from"),
                l("Решение — в разделе «Согласования» на портале.", "Decide in “Approvals” on the portal.")
            ),
        ),
        Msg::ApprovalDecided { approved, comment } => (
            if *approved {
                l("Доступ согласован", "Access approved")
            } else {
                l("В доступе отказано", "Access rejected")
            }
            .to_string(),
            comment.map(str::to_string).unwrap_or_else(|| l("Без комментария.", "No comment.").to_string()),
        ),
        Msg::Reopened { body } => (
            l("Заявка возвращена в работу", "Request reopened").to_string(),
            body.map(str::to_string).unwrap_or_else(|| {
                l("Заявитель сообщил, что проблема не решена.", "The requester says the problem is not resolved.")
                    .to_string()
            }),
        ),
    };
    let subject = if number > 0 { format!("[#{number}] {title}") } else { head.clone() };
    let mut text = format!("{head}\n\n{body}\n");
    let mut html =
        format!("<div style=\"font-family:sans-serif;font-size:14px\"><h3>{}</h3>{}", esc(&head), para(&body));
    if let Msg::Resolved { token, .. } = m
        && !c.portal.is_empty()
        && !token.is_empty()
    {
        let yes = format!("{}/ticket-response/{token}?a=yes", c.portal);
        let no = format!("{}/ticket-response/{token}?a=no", c.portal);
        let (y, n) = (l("Да, решена", "Yes, resolved"), l("Нет, не решена", "No, not resolved"));
        text.push_str(&format!("\n{}\n{y}: {yes}\n{n}: {no}\n", l("Проблема решена?", "Is the problem resolved?")));
        html.push_str(&format!(
            "<p>{}</p><p>{}{}</p>",
            esc(&l("Проблема решена?", "Is the problem resolved?")),
            button(&yes, &y, "#15803d"),
            button(&no, &n, "#b91c1c")
        ));
    }
    if !link.is_empty() && number > 0 {
        text.push_str(&format!("\n{}: {link}\n", l("Заявка", "Request")));
        html.push_str(&format!(
            "<p><a href=\"{}\">{} #{number}</a></p>",
            esc(&link),
            esc(&l("Открыть заявку", "Open request"))
        ));
    }
    // Почта — только уведомления: ответы в ящик никто не читает.
    let note = l(
        "Письмо отправлено автоматически, отвечать на него не нужно. Написать можно в заявке на портале.",
        "This is an automatic notification, please do not reply. You can write in the request on the portal.",
    );
    text.push_str(&format!("\n{note}\n"));
    html.push_str(&format!("<p style=\"color:#666;font-size:12px\">{}</p>", esc(&note)));
    html.push_str("</div>");
    (subject, text, html)
}

fn headers(id: Uuid, c: &Ctx) -> ThreadHeaders {
    let root = format!("<ticket-{id}@{}>", c.domain);
    ThreadHeaders {
        message_id: Some(format!("<ticket-{id}.{}@{}>", Uuid::now_v7(), c.domain)),
        in_reply_to: Some(root.clone()),
        references: Some(root),
    }
}

async fn queue(
    tx: &mut Transaction<'_, Postgres>,
    to: &str,
    lang: &str,
    t: &T,
    m: &Msg<'_>,
    c: &Ctx,
) -> Result<(), sqlx::Error> {
    let (subject, text, html) = render(lang, m, t.number, &t.title, c);
    enqueue_message(
        &mut **tx,
        &Outgoing { to: vec![to.to_string()], subject, text, html: Some(html), headers: headers(t.id, c) },
    )
    .await?;
    Ok(())
}

/// Письмо заявителю (обязательное). Нет адреса — молча пропускаем: заявка есть и без почты.
pub async fn to_requester(tx: &mut Transaction<'_, Postgres>, t: &T, m: &Msg<'_>, c: &Ctx) -> Result<(), sqlx::Error> {
    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT coalesce(nullif(p.email, ''), a.mail), u.locale
           FROM tickets t
           LEFT JOIN persons p ON p.id = t.requester_id
           LEFT JOIN users u ON u.id = coalesce(t.requester_user_id, (SELECT id FROM users WHERE person_id = t.requester_id LIMIT 1))
           LEFT JOIN ad_accounts a ON a.id = u.ad_account_id
          WHERE t.id = $1",
    )
    .bind(t.id)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((Some(to), lang)) = row
        && to.contains('@')
    {
        queue(tx, &to, lang.as_deref().unwrap_or("ru"), t, m, c).await?;
    }
    to_watchers(tx, t, m, c).await
}

/// Копия наблюдателям. Кнопки «Да / Нет» — только заявителю.
async fn to_watchers(tx: &mut Transaction<'_, Postgres>, t: &T, m: &Msg<'_>, c: &Ctx) -> Result<(), sqlx::Error> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT ON (lower(addr)) addr, locale FROM (
             SELECT coalesce(nullif(p.email, ''), a.mail) AS addr, u.locale
               FROM ticket_watchers w
               JOIN persons p ON p.id = w.person_id
               LEFT JOIN users u ON u.person_id = p.id
               LEFT JOIN ad_accounts a ON a.id = u.ad_account_id
              WHERE w.ticket_id = $1 AND p.id IS DISTINCT FROM $2) x
          WHERE addr LIKE '%@%'",
    )
    .bind(t.id)
    .bind(t.requester_id)
    .fetch_all(&mut **tx)
    .await?;
    let copy = match *m {
        Msg::Resolved { body, .. } => Msg::Resolved { body, token: "" },
        Msg::Created { .. } | Msg::Reply { .. } | Msg::Waiting { .. } | Msg::OnHold { .. } | Msg::Closed => *m,
        _ => return Ok(()),
    };
    for (to, lang) in rows {
        queue(tx, &to, lang.as_deref().unwrap_or("ru"), t, &copy, c).await?;
    }
    Ok(())
}

/// Письмо инженеру; необязательное — отключается в личных настройках.
pub async fn to_user(
    tx: &mut Transaction<'_, Postgres>,
    user: Uuid,
    kind: &str,
    t: &T,
    m: &Msg<'_>,
    c: &Ctx,
) -> Result<(), sqlx::Error> {
    let row: Option<(Option<String>, String, bool)> = sqlx::query_as(
        "SELECT coalesce(nullif(p.email, ''), a.mail), u.locale,
                coalesce((SELECT email FROM notification_prefs np WHERE np.user_id = u.id AND np.kind = $2), true)
           FROM users u
           JOIN ad_accounts a ON a.id = u.ad_account_id
           LEFT JOIN persons p ON p.id = u.person_id
          WHERE u.id = $1",
    )
    .bind(user)
    .bind(kind)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((Some(to), lang, true)) = row
        && to.contains('@')
    {
        queue(tx, &to, &lang, t, m, c).await?;
    }
    Ok(())
}

/// Колокольчик.
pub async fn bell(tx: &mut Transaction<'_, Postgres>, user: Uuid, kind: &str, t: &T) -> Result<(), sqlx::Error> {
    notify::notify(&mut **tx, user, kind, json!({"number": t.number, "title": t.title})).await
}

/// Заявка без исполнителя — немедленно всем суперадминистраторам (правило 4).
pub async fn unassigned_alert(tx: &mut Transaction<'_, Postgres>, t: &T, c: &Ctx) -> Result<(), sqlx::Error> {
    let admins: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT u.id FROM users u JOIN user_roles ur ON ur.user_id = u.id
           JOIN roles r ON r.id = ur.role_id AND r.code = $1 WHERE u.is_active",
    )
    .bind(ROLE_SUPERADMIN)
    .fetch_all(&mut **tx)
    .await?;
    for a in admins {
        bell(tx, a, "helpdesk.unassigned", t).await?;
        to_user(tx, a, "helpdesk.unassigned", t, &Msg::Assigned, c).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c() -> Ctx {
        Ctx { portal: "https://pult.example.test".into(), domain: "example.test".into() }
    }

    #[test]
    fn mail_says_not_to_reply() {
        let (_, text, html) = render("ru", &Msg::Closed, 7, "x", &c());
        assert!(text.contains("отвечать на него не нужно") && html.contains("отвечать на него не нужно"));
        let (_, text, _) = render("ru", &Msg::Waiting { body: None }, 7, "x", &c());
        assert!(!text.contains("ответьте на это письмо"));
    }

    #[test]
    fn chinese_mail() {
        let (_, text, html) = render("zh", &Msg::Resolved { body: "已更换墨盒", token: "tok" }, 7, "x", &c());
        assert!(html.contains("是，已解决") && html.contains("否，未解决"), "{html}");
        assert!(text.contains("请勿回复") && !text.chars().any(pult_core::l10n::is_cyrillic), "{text}");
    }

    #[test]
    fn subject_carries_ticket_number() {
        let (s, ..) = render("ru", &Msg::Closed, 1234, "Принтер", &c());
        assert_eq!(s, "[#1234] Принтер");
    }

    #[test]
    fn resolved_mail_has_both_links() {
        let (_, text, html) = render("ru", &Msg::Resolved { body: "Заменили картридж", token: "tok" }, 7, "x", &c());
        assert!(text.contains("/ticket-response/tok?a=yes") && text.contains("/ticket-response/tok?a=no"));
        assert!(html.contains("Да, решена") && html.contains("Нет, не решена"));
    }

    #[test]
    fn watcher_copy_has_no_buttons() {
        let (_, text, _) = render("ru", &Msg::Resolved { body: "Готово", token: "" }, 7, "x", &c());
        assert!(!text.contains("/ticket-response/"));
    }

    #[test]
    fn user_text_is_escaped_in_html() {
        let (_, _, html) = render("ru", &Msg::Reply { body: "<script>alert(1)</script>" }, 7, "x", &c());
        assert!(!html.contains("<script>") && html.contains("&lt;script&gt;"));
    }

    #[test]
    fn after_hours_text_with_emergency_phone() {
        let a = AfterHours { next_open: "2026-11-10 09:00".into(), emergency_phone: Some("100".into()) };
        let (_, text, _) = render("ru", &Msg::Created { assignee: None, after: Some(&a) }, 7, "x", &c());
        assert!(text.contains("нерабочее время") && text.contains("2026-11-10 09:00") && text.contains("звоните 100"));
    }

    #[test]
    fn english_template() {
        let (_, text, _) = render("en", &Msg::Closed, 7, "x", &c());
        assert!(text.contains("has been closed"));
    }
}
