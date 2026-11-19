//! Настройки агента и его состояние.
//!
//! Настройки — в реестре: `HKLM\SOFTWARE\Policies\Pult\Agent` (GPO, главнее) и `HKLM\SOFTWARE\Pult\Agent`
//! (свойства MSI): `PortalUrl`, `EnrollToken`, `UpdatePublicKey`. Состояние (номер устройства и ключ
//! агента) — в `%ProgramData%\Pult\Agent\state.json`; папка доступна только SYSTEM и администраторам.

use pult_core::Secret;
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};

/// Настройки установки.
#[derive(Debug, Default)]
pub struct Settings {
    pub portal: Option<String>,
    pub enroll_token: Option<Secret>,
    /// Открытый ключ обновлений, base64.
    pub update_key: Option<String>,
}

impl Settings {
    /// Первое непустое значение: GPO, затем MSI.
    pub fn merge(policy: Self, local: Self) -> Self {
        let pick = |a: Option<String>, b: Option<String>| {
            a.filter(|v| !v.trim().is_empty()).or(b.filter(|v| !v.trim().is_empty()))
        };
        Self {
            portal: pick(policy.portal, local.portal),
            enroll_token: match (policy.enroll_token, local.enroll_token) {
                (Some(t), _) if !t.expose().trim().is_empty() => Some(t),
                (_, Some(t)) if !t.expose().trim().is_empty() => Some(t),
                _ => None,
            },
            update_key: pick(policy.update_key, local.update_key),
        }
    }
}

/// Регистрация на портале.
pub struct State {
    pub device_id: String,
    pub agent_key: Secret,
}

#[derive(Serialize, Deserialize)]
struct StateFile {
    device_id: String,
    agent_key: String,
    /// Портал, на котором зарегистрирован агент: сменили адрес — регистрируемся заново.
    portal: String,
}

/// Папка агента в `%ProgramData%`.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("Pult").join("Agent")
}

pub fn state_path(dir: &Path) -> PathBuf {
    dir.join("state.json")
}

/// Состояние для этого портала; чужое или испорченное — как будто его нет.
pub fn load_state(path: &Path, portal: &str) -> Option<State> {
    let f: StateFile = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (f.portal == portal && !f.agent_key.is_empty())
        .then(|| State { device_id: f.device_id, agent_key: Secret::new(f.agent_key) })
}

/// Записывает состояние через временный файл: оборванная запись не портит ключ.
pub fn save_state(path: &Path, portal: &str, s: &State) -> io::Result<()> {
    let body = serde_json::to_vec(&StateFile {
        device_id: s.device_id.clone(),
        agent_key: s.agent_key.expose().to_string(),
        portal: portal.to_string(),
    })?;
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

pub fn clear_state(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(p: &str, t: &str, k: &str) -> Settings {
        let o = |v: &str| (!v.is_empty()).then(|| v.to_string());
        Settings { portal: o(p), enroll_token: o(t).map(Secret::new), update_key: o(k) }
    }

    #[test]
    fn policy_wins_over_msi() {
        let m = Settings::merge(s("https://gpo/", "", ""), s("https://msi/", "pae_msi", "KEY"));
        assert_eq!(m.portal.as_deref(), Some("https://gpo/"));
        assert_eq!(m.enroll_token.as_ref().map(|t| t.expose().to_string()).as_deref(), Some("pae_msi"));
        assert_eq!(m.update_key.as_deref(), Some("KEY"));
        let m = Settings::merge(s("  ", "", ""), s("", "", ""));
        assert!(m.portal.is_none() && m.enroll_token.is_none());
    }

    #[test]
    fn state_roundtrip() {
        let dir = std::env::temp_dir().join(format!("pult-agent-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = state_path(&dir);
        assert!(load_state(&p, "https://a/").is_none());
        save_state(&p, "https://a/", &State { device_id: "d1".into(), agent_key: Secret::new("pak_x") }).unwrap();
        let st = load_state(&p, "https://a/").unwrap();
        assert_eq!((st.device_id.as_str(), st.agent_key.expose()), ("d1", "pak_x"));
        assert!(load_state(&p, "https://b/").is_none(), "другой портал — новая регистрация");
        std::fs::write(&p, b"{broken").unwrap();
        assert!(load_state(&p, "https://a/").is_none());
        clear_state(&p);
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
