//! Миграции одной транзакцией: обновление либо применяется целиком, либо не меняет базу.
//!
//! Обычный мигратор sqlx применяет каждую миграцию в своей транзакции: если третья из пяти
//! сломана, первые две остаются, и база — ни старая, ни новая (прежний бинарник с ней не стартует).
//! Здесь все недостающие миграции идут в одной транзакции PostgreSQL (DDL в ней транзакционный),
//! поэтому сломанная миграция откатывает всё обновление. Таблица `_sqlx_migrations` — та же,
//! что у sqlx: тесты и `sqlx migrate` видят то же состояние.

use sqlx::{
    PgPool,
    migrate::{MigrationType, Migrator},
};
use std::time::Instant;

/// Ключ advisory-блокировки: две копии сервера не мигрируют одновременно.
const LOCK_KEY: i64 = 0x7075_6c74_6d69_6772; // "pultmigr"

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("миграция {version} ({description}) не применилась, обновление откачено: {source}")]
    Failed { version: i64, description: String, source: sqlx::Error },
    #[error(
        "в базе есть миграция {0}, которой нет в этой версии Pult: база от более новой версии. Установите её или восстановите копию"
    )]
    Newer(i64),
    #[error("миграция {0} в базе отличается от этой версии Pult (изменён уже применённый файл)")]
    Changed(i64),
    #[error("миграция {0} в базе отмечена как неудачная — восстановите базу из резервной копии")]
    Dirty(i64),
    #[error("миграция {0} требует выполнения вне транзакции — так обновлять нельзя")]
    NoTx(i64),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Применить недостающие миграции одной транзакцией. Возвращает число применённых.
