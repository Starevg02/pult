//! Каталог AD: запись в домен типизированными операциями (ADR-0002).
//!
//! Правила:
//! - по умолчанию только чтение; каждая операция записи — своё разрешение и ограничение областью
//!   (разрешённые OU для учёток, разрешённые группы для участия) — [`zones`];
//! - запрос операции: основание (комментарий и, по настройке, заявка), подтверждение в интерфейсе;
//! - выполняет фоновая задача под отдельной учёткой записи с делегированными правами;
//!   ключ идемпотентности: повторный запрос и повторная доставка задачи не выполняют операцию дважды;
//! - результат проверяется повторным чтением, в аудит пишется «было/стало»;
//! - пароль — только по LDAPS, временный пароль показывается запросившему один раз;
//! - нет обходных путей: API и очередь проверяют одни и те же разрешения ([`ops::authorize`]).
//!
//! Вход, сессии, роли, аудит, очередь и пул контроллеров — из ядра и `pult-ad`/`pult-people`.

pub mod api;
pub mod ops;
pub mod zones;

use pult_core::Permission;

pub mod perm {
    use pult_core::{
        Permission,
        rbac::{ROLE_ENGINEER, ROLE_SUPERADMIN},
    };

    pub const VIEW: Permission = Permission {
        code: "directory.account.view",
        description: "Видеть состояние учёток AD и операции каталога",
        default_roles: &[ROLE_ENGINEER, ROLE_SUPERADMIN],
    };
    pub const ADD_MEMBER: Permission = Permission {
        code: "directory.group.add_member",
        description: "Добавлять учётки в разрешённые группы AD",
        default_roles: &[ROLE_SUPERADMIN],
    };
    pub const UNLOCK: Permission = Permission {
        code: "directory.user.unlock",
        description: "Разблокировать учётки AD в разрешённых OU",
        default_roles: &[ROLE_SUPERADMIN],
    };
    pub const ENABLE: Permission = Permission {
        code: "directory.user.enable",
        description: "Включать и отключать учётки AD в разрешённых OU",
        default_roles: &[ROLE_SUPERADMIN],
    };
    pub const RESET_PASSWORD: Permission = Permission {
        code: "directory.user.reset_password",
        description: "Сбрасывать пароль учёток AD в разрешённых OU",
        default_roles: &[ROLE_SUPERADMIN],
    };
    pub const MANAGE: Permission = Permission {
        code: "directory.manage",
        description: "Настраивать запись в AD: учётку записи, разрешённые OU и группы",
        default_roles: &[ROLE_SUPERADMIN],
    };
}

pub fn permissions() -> Vec<Permission> {
    vec![perm::VIEW, perm::ADD_MEMBER, perm::UNLOCK, perm::ENABLE, perm::RESET_PASSWORD, perm::MANAGE]
}

/// Имя фоновой задачи «выполнить операцию каталога».
pub const JOB: &str = "directory.operation";

/// Модуль для сервера: маршруты и разрешения.
pub struct DirectoryModule;

impl<S> pult_core::Module<S> for DirectoryModule
where
    S: Clone + Send + Sync + 'static,
    pult_core::Core: axum::extract::FromRef<S>,
    pult_people::PeopleState: axum::extract::FromRef<S>,
{
    fn name(&self) -> &'static str {
        "directory"
    }

    fn routes(&self) -> utoipa_axum::router::OpenApiRouter<S> {
        use utoipa_axum::routes;
        utoipa_axum::router::OpenApiRouter::new()
            .routes(routes!(api::get_directory_config, api::put_directory_config))
            .routes(routes!(api::create_operation, api::list_operations))
            .routes(routes!(api::get_operation))
            .routes(routes!(api::reveal_password))
            .routes(routes!(api::person_account))
    }

    fn permissions(&self) -> Vec<Permission> {
        permissions()
    }
}
