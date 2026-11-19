//! Исполнитель очереди и планировщик расписаний.
//!
//! - Обработчики регистрируются по виду задачи (`ad.sync.incremental`); исполнитель берёт из очереди
//!   только известные ему виды — другие копии сервера с другим набором модулей их не потеряют.
//! - Планировщик раз в `tick` ставит задачи по расписанию с ключом идемпотентности
//!   `cron:<вид>:<момент>`: сколько бы копий сервера ни работало, слот ставится один раз.
//!   Пропущенные слоты (сервер стоял) не догоняются пачкой: в окне `misfire_grace` ставится
//!   только последний, более старые пропускаются.
//! - Обработчик ограничен `job_timeout` (меньше аренды), паника и превышение времени — обычная ошибка
//!   с повтором.
//! - Остановка: новых задач не берёт, текущие дожидается `drain_timeout`, остальные вернутся
//!   в очередь по истечении аренды.

use crate::{Job, JobError, NewJob, claim, complete, cron::Schedule, enqueue, fail};
use serde_json::json;
use sqlx::PgPool;
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};
use time::{OffsetDateTime, UtcOffset};
use tokio::{sync::watch, task::JoinSet};

/// Ошибка обработчика; `anyhow::Error` и любые `std::error::Error` приводятся через `?`/`into()`.
pub type HandlerError = Box<dyn std::error::Error + Send + Sync>;
type BoxFuture = Pin<Box<dyn Future<Output = Result<(), HandlerError>> + Send>>;
type Handler = Arc<dyn Fn(Job) -> BoxFuture + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("обработчик «{0}» уже зарегистрирован")]
    Duplicate(String),
    #[error("для расписания «{0}» нет обработчика")]
    NoHandler(String),
    #[error(transparent)]
    Cron(#[from] crate::cron::CronError),
}

/// Обработчики и расписания.
#[derive(Default, Clone)]
pub struct Registry {
    handlers: HashMap<String, Handler>,
    schedules: Vec<(String, Schedule)>,
}

impl Registry {
    pub fn handle<F, Fut>(&mut self, kind: &str, f: F) -> Result<&mut Self, RegistryError>
    where
        F: Fn(Job) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), HandlerError>> + Send + 'static,
    {
        if self.handlers.contains_key(kind) {
            return Err(RegistryError::Duplicate(kind.to_string()));
        }
        self.handlers.insert(kind.to_string(), Arc::new(move |job| Box::pin(f(job))));
        Ok(self)
    }

    /// Расписание для уже зарегистрированного обработчика. Расписание, которое никогда
    /// не сработает (`0 0 30 2 *`), отклоняется сразу.
    pub fn schedule(&mut self, kind: &str, cron: &str) -> Result<&mut Self, RegistryError> {
        if !self.handlers.contains_key(kind) {
            return Err(RegistryError::NoHandler(kind.to_string()));
        }
        let s = Schedule::parse(cron)?;
        if s.next_after(OffsetDateTime::now_utc(), UtcOffset::UTC).is_none() {
            return Err(crate::cron::CronError::Never(cron.to_string()).into());
        }
        self.schedules.push((kind.to_string(), s));
        Ok(self)
    }

    pub fn kinds(&self) -> Vec<String> {
        let mut k: Vec<String> = self.handlers.keys().cloned().collect();
        k.sort();
        k
    }

    pub fn schedules(&self) -> impl Iterator<Item = (&str, &Schedule)> {
        self.schedules.iter().map(|(k, s)| (k.as_str(), s))
    }
}

/// Параметры исполнителя.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Имя обработчика в `locked_by`: хост и PID.
    pub worker_id: String,
    /// Сколько задач выполняется одновременно.
    pub concurrency: usize,
    /// Пауза между опросами, когда очередь пуста.
    pub poll_interval: Duration,
    /// Аренда задачи; обработчик должен уложиться в `job_timeout` < `lease`.
    pub lease: Duration,
    pub job_timeout: Duration,
    /// Период планировщика.
    pub tick: Duration,
    /// Окно, в котором пропущенный слот расписания ещё запускается.
    pub misfire_grace: Duration,
    /// Смещение, в котором читаются расписания.
    pub offset: UtcOffset,
    /// Сколько ждать текущие задачи при остановке.
    pub drain_timeout: Duration,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            worker_id: format!("pult-{}", std::process::id()),
            concurrency: 4,
            poll_interval: Duration::from_secs(2),
            lease: Duration::from_secs(300),
            job_timeout: Duration::from_secs(240),
            tick: Duration::from_secs(30),
            misfire_grace: Duration::from_secs(3600),
            offset: UtcOffset::UTC,
            drain_timeout: Duration::from_secs(20),
        }
    }
}

