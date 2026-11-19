//! Очередь фоновых задач в PostgreSQL (ADR-0004).
//!
//! - Постановка — обычный `INSERT` в той же транзакции, что и бизнес-данные.
//! - Захват — `FOR UPDATE SKIP LOCKED`: несколько обработчиков (и несколько копий сервера)
//!   не берут одну задачу дважды.
//! - Аренда (`locked_until`): задача упавшего обработчика снова станет доступна.
//! - Повторы с нарастающей паузой; после `max_attempts` — статус `failed` для админки.
//! - Ключ идемпотентности: повторная постановка не создаёт дубль.
//! - Итог записывается, только если задача всё ещё за этой попыткой: обработчик, чья аренда
//!   истекла и задачу перехватили, не затрёт чужой результат.
//!
//! Расписания — [`cron`], исполнитель и планировщик — [`runner`].

pub mod cron;
pub mod runner;

pub use runner::{HandlerError, Registry, RunnerConfig};

use serde_json::Value;
use sqlx::{PgExecutor, PgPool, Row};
use std::time::Duration;
use time::OffsetDateTime;
use uuid::Uuid;

/// Новая задача.
#[derive(Debug, Clone)]
pub struct NewJob {
    pub kind: String,
    pub payload: Value,
    pub idempotency_key: Option<String>,
    /// Не раньше этого момента (`None` — сразу).
    pub run_at: Option<OffsetDateTime>,
    pub max_attempts: i32,
}

impl NewJob {
    pub fn new(kind: impl Into<String>, payload: Value) -> Self {
        Self { kind: kind.into(), payload, idempotency_key: None, run_at: None, max_attempts: 5 }
    }

    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    pub fn run_at(mut self, at: OffsetDateTime) -> Self {
        self.run_at = Some(at);
        self
    }

    pub fn max_attempts(mut self, n: i32) -> Self {
        self.max_attempts = n.max(1);
        self
    }
}

/// Захваченная задача.
#[derive(Debug, Clone)]
pub struct Job {
    pub id: Uuid,
    pub kind: String,
    pub payload: Value,
    /// Номер текущей попытки, начиная с 1.
    pub attempt: i32,
    pub max_attempts: i32,
}

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Ставит задачу. Возвращает `None`, если задача с таким ключом идемпотентности уже есть.
/// `ex` — пул или транзакция: задача появится, только если транзакция зафиксирована.
pub async fn enqueue<'e>(ex: impl PgExecutor<'e>, job: &NewJob) -> Result<Option<Uuid>, JobError> {
    let id = Uuid::now_v7();
    let row = sqlx::query(
        "INSERT INTO jobs (id, kind, payload, idempotency_key, run_at, max_attempts)
         VALUES ($1, $2, $3, $4, COALESCE($5, now()), $6)
         ON CONFLICT (idempotency_key) DO NOTHING
         RETURNING id",
    )
    .bind(id)
    .bind(&job.kind)
    .bind(&job.payload)
    .bind(&job.idempotency_key)
    .bind(job.run_at)
    .bind(job.max_attempts)
    .fetch_optional(ex)
    .await?;
    Ok(row.map(|r| r.get::<Uuid, _>("id")))
}

