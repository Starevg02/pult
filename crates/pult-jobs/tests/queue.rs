//! Интеграционные тесты очереди на настоящей PostgreSQL.
//! Нужен `PULT_TEST_DATABASE_URL` (в CI задан). Без него тесты пропускаются.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use pult_jobs::{NewJob, Registry, RunnerConfig, claim, complete, enqueue, fail, runner::enqueue_due};
use serde_json::json;
use sqlx::PgPool;
use std::{collections::HashSet, time::Duration};

async fn db() -> Option<PgPool> {
    let Ok(url) = std::env::var("PULT_TEST_DATABASE_URL") else {
        eprintln!("PULT_TEST_DATABASE_URL не задан — интеграционный тест пропущен");
        return None;
    };
    let pool = PgPool::connect(&url).await.expect("подключение к тестовой БД");
    sqlx::migrate!("../../migrations").run(&pool).await.expect("миграции");
    Some(pool)
}

fn kind(test: &str) -> String {
    format!("test.{test}.{}", uuid::Uuid::now_v7())
}

/// Захват только задач этого теста (тесты идут параллельно в одной БД).
async fn claim_kind(pool: &PgPool, k: &str, limit: i64) -> Vec<pult_jobs::Job> {
    claim(pool, "w", Some(&[k.to_string()]), limit, Duration::from_secs(30)).await.unwrap()
}

#[tokio::test]
async fn idempotency_key_prevents_duplicates() {
    let Some(pool) = db().await else { return };
    let key = format!("op-{}", uuid::Uuid::now_v7());
    let job = NewJob::new(kind("idem"), json!({"n": 1})).idempotency_key(&key);
    assert!(enqueue(&pool, &job).await.unwrap().is_some());
    assert!(enqueue(&pool, &job).await.unwrap().is_none(), "дубль не должен создаться");
}

#[tokio::test]
async fn enqueue_inside_rolled_back_transaction_leaves_nothing() {
    let Some(pool) = db().await else { return };
    let k = kind("tx");
    let mut tx = pool.begin().await.unwrap();
    enqueue(&mut *tx, &NewJob::new(&k, json!({}))).await.unwrap();
    tx.rollback().await.unwrap();
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = $1").bind(&k).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn concurrent_workers_never_take_the_same_job() {
    let Some(pool) = db().await else { return };
    let k = kind("concurrent");
    for i in 0..200 {
        enqueue(&pool, &NewJob::new(&k, json!({ "i": i }))).await.unwrap();
    }
    let mut handles = Vec::new();
    for w in 0..6 {
        let pool = pool.clone();
        let k = k.clone();
        handles.push(tokio::spawn(async move {
            let mut mine = Vec::new();
            loop {
                let kinds = [k.clone()];
                let batch = claim(&pool, &format!("w{w}"), Some(&kinds), 7, Duration::from_secs(30)).await.unwrap();
                if batch.is_empty() {
                    break;
                }
                for j in batch {
                    assert!(complete(&pool, &j).await.unwrap());
                    mine.push(j.id);
                }
            }
            mine
        }));
    }
    let mut all = Vec::new();
    for h in handles {
        all.extend(h.await.unwrap());
    }
    let unique: HashSet<_> = all.iter().collect();
    assert_eq!(all.len(), 200, "каждая задача выполнена");
    assert_eq!(unique.len(), 200, "ни одна задача не выполнена дважды");
}

