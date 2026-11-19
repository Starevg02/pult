//! Агент рабочей станции: служба Windows под LocalSystem.
//!
//! - Регистрируется на портале токеном из MSI/GPO и получает свой ключ ([`client`], [`config`]).
//! - Раз в минуту и сразу при входе, выходе, блокировке (уведомления SCM) сообщает: имя ПК,
//!   домен, версию ОС и агента, адреса, сессии пользователей (`WTSEnumerateSessions`) — [`run`].
//! - Только исходящий HTTPS, сертификат сервера проверяется по хранилищу Windows; при недоступности
//!   портала — повтор с растущей паузой, данные не копятся (нужно только текущее состояние).
//! - Обновление — по команде портала: пакет принимается, только если SHA-256 и подпись Ed25519
//!   сходятся с ключом, заданным при установке ([`update`]); ключ подписи на сервере не хранится,
//!   поэтому взлом портала не даёт запустить свой код на всех ПК от имени SYSTEM.

pub use pult_api_types::{AgentReport, Session, SessionState};

pub mod client;
pub mod config;
pub mod run;
pub mod update;

#[cfg(windows)]
pub mod host;
#[cfg(windows)]
pub mod service;
#[cfg(windows)]
pub mod sys;
#[cfg(windows)]
pub mod wts;

#[cfg(windows)]
pub use host::windows_agent;

/// Имя службы Windows (MSI: `ServiceInstall Name`).
pub const SERVICE_NAME: &str = "PultAgent";
