//! Фоновые задачи: ядро (очистка очереди и сеансов, проверка аудита) и модули
//! (синхронизация AD, проверка связи с доменом) — расписания из `Module::jobs()`.

use crate::api::ApiState;
use pult_core::{ApiError, Module};
use pult_helpdesk::jobs as helpdesk_jobs;
use pult_jobs::{Registry, RunnerConfig, runner::RegistryError};
use pult_people::{PeopleModule, jobs as people_jobs, sync::Kind};
use std::time::Duration;

/// Очистка очереди: выполненные задачи хранятся 7 дней, упавшие — 30 (их разбирают в админке).
pub const PRUNE: &str = "jobs.prune";
const PRUNE_SCHEDULE: &str = "17 3 * * *";
pub const SESSIONS_PRUNE: &str = "sessions.prune";
pub const AUDIT_VERIFY: &str = "audit.verify";

/// «Домен не настроен» и «синхронизация уже идёт» — не сбой задачи.
fn quiet(r: Result<(), ApiError>) -> Result<(), pult_jobs::runner::HandlerError> {
    match r {
        Ok(()) => Ok(()),
        Err(ApiError::BadRequest { code: "ad_not_configured" | "sync_running", .. }) => Ok(()),
        Err(e) => Err(Box::new(e)),
    }
}

/// Выключенный helpdesk не закрывает, не напоминает и не создаёт заявки в фоне: данные ждут включения.
async fn helpdesk_on(core: &pult_core::Core) -> bool {
    pult_core::org::module_enabled(core, pult_core::org::Gated::Helpdesk).await
}

