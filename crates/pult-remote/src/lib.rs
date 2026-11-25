//! Удалённая поддержка — замена RUSSIANANYDESK.
//!
//! - Агент на рабочей станции регистрируется многоразовым токеном и получает свой ключ ([`agent`]);
//!   раз в минуту и при входе/выходе сообщает сессии — отсюда «где залогинен» и имя ПК в заявке.
//! - Инженер нажимает «Подключиться»: сервер проверяет права и политику ([`policy`]), создаёт запись
//!   подключения и одноразовый токен на 60 с ([`connect`]); helper на ПК инженера обменивает токен
//!   на параметры и запускает `mstsc /shadow`. Каждое подключение и каждый отказ — в аудите.
//! - Администратор: токены регистрации, устройства, политика, пакеты агента и helper ([`admin`]).

pub mod admin;
pub mod agent;
pub mod connect;
pub mod devices;
pub mod inventory;
pub mod policy;
pub mod tokens;

use pult_core::{Permission, settings::SettingsGroup};
use serde::{Deserialize, Serialize};

pub mod perm {
    use pult_core::{
        Permission,
        rbac::{ROLE_ENGINEER, ROLE_SUPERADMIN},
    };

    pub const DEVICE_VIEW: Permission = Permission {
        code: "remote.device.view",
        description: "Видеть компьютеры с агентом, их сессии и «где залогинен» сотрудник",
        default_roles: &[ROLE_ENGINEER, ROLE_SUPERADMIN],
    };
    pub const CONNECT_VIEW: Permission = Permission {
        code: "remote.connect.view",
        description: "Подключаться к сессии пользователя в режиме просмотра (всегда с согласием)",
        default_roles: &[ROLE_ENGINEER, ROLE_SUPERADMIN],
    };
    pub const CONNECT_CONTROL: Permission = Permission {
        code: "remote.connect.control",
        description: "Подключаться с управлением и быстрыми действиями (C$, управление компьютером, PowerShell)",
        default_roles: &[ROLE_SUPERADMIN],
    };
    pub const MANAGE: Permission = Permission {
        code: "remote.manage",
        description: "Настраивать удалённую поддержку: токены агентов, устройства, политику, пакеты",
        default_roles: &[ROLE_SUPERADMIN],
    };
}

pub fn permissions() -> Vec<Permission> {
    vec![perm::DEVICE_VIEW, perm::CONNECT_VIEW, perm::CONNECT_CONTROL, perm::MANAGE]
}

/// Настройки удалённой поддержки (админка).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct RemoteSettings {
    /// Регистрация агента только с адреса, который DNS даёт этому ПК (защита от регистрации
    /// украденным токеном под чужим именем).
    pub require_dns_match: bool,
    /// ПК «в сети», если отчёт пришёл не позже стольких минут назад.
    pub online_minutes: u32,
    /// Пауза между отчётами агента, секунд.
    pub report_interval_secs: u32,
    /// Пакет обновления агента, назначенный всем агентам (`agent_packages.id`).
    pub update_package_id: Option<uuid::Uuid>,
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self { require_dns_match: true, online_minutes: 3, report_interval_secs: 60, update_package_id: None }
    }
}

impl SettingsGroup for RemoteSettings {
    const KEY: &'static str = "remote";
}

/// Модуль для сервера: маршруты и разрешения.
pub struct RemoteModule;

impl<S> pult_core::Module<S> for RemoteModule
where
    S: Clone + Send + Sync + 'static,
    pult_core::Core: axum::extract::FromRef<S>,
{
    fn name(&self) -> &'static str {
        "remote"
    }

    fn routes(&self) -> utoipa_axum::router::OpenApiRouter<S> {
        use utoipa_axum::routes;
        utoipa_axum::router::OpenApiRouter::new()
            .routes(routes!(agent::enroll))
            .routes(routes!(agent::report))
            .routes(routes!(agent::download_update))
            .routes(routes!(connect::create_connection))
            .routes(routes!(connect::get_connection))
            .routes(routes!(connect::exchange))
            .routes(routes!(connect::result))
            .routes(routes!(devices::list_devices))
            .routes(routes!(devices::get_device))
            .routes(routes!(inventory::search_software))
            .routes(routes!(devices::where_is))
            .routes(routes!(devices::person_sessions))
            .routes(routes!(devices::put_favorite, devices::delete_favorite))
            .routes(routes!(devices::download_package))
            .routes(routes!(admin::list_enrollment_tokens, admin::create_enrollment_token))
            .routes(routes!(admin::revoke_enrollment_token))
            .routes(routes!(admin::list_remote_policies, admin::create_remote_policy))
            .routes(routes!(admin::delete_remote_policy))
            .routes(routes!(admin::import_remote_policy))
            .routes(routes!(admin::deactivate_device))
            .routes(routes!(admin::activate_device))
            .routes(routes!(admin::get_remote_settings, admin::put_remote_settings))
            .routes(routes!(admin::make_current))
            .merge(
                utoipa_axum::router::OpenApiRouter::new()
                    .routes(routes!(admin::list_packages, admin::upload_package))
                    .layer(axum::extract::DefaultBodyLimit::max(admin::MAX_PACKAGE_BYTES + 4096)),
            )
    }

    fn permissions(&self) -> Vec<Permission> {
        permissions()
    }
}
