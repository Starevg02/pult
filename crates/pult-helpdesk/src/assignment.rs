//! Назначение исполнителя. Все правила — здесь.

use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Условие «пользователь `u` может работать с заявками»: суперадминистратор или роль с правом
/// `helpdesk.ticket.work` (в том числе неявная «Сотрудник», если ей дали это право). Без него инженер,
/// у которого сняли роль, но оставили в площадке или группе, получал бы заявки, которых не видит.
#[macro_export]
macro_rules! can_work_sql {
    () => {
        " AND (EXISTS (SELECT 1 FROM user_roles ur JOIN roles r ON r.id = ur.role_id
                        LEFT JOIN role_permissions rp ON rp.role_id = r.id AND rp.permission = 'helpdesk.ticket.work'
                       WHERE ur.user_id = u.id AND (r.code = 'superadmin' OR rp.permission IS NOT NULL))
               OR EXISTS (SELECT 1 FROM roles r JOIN role_permissions rp ON rp.role_id = r.id
                           WHERE r.code = 'employee' AND rp.permission = 'helpdesk.ticket.work'))"
    };
}

/// Статусы, которые считаются нагрузкой инженера (правило 3).
pub const LOAD_STATUSES: &[&str] = &["assigned", "in_progress", "waiting", "on_hold", "waiting_approval"];

/// Выбор по правилам 2–3, 9: активные инженеры линии площадки без отметки отсутствия,
/// наименее загруженный, при равенстве — кто дольше не получал заявку.
///
/// Строки инженеров площадки блокируются первым запросом, нагрузка считается вторым: на уровне
/// READ COMMITTED у него свежий снимок, и конкурентная заявка, дождавшаяся блокировки, видит
/// назначение предыдущей. Поэтому два одновременных создания не отдадут оба одному инженеру.
pub async fn pick(tx: &mut Transaction<'_, Postgres>, site: Uuid, line: &str) -> Result<Option<Uuid>, sqlx::Error> {
    let candidates: Vec<Uuid> = sqlx::query_scalar(concat!(
        "SELECT se.user_id
           FROM site_engineers se
           JOIN users u ON u.id = se.user_id
           JOIN ad_accounts a ON a.id = u.ad_account_id
          WHERE se.site_id = $1 AND se.line = $2 AND u.is_active AND a.is_enabled AND NOT a.is_missing
            AND NOT EXISTS (SELECT 1 FROM engineer_absences ab
                             WHERE ab.user_id = se.user_id AND ab.starts_at <= now() AND ab.ends_at > now())
         ",
        can_work_sql!(),
        " ORDER BY se.user_id FOR UPDATE OF se",
    ))
    .bind(site)
    .bind(line)
    .fetch_all(&mut **tx)
    .await?;
    if candidates.is_empty() {
        return Ok(None);
    }
    sqlx::query_scalar(
        "SELECT c.id
           FROM unnest($1::uuid[]) AS c(id)
           LEFT JOIN LATERAL (
                SELECT count(*) FILTER (WHERE t.status = ANY($2)) AS load, max(t.assigned_at) AS last_assigned
                  FROM tickets t WHERE t.assignee_id = c.id
           ) l ON true
          ORDER BY coalesce(l.load, 0), l.last_assigned NULLS FIRST, c.id
          LIMIT 1",
    )
    .bind(&candidates)
    .bind(LOAD_STATUSES)
    .fetch_optional(&mut **tx)
    .await
}

/// Наименее загруженный активный участник группы без отметки отсутствия.
pub async fn pick_in_group(tx: &mut Transaction<'_, Postgres>, group: Uuid) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(concat!(
        "SELECT m.user_id
           FROM engineer_group_members m
           JOIN users u ON u.id = m.user_id
           JOIN ad_accounts a ON a.id = u.ad_account_id
           LEFT JOIN LATERAL (
                SELECT count(*) FILTER (WHERE t.status = ANY($2)) AS load, max(t.assigned_at) AS last_assigned
                  FROM tickets t WHERE t.assignee_id = m.user_id
           ) l ON true
          WHERE m.group_id = $1 AND u.is_active AND a.is_enabled AND NOT a.is_missing
            AND NOT EXISTS (SELECT 1 FROM engineer_absences ab
                             WHERE ab.user_id = m.user_id AND ab.starts_at <= now() AND ab.ends_at > now())
         ",
        can_work_sql!(),
        " ORDER BY coalesce(l.load, 0), l.last_assigned NULLS FIRST, m.user_id LIMIT 1",
    ))
    .bind(group)
    .bind(LOAD_STATUSES)
    .fetch_optional(&mut **tx)
    .await
}

/// Инженер активен и его можно оставить исполнителем.
pub async fn is_active(tx: &mut Transaction<'_, Postgres>, user: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(concat!(
        "SELECT EXISTS (SELECT 1 FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id
                         WHERE u.id = $1 AND u.is_active AND a.is_enabled AND NOT a.is_missing",
        can_work_sql!(),
        ")",
    ))
    .bind(user)
    .fetch_one(&mut **tx)
    .await
}

/// Пользователь — инженер хотя бы одной площадки.
pub async fn is_engineer(tx: &mut Transaction<'_, Postgres>, user: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM site_engineers WHERE user_id = $1)")
        .bind(user)
        .fetch_one(&mut **tx)
        .await
}
