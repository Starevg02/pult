//! Сквозные сценарии удалённой поддержки на настоящей PostgreSQL и поддельном DC.
//! Нужен `PULT_TEST_DATABASE_URL` (в CI задан). Тест создаёт отдельную базу.
//!
//! Сценарий плана: агент регистрируется и показывает ПК в портале; из карточки заявки инженер одним
//! нажатием подключается к сессии заявителя в режиме просмотра с запросом согласия; на ПК из списка
//! «обязательное согласие» без запроса нельзя даже с правом управления; на запрещённый ПК — нельзя
//! совсем; каждое подключение — в аудите; новая заявка с ПК заявителя сама получает имя этого ПК.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, FromRef},
    http::{Method, Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use http_body_util::BodyExt;
use pult_core::{Core, Module, Secret, api::CoreModule, crypto::SecretBox, session::CookiePolicy};
use pult_helpdesk::{HelpdeskState, api::HelpdeskModule};
use pult_people::{PeopleModule, PeopleState};
use pult_remote::RemoteModule;
use pult_testkit::{BASE_DN, Directory, FakeDc, FakeUser, SVC_DN, SVC_PASS, TestCa, USER_FILTER, USER_PASS};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, PgPool};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Clone)]
struct St {
    core: Core,
    people: PeopleState,
    helpdesk: HelpdeskState,
}

impl FromRef<St> for Core {
    fn from_ref(s: &St) -> Self {
        s.core.clone()
    }
}
impl FromRef<St> for PeopleState {
    fn from_ref(s: &St) -> Self {
        s.people.clone()
    }
}
impl FromRef<St> for HelpdeskState {
    fn from_ref(s: &St) -> Self {
        s.helpdesk.clone()
    }
}

fn permissions() -> Vec<pult_core::Permission> {
    let mut p = <CoreModule as Module<St>>::permissions(&CoreModule);
    p.extend(<PeopleModule as Module<St>>::permissions(&PeopleModule));
    p.extend(pult_helpdesk::permissions());
    p.extend(pult_remote::permissions());
    p
}

/// Запрос «с адреса» `ip`: так сервер видит агента, helper и браузер.
async fn call(
    app: &Router,
    ip: &str,
    method: Method,
    uri: &str,
    headers: &[(&str, String)],
    body: Option<Value>,
) -> (StatusCode, Value, Vec<u8>) {
    let mut req = Request::builder().method(method).uri(uri).header(header::CONTENT_TYPE, "application/json");
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let mut req = req.body(Body::from(body.map(|b| b.to_string()).unwrap_or_default())).unwrap();
    req.extensions_mut().insert(ConnectInfo(format!("{ip}:5000").parse::<SocketAddr>().unwrap()));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), bytes)
}

struct Browser {
    app: Router,
    ip: String,
    cookies: HashMap<String, String>,
}

impl Browser {
    async fn raw(
        &mut self,
        method: Method,
        uri: &str,
        body: Option<Value>,
        bytes: Option<Vec<u8>>,
    ) -> (StatusCode, Value, Vec<u8>) {
        let mut req = Request::builder().method(method.clone()).uri(uri);
        if !self.cookies.is_empty() {
            let c = self.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
            req = req.header(header::COOKIE, c);
        }
        if method != Method::GET
            && let Some(t) = self.cookies.get("__Host-pult_csrf")
        {
            req = req.header("x-csrf-token", t.clone());
        }
        let body = match (body, bytes) {
            (_, Some(b)) => {
                req = req.header(header::CONTENT_TYPE, "application/octet-stream");
                Body::from(b)
            }
            (b, None) => {
                req = req.header(header::CONTENT_TYPE, "application/json");
                Body::from(b.map(|b| b.to_string()).unwrap_or_default())
            }
        };
        let mut req = req.body(body).unwrap();
        req.extensions_mut().insert(ConnectInfo(format!("{}:5000", self.ip).parse::<SocketAddr>().unwrap()));
        let resp = self.app.clone().oneshot(req).await.unwrap();
        for v in resp.headers().get_all(header::SET_COOKIE) {
            let s = v.to_str().unwrap();
            let (kv, _) = s.split_once(';').unwrap_or((s, ""));
            let (k, v) = kv.split_once('=').unwrap();
            self.cookies.insert(k.to_string(), v.to_string());
        }
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), bytes)
    }

    async fn get(&mut self, uri: &str) -> (StatusCode, Value) {
        let (c, v, _) = self.raw(Method::GET, uri, None, None).await;
        (c, v)
    }

    async fn post(&mut self, uri: &str, body: Value) -> (StatusCode, Value) {
        let (c, v, _) = self.raw(Method::POST, uri, Some(body), None).await;
        (c, v)
    }

    async fn put(&mut self, uri: &str, body: Value) -> (StatusCode, Value) {
        let (c, v, _) = self.raw(Method::PUT, uri, Some(body), None).await;
        (c, v)
    }

    async fn login(app: &Router, login: &str, ip: &str) -> Self {
        let mut b = Self { app: app.clone(), ip: ip.to_string(), cookies: HashMap::new() };
        let (code, _) = b.post("/api/auth/login", json!({"login": login, "password": USER_PASS})).await;
        assert_eq!(code, StatusCode::NO_CONTENT, "вход {login}");
        b
    }
}

async fn db() -> Option<PgPool> {
    let Ok(url) = std::env::var("PULT_TEST_DATABASE_URL") else {
        assert!(std::env::var("CI").is_err(), "в CI нужен PULT_TEST_DATABASE_URL");
        eprintln!("PULT_TEST_DATABASE_URL не задан — интеграционный тест пропущен");
        return None;
    };
    let name = format!("pult_stage5_{}", std::process::id());
    let mut c = PgConnection::connect(&url).await.expect("подключение");
    // Имя — по номеру процесса: после перезапуска машины номер повторяется, старая база мешать не должна.
    sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
        .execute(&mut c)
        .await
        .expect("удаление старой базы");
    sqlx::query(&format!("CREATE DATABASE {name}")).execute(&mut c).await.expect("создание базы");
    let (base, _) = url.rsplit_once('/').unwrap();
    let pool = PgPool::connect(&format!("{base}/{name}")).await.expect("подключение к тестовой базе");
    sqlx::migrate!("../../migrations").run(&pool).await.expect("миграции");
    Some(pool)
}

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap_or_else(|| panic!("нет id: {v}")).to_string()
}