#[tokio::test]
async fn failures_retry_then_give_up() {
    let Some(pool) = db().await else { return };
    let k = kind("retry");
    let id = enqueue(&pool, &NewJob::new(&k, json!({})).max_attempts(2)).await.unwrap().unwrap();
    let j = claim_kind(&pool, &k, 1).await.pop().unwrap();
    assert_eq!(j.attempt, 1);
    assert!(fail(&pool, &j, "сбой 1").await.unwrap());
    // Пауза перед повтором — задача пока не готова.
    assert!(claim_kind(&pool, &k, 1).await.is_empty());
    sqlx::query("UPDATE jobs SET run_at = now() WHERE id = $1").bind(id).execute(&pool).await.unwrap();
    let j = claim_kind(&pool, &k, 1).await.pop().unwrap();
    assert_eq!(j.attempt, 2);
    assert!(fail(&pool, &j, "сбой 2").await.unwrap());
    let (status, err): (String, String) =
        sqlx::query_as("SELECT status, last_error FROM jobs WHERE id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!((status.as_str(), err.as_str()), ("failed", "сбой 2"));
}

#[tokio::test]
async fn expired_lease_is_reclaimed() {
    let Some(pool) = db().await else { return };
    let k = kind("lease");
    let id = enqueue(&pool, &NewJob::new(&k, json!({}))).await.unwrap().unwrap();
    let j = claim_kind(&pool, &k, 1).await.pop().unwrap();
    assert_eq!(j.id, id);
    // Обработчик «упал»: аренда истекла.
    sqlx::query("UPDATE jobs SET locked_until = now() - interval '1 second' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let again = claim_kind(&pool, &k, 1).await.pop().unwrap();
    assert_eq!((again.id, again.attempt), (id, 2));
}

#[tokio::test]
async fn job_that_kills_its_worker_stops_after_max_attempts() {
    let Some(pool) = db().await else { return };
    let k = kind("crash");
    let id = enqueue(&pool, &NewJob::new(&k, json!({})).max_attempts(2)).await.unwrap().unwrap();
    for attempt in 1..=2 {
        let j = claim_kind(&pool, &k, 1).await.pop().unwrap();
        assert_eq!(j.attempt, attempt);
        // Процесс обработчика погиб на этой задаче: итога нет, аренда истекает.
        sqlx::query("UPDATE jobs SET locked_until = now() - interval '1 second' WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
    assert!(claim_kind(&pool, &k, 1).await.is_empty(), "попытки кончились — больше не берём");
    let status: String =
        sqlx::query_scalar("SELECT status FROM jobs WHERE id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(status, "failed");
}

#[tokio::test]
async fn stale_worker_cannot_overwrite_reclaimed_job() {
    let Some(pool) = db().await else { return };
    let k = kind("stale");
    let id = enqueue(&pool, &NewJob::new(&k, json!({}))).await.unwrap().unwrap();
    let first = claim_kind(&pool, &k, 1).await.pop().unwrap();
    sqlx::query("UPDATE jobs SET locked_until = now() - interval '1 second' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let second = claim_kind(&pool, &k, 1).await.pop().unwrap();
    // Первый обработчик «проснулся» после истечения аренды — его итог не принимается.
    assert!(!complete(&pool, &first).await.unwrap());
    assert!(!fail(&pool, &first, "поздно").await.unwrap());
    assert!(complete(&pool, &second).await.unwrap());
}

#[tokio::test]
async fn schedule_slot_is_enqueued_once_by_many_copies() {
    let Some(pool) = db().await else { return };
    let k = kind("cron");
    let mut reg = Registry::default();
    reg.handle(&k, |_| async { Ok(()) }).unwrap().schedule(&k, "* * * * *").unwrap();
    let now = time::OffsetDateTime::now_utc();
    let grace = Duration::from_secs(3600);
    // Три копии сервера в один момент.
    let mut created = 0;
    for _ in 0..3 {
        created += enqueue_due(&pool, &reg, now, grace, time::UtcOffset::UTC).await.unwrap();
    }
    assert_eq!(created, 1, "слот ставится один раз");
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = $1").bind(&k).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1, "пропущенные слоты окна не догоняются пачкой");
}

#[tokio::test]
async fn runner_executes_retries_panics_and_timeouts() {
    let Some(pool) = db().await else { return };
    let (ok, err, panics, slow) = (kind("ok"), kind("err"), kind("panic"), kind("slow"));
    let mut reg = Registry::default();
    reg.handle(&ok, |_| async { Ok(()) }).unwrap();
    reg.handle(&err, |_| async { Err("не вышло".into()) }).unwrap();
    reg.handle(&panics, |_| async { panic!("сбой обработчика") }).unwrap();
    reg.handle(&slow, |_| async {
        tokio::time::sleep(Duration::from_secs(30)).await;
        Ok(())
    })
    .unwrap();
    let mut ids = Vec::new();
    for k in [&ok, &err, &panics, &slow] {
        ids.push(enqueue(&pool, &NewJob::new(k, json!({})).max_attempts(1)).await.unwrap().unwrap());
    }
    let cfg = RunnerConfig {
        poll_interval: Duration::from_millis(100),
        job_timeout: Duration::from_millis(500),
        tick: Duration::from_secs(3600),
        ..RunnerConfig::default()
    };
    let (stop, rx) = tokio::sync::watch::channel(false);
    let runner = tokio::spawn(pult_jobs::runner::run(pool.clone(), reg, cfg, rx));

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let statuses = loop {
        let rows: Vec<(uuid::Uuid, String, Option<String>)> =
            sqlx::query_as("SELECT id, status, last_error FROM jobs WHERE id = ANY($1)")
                .bind(&ids)
                .fetch_all(&pool)
                .await
                .unwrap();
        if rows.iter().all(|(_, s, _)| s == "done" || s == "failed") || tokio::time::Instant::now() > deadline {
            break rows;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    stop.send(true).unwrap();
    runner.await.unwrap();

    let get = |id: uuid::Uuid| statuses.iter().find(|(i, _, _)| *i == id).cloned().unwrap();
    assert_eq!(get(ids[0]).1, "done");
    assert_eq!((get(ids[1]).1.as_str(), get(ids[1]).2.as_deref()), ("failed", Some("не вышло")));
    assert_eq!(get(ids[2]).2.as_deref(), Some("обработчик завершился паникой"));
    assert!(get(ids[3]).2.unwrap_or_default().starts_with("превышено время"));
}
