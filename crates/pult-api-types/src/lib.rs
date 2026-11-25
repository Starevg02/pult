//! Общие типы данных между крейтами и клиентами API.
//!
//! Протокол агента и helper с порталом: регистрация, отчёт, обновление, обмен токена подключения.
//! Поля, добавленные после первой версии агента, необязательны: старый агент продолжает работать.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Состояние сессии (`WTS_CONNECTSTATE_CLASS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Active,
    Connected,
    ConnectQuery,
    Shadow,
    Disconnected,
    Idle,
    Listen,
    Reset,
    Down,
    Init,
    Unknown,
}

impl SessionState {
    /// Числовое значение `WTS_CONNECTSTATE_CLASS` → состояние.
    pub fn from_wts(v: i32) -> Self {
        match v {
            0 => Self::Active,
            1 => Self::Connected,
            2 => Self::ConnectQuery,
            3 => Self::Shadow,
            4 => Self::Disconnected,
            5 => Self::Idle,
            6 => Self::Listen,
            7 => Self::Reset,
            8 => Self::Down,
            9 => Self::Init,
            _ => Self::Unknown,
        }
    }
}

/// Сессия на ПК — будущая строка `device_sessions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Session {
    pub id: u32,
    /// `Console`, `RDP-Tcp#3`, `Services`…
    pub station: String,
    pub state: SessionState,
    pub user: Option<String>,
    pub domain: Option<String>,
    /// Имя клиента для RDP-сессий.
    pub client_name: Option<String>,
    /// Время входа, секунды Unix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logon_at: Option<i64>,
}

impl Session {
    /// Сессия, к которой имеет смысл подключаться инженеру: есть пользователь, не служебная.
    pub fn is_user_session(&self) -> bool {
        self.user.as_deref().is_some_and(|u| !u.is_empty())
            && self.id != 0
            && matches!(self.state, SessionState::Active | SessionState::Disconnected | SessionState::Idle)
    }
}

/// Отчёт агента порталу.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AgentReport {
    pub hostname: String,
    pub domain: Option<String>,
    pub os: String,
    pub agent_version: String,
    pub sessions: Vec<Session>,
    /// Полное DNS-имя ПК.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fqdn: Option<String>,
    /// Версия Windows, например `Windows 11 Pro 23H2 (22631)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    /// IP-адреса ПК, как их видит агент.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ip_addrs: Vec<String>,
    /// Оборудование и программы: при запуске агента и когда портал попросит (`want_inventory`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory: Option<HardwareInventory>,
}

/// Оборудование и установленные программы ПК.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HardwareInventory {
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    /// Серийный номер из SMBIOS.
    pub serial_number: Option<String>,
    pub bios_version: Option<String>,
    pub cpu: Option<String>,
    /// Оперативная память, МБ.
    pub memory_mb: Option<u64>,
    #[serde(default)]
    pub disks: Vec<DeviceDisk>,
    /// Программы из «Программы и компоненты» (без системных компонентов и обновлений).
    #[serde(default)]
    pub software: Vec<InstalledSoftware>,
}

/// Локальный диск.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeviceDisk {
    /// Буква диска, например `C:`.
    pub name: String,
    pub size_gb: u64,
    pub free_gb: u64,
}

/// Установленная программа.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InstalledSoftware {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
}

/// Регистрация агента: токен регистрации из MSI/GPO и сведения о ПК.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EnrollRequest {
    pub token: String,
    pub report: AgentReport,
}

/// Ключ агента — показывается один раз, дальше агент предъявляет его в `Authorization: Bearer`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EnrollResponse {
    pub device_id: String,
    pub agent_key: String,
}

/// Обновление агента, назначенное порталом. Подпись Ed25519 — над [`update_message`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AgentUpdate {
    pub package_id: String,
    pub version: String,
    /// SHA-256 пакета, hex.
    pub sha256: String,
    /// Подпись, base64.
    pub signature: String,
}

/// Что подписывается ключом обновлений: тип, платформа, версия и хэш — подпись нельзя переложить
/// на другой файл, версию или платформу.
pub fn update_message(version: &str, sha256_hex: &str) -> Vec<u8> {
    format!("pult-agent-update\n{UPDATE_TARGET}\n{version}\n{}\n", sha256_hex.to_ascii_lowercase()).into_bytes()
}

/// Платформа сборки агента, для которой подписано обновление.
pub const UPDATE_TARGET: &str = "windows-x86_64";