async fn audit(pool: &PgPool, action: &str) -> Vec<Value> {
    sqlx::query_scalar("SELECT details FROM audit_log WHERE action = $1 ORDER BY id")
        .bind(action)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// Отчёт агента: ПК и сессии `(id, логин, состояние)`.
fn report(host: &str, sessions: &[(u32, &str, &str)], version: &str) -> Value {
    json!({
        "hostname": host, "domain": "PULT", "os": "windows-x86_64", "agent_version": version,
        "fqdn": format!("{}.pult.test", host.to_lowercase()), "os_version": "Windows 11 Pro 23H2 (22631)",
        "ip_addrs": ["10.1.1.200"],
        "sessions": sessions.iter().map(|(id, user, state)| json!({
            "id": id, "station": "Console", "state": state, "user": user, "domain": "PULT",
            "client_name": null, "logon_at": 1_790_000_000
        })).chain([json!({"id": 0, "station": "Services", "state": "disconnected", "user": null, "domain": null, "client_name": null})])
        .collect::<Vec<_>>(),
    })
}

fn bearer(key: &str) -> Vec<(&'static str, String)> {
    vec![("authorization", format!("Bearer {key}"))]
}

#[tokio::test]
async fn stage5_acceptance() {
    let Some(pool) = db().await else { return };
    let core = Core {
        db: pool.clone(),
        secrets: Arc::new(SecretBox::new(&Secret::new("test-key-test-key-test-key-test-key-0"))),
        cookies: CookiePolicy { secure: true },
        permissions: Arc::new(permissions()),
        events: pult_core::events::Hub::default(),
        insecure_mail: false,
        org: Default::default(),
    };
    pult_core::rbac::seed_defaults(&pool, &core.permissions).await.unwrap();

    let dir = Directory::new(vec![
        FakeUser::person("admin", "Админов", "Адам", 1),
        FakeUser::person("eng1", "Инженеров", "Первый", 2),
        FakeUser::person("eng2", "Техников", "Второй", 3),
        FakeUser::person("emp", "Сотрудников", "Иван", 5),
        FakeUser::person("emp2", "Другой", "Пётр", 6),
    ]);
    let ca = TestCa::new("Pult Test CA");
    let dc = FakeDc::start(&ca, dir.clone()).await;
    let domain: Uuid = sqlx::query_scalar(
        "INSERT INTO ad_domains (dns_name, base_dn, controllers, tls_mode, bind_dn, bind_secret, ca_cert_pem, user_filter, timeout_secs)
         VALUES ('pult.test', $1, $2, 'ldaps', $3, $4, $5, $6, 2) RETURNING id",
    )
    .bind(BASE_DN)
    .bind(vec![dc.endpoint()])
    .bind(SVC_DN)
    .bind(core.secrets.encrypt(&Secret::new(SVC_PASS)).unwrap())
    .bind(ca.pem())
    .bind(USER_FILTER)
    .fetch_one(&pool)
    .await
    .unwrap();
    // Компьютеры домена (их синхронизирует каталог; адреса — из DNS).
    for (name, addrs) in [
        ("WS-EMP", vec!["10.1.1.50"]),
        ("WS-BUH", vec!["10.1.1.51"]),
        ("WS-SECRET", vec!["10.1.1.52"]),
        ("WS-NODNS", vec![]),
        ("WS-OFF", vec!["10.1.1.54"]),
    ] {
        sqlx::query(
            "INSERT INTO ad_computers (domain_id, object_guid, name, dns_host_name, is_enabled, addresses)
             VALUES ($1, $2, $3, $4, $5, $6::text[]::inet[])",
        )
        .bind(domain)
        .bind(Uuid::now_v7())
        .bind(name)
        .bind(format!("{}.pult.test", name.to_lowercase()))
        .bind(name != "WS-OFF")
        .bind(addrs)
        .execute(&pool)
        .await
        .unwrap();
    }
    let st = St {
        core: core.clone(),
        people: PeopleState { ad: Default::default(), initial_superadmin: Some("admin".into()) },
        helpdesk: HelpdeskState { files_dir: std::env::temp_dir().join(format!("pult-stage5-{}", std::process::id())) },
    };
    let (router, _) = utoipa_axum::router::OpenApiRouter::new()
        .merge(<CoreModule as Module<St>>::routes(&CoreModule))
        .merge(<PeopleModule as Module<St>>::routes(&PeopleModule))
        .merge(<HelpdeskModule as Module<St>>::routes(&HelpdeskModule))
        .merge(<RemoteModule as Module<St>>::routes(&RemoteModule))
        .split_for_parts();
    let app: Router = router.with_state(st.clone());

    let mut admin = Browser::login(&app, "admin", "10.1.1.10").await;
    let (code, s) = admin.post("/api/admin/ad/sync", json!({"kind": "full"})).await;
    assert_eq!(code, StatusCode::OK, "{s}");
    let mut eng1 = Browser::login(&app, "eng1", "10.1.1.11").await;
    let mut eng2 = Browser::login(&app, "eng2", "10.1.1.12").await;
    // Сотрудник работает за WS-EMP: браузер — с адреса этого ПК.
    let mut emp = Browser::login(&app, "emp", "10.1.1.50").await;
    let user_of = |login: &'static str| {
        let pool = pool.clone();
        async move {
            let id: Uuid = sqlx::query_scalar(
                "SELECT u.id FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id WHERE a.sam_account_name = $1",
            )
            .bind(login)
            .fetch_one(&pool)
            .await
            .unwrap();
            id
        }
    };
    let (u1, u2) = (user_of("eng1").await, user_of("eng2").await);
    for u in [u1, u2] {
        let (code, b) = admin.put(&format!("/api/admin/users/{u}/roles"), json!({"roles": ["engineer"]})).await;
        assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    }
    // eng2 — с правом управления (бывшая роль Full); eng1 — обычный инженер (просмотр, бывшая Viewer).
    sqlx::query("INSERT INTO roles (code, name) VALUES ('remote_operator', 'Удалённое управление')")
        .execute(&pool)
        .await
        .unwrap();
    let (code, b) = admin
        .put("/api/admin/roles/remote_operator/permissions", json!({"permissions": ["remote.connect.control"]}))
        .await;
    assert!(code.is_success(), "{b}");
    let (code, b) =
        admin.put(&format!("/api/admin/users/{u2}/roles"), json!({"roles": ["engineer", "remote_operator"]})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (_, site) =
        admin.post("/api/admin/sites", json!({"name": "Офис А", "timezone": "Europe/Moscow", "is_active": true})).await;
    let site = id(&site);
    let (code, r) = admin.put(&format!("/api/admin/sites/{site}/engineers"), json!({"l1": [u1, u2], "l2": []})).await;
    assert!(code.is_success(), "{r}");
    sqlx::query("UPDATE persons SET site_id = $1::uuid").bind(&site).execute(&pool).await.unwrap();
    let (_, cats) = emp.get("/api/helpdesk/categories").await;
    let printers = cats.as_array().unwrap().iter().find(|c| c["name"] == "Принтеры и сканеры").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // --- Токен регистрации агентов: только администратор, сам токен — один раз ---
    let (code, _) = eng1.post("/api/admin/remote/tokens", json!({"name": "x"})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "токены — только remote.manage");
    let (code, t) = admin.post("/api/admin/remote/tokens", json!({"name": "GPO: тестовые ПК"})).await;
    assert_eq!(code, StatusCode::CREATED, "{t}");
    let token = t["token"].as_str().unwrap().to_string();
    let (_, list) = admin.get("/api/admin/remote/tokens").await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert!(!list.to_string().contains(&token), "сам токен в списке не показывается: {list}");
    let stored: i64 =
        sqlx::query_scalar("SELECT count(*) FROM agent_enrollment_tokens WHERE token_hash = sha256($1::bytea)")
            .bind(token.as_bytes())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, 1, "в базе — только хэш");

    // --- Регистрация агента ---
    let enroll =
        |ip: &'static str, host: &'static str, token: String, sessions: Vec<(u32, &'static str, &'static str)>| {
            let app = app.clone();
            async move {
                let (code, v, _) = call(
                    &app,
                    ip,
                    Method::POST,
                    "/api/agent/v1/enroll",
                    &[],
                    Some(json!({"token": token, "report": report(host, &sessions, "0.5.0")})),
                )
                .await;
                (code, v)
            }
        };
    let (code, _) = enroll("10.1.1.50", "WS-EMP", format!("pae_{}", "x".repeat(43)), vec![]).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "чужой токен");
    let (code, _) = enroll("10.1.1.50", "WS-EMP", "garbage".into(), vec![]).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "токен неверного вида");
    let (code, e) = enroll("10.1.1.99", "WS-EMP", token.clone(), vec![]).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("address_mismatch")), "чужой адрес: {e}");
    let (code, e) = enroll("10.1.1.50", "WS-GHOST", token.clone(), vec![]).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("unknown_computer")), "{e}");
    let (code, e) = enroll("10.1.1.54", "WS-OFF", token.clone(), vec![]).await;
    assert_eq!(e["error"], "unknown_computer", "отключённый компьютер AD: {code} {e}");
    let (code, e) = enroll("10.1.1.53", "WS-NODNS", token.clone(), vec![]).await;
    assert_eq!(e["error"], "address_mismatch", "без адреса в DNS — нельзя, пока проверка включена: {code} {e}");
    let (code, k) = enroll("10.1.1.50", "ws-emp", token.clone(), vec![(2, "emp", "active")]).await;
    assert_eq!(code, StatusCode::CREATED, "{k}");
    let key_emp = k["agent_key"].as_str().unwrap().to_string();
    let (code, k) = enroll("10.1.1.51", "WS-BUH", token.clone(), vec![(3, "emp2", "active")]).await;
    assert_eq!(code, StatusCode::CREATED, "{k}");
    let key_buh = k["agent_key"].as_str().unwrap().to_string();
    let (code, k) = enroll("10.1.1.52", "WS-SECRET", token.clone(), vec![(4, "emp2", "disconnected")]).await;
    assert_eq!(code, StatusCode::CREATED, "{k}");
    let enrolled = audit(&pool, "remote.device.enrolled").await;
    assert_eq!(enrolled.len(), 3);
    assert!(!enrolled.iter().any(|d| d.to_string().contains(&token)), "токен не попадает в аудит");
    let uses: i32 = sqlx::query_scalar("SELECT uses FROM agent_enrollment_tokens").fetch_one(&pool).await.unwrap();
    assert_eq!(uses, 3, "токен многоразовый, использования считаются");

    // --- Отчёты ---
    let send = |key: String, body: Value| {
        let app = app.clone();
        async move {
            let (code, v, _) =
                call(&app, "10.1.1.50", Method::POST, "/api/agent/v1/report", &bearer(&key), Some(body)).await;
            (code, v)
        }
    };
    let (code, r) =
        send(key_emp.clone(), report("WS-EMP", &[(2, "emp", "active"), (5, "eng1", "disconnected")], "0.5.0")).await;
    assert_eq!(code, StatusCode::OK, "{r}");
    assert_eq!(r["next_report_secs"], 60);
    assert!(r.get("update").is_none(), "обновление не назначено: {r}");
    assert_eq!(r["want_inventory"], true, "сведений об оборудовании ещё нет: {r}");
    let (code, _) = send(format!("pak_{}", "y".repeat(43)), report("WS-EMP", &[], "0.5.0")).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "чужой ключ");
    let (code, _) = send(key_emp.clone(), report("WS-BUH", &[], "0.5.0")).await;
    assert_eq!(code, StatusCode::CONFLICT, "ключ одного ПК не годится для отчёта под другим именем");

    // --- ПК в портале: «Компьютеры» и «где залогинен» ---
    let (code, _) = emp.get("/api/remote/devices").await;
    assert_eq!(code, StatusCode::FORBIDDEN, "сотруднику список ПК не показываем");
    let (code, devices) = eng1.get("/api/remote/devices").await;
    assert_eq!(code, StatusCode::OK, "{devices}");
    let ws = devices.as_array().unwrap().iter().find(|d| d["hostname"] == "WS-EMP").expect("WS-EMP в списке").clone();
    assert_eq!(ws["online"], true, "в сети сразу после отчёта: {ws}");
    assert_eq!(ws["users"], json!(["emp", "eng1"]), "служебная сессия 0 не показывается: {ws}");
    assert_eq!(ws["os_version"], "Windows 11 Pro 23H2 (22631)");
    let ws_emp = id(&ws);
    let (_, found) = eng1.get("/api/remote/sessions?q=Сотрудников").await;
    let f = found.as_array().unwrap();
    assert_eq!(f.len(), 1, "по ФИО: {found}");
    assert_eq!(
        (f[0]["hostname"].as_str(), f[0]["session_id"].as_i64(), f[0]["state"].as_str()),
        (Some("WS-EMP"), Some(2), Some("active"))
    );
    let emp_person = f[0]["person_id"].as_str().unwrap().to_string();
    let (_, found) = eng1.get("/api/remote/sessions?q=emp2").await;
    assert_eq!(found.as_array().unwrap().len(), 2, "emp2 на двух ПК: {found}");
    let (_, mine) = eng1.get(&format!("/api/remote/persons/{emp_person}/sessions")).await;
    assert_eq!(mine.as_array().unwrap().len(), 1, "{mine}");

    // --- Новая заявка с ПК заявителя сама получает имя этого ПК ---
    let (_, ctx) = emp.get("/api/helpdesk/context").await;
    assert_eq!(ctx["computer_name"], "WS-EMP", "форма заявки: ПК из данных агента: {ctx}");
    let (code, c) = emp
        .post(
            "/api/helpdesk/tickets",
            json!({"title": "Не печатает принтер", "site_id": site, "category_id": printers}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{c}");
    let n = c["number"].as_i64().unwrap();
    let (computer, device): (Option<String>, Option<Uuid>) =
        sqlx::query_as("SELECT computer_name, device_id FROM tickets WHERE number = $1")
            .bind(n)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(computer.as_deref(), Some("WS-EMP"), "имя ПК без ввода");
    assert_eq!(device.map(|d| d.to_string()), Some(ws_emp.clone()), "заявка связана с устройством");
    let (_, card) = eng1.get(&format!("/api/remote/devices/{ws_emp}")).await;
    assert!(card["tickets"].as_array().unwrap().iter().any(|t| t["number"] == n), "заявка в карточке ПК: {card}");

    // --- Оборудование и программы: агент присылает по просьбе портала ---
    assert!(card["inventory"].is_null(), "сведений ещё нет: {card}");
    let with_inventory = |software: Value| {
        let mut r = report("WS-EMP", &[(2, "emp", "active")], "0.5.0");
        r["inventory"] = json!({
            "manufacturer": "Dell Inc.", "model": "OptiPlex 7090", "serial_number": "To be filled by O.E.M.",
            "bios_version": "1.2.3", "cpu": "Intel(R) Core(TM) i5-10500 CPU @ 3.10GHz", "memory_mb": 16384,
            "disks": [{"name": "C:", "size_gb": 476, "free_gb": 120}],
            "software": software,
        });
        r
    };
    let (code, r) = send(
        key_emp.clone(),
        with_inventory(json!([
            {"name": "7-Zip 24.08 (x64)", "version": "24.08", "publisher": "Igor Pavlov"},
            {"name": "Google Chrome", "version": "131.0"},
            {"name": "google chrome", "version": "131.0"},
            {"name": "100% Tool_x"}
        ])),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "{r}");
    assert!(r.get("want_inventory").is_none(), "сведения свежие — больше не просим: {r}");
    let (_, card) = eng1.get(&format!("/api/remote/devices/{ws_emp}")).await;
    let inv = &card["inventory"];
    assert_eq!(inv["model"], "OptiPlex 7090", "{card}");
    assert!(inv["serial_number"].is_null(), "заглушка производителя вместо серийного номера отброшена");
    assert_eq!(inv["memory_mb"], 16384);
    assert_eq!(inv["disks"][0]["free_gb"], 120);
    let names: Vec<&str> = inv["software"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["100% Tool_x", "7-Zip 24.08 (x64)", "Google Chrome"], "без повторов, по алфавиту");
    let (code, hits) = eng1.get("/api/remote/software?q=chrome").await;
    assert_eq!(code, StatusCode::OK, "{hits}");
    assert_eq!(hits.as_array().unwrap().len(), 1, "{hits}");
    assert_eq!((hits[0]["hostname"].as_str(), hits[0]["version"].as_str()), (Some("WS-EMP"), Some("131.0")));
    let (_, hits) = eng1.get("/api/remote/software?q=00%25").await;
    assert_eq!(hits.as_array().unwrap().len(), 1, "% ищется как символ: {hits}");
    let (_, hits) = eng1.get("/api/remote/software?q=ol_").await;
    assert_eq!(hits.as_array().unwrap().len(), 1, "_ ищется как символ: {hits}");
    let (code, _) = eng1.get("/api/remote/software?q=ch").await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "слишком короткий запрос");
    let (code, _) = emp.get("/api/remote/software?q=chrome").await;
    assert_eq!(code, StatusCode::FORBIDDEN, "сотруднику — нет");
    // Через сутки портал просит снова; новый список целиком заменяет прежний.
    sqlx::query("UPDATE device_inventory SET collected_at = now() - interval '25 hours'").execute(&pool).await.unwrap();
    let (_, r) = send(key_emp.clone(), report("WS-EMP", &[(2, "emp", "active")], "0.5.0")).await;
    assert_eq!(r["want_inventory"], true, "сведения устарели: {r}");
    let (code, _) = send(key_emp.clone(), with_inventory(json!([{"name": "Google Chrome", "version": "132.0"}]))).await;
    assert_eq!(code, StatusCode::OK);
    let (_, hits) = eng1.get("/api/remote/software?q=chrome").await;
    assert_eq!(hits[0]["version"], "132.0", "{hits}");
    let (_, hits) = eng1.get("/api/remote/software?q=zip").await;
    assert_eq!(hits.as_array().unwrap().len(), 0, "удалённая программа пропала: {hits}");
    // Заявку за сотрудника создаёт инженер: адрес запроса — инженера, ПК не подставляется
    // (иначе подделанная агентом сессия привязала бы к заявке чужой компьютер).
    let (code, c) = eng1
        .post(
            "/api/helpdesk/tickets",
            json!({"title": "Звонок: не печатает", "site_id": site, "category_id": printers, "on_behalf_of": emp_person}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{c}");
    let by_phone: Option<String> = sqlx::query_scalar("SELECT computer_name FROM tickets WHERE number = $1")
        .bind(c["number"].as_i64().unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(by_phone, None, "ПК подставляется только по адресу самого заявителя");
    // Имя ПК, введённое самим заявителем, связывает заявку с устройством, только если агент видит
    // заявителя на этом ПК: иначе сотрудник вписал бы чужой ПК (сервер), и быстрые действия инженера
    // из заявки (C$, «Управление компьютером», PowerShell) ушли бы туда.
    let device_of = |n: i64| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<Uuid>>("SELECT device_id FROM tickets WHERE number = $1")
                .bind(n)
                .fetch_one(&pool)
                .await
                .unwrap()
                .map(|d| d.to_string())
        }
    };
    let typed = |computer: &'static str| json!({"title": "Не открывается папка", "site_id": site, "category_id": printers, "computer_name": computer});
    let (code, c) = emp.post("/api/helpdesk/tickets", typed("WS-BUH")).await;
    assert_eq!(code, StatusCode::CREATED, "{c}");
    let foreign = c["number"].as_i64().unwrap();
    assert_eq!(device_of(foreign).await, None, "чужой ПК из поля заявителя не связывается с устройством");
    let (code, e) = eng1.post("/api/remote/connections", json!({"ticket_number": foreign, "action": "probe"})).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("no_device")), "{e}");
    let (code, c) = emp.post("/api/helpdesk/tickets", typed("ws-emp.pult.test")).await;
    assert_eq!(code, StatusCode::CREATED, "{c}");
    assert_eq!(device_of(c["number"].as_i64().unwrap()).await, Some(ws_emp.clone()), "свой ПК — связывается");
    // Инженер (за сотрудника) указывает ПК сам — ему доверяем.
    let (code, c) = eng1
        .post(
            "/api/helpdesk/tickets",
            json!({"title": "Звонок: папка", "site_id": site, "category_id": printers, "on_behalf_of": emp_person,
                   "computer_name": "WS-BUH"}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{c}");
    assert!(device_of(c["number"].as_i64().unwrap()).await.is_some(), "ПК, указанный инженером, связывается");

    // --- Политика из policy.json ---
    let policy = json!({"Domain": "PULT", "ConsentRequiredComputers": ["ws-buh", "bad name!"], "DeniedComputers": ["WS-SECRET"],
                        "FullAccessGroups": ["Domain Admins"], "DefaultAuthenticatedRole": "Viewer"});
    let (code, _) = eng2.post("/api/admin/remote/policies/import", policy.clone()).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "политику меняет администратор");
    let (code, r) = admin.post("/api/admin/remote/policies/import", policy.clone()).await;
    assert_eq!(code, StatusCode::OK, "{r}");
    assert_eq!((r["added"].as_u64(), r["invalid"].as_array().unwrap().len()), (Some(2), 1), "{r}");
    let (_, r) = admin.post("/api/admin/remote/policies/import", policy).await;
    assert_eq!(r["skipped"], 2, "повторный импорт ничего не дублирует: {r}");

    // --- Из карточки заявки одним нажатием: просмотр с запросом согласия ---
    let (code, conn) = eng1.post("/api/remote/connections", json!({"ticket_number": n, "action": "shadow"})).await;
    assert_eq!(code, StatusCode::CREATED, "{conn}");
    assert_eq!(
        (conn["mode"].as_str(), conn["consent"].as_str(), conn["session_id"].as_i64(), conn["target_user"].as_str()),
        (Some("view"), Some("prompted"), Some(2), Some("emp")),
        "сессия заявителя на его ПК, просмотр, согласие: {conn}"
    );
    let link = conn["link"].as_str().unwrap();
    let t1 = link.strip_prefix("pult://connect?t=").expect("ссылка для helper").to_string();
    assert_eq!(pult_helper::parse_link(link).unwrap(), t1, "helper принимает ссылку");
    let helper = |path: &'static str, body: Value| {
        let app = app.clone();
        async move {
            let (code, v, _) = call(&app, "10.1.1.11", Method::POST, path, &[], Some(body)).await;
            (code, v)
        }
    };
    let (code, p) = helper("/api/remote/v1/exchange", json!({"token": t1})).await;
    assert_eq!(code, StatusCode::OK, "{p}");
    assert_eq!(
        (
            p["action"].as_str(),
            p["computer"].as_str(),
            p["session_id"].as_i64(),
            p["mode"].as_str(),
            p["consent_required"].as_bool()
        ),
        (Some("shadow"), Some("ws-emp.pult.test"), Some(2), Some("view"), Some(true)),
        "{p}"
    );
    let (code, e) = helper("/api/remote/v1/exchange", json!({"token": t1})).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("token_invalid")), "токен одноразовый");
    let (code, _) = helper("/api/remote/v1/result", json!({"token": t1, "ok": true})).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let (code, _) = helper("/api/remote/v1/result", json!({"token": t1, "ok": false, "error": "x"})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "итог сообщается один раз");
    let (_, st1) = eng1.get(&format!("/api/remote/connections/{}", id(&conn))).await;
    assert_eq!(st1["status"], "started", "{st1}");
    let (code, _) = eng2.get(&format!("/api/remote/connections/{}", id(&conn))).await;
    assert_eq!(code, StatusCode::NOT_FOUND, "чужое подключение не видно");
    let notes: Vec<String> = sqlx::query_scalar(
        "SELECT c.body FROM ticket_comments c JOIN tickets t ON t.id = c.ticket_id WHERE t.number = $1 AND c.visibility = 'internal'",
    )
    .bind(n)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(notes.iter().any(|b| b.contains("WS-EMP") && b.contains("просмотр")), "заметка в заявке: {notes:?}");

    // Просмотр всегда с согласием, даже если попросить без; управление без права — нельзя.
    let (_, c2) = eng1
        .post(
            "/api/remote/connections",
            json!({"ticket_number": n, "action": "shadow", "mode": "view", "skip_consent": true}),
        )
        .await;
    assert_eq!(c2["consent"], "prompted", "{c2}");
    let (code, e) = eng1
        .post(
            "/api/remote/connections",
            json!({"device_id": ws_emp, "action": "shadow", "session_id": 2, "mode": "control"}),
        )
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN, "управление — только с remote.connect.control: {e}");
    let (code, e) = eng1.post("/api/remote/connections", json!({"device_id": ws_emp, "action": "files"})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "C$ — как управление: {e}");
    let (code, e) =
        emp.post("/api/remote/connections", json!({"device_id": ws_emp, "action": "shadow", "session_id": 2})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "сотрудник не подключается: {e}");
    let (code, e) =
        eng1.post("/api/remote/connections", json!({"device_id": ws_emp, "action": "shadow", "session_id": 99})).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("session_not_found")), "{e}");

    // Управление без запроса — там, где политика позволяет.
    let (code, c3) = eng2
        .post(
            "/api/remote/connections",
            json!({"device_id": ws_emp, "action": "shadow", "session_id": 2, "mode": "control", "skip_consent": true}),
        )
        .await;
    assert_eq!((code, c3["consent"].as_str()), (StatusCode::CREATED, Some("skipped")), "{c3}");
    let t3 = c3["link"].as_str().unwrap().strip_prefix("pult://connect?t=").unwrap().to_string();
    let (_, p3) = helper("/api/remote/v1/exchange", json!({"token": t3})).await;
    assert_eq!((p3["mode"].as_str(), p3["consent_required"].as_bool()), (Some("control"), Some(false)), "{p3}");
    let (_, _) =
        helper("/api/remote/v1/result", json!({"token": t3, "ok": false, "error": "Отказано в доступе"})).await;

    // --- ПК из списка «обязательное согласие»: без запроса нельзя даже с правом управления ---
    let ws_buh = devices.as_array().unwrap().iter().find(|d| d["hostname"] == "WS-BUH").map(id).unwrap();
    // Владелец ПК подменил dNSHostName в AD, а агент сообщает своё DNS-имя: подключаться helper
    // всё равно будет к имени ПК в домене из настроек Pult.
    sqlx::query("UPDATE ad_computers SET dns_host_name = 'ws-buh.attacker.example' WHERE name = 'WS-BUH'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE devices SET fqdn = 'evil.attacker.example' WHERE hostname = 'WS-BUH'")
        .execute(&pool)
        .await
        .unwrap();
    let (code, c4) = eng2
        .post(
            "/api/remote/connections",
            json!({"device_id": ws_buh, "action": "shadow", "session_id": 3, "mode": "control", "skip_consent": true}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{c4}");
    assert_eq!((c4["mode"].as_str(), c4["consent"].as_str()), (Some("control"), Some("prompted")), "{c4}");
    let t4 = c4["link"].as_str().unwrap().strip_prefix("pult://connect?t=").unwrap().to_string();
    let (_, p4) = helper("/api/remote/v1/exchange", json!({"token": t4})).await;
    assert_eq!(p4["consent_required"], true, "helper получает требование согласия: {p4}");
    assert_eq!(p4["computer"], "ws-buh.pult.test", "цель — имя из AD и домен Pult: {p4}");
    // На ПК «только с согласием» нет действий без ведома пользователя (файлы, управление, PowerShell).
    for action in ["files", "manage", "powershell"] {
        let (code, e) = eng2.post("/api/remote/connections", json!({"device_id": ws_buh, "action": action})).await;
        assert_eq!((code, e["error"].as_str()), (StatusCode::FORBIDDEN, Some("consent_only")), "{action}: {e}");
    }
    let (code, e) = eng2.post("/api/remote/connections", json!({"device_id": ws_buh, "action": "probe"})).await;
    assert_eq!(code, StatusCode::CREATED, "проверка доступности не трогает сессию: {e}");
    let args = pult_helper::mstsc_args(&pult_helper::ConnectInfo {
        computer: p4["computer"].as_str().unwrap().into(),
        session_id: p4["session_id"].as_u64().unwrap() as u32,
        mode: pult_helper::Mode::Control,
        consent_required: p4["consent_required"].as_bool().unwrap(),
        prompt_credentials: false,
    })
    .unwrap();
    assert!(!args.iter().any(|a| a == "/noConsentPrompt"), "{args:?}");

    // --- Запрещённый ПК: нельзя совсем ---
    let ws_secret = devices.as_array().unwrap().iter().find(|d| d["hostname"] == "WS-SECRET").map(id).unwrap();
    for body in [
        json!({"device_id": ws_secret, "action": "shadow", "session_id": 4}),
        json!({"device_id": ws_secret, "action": "shadow", "session_id": 4, "mode": "control"}),
        json!({"device_id": ws_secret, "action": "files"}),
        json!({"device_id": ws_secret, "action": "probe"}),
    ] {
        let (code, e) = eng2.post("/api/remote/connections", body).await;
        assert_eq!((code, e["error"].as_str()), (StatusCode::FORBIDDEN, Some("device_denied")), "{e}");
    }
    let denied = audit(&pool, "remote.connect.denied").await;
    assert!(denied.iter().filter(|d| d["reason"] == "device_denied").count() >= 4, "отказы — в аудите: {denied:?}");
    let issued: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_connections WHERE device_id = $1::uuid")
        .bind(&ws_secret)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(issued, 0, "ссылки на запрещённый ПК не выдаются");

    // --- Каждое подключение — в аудите ---
    let requested = audit(&pool, "remote.connect.requested").await;
    let first = &requested[0];
    assert!(
        requested.iter().filter(|r| r["ticket"].is_null()).all(|r| r["requester_session"].is_null()),
        "без заявки нет и заявителя"
    );
    assert_eq!(
        (
            first["hostname"].as_str(),
            first["user"].as_str(),
            first["mode"].as_str(),
            first["consent"].as_str(),
            first["ticket"].as_i64(),
            first["session"].as_i64()
        ),
        (Some("WS-EMP"), Some("emp"), Some("view"), Some("prompted"), Some(n), Some(2)),
        "{first}"
    );
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM remote_connections").fetch_one(&pool).await.unwrap();
    assert_eq!(requested.len() as i64, total, "каждая выданная ссылка — запись аудита");
    let started = audit(&pool, "remote.connect.started").await;
    assert_eq!((started.len(), started[0]["result"].is_null()), (1, true));
    let failed = audit(&pool, "remote.connect.failed").await;
    assert_eq!(failed[0]["error"], "Отказано в доступе");
    let opened: Option<Uuid> =
        sqlx::query_scalar("SELECT actor_id FROM audit_log WHERE action = 'remote.connect.opened' ORDER BY id LIMIT 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(opened, Some(u1), "в аудите helper — от имени инженера");
    assert!(
        !audit(&pool, "remote.connect.opened").await.iter().any(|d| d.to_string().contains("pc_")),
        "токен не в аудите"
    );

    // --- Срок токена: 60 с ---
    let (_, c5) = eng1.post("/api/remote/connections", json!({"ticket_number": n, "action": "shadow"})).await;
    sqlx::query("UPDATE remote_connections SET created_at = now() - interval '61 seconds' WHERE id = $1::uuid")
        .bind(id(&c5))
        .execute(&pool)
        .await
        .unwrap();
    let t5 = c5["link"].as_str().unwrap().strip_prefix("pult://connect?t=").unwrap().to_string();
    let (code, _) = helper("/api/remote/v1/exchange", json!({"token": t5})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "просроченная ссылка не работает");
    let (_, st5) = eng1.get(&format!("/api/remote/connections/{}", id(&c5))).await;
    assert_eq!(st5["status"], "expired", "{st5}");

    // --- Быстрые действия ---
    let (code, q) = eng2.post("/api/remote/connections", json!({"device_id": ws_emp, "action": "files"})).await;
    assert_eq!(code, StatusCode::CREATED, "{q}");
    let tq = q["link"].as_str().unwrap().strip_prefix("pult://connect?t=").unwrap().to_string();
    let (_, pq) = helper("/api/remote/v1/exchange", json!({"token": tq})).await;
    assert_eq!((pq["action"].as_str(), pq.get("session_id")), (Some("files"), None), "{pq}");
    let (code, pr) = eng1.post("/api/remote/connections", json!({"device_id": ws_emp, "action": "probe"})).await;
    assert_eq!(code, StatusCode::CREATED, "проверка доступности — инженеру с просмотром: {pr}");
    let tp = pr["link"].as_str().unwrap().strip_prefix("pult://connect?t=").unwrap().to_string();
    helper("/api/remote/v1/exchange", json!({"token": tp})).await;
    let (code, _) = helper(
        "/api/remote/v1/result",
        json!({"token": tp, "ok": true, "details": {"ping": true, "3389": false, "445": true, "5985": true}}),
    )
    .await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let (_, sp) = eng1.get(&format!("/api/remote/connections/{}", id(&pr))).await;
    assert_eq!(sp["result"]["445"], true, "{sp}");

    // --- ПК не на связи: к сессии не подключиться ---
    sqlx::query("UPDATE devices SET last_seen_at = now() - interval '1 hour' WHERE hostname = 'WS-BUH'")
        .execute(&pool)
        .await
        .unwrap();
    let (code, e) =
        eng2.post("/api/remote/connections", json!({"device_id": ws_buh, "action": "shadow", "session_id": 3})).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("device_offline")), "{e}");
    let (code, _, _) = call(
        &app,
        "10.1.1.51",
        Method::POST,
        "/api/agent/v1/report",
        &bearer(&key_buh),
        Some(report("WS-BUH", &[(3, "emp2", "active")], "0.5.0")),
    )
    .await;
    assert_eq!(code, StatusCode::OK, "отчёт вернул ПК в сеть");

    // --- Отключение агента администратором ---
    let (code, _) = eng2.post(&format!("/api/admin/remote/devices/{ws_emp}/deactivate"), json!({})).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let (code, _) = admin.post(&format!("/api/admin/remote/devices/{ws_emp}/deactivate"), json!({})).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let (code, _) = send(key_emp.clone(), report("WS-EMP", &[(2, "emp", "active")], "0.5.0")).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "ключ отключённого ПК не действует");
    let (code, e) =
        eng1.post("/api/remote/connections", json!({"device_id": ws_emp, "action": "shadow", "session_id": 2})).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("device_inactive")), "{e}");
    let (code, e) = enroll("10.1.1.50", "WS-EMP", token.clone(), vec![(2, "emp", "active")]).await;
    assert_eq!(
        (code, e["error"].as_str()),
        (StatusCode::FORBIDDEN, Some("device_deactivated")),
        "отключённый ПК сам не возвращается: {e}"
    );
    let (code, _) = eng2.post(&format!("/api/admin/remote/devices/{ws_emp}/activate"), json!({})).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let (code, _) = admin.post(&format!("/api/admin/remote/devices/{ws_emp}/activate"), json!({})).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let (code, _) = send(key_emp.clone(), report("WS-EMP", &[(2, "emp", "active")], "0.5.0")).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "включение не возвращает прежний ключ");
    let (code, k) = enroll("10.1.1.50", "WS-EMP", token.clone(), vec![(2, "emp", "active")]).await;
    assert_eq!(code, StatusCode::CREATED, "после включения — новая регистрация: {k}");
    let key_emp2 = k["agent_key"].as_str().unwrap().to_string();
    let (code, _) = send(key_emp.clone(), report("WS-EMP", &[], "0.5.0")).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "старый ключ больше не действует");
    let reenrolled = audit(&pool, "remote.device.enrolled").await;
    assert_eq!(reenrolled.last().unwrap()["re_enrolled"], true);
    // Агент Linux передаёт сеансы и оборудование, но подключение к такому ПК не предлагается.
    sqlx::query("UPDATE devices SET os = 'linux-x86_64' WHERE id = $1::uuid")
        .bind(&ws_emp)
        .execute(&pool)
        .await
        .unwrap();
    let (code, e) =
        eng1.post("/api/remote/connections", json!({"device_id": ws_emp, "action": "shadow", "session_id": 2})).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("unsupported_os")), "{e}");
    sqlx::query("UPDATE devices SET os = 'windows-x86_64' WHERE id = $1::uuid")
        .bind(&ws_emp)
        .execute(&pool)
        .await
        .unwrap();

    // --- Пакеты: helper для инженеров, MSI и обновление — администратору ---
    let helper_exe = b"MZ fake helper".to_vec();
    let (code, up, _) = admin
        .raw(
            Method::POST,
            "/api/admin/remote/packages?kind=helper&version=0.5.0&file_name=pult-helper.exe",
            None,
            Some(helper_exe.clone()),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{up}");
    let (code, _, bytes) = eng1.raw(Method::GET, "/api/remote/packages/helper/download", None, None).await;
    assert_eq!((code, bytes), (StatusCode::OK, helper_exe), "инженер скачивает helper");
    let (code, _, _) = emp.raw(Method::GET, "/api/remote/packages/helper/download", None, None).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let (code, _, _) = eng1.raw(Method::GET, "/api/remote/packages/agent_msi/download", None, None).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "MSI — администратору");
    let agent_exe = b"MZ fake agent 0.6.0".to_vec();
    let (code, e, _) = admin
        .raw(
            Method::POST,
            "/api/admin/remote/packages?kind=agent_update&version=0.6.0&file_name=pult-agent.exe",
            None,
            Some(agent_exe.clone()),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "обновление без подписи не принимается: {e}");
    let sig = STANDARD.encode([7u8; 64]);
    let (code, pkg, _) = admin
        .raw(
            Method::POST,
            &format!(
                "/api/admin/remote/packages?kind=agent_update&version=0.6.0&file_name=pult-agent.exe&signature={}",
                sig.replace('+', "%2B").replace('/', "%2F").replace('=', "%3D")
            ),
            None,
            Some(agent_exe.clone()),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{pkg}");
    let pkg = id(&pkg);
    let (code, _) = admin.post(&format!("/api/admin/remote/packages/{pkg}/current"), json!({})).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let (_, r) = send(key_emp2.clone(), report("WS-EMP", &[(2, "emp", "active")], "0.5.0")).await;
    let upd = &r["update"];
    assert_eq!((upd["version"].as_str(), upd["package_id"].as_str()), (Some("0.6.0"), Some(pkg.as_str())), "{r}");
    let sha: String = pult_core::crypto::sha256(&agent_exe).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!((upd["sha256"].as_str(), upd["signature"].as_str()), (Some(sha.as_str()), Some(sig.as_str())));
    let (code, _, data) =
        call(&app, "10.1.1.50", Method::GET, &format!("/api/agent/v1/update/{pkg}"), &bearer(&key_emp2), None).await;
    assert_eq!((code, data), (StatusCode::OK, agent_exe.clone()), "агент скачивает назначенное обновление");
    let (code, _, data) =
        call(&app, "10.1.1.50", Method::GET, &format!("/api/agent/v1/update/{pkg}"), &bearer(&key_emp2), None).await;
    assert_eq!((code, data), (StatusCode::OK, agent_exe), "повторно — та же копия пакета");
    let (code, _, _) = call(&app, "10.1.1.50", Method::GET, &format!("/api/agent/v1/update/{pkg}"), &[], None).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "без ключа агента — нет");
    let (_, r) = send(key_emp2.clone(), report("WS-EMP", &[(2, "emp", "active")], "0.6.0")).await;
    assert!(r.get("update").is_none(), "обновлённый агент обновление больше не получает: {r}");

    // --- Настройка: без проверки по DNS ---
    let (code, cfg) = admin.get("/api/admin/remote/settings").await;
    assert_eq!((code, cfg["require_dns_match"].as_bool()), (StatusCode::OK, Some(true)), "по умолчанию включена");
    let mut cfg = cfg;
    cfg["require_dns_match"] = json!(false);
    let (code, _) = admin.put("/api/admin/remote/settings", cfg).await;
    assert_eq!(code, StatusCode::OK);
    let (code, k) = enroll("10.1.1.53", "WS-NODNS", token.clone(), vec![]).await;
    assert_eq!(code, StatusCode::CREATED, "{k}");

    // --- Отозванный токен ---
    let tid = id(&list[0]);
    let (code, _) = admin.post(&format!("/api/admin/remote/tokens/{tid}/revoke"), json!({})).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let (code, _) = enroll("10.1.1.51", "WS-BUH", token.clone(), vec![]).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "отозванным токеном не зарегистрироваться");

    // --- Заявитель без карточки в справочнике (например, отдельная админская учётка):
    // сессия заявителя — по учётной записи AD его пользователя Pult ---
    let emp_user = user_of("emp").await;
    sqlx::query("UPDATE tickets SET requester_id = NULL, requester_user_id = $2 WHERE number = $1")
        .bind(n)
        .bind(emp_user)
        .execute(&pool)
        .await
        .unwrap();
    let (code, conn) = eng1.post("/api/remote/connections", json!({"ticket_number": n, "action": "shadow"})).await;
    assert_eq!(code, StatusCode::CREATED, "{conn}");
    assert_eq!(
        (conn["session_id"].as_i64(), conn["target_user"].as_str(), conn["consent"].as_str()),
        (Some(2), Some("emp"), Some("prompted")),
        "{conn}"
    );
    let requested = audit(&pool, "remote.connect.requested").await;
    assert_eq!(requested.last().unwrap()["requester_session"], true, "в аудите: сессия заявителя");
    for a in [
        "remote.token.created",
        "remote.token.revoked",
        "remote.policy.imported",
        "remote.device.deactivated",
        "remote.device.activated",
        "remote.package.uploaded",
        "remote.package.current",
        "remote.settings.updated",
    ] {
        assert!(!audit(&pool, a).await.is_empty(), "аудит {a}");
    }
}
