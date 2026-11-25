//! Прикладное API: маршруты модулей ядра, людей и справочника на общем состоянии.

use axum::extract::FromRef;
use pult_chat::ChatModule;
use pult_core::{Core, Module, Permission, api::CoreModule};
use pult_directory::DirectoryModule;
use pult_helpdesk::{HelpdeskState, api::HelpdeskModule};
use pult_people::{PeopleModule, PeopleState};
use pult_phonebook::api::PhonebookModule;
use pult_remote::RemoteModule;
use utoipa_axum::router::OpenApiRouter;

/// Состояние прикладного API. Модули достают из него своё через `FromRef`.
#[derive(Clone)]
pub struct ApiState {
    pub core: Core,
    pub people: PeopleState,
    pub helpdesk: HelpdeskState,
    pub backup: crate::backup::BackupConfig,
    /// Код мастера первого запуска.
    pub setup: crate::setup::SetupState,
    /// Сертификат под управлением портала; `None` — HTTPS настроен снаружи.
    pub tls: Option<crate::tls::ManagedTls>,
}

impl FromRef<ApiState> for Core {
    fn from_ref(s: &ApiState) -> Self {
        s.core.clone()
    }
}

impl FromRef<ApiState> for HelpdeskState {
    fn from_ref(s: &ApiState) -> Self {
        s.helpdesk.clone()
    }
}

impl FromRef<ApiState> for PeopleState {
    fn from_ref(s: &ApiState) -> Self {
        s.people.clone()
    }
}

/// Маршруты всех модулей с их описанием OpenAPI.
pub fn modules() -> OpenApiRouter<ApiState> {
    OpenApiRouter::new()
        .merge(<CoreModule as Module<ApiState>>::routes(&CoreModule))
        .merge(<PeopleModule as Module<ApiState>>::routes(&PeopleModule))
        .merge(<PhonebookModule as Module<ApiState>>::routes(&PhonebookModule))
        .merge(<HelpdeskModule as Module<ApiState>>::routes(&HelpdeskModule))
        .merge(<DirectoryModule as Module<ApiState>>::routes(&DirectoryModule))
        .merge(<RemoteModule as Module<ApiState>>::routes(&RemoteModule))
        .merge(<ChatModule as Module<ApiState>>::routes(&ChatModule))
        .merge(crate::setup::routes())
        .routes(utoipa_axum::routes!(crate::negotiate::kerberos))
}

/// Все разрешения всех модулей — для матрицы ролей и начальной раздачи системным ролям.
pub fn permissions() -> Vec<Permission> {
    let mut all = <CoreModule as Module<ApiState>>::permissions(&CoreModule);
    all.extend(<PeopleModule as Module<ApiState>>::permissions(&PeopleModule));
    all.extend(<PhonebookModule as Module<ApiState>>::permissions(&PhonebookModule));
    all.extend(<HelpdeskModule as Module<ApiState>>::permissions(&HelpdeskModule));
    all.extend(<DirectoryModule as Module<ApiState>>::permissions(&DirectoryModule));
    all.extend(<RemoteModule as Module<ApiState>>::permissions(&RemoteModule));
    all.extend(<ChatModule as Module<ApiState>>::permissions(&ChatModule));
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_codes_are_unique_and_namespaced() {
        let all = permissions();
        let mut codes: Vec<_> = all.iter().map(|p| p.code).collect();
        codes.sort_unstable();
        let n = codes.len();
        codes.dedup();
        assert_eq!(n, codes.len(), "коды разрешений повторяются");
        assert!(all.iter().all(|p| (2..=3).contains(&p.code.split('.').count()) && !p.description.is_empty()));
    }
}