/// Ответ на отчёт.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportResponse {
    /// Через сколько секунд прислать следующий отчёт.
    pub next_report_secs: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<AgentUpdate>,
    /// Прислать оборудование и программы в следующем отчёте: у портала их нет или они старше суток.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub want_inventory: bool,
}

/// Действие helper на ПК инженера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAction {
    /// `mstsc /shadow` к сессии пользователя.
    Shadow,
    /// `\\ПК\C$` в проводнике.
    Files,
    /// «Управление компьютером».
    Manage,
    /// PowerShell Remoting.
    Powershell,
    /// Проверка доступности: ping и порты 3389/445/5985.
    Probe,
}

impl RemoteAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shadow => "shadow",
            Self::Files => "files",
            Self::Manage => "manage",
            Self::Powershell => "powershell",
            Self::Probe => "probe",
        }
    }
}

/// Режим теневого подключения.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShadowMode {
    View,
    Control,
}

/// Helper предъявляет одноразовый токен из ссылки `pult://connect?t=…`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExchangeRequest {
    pub token: String,
}

/// Параметры подключения, решённые порталом.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConnectParams {
    pub connection_id: String,
    pub action: RemoteAction,
    /// FQDN или имя ПК.
    pub computer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ShadowMode>,
    /// Пользователь увидит запрос согласия (без `/noConsentPrompt`).
    pub consent_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_user: Option<String>,
}

/// Итог действия helper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConnectResult {
    pub token: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Подробности (проверка доступности: `{"ping": true, "3389": false, …}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub details: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: u32, state: SessionState, user: Option<&str>) -> Session {
        Session {
            id,
            station: "x".into(),
            state,
            user: user.map(Into::into),
            domain: None,
            client_name: None,
            logon_at: None,
        }
    }

    #[test]
    fn user_sessions_filter() {
        assert!(s(1, SessionState::Active, Some("ivanov")).is_user_session());
        assert!(s(2, SessionState::Disconnected, Some("ivanov")).is_user_session());
        assert!(!s(0, SessionState::Disconnected, Some("SYSTEM")).is_user_session(), "сессия 0 — службы");
        assert!(!s(3, SessionState::Listen, None).is_user_session());
        assert!(!s(4, SessionState::Active, Some("")).is_user_session());
    }

    #[test]
    fn wts_state_mapping() {
        assert_eq!(SessionState::from_wts(0), SessionState::Active);
        assert_eq!(SessionState::from_wts(4), SessionState::Disconnected);
        assert_eq!(SessionState::from_wts(42), SessionState::Unknown);
    }

    #[test]
    fn report_json_shape() {
        let r = AgentReport {
            hostname: "WS-001".into(),
            domain: Some("CORP".into()),
            os: "windows".into(),
            agent_version: "0.0.1".into(),
            sessions: vec![s(1, SessionState::Active, Some("ivanov"))],
            fqdn: None,
            os_version: None,
            ip_addrs: vec![],
            inventory: None,
        };
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["sessions"][0]["state"], "active");
        assert!(j.get("inventory").is_none(), "без оборудования поле не пишется");
        let mut r = r;
        r.inventory = Some(HardwareInventory {
            model: Some("OptiPlex 7090".into()),
            memory_mb: Some(16384),
            disks: vec![DeviceDisk { name: "C:".into(), size_gb: 476, free_gb: 120 }],
            software: vec![InstalledSoftware { name: "7-Zip".into(), version: Some("24.08".into()), publisher: None }],
            ..Default::default()
        });
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["inventory"]["software"][0]["name"], "7-Zip");
        assert_eq!(serde_json::from_value::<AgentReport>(j).unwrap(), r);
        let resp: ReportResponse = serde_json::from_value(serde_json::json!({"next_report_secs": 60})).unwrap();
        assert!(!resp.want_inventory, "старый портал не просит оборудование");
    }

    #[test]
    fn prototype_report_still_accepted() {
        let j = serde_json::json!({"hostname": "WS-1", "domain": null, "os": "windows", "agent_version": "0.0.1",
            "sessions": [{"id": 1, "station": "Console", "state": "active", "user": "u", "domain": null, "client_name": null}]});
        let r: AgentReport = serde_json::from_value(j).unwrap();
        assert!(r.fqdn.is_none() && r.ip_addrs.is_empty() && r.sessions[0].logon_at.is_none());
    }

    #[test]
    fn update_message_binds_version_and_hash() {
        assert_eq!(update_message("1.2.3", "ABcd"), b"pult-agent-update\nwindows-x86_64\n1.2.3\nabcd\n".to_vec());
        assert_ne!(update_message("1.2.3", "ab"), update_message("1.2.4", "ab"));
    }
}
