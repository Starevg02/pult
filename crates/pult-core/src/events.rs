//! События для браузера (SSE): «заявка изменилась», «новое уведомление».
//!
//! Публикуются через `pg_notify` внутри транзакции изменения — уходят только после её фиксации и
//! доходят до всех копий сервера. В событии нет содержимого: браузер перечитывает данные через API,
//! где действуют обычные проверки прав. Получателей фильтрует сервер.

use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool, postgres::PgListener};
use std::{sync::Arc, time::Duration};
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

pub const CHANNEL: &str = "pult_events";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// `ticket` или `notification`.
    pub kind: String,
    #[serde(default)]
    pub ticket: Option<Uuid>,
    /// Кому адресовано лично.
    #[serde(default)]
    pub users: Vec<Uuid>,
    /// Всем, у кого есть это разрешение (инженеры); `*` — всем вошедшим.
    #[serde(default)]
    pub permission: Option<String>,
    /// Чат, к которому относится событие (`chat`, `chat_read`, `typing`).
    #[serde(default)]
    pub chat: Option<Uuid>,
    /// Кто сделал (`typing` — кто печатает).
    #[serde(default)]
    pub from: Option<Uuid>,
}

impl Event {
    /// Должен ли это получить пользователь.
    pub fn visible_to(&self, user: Uuid, grants: &crate::rbac::Grants) -> bool {
        self.users.contains(&user)
            || self
                .permission
                .as_deref()
                .is_some_and(|p| p == "*" || grants.is_superadmin() || grants.permissions.contains(p))
    }
}

pub async fn publish(db: impl PgExecutor<'_>, e: &Event) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(CHANNEL)
        .bind(serde_json::to_string(e).unwrap_or_default())
        .execute(db)
        .await?;
    Ok(())
}

/// Раздаёт события подключённым браузерам этой копии сервера.
#[derive(Clone)]
pub struct Hub {
    tx: broadcast::Sender<Arc<Event>>,
    stop_tx: Arc<watch::Sender<bool>>,
}

impl Default for Hub {
    fn default() -> Self {
        Self { tx: broadcast::channel(1024).0, stop_tx: Arc::new(watch::channel(false).0) }
    }
}

impl Hub {
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.tx.subscribe()
    }

    pub fn stopped(&self) -> watch::Receiver<bool> {
        self.stop_tx.subscribe()
    }

    /// При остановке сервера: потоки завершаются, иначе они держали бы его до тайм-аута.
    pub fn stop(&self) {
        self.stop_tx.send_replace(true);
    }

    pub fn send(&self, e: Event) {
        let _ = self.tx.send(Arc::new(e));
    }

    /// Слушает канал PostgreSQL; при обрыве переподключается.
    pub fn spawn_listener(&self, db: PgPool) {
        let hub = self.clone();
        tokio::spawn(async move {
            let mut stopped = hub.stopped();
            loop {
                let run = async {
                    let mut l = PgListener::connect_with(&db).await?;
                    l.listen(CHANNEL).await?;
                    tracing::info!("подписка на события PostgreSQL установлена");
                    loop {
                        let n = l.recv().await?;
                        match serde_json::from_str::<Event>(n.payload()) {
                            Ok(e) => hub.send(e),
                            Err(e) => tracing::warn!(error = %e, "непонятное событие"),
                        }
                    }
                    #[allow(unreachable_code)]
                    Ok::<(), sqlx::Error>(())
                };
                tokio::select! {
                    r = run => {
                        if let Err(e) = r {
                            tracing::warn!(error = %e, "подписка на события оборвалась — повтор через 5 с");
                        }
                    }
                    _ = stopped.wait_for(|s| *s) => return,
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rbac::Grants;

    #[test]
    fn personal_and_permission_filters() {
        let (me, other) = (Uuid::now_v7(), Uuid::now_v7());
        let personal = Event { kind: "ticket".into(), users: vec![me], ..Default::default() };
        let engineers =
            Event { kind: "ticket".into(), permission: Some("helpdesk.ticket.work".into()), ..Default::default() };
        let plain = Grants::default();
        let mut engineer = Grants::default();
        engineer.permissions.insert("helpdesk.ticket.work".into());
        let mut admin = Grants::default();
        admin.roles.insert(crate::rbac::ROLE_SUPERADMIN.into());
        assert!(personal.visible_to(me, &plain));
        assert!(!personal.visible_to(other, &plain));
        assert!(!engineers.visible_to(me, &plain));
        assert!(engineers.visible_to(me, &engineer));
        assert!(engineers.visible_to(me, &admin));
    }
}