pub async fn run(pool: &PgPool, migrator: &Migrator) -> Result<usize, MigrateError> {
    let ups: Vec<_> = migrator.iter().filter(|m| !matches!(m.migration_type, MigrationType::ReversibleDown)).collect();
    if let Some(m) = ups.iter().find(|m| m.no_tx) {
        return Err(MigrateError::NoTx(m.version));
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)").bind(LOCK_KEY).execute(&mut *tx).await?;
    sqlx::raw_sql(
        "CREATE TABLE IF NOT EXISTS _sqlx_migrations (
            version BIGINT PRIMARY KEY,
            description TEXT NOT NULL,
            installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),
            success BOOLEAN NOT NULL,
            checksum BYTEA NOT NULL,
            execution_time BIGINT NOT NULL
        )",
    )
    .execute(&mut *tx)
    .await?;
    let applied: Vec<(i64, bool, Vec<u8>)> =
        sqlx::query_as("SELECT version, success, checksum FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut *tx)
            .await?;
    for (version, success, checksum) in &applied {
        if !success {
            return Err(MigrateError::Dirty(*version));
        }
        match ups.iter().find(|m| m.version == *version) {
            None => return Err(MigrateError::Newer(*version)),
            Some(m) if m.checksum.as_ref() != checksum.as_slice() => return Err(MigrateError::Changed(*version)),
            Some(_) => {}
        }
    }
    let mut n = 0;
    for m in ups.iter().filter(|m| !applied.iter().any(|(v, _, _)| *v == m.version)) {
        let started = Instant::now();
        sqlx::raw_sql(&m.sql).execute(&mut *tx).await.map_err(|source| MigrateError::Failed {
            version: m.version,
            description: m.description.to_string(),
            source,
        })?;
        let took = i64::try_from(started.elapsed().as_nanos()).unwrap_or(i64::MAX);
        sqlx::query(
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES ($1, $2, true, $3, $4)",
        )
        .bind(m.version)
        .bind(m.description.as_ref())
        .bind(m.checksum.as_ref())
        .bind(took)
        .execute(&mut *tx)
        .await?;
        n += 1;
    }
    tx.commit().await?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::{Connection, PgConnection};
    use std::path::{Path, PathBuf};

    /// Отдельная база на тест; без `PULT_TEST_DATABASE_URL` тест пропускается (в CI он задан).
    async fn db(tag: &str) -> Option<PgPool> {
        let Ok(url) = std::env::var("PULT_TEST_DATABASE_URL") else {
            assert!(std::env::var("CI").is_err(), "в CI нужен PULT_TEST_DATABASE_URL");
            return None;
        };
        let name = format!("pult_migrate_{tag}_{}", std::process::id());
        let mut c = PgConnection::connect(&url).await.expect("подключение");
        sqlx::query(&format!("DROP DATABASE IF EXISTS {name}")).execute(&mut c).await.expect("удаление");
        sqlx::query(&format!("CREATE DATABASE {name}")).execute(&mut c).await.expect("создание базы");
        let (base, _) = url.rsplit_once('/').expect("адрес базы");
        Some(PgPool::connect(&format!("{base}/{name}")).await.expect("подключение к тестовой базе"))
    }

    fn dir(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pult-migrations-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("каталог");
        for (name, sql) in files {
            std::fs::write(d.join(name), sql).expect("файл миграции");
        }
        d
    }

    async fn migrator(d: &Path) -> Migrator {
        Migrator::new(d.to_path_buf()).await.expect("миграции")
    }

    async fn tables(pool: &PgPool) -> Vec<String> {
        sqlx::query_scalar("SELECT tablename::text FROM pg_tables WHERE schemaname = 'public' ORDER BY 1")
            .fetch_all(pool)
            .await
            .expect("список таблиц")
    }

    const ONE: (&str, &str) = ("1_one.sql", "CREATE TABLE one (id int); INSERT INTO one VALUES (1);");
    const TWO: (&str, &str) = ("2_two.sql", "CREATE TABLE two (id int);");
    const BROKEN: (&str, &str) = ("3_three.sql", "CREATE TABLE three (id int); SELECT * FROM missing_table;");
    const FIXED: (&str, &str) = ("3_three.sql", "CREATE TABLE three (id int);");

    #[tokio::test]
    async fn broken_migration_rolls_back_the_whole_update() {
        let Some(pool) = db("broken").await else { return };
        // Установка версии 1: одна миграция.
        assert_eq!(run(&pool, &migrator(&dir("v1", &[ONE])).await).await.expect("v1"), 1);
        // Обновление на версию 2: вторая хорошая, третья сломана — не остаётся ничего из обновления.
        let err = run(&pool, &migrator(&dir("v2", &[ONE, TWO, BROKEN])).await).await.expect_err("должна упасть");
        assert!(matches!(err, MigrateError::Failed { version: 3, .. }), "{err}");
        assert_eq!(tables(&pool).await, ["_sqlx_migrations", "one"], "база как до обновления");
        let versions: Vec<i64> =
            sqlx::query_scalar("SELECT version FROM _sqlx_migrations").fetch_all(&pool).await.unwrap();
        assert_eq!(versions, [1]);
        // Исправленная версия применяется целиком; повтор ничего не делает.
        let fixed = migrator(&dir("v2fixed", &[ONE, TWO, FIXED])).await;
        assert_eq!(run(&pool, &fixed).await.expect("v2"), 2);
        assert_eq!(run(&pool, &fixed).await.expect("повтор"), 0);
        assert_eq!(tables(&pool).await, ["_sqlx_migrations", "one", "three", "two"]);
        // Прежняя версия на новой базе не стартует: откат — только с копией базы.
        let old = run(&pool, &migrator(&dir("v1again", &[ONE])).await).await.expect_err("база новее");
        assert!(matches!(old, MigrateError::Newer(2)), "{old}");
        // Изменённый применённый файл — ошибка, а не тихое расхождение.
        let changed = dir("changed", &[("1_one.sql", "CREATE TABLE one (id bigint);"), TWO, FIXED]);
        assert!(matches!(run(&pool, &migrator(&changed).await).await, Err(MigrateError::Changed(1))));
    }

    #[tokio::test]
    async fn compatible_with_sqlx_migrator() {
        let Some(pool) = db("compat").await else { return };
        let m = migrator(&dir("compat", &[ONE, TWO])).await;
        // База, которую мигрировал sqlx (старые установки и тесты), принимается без повторов.
        m.run(&pool).await.expect("sqlx");
        assert_eq!(run(&pool, &m).await.expect("наш"), 0);
        // И наоборот: sqlx видит наши записи как применённые.
        let m3 = migrator(&dir("compat3", &[ONE, TWO, FIXED])).await;
        assert_eq!(run(&pool, &m3).await.expect("наш"), 1);
        m3.run(&pool).await.expect("sqlx после нашего");
    }

    #[test]
    fn project_migrations_run_in_a_transaction() {
        // Сломанное обновление откатывается, только если все миграции — в транзакции.
        let m = sqlx::migrate!("../../migrations");
        assert!(m.iter().all(|m| !m.no_tx), "миграция с -- no-transaction ломает откат обновления");
    }
}