/// Ключ идемпотентности слота расписания.
pub fn slot_key(kind: &str, slot: OffsetDateTime) -> String {
    format!("cron:{kind}:{}", slot.unix_timestamp())
}

/// Ставит задачи, слоты которых наступили в окне `(now − grace, now]`. Возвращает число новых задач.
pub async fn enqueue_due(
    pool: &PgPool,
    registry: &Registry,
    now: OffsetDateTime,
    grace: Duration,
    offset: UtcOffset,
) -> Result<usize, JobError> {
    let from = now - grace;
    let mut created = 0;
    for (kind, schedule) in registry.schedules() {
        let Some(slot) = schedule.latest_in(from, now, offset) else { continue };
        let job =
            NewJob::new(kind, json!({ "scheduled_for": slot.unix_timestamp() })).idempotency_key(slot_key(kind, slot));
        if enqueue(pool, &job).await?.is_some() {
            tracing::info!(kind, slot = %slot, "задача по расписанию поставлена");
            created += 1;
        }
    }
    Ok(created)
}

/// Запускает исполнитель и планировщик до сигнала остановки (`true` в `shutdown`).
pub async fn run(pool: PgPool, registry: Registry, cfg: RunnerConfig, mut shutdown: watch::Receiver<bool>) {
    let registry = Arc::new(registry);
    let kinds = registry.kinds();
    tracing::info!(worker = %cfg.worker_id, ?kinds, schedules = registry.schedules.len(), "исполнитель задач запущен");
    let mut running: JoinSet<()> = JoinSet::new();
    let mut next_tick = tokio::time::Instant::now();
    let mut db_backoff = Duration::ZERO;

    while !*shutdown.borrow() {
        while running.try_join_next().is_some() {}

        if tokio::time::Instant::now() >= next_tick {
            next_tick = tokio::time::Instant::now() + cfg.tick;
            let now = OffsetDateTime::now_utc();
            match enqueue_due(&pool, &registry, now, cfg.misfire_grace, cfg.offset).await {
                Ok(_) => {
                    db_backoff = Duration::ZERO;
                    report_depth(&pool).await;
                }
                Err(e) => db_backoff = backoff(db_backoff, &e),
            }
        }

        let free = cfg.concurrency.saturating_sub(running.len());
        let mut wait = cfg.poll_interval.min(cfg.tick);
        if free > 0 && !kinds.is_empty() {
            match claim(&pool, &cfg.worker_id, Some(&kinds), free as i64, cfg.lease).await {
                Ok(jobs) => {
                    db_backoff = Duration::ZERO;
                    // Очередь не пуста — сразу за следующими, без паузы.
                    if jobs.len() == free {
                        wait = Duration::from_millis(50);
                    }
                    for job in jobs {
                        if let Some(h) = registry.handlers.get(&job.kind).cloned() {
                            running.spawn(execute(pool.clone(), h, job, cfg.job_timeout));
                        }
                    }
                }
                Err(e) => db_backoff = backoff(db_backoff, &e),
            }
        }
        if !db_backoff.is_zero() {
            wait = db_backoff;
        }
        tokio::select! {
            _ = tokio::time::sleep(wait) => {}
            changed = shutdown.changed() => {
                // Отправитель исчез — сигнала больше не будет, считаем это остановкой.
                if changed.is_err() {
                    break;
                }
            }
            Some(_) = running.join_next(), if !running.is_empty() => {}
        }
    }

    tracing::info!(running = running.len(), "исполнитель задач останавливается");
    let drained = tokio::time::timeout(cfg.drain_timeout, async { while running.join_next().await.is_some() {} }).await;
    if drained.is_err() {
        tracing::warn!(left = running.len(), "не все задачи завершились — вернутся в очередь после аренды");
        running.abort_all();
    }
}

/// Пауза при недоступной БД: 1 с → 60 с.
fn backoff(prev: Duration, e: &JobError) -> Duration {
    let next = if prev.is_zero() { Duration::from_secs(1) } else { (prev * 2).min(Duration::from_secs(60)) };
    tracing::warn!(error = %e, retry_in = ?next, "очередь задач: нет доступа к БД");
    next
}

