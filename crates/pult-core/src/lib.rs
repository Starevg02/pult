//! Ядро Pult: контракт модуля, сеансы, права, аудит, уведомления, настройки, секреты.
//!
//! Модуль — это крейт с тремя точками входа в ядро (план, раздел 4):
//! [`Module::routes`], [`Module::permissions`] и [`Module::jobs`].
//! Модули обращаются друг к другу только через публичные сервисы крейтов,
//! не через чужие таблицы.

pub mod api;
pub mod audit;
pub mod crypto;
pub mod error;
pub mod events;
pub mod l10n;
pub mod mail;
pub mod notify;
pub mod org;
pub mod rbac;
pub mod retention;
pub mod secret;
pub mod session;
pub mod settings;
pub mod totp;

pub use error::{ApiError, ApiResult};
pub use secret::Secret;
pub use session::CurrentUser;

use std::sync::Arc;

/// Разрешение, которое объявляет модуль, например `helpdesk.ticket.view_all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Permission {
    /// Машинное имя: `<модуль>.<объект>.<действие>`.
    pub code: &'static str,
    /// Описание для экрана «Роли и разрешения».
    pub description: &'static str,
    /// Системные роли, которые получают разрешение при первом появлении (дальше — как настроит администратор).
    pub default_roles: &'static [&'static str],
}

/// Фоновая задача модуля (очередь `pult-jobs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobDefinition {
    /// Уникальное имя задачи, например `ad.sync.incremental`.
    pub name: &'static str,
    /// Расписание в формате cron или `None` для задач по событию.
    pub schedule: Option<&'static str>,
}

/// Общее состояние, которое нужно ядру в каждом запросе.
#[derive(Clone)]
pub struct Core {
    pub db: sqlx::PgPool,
    pub secrets: Arc<crypto::SecretBox>,
    pub cookies: session::CookiePolicy,
    /// Все разрешения всех модулей — для матрицы ролей.
    pub permissions: Arc<Vec<Permission>>,
    /// События для браузеров (SSE).
    pub events: events::Hub,
    /// Разрешить почту без TLS (`PULT_INSECURE_MAIL=1`) — только для разработки и CI.
    pub insecure_mail: bool,
    /// Включённые модули (кэш для [`org::gate`]).
    pub org: org::ModuleCache,
}

/// Контракт модуля с ядром.
pub trait Module<S: Clone + Send + Sync + 'static> {
    /// Короткое имя модуля: `helpdesk`, `people`…
    fn name(&self) -> &'static str;
    /// Маршруты модуля вместе с описанием OpenAPI; ядро монтирует их в корень.
    fn routes(&self) -> utoipa_axum::router::OpenApiRouter<S>;
    /// Разрешения, которые модуль проверяет.
    fn permissions(&self) -> Vec<Permission> {
        Vec::new()
    }
    /// Фоновые задачи модуля.
    fn jobs(&self) -> Vec<JobDefinition> {
        Vec::new()
    }
}