/// Захватывает до `limit` готовых задач (и задачи с истёкшей арендой) на время `lease`.
/// `kinds` — только эти виды (например, отдельный обработчик операций каталога AD); `None` — любые.
pub async fn claim(
    pool: &PgPool,
    worker: &str,
    kinds: Option<&[String]>,
    limit: i64,
    lease: Duration,
) -> Result<Vec<Job>, JobError> {
    // Аренда истекла, а попытки кончились: обработчик, скорее всего, гибнет на этой задаче.
    // Не берём её снова по кругу, а отдаём в админку как упавшую.
    let dead = sqlx::query(
        "UPDATE jobs SET status = 'failed', locked_by = NULL, locked_until = NULL, updated_at = now(),
                         last_error = coalesce(last_error, 'Обработчик не завершил задачу: аренда истекла.')
         WHERE status = 'running' AND locked_until < now() AND attempts >= max_attempts
           AND ($1::text[] IS NULL OR kind = ANY($1))",
    )
    .bind(kinds)
    .execute(pool)
    .await?;
    if dead.rows_affected() > 0 {
        tracing::warn!(jobs = dead.rows_affected(), "задачи с истёкшей арендой и без попыток отмечены упавшими");
    }
    let rows = sqlx::query(
        "UPDATE jobs SET status = 'running', attempts = attempts + 1, locked_by = $1,
                         locked_until = now() + make_interval(secs => $2), updated_at = now()
         WHERE id IN (
             SELECT id FROM jobs
             WHERE ((status = 'queued' AND run_at <= now())
                 OR (status = 'running' AND locked_until < now() AND attempts < max_attempts))
               AND ($4::text[] IS NULL OR kind = ANY($4))
             ORDER BY run_at
             FOR UPDATE SKIP LOCKED
             LIMIT $3)
         RETURNING id, kind, payload, attempts, max_attempts",
    )
    .bind(worker)
    .bind(lease.as_secs_f64())
    .bind(limit)
    .bind(kinds)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Job {
            id: r.get("id"),
            kind: r.get("kind"),
            payload: r.get("payload"),
            attempt: r.get("attempts"),
            max_attempts: r.get("max_attempts"),
        })
        .collect())
}

/// Задача выполнена. `false` — задачу уже перехватил другой обработчик (аренда истекла).
pub async fn complete(pool: &PgPool, job: &Job) -> Result<bool, JobError> {
    let r = sqlx::query(
        "UPDATE jobs SET status = 'done', locked_by = NULL, locked_until = NULL, updated_at = now()
         WHERE id = $1 AND attempts = $2 AND status = 'running'",
    )
    .bind(job.id)
    .bind(job.attempt)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// Ошибка выполнения: повтор через паузу или `failed`, если попытки кончились.
pub async fn fail(pool: &PgPool, job: &Job, error: &str) -> Result<bool, JobError> {
    let delay = retry_delay(job.attempt);
    let give_up = job.attempt >= job.max_attempts;
    tracing::warn!(job = %job.id, kind = %job.kind, attempt = job.attempt, give_up, "задача завершилась ошибкой");
    let r = sqlx::query(
        "UPDATE jobs SET status = CASE WHEN $2 THEN 'failed' ELSE 'queued' END,
                         run_at = now() + make_interval(secs => $3),
                         last_error = left($4, 2000), locked_by = NULL, locked_until = NULL, updated_at = now()
         WHERE id = $1 AND attempts = $5 AND status = 'running'",
    )
    .bind(job.id)
    .bind(give_up)
    .bind(delay.as_secs_f64())
    .bind(error)
    .bind(job.attempt)
    .execute(pool)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// Удаляет выполненные задачи старше `done_after` и упавшие старше `failed_after`.
/// Упавшие живут дольше: их разбирают в админке.
pub async fn prune(pool: &PgPool, done_after: Duration, failed_after: Duration) -> Result<u64, JobError> {
    let r = sqlx::query(
        "DELETE FROM jobs
         WHERE (status = 'done' AND updated_at < now() - make_interval(secs => $1))
            OR (status = 'failed' AND updated_at < now() - make_interval(secs => $2))",
    )
    .bind(done_after.as_secs_f64())
    .bind(failed_after.as_secs_f64())
    .execute(pool)
    .await?;
    Ok(r.rows_affected())
}

/// Пауза перед повтором: 30 с · 2^(n−1), не больше часа.
pub fn retry_delay(attempt: i32) -> Duration {
    let exp = attempt.saturating_sub(1).clamp(0, 16) as u32;
    Duration::from_secs((30u64 << exp).min(3600))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(retry_delay(1), Duration::from_secs(30));
        assert_eq!(retry_delay(2), Duration::from_secs(60));
        assert_eq!(retry_delay(4), Duration::from_secs(240));
        assert_eq!(retry_delay(10), Duration::from_secs(3600));
        assert_eq!(retry_delay(0), Duration::from_secs(30));
        assert_eq!(retry_delay(i32::MAX), Duration::from_secs(3600));
    }

    #[test]
    fn builder() {
        let j = NewJob::new("ad.sync.full", serde_json::json!({})).idempotency_key("k").max_attempts(0);
        assert_eq!((j.max_attempts, j.idempotency_key.as_deref()), (1, Some("k")));
    }
}
