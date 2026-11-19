//! Основной цикл агента: регистрация при необходимости, отчёт, обновление; пауза до следующего
//! отчёта или до события сессии (вход, выход, блокировка).

use crate::{
    AgentReport,
    client::{Portal, PortalError},
    config::{self, Settings, State},
    update,
};
use pult_api_types::AgentUpdate;
use pult_core::Secret;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Notify, watch};

/// Что зависит от платформы: сбор сведений и установка обновления.
pub trait Host: Send + Sync {
    fn collect(&self) -> anyhow::Result<AgentReport>;
    /// Ставит проверенный пакет. На Windows — замена exe и выход: служба перезапустится новой версией.
    fn install_update(&self, data: &[u8]) -> anyhow::Result<()>;
}

/// Пауза после `failures` неудач подряд: 1, 2, 4, 8, 10 минут — портал не заваливают повторами.
pub fn backoff(failures: u32) -> Duration {
    Duration::from_secs((60u64 << failures.saturating_sub(1).min(4)).min(600))
}

/// После события сессии отчёт уходит не сразу: вход порождает несколько событий подряд.
pub const SESSION_EVENT_DELAY: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub enum CycleError {
    #[error("не задан адрес портала (PortalUrl)")]
    NoPortal,
    #[error("агент не зарегистрирован, а токена регистрации нет (EnrollToken)")]
    NoToken,
    #[error(transparent)]
    Portal(#[from] PortalError),
    #[error("{0}")]
    Other(String),
}

pub struct Agent<H: Host> {
    pub host: H,
    pub settings: Settings,
    pub state_path: PathBuf,
    portal: Option<Portal>,
    /// Пакет, который не прошёл проверку: не скачиваем его снова каждую минуту.
    rejected_update: Option<String>,
}

impl<H: Host> Agent<H> {
    pub fn new(host: H, settings: Settings, state_path: PathBuf) -> Result<Self, CycleError> {
        let portal = match settings.portal.as_deref() {
            Some(p) => Some(Portal::new(p)?),
            None => None,
        };
        Ok(Self { host, settings, state_path, portal, rejected_update: None })
    }

    fn portal(&self) -> Result<&Portal, CycleError> {
        self.portal.as_ref().ok_or(CycleError::NoPortal)
    }

    fn portal_url(&self) -> &str {
        self.settings.portal.as_deref().unwrap_or_default()
    }

    async fn enroll(&self, report: &AgentReport) -> Result<State, CycleError> {
        let token = self.settings.enroll_token.as_ref().ok_or(CycleError::NoToken)?;
        let r = self.portal()?.enroll(token, report).await?;
        let st = State { device_id: r.device_id, agent_key: Secret::new(r.agent_key) };
        config::save_state(&self.state_path, self.portal_url(), &st).map_err(|e| CycleError::Other(e.to_string()))?;
        tracing::info!(device = %st.device_id, "агент зарегистрирован на портале");
        Ok(st)
    }

    /// Один отчёт. Возвращает паузу до следующего.
    pub async fn cycle(&mut self) -> Result<Duration, CycleError> {
        let report = self.host.collect().map_err(|e| CycleError::Other(format!("{e:#}")))?;
        let state = match config::load_state(&self.state_path, self.portal_url()) {
            Some(s) => s,
            None => self.enroll(&report).await?,
        };
        let resp = match self.portal()?.report(&state.agent_key, &report).await {
            Ok(r) => r,
            // Ключ отозван (ПК отключили в админке) или ПК переименовали: регистрируемся заново.
            Err(PortalError::Unauthorized | PortalError::Renamed) => {
                tracing::warn!("портал не принял ключ агента — повторная регистрация");
                config::clear_state(&self.state_path);
                let st = self.enroll(&report).await?;
                self.portal()?.report(&st.agent_key, &report).await?
            }
            Err(e) => return Err(e.into()),
        };
        if let Some(u) = resp.update {
            self.update(&state.agent_key, &u).await;
        }
        Ok(Duration::from_secs(u64::from(resp.next_report_secs.clamp(30, 600))))
    }

    async fn update(&mut self, key: &Secret, u: &AgentUpdate) {
        if self.rejected_update.as_deref() == Some(u.package_id.as_str()) {
            return;
        }
        if !update::newer(&u.version, env!("CARGO_PKG_VERSION")) {
            tracing::warn!(version = %u.version, current = env!("CARGO_PKG_VERSION"), "назначенная версия не новее установленной — пропускаю");
            self.rejected_update = Some(u.package_id.clone());
            return;
        }
        // Этот пакет уже ставили, а версия не сменилась (пакет собран не той версией или не запускается):
        // не перезапускаемся по кругу.
        let marker = attempt_path(&self.state_path);
        if last_attempt(&marker).as_deref() == Some(u.package_id.as_str()) {
            tracing::error!(version = %u.version, "пакет обновления уже устанавливался, но версия не сменилась — пропускаю");
            self.rejected_update = Some(u.package_id.clone());
            return;
        }
        let Some(public) = self.settings.update_key.as_deref() else {
            tracing::warn!(version = %u.version, "портал назначил обновление, но ключ обновлений (UpdatePublicKey) не задан — пропускаю");
            self.rejected_update = Some(u.package_id.clone());
            return;
        };
        let key_bytes = match update::parse_public_key(public) {
            Ok(k) => k,
            Err(e) => {
                tracing::error!(error = %e, "ключ обновлений (UpdatePublicKey) не читается — пропускаю");
                self.rejected_update = Some(u.package_id.clone());
                return;
            }
        };
        // Портал занят или сеть моргнула — попробуем в следующем цикле, пакет не отклоняется.
        let data = match self.portal() {
            Ok(p) => p.download_update(key, &u.package_id).await.map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        let data = match data {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(version = %u.version, error = %e, "пакет обновления не скачан — повторю позже");
                return;
            }
        };
        let result = update::verify(&data, u, &key_bytes).map_err(|e| e.to_string()).and_then(|()| {
            tracing::info!(version = %u.version, sha256 = %u.sha256, "обновление проверено, устанавливаю");
            if let Err(e) = std::fs::write(&marker, &u.package_id) {
                tracing::warn!(error = %e, "не удалось запомнить устанавливаемый пакет");
            }
            self.host.install_update(&data).map_err(|e| format!("{e:#}"))
        });
        if let Err(e) = result {
            tracing::error!(version = %u.version, error = %e, "обновление отклонено");
            self.rejected_update = Some(u.package_id.clone());
        }
    }
}

/// Метка «этот пакет уже ставили» — рядом с состоянием агента (папка только для SYSTEM и администраторов).
fn attempt_path(state: &std::path::Path) -> PathBuf {
    state.with_file_name("update-attempt.txt")
}

fn last_attempt(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Цикл до остановки службы.
pub async fn run<H: Host>(mut agent: Agent<H>, wake: Arc<Notify>, mut stop: watch::Receiver<bool>) {
    let mut failures = 0u32;
    loop {
        let pause = match agent.cycle().await {
            Ok(p) => {
                failures = 0;
                p
            }
            Err(e) => {
                failures = failures.saturating_add(1);
                let p = backoff(failures);
                tracing::warn!(error = %e, retry_in_secs = p.as_secs(), "отчёт не отправлен");
                p
            }
        };
        tokio::select! {
            _ = tokio::time::sleep(pause) => {}
            _ = wake.notified() => tokio::time::sleep(SESSION_EVENT_DELAY).await,
            _ = stop.changed() => {}
        }
        if *stop.borrow() {
            break;
        }
    }
    tracing::info!("агент остановлен");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let secs: Vec<u64> = (1..=7).map(|f| backoff(f).as_secs()).collect();
        assert_eq!(secs, vec![60, 120, 240, 480, 600, 600, 600]);
        assert_eq!(backoff(0).as_secs(), 60);
    }

    struct Fake;
    impl Host for Fake {
        fn collect(&self) -> anyhow::Result<AgentReport> {
            anyhow::bail!("не нужно")
        }
        fn install_update(&self, _: &[u8]) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn update_attempt_marker() {
        let dir = std::env::temp_dir().join(format!("pult-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = attempt_path(&dir.join("state.json"));
        assert_eq!(marker, dir.join("update-attempt.txt"));
        assert_eq!(last_attempt(&marker), None);
        std::fs::write(&marker, "0193-abc\n").unwrap();
        assert_eq!(last_attempt(&marker).as_deref(), Some("0193-abc"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plain_http_portal_is_refused() {
        let s = Settings { portal: Some("http://pult.example.local/".into()), ..Default::default() };
        assert!(matches!(Agent::new(Fake, s, PathBuf::from("x")), Err(CycleError::Portal(PortalError::NotHttps))));
    }

    #[tokio::test]
    async fn no_portal_configured() {
        let mut a = Agent::new(Fake, Settings::default(), PathBuf::from("x")).unwrap();
        assert!(matches!(a.cycle().await, Err(CycleError::Other(_))), "сбор сведений идёт первым");
        assert!(matches!(a.portal(), Err(CycleError::NoPortal)));
    }
}
