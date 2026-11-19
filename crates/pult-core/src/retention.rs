//! Сроки хранения служебных записей (docs/admin-guide.md): без очистки таблицы растут бесконечно.
//! Журнал аудита сюда не входит — он хранится целиком (цепочка хешей). Запускает задача `jobs.prune`.

use sqlx::PgPool;

const RULES: &[&str] = &[
    // Отправленные письма — 90 дней, неотправленные (failed) — полгода для разбора.
    "DELETE FROM outbox_emails WHERE (status = 'sent' AND created_at < now() - interval '90 days')
        OR (status = 'failed' AND created_at < now() - interval '180 days')",
    // Прочитанные уведомления — полгода.
    "DELETE FROM notifications WHERE read_at IS NOT NULL AND read_at < now() - interval '180 days'",
    // Ссылки из писем: погашенные и истёкшие — через месяц.
    "DELETE FROM ticket_tokens WHERE (used_at IS NOT NULL OR expires_at < now()) AND created_at < now() - interval '30 days'",
    // Журнал синхронизации AD — 90 дней.
    "DELETE FROM ad_sync_runs WHERE started_at < now() - interval '90 days'",
];

/// Удалить записи старше срока хранения; возвращает число удалённых.
pub async fn prune(db: &PgPool) -> Result<u64, sqlx::Error> {
    let mut n = 0;
    for q in RULES {
        n += sqlx::query(q).execute(db).await?.rows_affected();
    }
    Ok(n)
}
