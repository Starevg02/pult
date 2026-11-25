//! Схема OpenAPI, собранная из аннотаций обработчиков (`utoipa`).
//! CI генерирует из неё TS-клиент для `web/` (`openapi-typescript`).

use utoipa::{
    Modify, OpenApi,
    openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme},
};

#[derive(OpenApi)]
#[openapi(
    info(title = "Pult API", description = "API портала IT-службы Pult."),
    paths(crate::http::health, crate::negotiate::negotiate),
    components(schemas(crate::http::Health, crate::http::Check, pult_api_types::AgentReport, pult_api_types::Session, pult_api_types::SessionState, crate::negotiate::NegotiateResult, crate::negotiate::NegotiateError, crate::negotiate::DenyReason)),
    modifiers(&AgentKey),
    tags((name = "system", description = "Состояние сервера"), (name = "auth", description = "Вход и сеанс"), (name = "phonebook", description = "Справочник"), (name = "sites", description = "Площадки"), (name = "notifications", description = "Уведомления"), (name = "chat", description = "Коммуникации"), (name = "admin", description = "Администрирование"), (name = "agent", description = "Агент рабочей станции"), (name = "remote", description = "Удалённая поддержка"), (name = "remote-helper", description = "Helper на ПК инженера"), (name = "remote-admin", description = "Удалённая поддержка: администрирование"), (name = "auth-prototype", description = "Прототип П4: вход через домен (Kerberos)"))
)]
struct ApiDoc;

struct AgentKey;

impl Modify for AgentKey {
    fn modify(&self, api: &mut utoipa::openapi::OpenApi) {
        if let Some(c) = api.components.as_mut() {
            c.add_security_scheme(
                "agent_key",
                SecurityScheme::Http(HttpBuilder::new().scheme(HttpAuthScheme::Bearer).build()),
            );
        }
    }
}

/// Схема с версией из Cargo.
pub fn spec() -> utoipa::openapi::OpenApi {
    let mut api = ApiDoc::openapi();
    api.merge(crate::api::modules().split_for_parts().1);
    api.info.version = env!("CARGO_PKG_VERSION").to_string();
    api
}

#[cfg(test)]
mod tests {
    #[test]
    fn spec_has_paths_and_schemas() {
        let json = serde_json::to_value(super::spec()).unwrap();
        for p in [
            "/health",
            "/api/agent/v1/enroll",
            "/api/agent/v1/report",
            "/api/remote/connections",
            "/api/remote/v1/exchange",
            "/api/auth/login",
            "/api/phonebook/persons",
            "/api/admin/audit",
        ] {
            assert!(json["paths"].get(p).is_some(), "нет пути {p}");
        }
        for s in ["Health", "AgentReport", "SessionState"] {
            assert!(json["components"]["schemas"].get(s).is_some(), "нет схемы {s}");
        }
        assert_eq!(json["components"]["securitySchemes"]["agent_key"]["scheme"], "bearer");
    }

    /// Одинаковые имена обработчиков в разных модулях дают дубли `operationId`, а одинаковые имена
    /// типов молча затирают схему друг друга — TS-клиент тогда врёт (так было с `get_config` и `Kind`).
    #[test]
    fn operation_ids_and_schemas_do_not_collide() {
        let json = serde_json::to_value(super::spec()).unwrap();
        let mut ids: Vec<String> = json["paths"]
            .as_object()
            .into_iter()
            .flat_map(|p| p.values())
            .filter_map(|ops| ops.as_object())
            .flat_map(|ops| ops.values())
            .filter_map(|op| op["operationId"].as_str().map(str::to_string))
            .collect();
        ids.sort();
        let n = ids.len();
        ids.dedup();
        assert_eq!(n, ids.len(), "operationId повторяются");
        let s = &json["components"]["schemas"];
        assert!(s["Kind"]["enum"].as_array().is_some_and(|e| e.iter().any(|v| v == "incremental")));
        assert!(s["OperationKind"]["enum"].as_array().is_some_and(|e| e.iter().any(|v| v == "user.unlock")));
    }
}
