//! Клиент Active Directory: вход (bind), поиск, синхронизация, переключение между DC.
//!
//! Клиент AD: bind по LDAPS к обоим DC и переключение,
//! полная (постранично) и инкрементальная (по `uSNChanged`) синхронизация.
//!
//! Правила (ADR-0002):
//! - подключение только LDAPS или StartTLS — незащищённый режим не выражается типами;
//! - пароль живёт только в памяти на время bind и не попадает в логи и ошибки;
//! - неверный пароль не повторяется на другом DC (не удваиваем счётчик блокировки в домене).

pub mod config;
pub mod errors;
pub mod failover;
pub mod pool;
pub mod sync;
pub mod write;

pub use config::{AdConfig, ConfigError, DcEndpoint, TlsMode};
pub use errors::{AdError, BindFailure};
pub use pool::{AuthenticatedUser, DcCheck, DcPool, connection_hint};
pub use sync::{AdAccount, AdComputer, SyncBatch};
pub use write::{AccountState, Applied, Change};