pub fn registry(api: &ApiState) -> Result<Registry, RegistryError> {
    let mut r = Registry::default();
    let p = api.core.db.clone();
    r.handle(PRUNE, move |_| {
        let p = p.clone();
        async move {
            let n = pult_jobs::prune(&p, Duration::from_secs(7 * 86_400), Duration::from_secs(30 * 86_400)).await?;
            tracing::info!(deleted = n, "очередь задач очищена");
            let r = pult_core::retention::prune(&p).await?;
            tracing::info!(deleted = r, "старые письма, уведомления, токены и журнал синхронизации AD удалены");
            Ok(())
        }
    })?
    .schedule(PRUNE, PRUNE_SCHEDULE)?;

    let p = api.core.db.clone();
    r.handle(SESSIONS_PRUNE, move |_| {
        let p = p.clone();
        async move {
            let n = pult_core::session::prune(&p).await?;
            sqlx::query("DELETE FROM login_attempts WHERE ts < now() - interval '30 days'").execute(&p).await?;
            // Неполученные временные пароли каталога AD — не дольше суток.
            pult_directory::ops::prune_secrets(&p).await?;
            tracing::debug!(deleted = n, "истёкшие сеансы удалены");
            Ok(())
        }
    })?
    .schedule(SESSIONS_PRUNE, "7 * * * *")?;

    let p = api.core.db.clone();
    r.handle(AUDIT_VERIFY, move |_| {
        let p = p.clone();
        async move {
            let v = pult_core::api::verify_and_store(&p).await?;
            tracing::info!(total = v.total, broken = ?v.broken_at, "проверка цепочки аудита");
            Ok(())
        }
    })?
    .schedule(AUDIT_VERIFY, "40 2 * * *")?;

    // Коммуникации: отключённые в AD выбывают из групповых чатов (при выключенном модуле — ждёт включения).
    let st = api.clone();
    r.handle(pult_chat::SWEEP_JOB, move |_| {
        let st = st.clone();
        async move {
            if pult_core::org::module_enabled(&st.core, pult_core::org::Gated::Chat).await {
                pult_chat::sweep_blocked(&st.core.db).await?;
            }
            Ok(())
        }
    })?
    .schedule(pult_chat::SWEEP_JOB, "*/5 * * * *")?;

    for (name, kind) in [(people_jobs::SYNC_INCREMENTAL, Kind::Incremental), (people_jobs::SYNC_FULL, Kind::Full)] {
        let st = api.clone();
        r.handle(name, move |_| {
            let st = st.clone();
            async move { quiet(pult_people::sync::run(&st.core, &st.people.ad, kind, false).await.map(|_| ())) }
        })?;
    }
    let st = api.clone();
    r.handle(people_jobs::HEALTH, move |_| {
        let st = st.clone();
        async move { quiet(pult_people::health::check(&st.core, &st.people.ad).await.map(|_| ())) }
    })?;
    let st = api.clone();
    r.handle(people_jobs::PUSH, move |_| {
        let st = st.clone();
        async move { quiet(pult_people::push::deliver(&st.core).await.map(|_| ())) }
    })?;
    let st = api.clone();
    r.handle(people_jobs::COMPUTERS, move |_| {
        let st = st.clone();
        async move { quiet(pult_people::computers::run(&st.core, &st.people.ad).await.map(|_| ())) }
    })?;
    for j in <PeopleModule as Module<ApiState>>::jobs(&PeopleModule) {
        if let Some(cron) = j.schedule {
            r.schedule(j.name, cron)?;
        }
    }

    let st = api.clone();
    r.handle(helpdesk_jobs::AUTO_CLOSE, move |_| {
        let st = st.clone();
        async move {
            if !helpdesk_on(&st.core).await {
                return Ok(());
            }
            let n = pult_helpdesk::tickets::auto_close(&st.core).await?;
            if n > 0 {
                tracing::info!(closed = n, "решённые заявки закрыты автоматически");
            }
            Ok(())
        }
    })?;
    let st = api.clone();
    r.handle(helpdesk_jobs::ORPHANS, move |_| {
        let st = st.clone();
        async move {
            if !helpdesk_on(&st.core).await {
                return Ok(());
            }
            let n = pult_helpdesk::tickets::release_orphans(&st.core).await?;
            if n > 0 {
                tracing::warn!(released = n, "заявки отключённых инженеров возвращены в очередь");
            }
            Ok(())
        }
    })?;
    let st = api.clone();
    r.handle(helpdesk_jobs::FILES_GC, move |_| {
        let st = st.clone();
        async move {
            pult_helpdesk::jobs::files_gc(&st.core, &st.helpdesk).await?;
            Ok(())
        }
    })?;
    let st = api.clone();
    r.handle(helpdesk_jobs::SLA, move |_| {
        let st = st.clone();
        async move {
            if !helpdesk_on(&st.core).await {
                return Ok(());
            }
            let n = pult_helpdesk::sla::check(&st.core).await?;
            if n > 0 {
                tracing::info!(tickets = n, "предупреждения SLA отправлены");
            }
            Ok(())
        }
    })?;
    let st = api.clone();
    r.handle(helpdesk_jobs::WAITING, move |_| {
        let st = st.clone();
        async move {
            if !helpdesk_on(&st.core).await {
                return Ok(());
            }
            let n = pult_helpdesk::service::waiting_reminders(&st.core).await?;
            if n > 0 {
                tracing::info!(tickets = n, "напоминания об ожидании ответа отправлены");
            }
            Ok(())
        }
    })?;
    let st = api.clone();
    r.handle(helpdesk_jobs::RECURRING, move |_| {
        let st = st.clone();
        async move {
            if !helpdesk_on(&st.core).await {
                return Ok(());
            }
            let n = pult_helpdesk::templates::run_due(&st.core).await?;
            if n > 0 {
                tracing::info!(tickets = n, "регламентные заявки созданы");
            }
            Ok(())
        }
    })?;
    let st = api.clone();
    r.handle(helpdesk_jobs::WEEKLY_REPORT, move |_| {
        let st = st.clone();
        async move {
            // Выключенный helpdesk отчётов не шлёт.
            if helpdesk_on(&st.core).await {
                pult_helpdesk::reports::weekly(&st.core).await?;
            }
            Ok(())
        }
    })?;
    let st = api.clone();
    r.handle(helpdesk_jobs::NETBOX, move |_| {
        let st = st.clone();
        async move {
            let on: pult_helpdesk::inventory::NetboxSettings = pult_core::settings::load(&st.core.db).await?;
            if on.enabled && helpdesk_on(&st.core).await {
                let s = pult_helpdesk::inventory::sync(&st.core).await?;
                tracing::info!(fetched = s.fetched, added = s.added, missing = s.missing, "синхронизация NetBox");
            }
            Ok(())
        }
    })?;
    // Операция каталога AD: запись учёткой записи, проверка чтением, итог в заявке.
    // Повторная доставка выполненной операции ничего не делает (`pult_directory::ops::execute`).
    let st = api.clone();
    r.handle(pult_directory::JOB, move |job| {
        let st = st.clone();
        async move {
            let op = job.payload["operation"]
                .as_str()
                .and_then(|s| s.parse::<sqlx::types::Uuid>().ok())
                .ok_or_else(|| ApiError::internal("задача каталога без операции"))?;
            pult_helpdesk::access::run_operation(&st.core, op).await?;
            Ok(())
        }
    })?;
    let st = api.clone();
    r.handle(helpdesk_jobs::WEBHOOK, move |job| {
        let st = st.clone();
        async move {
            pult_helpdesk::integrations::deliver(&st.core, &job.payload).await?;
            Ok(())
        }
    })?;
    for j in pult_helpdesk::jobs::definitions() {
        if let Some(cron) = j.schedule {
            r.schedule(j.name, cron)?;
        }
    }

    let st = api.clone();
    r.handle(BACKUP, move |_| {
        let st = st.clone();
        async move {
            crate::backup::run_and_record(&st.backup, &st.core.db).await?;
            Ok(())
        }
    })?
    .schedule(BACKUP, "30 2 * * *")?;

    let st = api.clone();
    r.handle(MAIL_SEND, move |_| {
        let st = st.clone();
        async move {
            let n = pult_core::mail::send_due(&st.core.db, &st.core.secrets).await?;
            if n > 0 {
                tracing::info!(sent = n, "письма отправлены");
            }
            Ok(())
        }
    })?
    .schedule(MAIL_SEND, "* * * * *")?;
    Ok(r)
}

