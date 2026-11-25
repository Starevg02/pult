//! Фоновые задачи Helpdesk.

use crate::{HelpdeskState, files};
use pult_core::{ApiResult, Core, JobDefinition};
use uuid::Uuid;

pub const AUTO_CLOSE: &str = "helpdesk.auto_close";
pub const ORPHANS: &str = "helpdesk.orphans";
pub const FILES_GC: &str = "helpdesk.files_gc";
pub const SLA: &str = "helpdesk.sla";
pub const WAITING: &str = "helpdesk.waiting";
pub const RECURRING: &str = "helpdesk.recurring";
pub const WEEKLY_REPORT: &str = "helpdesk.weekly_report";
pub use crate::integrations::WEBHOOK_JOB as WEBHOOK;
pub use crate::inventory::SYNC_JOB as NETBOX;

pub fn definitions() -> Vec<JobDefinition> {
    vec![
        JobDefinition { name: AUTO_CLOSE, schedule: Some("*/10 * * * *") },
        JobDefinition { name: ORPHANS, schedule: Some("* * * * *") },
        JobDefinition { name: FILES_GC, schedule: Some("23 4 * * *") },
        JobDefinition { name: SLA, schedule: Some("* * * * *") },
        JobDefinition { name: WAITING, schedule: Some("17 * * * *") },
        JobDefinition { name: RECURRING, schedule: Some("* * * * *") },
        JobDefinition { name: WEEKLY_REPORT, schedule: Some("0 8 * * 1") },
        JobDefinition { name: WEBHOOK, schedule: None },
        JobDefinition { name: NETBOX, schedule: Some("41 3 * * *") },
    ]
}

/// Файлы, загруженные, но так и не прикреплённые за сутки.
pub async fn files_gc(core: &Core, st: &HelpdeskState) -> ApiResult<usize> {
    let gone: Vec<(Uuid, String)> = sqlx::query_as(
        "DELETE FROM files f WHERE f.created_at < now() - interval '1 day'
            AND NOT EXISTS (SELECT 1 FROM ticket_attachments a WHERE a.file_id = f.id)
         RETURNING f.id, f.sha256",
    )
    .fetch_all(&core.db)
    .await?;
    for (_, sha) in &gone {
        let used: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM files WHERE sha256 = $1)")
            .bind(sha)
            .fetch_one(&core.db)
            .await?;
        if !used {
            let _ = tokio::fs::remove_file(files::path_of(st, sha)).await;
        }
    }
    Ok(gone.len())
}