async fn execute(pool: PgPool, handler: Handler, job: Job, limit: Duration) {
    let started = std::time::Instant::now();
    let kind = job.kind.clone();
    let mut task = tokio::spawn(handler(job.clone()));
    let result = match tokio::time::timeout(limit, &mut task).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(e))) => Err(e.to_string()),
        Ok(Err(join)) if join.is_panic() => Err("обработчик завершился паникой".to_string()),
        Ok(Err(_)) => Err("обработчик отменён".to_string()),
        Err(_) => {
            task.abort();
            Err(format!("превышено время выполнения ({} с)", limit.as_secs()))
        }
    };
    let outcome = match &result {
        Ok(()) => "done",
        Err(_) if job.attempt >= job.max_attempts => "failed",
        Err(_) => "retry",
    };
    let saved = match result {
        Ok(()) => complete(&pool, &job).await,
        Err(e) => fail(&pool, &job, &e).await,
    };
    if let Err(e) = saved {
        tracing::warn!(job = %job.id, %kind, error = %e, "не удалось сохранить итог задачи — она вернётся по аренде");
    }
    let labels = [("kind", kind), ("outcome", outcome.to_string())];
    metrics::counter!("pult_jobs_total", &labels).increment(1);
    metrics::histogram!("pult_job_duration_seconds", &labels).record(started.elapsed().as_secs_f64());
}

/// Глубина очереди для `/metrics`: ждут, выполняются, упали окончательно.
async fn report_depth(pool: &PgPool) {
    let rows: Result<Vec<(String, i64)>, _> =
        sqlx::query_as("SELECT status, count(*) FROM jobs WHERE status <> 'done' GROUP BY status")
            .fetch_all(pool)
            .await;
    let Ok(rows) = rows else { return };
    for status in ["queued", "running", "failed"] {
        let n = rows.iter().find(|(s, _)| s == status).map_or(0, |(_, n)| *n);
        metrics::gauge!("pult_jobs", "status" => status).set(n as f64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_rules() {
        let mut r = Registry::default();
        r.handle("a", |_| async { Ok(()) }).unwrap();
        assert!(matches!(r.handle("a", |_| async { Ok(()) }), Err(RegistryError::Duplicate(_))));
        assert!(matches!(r.schedule("b", "* * * * *"), Err(RegistryError::NoHandler(_))));
        assert!(matches!(r.schedule("a", "0 0 30 2 *"), Err(RegistryError::Cron(_))));
        assert!(matches!(r.schedule("a", "bad"), Err(RegistryError::Cron(_))));
        r.schedule("a", "@daily").unwrap();
        assert_eq!(r.kinds(), vec!["a".to_string()]);
        assert_eq!(r.schedules().count(), 1);
    }

    #[test]
    fn slot_keys_are_stable() {
        let t = OffsetDateTime::from_unix_timestamp(1_793_664_000).unwrap();
        assert_eq!(slot_key("jobs.prune", t), "cron:jobs.prune:1793664000");
    }

    #[tokio::test]
    async fn stops_promptly_when_database_is_down() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(300))
            .connect_lazy("postgres://u:p@127.0.0.1:1/x")
            .unwrap();
        let mut reg = Registry::default();
        reg.handle("a", |_| async { Ok(()) }).unwrap().schedule("a", "* * * * *").unwrap();
        let (stop, rx) = watch::channel(false);
        let task = tokio::spawn(run(pool, reg, RunnerConfig::default(), rx));
        tokio::time::sleep(Duration::from_millis(500)).await;
        stop.send(true).unwrap();
        // Исполнитель в паузе после ошибки БД — сигнал остановки прерывает паузу.
        tokio::time::timeout(Duration::from_secs(3), task).await.expect("остановился").unwrap();
    }

    #[test]
    fn backoff_grows_to_a_minute() {
        let e = JobError::Db(sqlx::Error::PoolTimedOut);
        let mut d = Duration::ZERO;
        let seq: Vec<u64> = (0..8)
            .map(|_| {
                d = backoff(d, &e);
                d.as_secs()
            })
            .collect();
        assert_eq!(seq, vec![1, 2, 4, 8, 16, 32, 60, 60]);
    }
}