pub const MAIL_SEND: &str = "mail.send";
pub const BACKUP: &str = "system.backup";

/// Имя этого сервера.
pub fn host() -> String {
    host_name(|k| std::env::var(k).ok(), || std::fs::read_to_string("/etc/hostname").ok())
}

pub fn runner_config(cfg: &crate::config::JobsConfig) -> anyhow::Result<RunnerConfig> {
    Ok(RunnerConfig {
        worker_id: format!("{}:{}", host(), std::process::id()),
        concurrency: cfg.concurrency,
        offset: pult_jobs::cron::parse_offset(&cfg.utc_offset)?,
        ..RunnerConfig::default()
    })
}

/// Имя хоста для `locked_by`. Под systemd переменных `HOSTNAME`/`COMPUTERNAME` нет — берём `/etc/hostname`
/// (при `ProtectSystem=strict` он доступен на чтение, а `/proc/sys` скрыт `ProcSubset=pid`).
fn host_name(env: impl Fn(&str) -> Option<String>, etc_hostname: impl FnOnce() -> Option<String>) -> String {
    ["COMPUTERNAME", "HOSTNAME"]
        .iter()
        .find_map(|k| env(k))
        .or_else(etc_hostname)
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "host".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_name_falls_back_to_etc_hostname() {
        assert_eq!(host_name(|_| None, || Some("pult-srv\n".into())), "pult-srv");
        assert_eq!(host_name(|k| (k == "HOSTNAME").then(|| "web1".into()), || None), "web1");
        assert_eq!(host_name(|_| None, || Some("  \n".into())), "host");
        assert_eq!(host_name(|_| None, || None), "host");
    }

    #[tokio::test]
    async fn core_jobs_are_registered_with_schedules() {
        let pool = sqlx::postgres::PgPoolOptions::new().connect_lazy("postgres://u@localhost/x").unwrap();
        let st = ApiState {
            core: pult_core::Core {
                db: pool,
                secrets: std::sync::Arc::new(pult_core::crypto::SecretBox::new(&pult_core::Secret::new(
                    "k".repeat(32),
                ))),
                cookies: pult_core::session::CookiePolicy { secure: true },
                permissions: std::sync::Arc::new(crate::api::permissions()),
                events: pult_core::events::Hub::default(),
                insecure_mail: false,
                org: Default::default(),
            },
            people: pult_people::PeopleState::default(),
            helpdesk: pult_helpdesk::HelpdeskState { files_dir: "files".into() },
            backup: crate::backup::BackupConfig {
                dir: "backups".into(),
                data_dir: None,
                files_dir: "files".into(),
                database_url: pult_core::Secret::new("postgres://u@localhost/x"),
            },
            setup: crate::setup::SetupState::new().unwrap(),
            tls: None,
        };
        let r = registry(&st).unwrap();
        let mut kinds = r.kinds();
        kinds.sort();
        assert_eq!(
            kinds,
            [
                "ad.computers",
                "ad.health",
                "ad.sync.full",
                "ad.sync.incremental",
                "audit.verify",
                "chat.blocked_sweep",
                "directory.operation",
                "helpdesk.auto_close",
                "helpdesk.files_gc",
                "helpdesk.netbox_sync",
                "helpdesk.orphans",
                "helpdesk.recurring",
                "helpdesk.sla",
                "helpdesk.waiting",
                "helpdesk.webhook",
                "helpdesk.weekly_report",
                "jobs.prune",
                "mail.send",
                "notify.push",
                "sessions.prune",
                "system.backup"
            ]
        );
        let s: Vec<_> = r.schedules().map(|(k, s)| (k.to_string(), s.source().to_string())).collect();
        assert!(s.contains(&(PRUNE.to_string(), PRUNE_SCHEDULE.to_string())));
        assert!(s.contains(&("ad.sync.incremental".to_string(), "*/5 * * * *".to_string())));
        // Вебхук — по событию, без расписания.
        assert_eq!(s.len(), 19, "у каждой периодической задачи — расписание");
    }
}
