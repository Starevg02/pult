//! Люди портала: учётки AD и синхронизация, пользователи и вход, площадки.

pub mod admin;
pub mod auth;
pub mod computers;
pub mod domain;
pub mod health;
pub mod oidc;
pub mod profile;
pub mod push;
pub mod sites;
pub mod social;
pub mod sync;
pub mod twofactor;

use axum::extract::FromRef;
use pult_core::{Core, Module, Permission};
use utoipa_axum::{router::OpenApiRouter, routes};

pub mod perm {
    use pult_core::{Permission, rbac::ROLE_SUPERADMIN};

    pub const AD_MANAGE: Permission = Permission {
        code: "people.ad.manage",
        description: "Настраивать подключение к AD и запускать синхронизацию",
        default_roles: &[ROLE_SUPERADMIN],
    };
    pub const SITES_MANAGE: Permission = Permission {
        code: "people.sites.manage",
        description: "Вести площадки и ответственных инженеров",
        default_roles: &[ROLE_SUPERADMIN],
    };
    pub const LINK_MANAGE: Permission = Permission {
        code: "people.link.manage",
        description: "Связывать учётки AD с сотрудниками справочника",
        default_roles: &[ROLE_SUPERADMIN],
    };
}

/// Состояние модуля: пул контроллеров и первый суперадминистратор из конфига.
#[derive(Clone, Default)]
pub struct PeopleState {
    pub ad: domain::AdHandle,
    /// `PULT_INITIAL_SUPERADMIN` (`DOMAIN\login`) — применяется, только пока суперадминистраторов нет.
    pub initial_superadmin: Option<String>,
}

pub struct PeopleModule;

/// Имена фоновых задач модуля.
pub mod jobs {
    pub const SYNC_INCREMENTAL: &str = "ad.sync.incremental";
    pub const SYNC_FULL: &str = "ad.sync.full";
    pub const HEALTH: &str = "ad.health";
    pub const COMPUTERS: &str = "ad.computers";
    /// Рассылка уведомлений на устройства; кроме расписания, запускается сразу по событию уведомления.
    pub const PUSH: &str = "notify.push";
}

impl<S> Module<S> for PeopleModule
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
    PeopleState: FromRef<S>,
{
    fn name(&self) -> &'static str {
        "people"
    }

    fn routes(&self) -> OpenApiRouter<S> {
        OpenApiRouter::new()
            .routes(routes!(auth::login_handler))
            .routes(routes!(auth::logout_handler))
            .routes(routes!(auth::me_handler))
            .routes(routes!(auth::get_my_settings, auth::put_my_settings))
            .routes(routes!(auth::put_my_locale))
            .routes(routes!(profile::put_my_prefs))
            .routes(routes!(profile::get_my_profile))
            .routes(routes!(profile::change_my_password))
            .routes(routes!(push::get_my_push, push::subscribe_my_push, push::unsubscribe_my_push))
            .routes(routes!(push::test_my_push))
            .routes(routes!(push::get_push_settings, push::put_push_settings))
            .routes(routes!(profile::list_my_sessions))
            .routes(routes!(profile::revoke_my_session))
            .routes(routes!(profile::revoke_other_sessions))
            .routes(routes!(social::get_my_card))
            .routes(routes!(social::put_my_status))
            .routes(routes!(social::put_my_about))
            .routes(routes!(social::heartbeat))
            .routes(routes!(social::put_my_avatar, social::delete_my_avatar))
            .routes(routes!(social::get_avatar, social::moderate_avatar))
            .routes(routes!(social::presence_lookup))
            .routes(routes!(social::get_user_card))
            .routes(routes!(social::get_wall, social::create_wall_post))
            .routes(routes!(social::delete_wall_post))
            .routes(routes!(social::like_post, social::unlike_post))
            .routes(routes!(twofactor::auth_context))
            .routes(routes!(twofactor::get_my_totp))
            .routes(routes!(twofactor::setup_my_totp))
            .routes(routes!(twofactor::confirm_my_totp))
            .routes(routes!(twofactor::disable_my_totp))
            .routes(routes!(twofactor::reset_user_totp))
            .routes(routes!(sites::list_sites))
            .routes(routes!(sites::create_site))
            .routes(routes!(sites::update_site))
            .routes(routes!(sites::get_engineers, sites::put_engineers, sites::add_engineer))
            .routes(routes!(sites::engineer_candidates))
            .routes(routes!(sites::remove_engineer))
            .routes(routes!(admin::get_ad, admin::put_ad))
            .routes(routes!(admin::test_ad))
            .routes(routes!(admin::run_sync))
            .routes(routes!(admin::list_candidates))
            .routes(routes!(admin::decide_candidate))
            .routes(routes!(admin::get_link_settings, admin::put_link_settings))
            .routes(routes!(oidc::oidc_start))
            .routes(routes!(oidc::oidc_callback))
            .routes(routes!(oidc::get_oidc, oidc::put_oidc))
            .routes(routes!(oidc::test_oidc))
            .routes(routes!(oidc::reset_oidc_identities))
    }

    fn permissions(&self) -> Vec<Permission> {
        vec![perm::AD_MANAGE, perm::SITES_MANAGE, perm::LINK_MANAGE, social::MODERATE]
    }

    fn jobs(&self) -> Vec<pult_core::JobDefinition> {
        vec![
            pult_core::JobDefinition { name: jobs::SYNC_INCREMENTAL, schedule: Some("*/5 * * * *") },
            pult_core::JobDefinition { name: jobs::SYNC_FULL, schedule: Some("0 3 * * *") },
            pult_core::JobDefinition { name: jobs::HEALTH, schedule: Some("* * * * *") },
            pult_core::JobDefinition { name: jobs::COMPUTERS, schedule: Some("*/15 * * * *") },
            pult_core::JobDefinition { name: jobs::PUSH, schedule: Some("* * * * *") },
        ]
    }
}
