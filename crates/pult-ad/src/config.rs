//! Настройки подключения к домену.

use pult_core::Secret;
use serde::Deserialize;
use std::{fmt, path::PathBuf, time::Duration};

/// Способ защиты соединения. Незащищённого варианта нет намеренно (
/// «Bind без TLS запрещён на уровне кода, не настройки»).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    /// LDAP поверх TLS, порт 636.
    #[default]
    Ldaps,
    /// LDAP с командой StartTLS перед любыми другими операциями, порт 389.
    Starttls,
}

/// Контроллер домена: имя и порт.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DcEndpoint {
    pub host: String,
    pub port: u16,
}

impl DcEndpoint {
    /// Разбирает `dc1.example.local` или `dc1.example.local:636`.
    /// Адреса со схемой (`ldap://…`, `ldaps://…`) не принимаются: схему задаёт [`TlsMode`],
    /// чтобы `ldap://` без StartTLS нельзя было вписать в конфиг.
    pub fn parse(s: &str, tls: TlsMode) -> Result<Self, ConfigError> {
        let s = s.trim();
        if s.contains("://") {
            return Err(ConfigError::SchemeNotAllowed(s.to_string()));
        }
        if s.is_empty() {
            return Err(ConfigError::EmptyController);
        }
        let default_port = match tls {
            TlsMode::Ldaps => 636,
            TlsMode::Starttls => 389,
        };
        match s.rsplit_once(':') {
            Some((host, port)) if !host.is_empty() => {
                let port = port.parse().map_err(|_| ConfigError::BadPort(s.to_string()))?;
                Ok(Self { host: host.to_string(), port })
            }
            _ => Ok(Self { host: s.to_string(), port: default_port }),
        }
    }

    /// URL для ldap3. Для StartTLS схема `ldap://`, но соединение переводится в TLS
    /// до bind — см. [`crate::pool`].
    pub fn url(&self, tls: TlsMode) -> String {
        match tls {
            TlsMode::Ldaps => format!("ldaps://{}:{}", self.host, self.port),
            // SAFE: StartTLS — ldap3 переводит соединение в TLS до любых операций (set_starttls)
            TlsMode::Starttls => format!("ldap://{}:{}", self.host, self.port), // SAFE: StartTLS
        }
    }
}

impl fmt::Display for DcEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

/// Подключение к домену (в продукте — строка `ad_domains`, для команд проверки — секция `[ad]` конфига).
#[derive(Debug, Clone, Deserialize)]
pub struct AdConfig {
    /// Контроллеры в порядке приоритета.
    pub controllers: Vec<String>,
    #[serde(default)]
    pub tls: TlsMode,
    /// Корень поиска, например `DC=example,DC=local`.
    pub base_dn: String,
    /// Служебная учётка только на чтение (DN или UPN).
    pub bind_dn: String,
    pub bind_password: Secret,
    /// Сертификат CA домена (PEM), если он не в системном хранилище.
    pub ca_cert: Option<PathBuf>,
    /// То же содержимым (PEM) — для домена, заведённого в админке.
    #[serde(default)]
    pub ca_cert_pem: Option<String>,
    /// Фильтр пользователей для синхронизации.
    #[serde(default = "default_user_filter")]
    pub user_filter: String,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

fn default_user_filter() -> String {
    "(&(objectCategory=person)(objectClass=user))".to_string()
}

fn default_timeout_secs() -> u64 {
    5
}

impl AdConfig {
    /// Проверяет и разбирает список контроллеров.
    pub fn endpoints(&self) -> Result<Vec<DcEndpoint>, ConfigError> {
        if self.controllers.is_empty() {
            return Err(ConfigError::NoControllers);
        }
        self.controllers.iter().map(|c| DcEndpoint::parse(c, self.tls)).collect()
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs.max(1))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("в списке контроллеров нет ни одного DC")]
    NoControllers,
    #[error("пустое имя контроллера")]
    EmptyController,
    #[error("укажите контроллер без схемы (`dc1.example.local[:порт]`), режим TLS задаётся параметром `tls`: {0}")]
    SchemeNotAllowed(String),
    #[error("неверный порт: {0}")]
    BadPort(String),
    #[error("не удалось прочитать сертификат CA: {0}")]
    CaCert(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_ldap_url_is_rejected() {
        assert!(matches!(
            DcEndpoint::parse("ldap://dc1.example.local", TlsMode::Starttls), // SAFE: тест отказа
            Err(ConfigError::SchemeNotAllowed(_))
        ));
        assert!(matches!(
            DcEndpoint::parse("ldaps://dc1.example.local", TlsMode::Ldaps),
            Err(ConfigError::SchemeNotAllowed(_))
        ));
    }

    #[test]
    fn default_ports_follow_tls_mode() {
        let ldaps = DcEndpoint::parse("dc1.example.local", TlsMode::Ldaps).unwrap();
        assert_eq!((ldaps.port, ldaps.url(TlsMode::Ldaps).as_str()), (636, "ldaps://dc1.example.local:636"));
        let st = DcEndpoint::parse("dc2.example.local", TlsMode::Starttls).unwrap();
        assert_eq!(st.port, 389);
    }

    #[test]
    fn explicit_port() {
        let e = DcEndpoint::parse("10.0.0.5:3269", TlsMode::Ldaps).unwrap();
        assert_eq!(e, DcEndpoint { host: "10.0.0.5".into(), port: 3269 });
        assert!(matches!(DcEndpoint::parse("dc1:abc", TlsMode::Ldaps), Err(ConfigError::BadPort(_))));
    }

    #[test]
    fn config_debug_hides_password() {
        let cfg: AdConfig = toml_like();
        let s = format!("{cfg:?}");
        assert!(!s.contains("svc-secret"));
    }

    fn toml_like() -> AdConfig {
        AdConfig {
            controllers: vec!["dc1".into()],
            tls: TlsMode::Ldaps,
            base_dn: "DC=example,DC=local".into(),
            bind_dn: "svc-pult@example.local".into(),
            bind_password: Secret::new("svc-secret"),
            ca_cert: None,
            ca_cert_pem: None,
            user_filter: default_user_filter(),
            timeout_secs: 5,
        }
    }
}
