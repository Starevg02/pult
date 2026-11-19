//! Роли и разрешения.
//!
//! Разрешения объявляются модулями в коде, роли и их состав — в БД (матрица настраивается).
//! Суперадминистратор имеет все разрешения независимо от матрицы. Членство в группах AD
//! роли не определяет.

use crate::Permission;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::BTreeSet;

pub const ROLE_EMPLOYEE: &str = "employee";
pub const ROLE_ENGINEER: &str = "engineer";
pub const ROLE_PHONEBOOK_EDITOR: &str = "phonebook_editor";
pub const ROLE_SUPERADMIN: &str = "superadmin";
pub const ROLE_AUDITOR: &str = "auditor";

/// Разрешения ядра.
pub mod perm {
    use crate::Permission;
    use crate::rbac::{ROLE_AUDITOR, ROLE_SUPERADMIN};

    pub const SETTINGS_MANAGE: Permission = Permission {
        code: "core.settings.manage",
        description: "Изменять настройки системы и безопасности",
        default_roles: &[ROLE_SUPERADMIN],
    };
    pub const ROLES_MANAGE: Permission = Permission {
        code: "core.roles.manage",
        description: "Назначать роли и менять матрицу разрешений",
        default_roles: &[ROLE_SUPERADMIN],
    };
    pub const AUDIT_VIEW: Permission = Permission {
        code: "core.audit.view",
        description: "Просматривать и выгружать журнал аудита",
        default_roles: &[ROLE_AUDITOR, ROLE_SUPERADMIN],
    };
    pub const MAIL_MANAGE: Permission = Permission {
        code: "core.mail.manage",
        description: "Настраивать отправку почты (SMTP)",
        default_roles: &[ROLE_SUPERADMIN],
    };

    pub const ALL: &[Permission] = &[SETTINGS_MANAGE, ROLES_MANAGE, AUDIT_VIEW, MAIL_MANAGE];
}

/// Разрешения пользователя на время запроса.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Grants {
    pub roles: BTreeSet<String>,
    pub permissions: BTreeSet<String>,
}

impl Grants {
    pub fn is_superadmin(&self) -> bool {
        self.roles.contains(ROLE_SUPERADMIN)
    }

    pub fn has(&self, p: &Permission) -> bool {
        self.is_superadmin() || self.permissions.contains(p.code)
    }
}

pub async fn load_grants(db: &PgPool, user_id: uuid::Uuid) -> Result<Grants, sqlx::Error> {
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT r.code, rp.permission
           FROM user_roles ur
           JOIN roles r ON r.id = ur.role_id
           LEFT JOIN role_permissions rp ON rp.role_id = r.id
          WHERE ur.user_id = $1",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    let mut g = Grants::default();
    // Роль «Сотрудник» есть у всех активных пользователей — без записи в user_roles.
    g.roles.insert(ROLE_EMPLOYEE.to_string());
    for (role, p) in rows {
        g.roles.insert(role);
        if let Some(p) = p {
            g.permissions.insert(p);
        }
    }
    let base: Vec<String> = sqlx::query_scalar(
        "SELECT rp.permission FROM role_permissions rp JOIN roles r ON r.id = rp.role_id WHERE r.code = $1",
    )
    .bind(ROLE_EMPLOYEE)
    .fetch_all(db)
    .await?;
    g.permissions.extend(base);
    Ok(g)
}

/// Выдаёт системным ролям разрешения по умолчанию — только разрешениям, которые появились
/// впервые. Если администратор снял разрешение с роли, при следующем старте оно не вернётся.
pub async fn seed_defaults(db: &PgPool, all: &[Permission]) -> Result<usize, sqlx::Error> {
    let mut tx = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(7231002)").execute(&mut *tx).await?;
    let seeded: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'rbac_seeded'").fetch_optional(&mut *tx).await?;
    let mut done: BTreeSet<String> = seeded.and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    let mut added = 0;
    let fresh: Vec<&Permission> = all.iter().filter(|p| !done.contains(p.code)).collect();
    for p in fresh {
        for role in p.default_roles {
            added += sqlx::query(
                "INSERT INTO role_permissions (role_id, permission)
                 SELECT id, $2 FROM roles WHERE code = $1 ON CONFLICT DO NOTHING",
            )
            .bind(role)
            .bind(p.code)
            .execute(&mut *tx)
            .await?
            .rows_affected() as usize;
        }
        done.insert(p.code.to_string());
    }
    let v = serde_json::to_value(&done).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    sqlx::query(
        "INSERT INTO settings (key, value) VALUES ('rbac_seeded', $1)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(v)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn superadmin_has_everything() {
        let mut g = Grants::default();
        assert!(!g.has(&perm::AUDIT_VIEW));
        g.permissions.insert(perm::AUDIT_VIEW.code.into());
        assert!(g.has(&perm::AUDIT_VIEW) && !g.has(&perm::ROLES_MANAGE));
        g.roles.insert(ROLE_SUPERADMIN.into());
        assert!(g.has(&perm::ROLES_MANAGE));
    }
}
