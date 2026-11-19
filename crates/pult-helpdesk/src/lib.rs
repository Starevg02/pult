//! Helpdesk: заявки, назначение, лента, почта, массовые сбои.

pub mod access;
pub mod admin;
pub mod api;
pub mod assignment;
pub mod card;
pub mod checklist;
pub mod files;
pub mod integrations;
pub mod jobs;
pub mod kb;
pub mod notify;
pub mod objects;
pub mod public;
pub mod reports;
pub mod rules;
pub mod schedule;
pub mod service;
pub mod settings;
pub mod sla;
pub mod starter;
pub mod templates;
pub mod tickets;

use pult_core::Permission;
use std::path::PathBuf;

pub mod perm {
    use pult_core::{
        Permission,
        rbac::{ROLE_ENGINEER, ROLE_SUPERADMIN},
    };

    pub const WORK: Permission = Permission {
        code: "helpdesk.ticket.work",
        description: "Работать с заявками: видеть все, отвечать, менять, передавать, решать",
        default_roles: &[ROLE_ENGINEER, ROLE_SUPERADMIN],
    };
    pub const ON_BEHALF: Permission = Permission {
        code: "helpdesk.ticket.create_on_behalf",
        description: "Регистрировать заявку за сотрудника (по звонку или лично)",
        default_roles: &[ROLE_ENGINEER, ROLE_SUPERADMIN],
    };
    pub const ADMIN: Permission = Permission {
        code: "helpdesk.admin",
        description: "Настраивать Helpdesk: категории, рабочее время, отсутствия инженеров",
        default_roles: &[ROLE_SUPERADMIN],
    };
}

pub fn permissions() -> Vec<Permission> {
    vec![perm::WORK, perm::ON_BEHALF, perm::ADMIN, objects::MANAGE_ASSETS, kb::EDIT, reports::VIEW, access::RESOURCES]
}

/// Состояние модуля.
#[derive(Clone)]
pub struct HelpdeskState {
    /// Каталог вложений.
    pub files_dir: PathBuf,
}
