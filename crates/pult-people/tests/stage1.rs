//! Сквозной сценарий входа, сеансов и справочника на настоящей PostgreSQL и поддельных DC по LDAPS:
//! вход паролем AD, CSRF, лимит попыток, первый суперадминистратор, синхронизация и связывание,
//! поиск с исправлением раскладки и по номеру, правка с конфликтом версий и аудитом, права,
//! отключение учётки в AD → нет доступа и запись в архиве, целостность журнала аудита.
//!
//! Нужен `PULT_TEST_DATABASE_URL` (в CI задан). Без него тест пропускается.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, FromRef},
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use pult_core::{Core, Module, Secret, api::CoreModule, crypto::SecretBox, session::CookiePolicy};
use pult_people::{PeopleModule, PeopleState};
use pult_phonebook::api::PhonebookModule;
use pult_testkit::{BASE_DN, Directory, FakeDc, FakeUser, SVC_DN, SVC_PASS, TestCa, USER_FILTER, USER_PASS};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tower::ServiceExt;

#[derive(Clone)]
struct St {
    core: Core,
    people: PeopleState,
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

fn app(st: St) -> Router {
    let (router, _) = utoipa_axum::router::OpenApiRouter::new()
        .merge(<CoreModule as Module<St>>::routes(&CoreModule))
        .merge(<PeopleModule as Module<St>>::routes(&PeopleModule))
        .merge(<PhonebookModule as Module<St>>::routes(&PhonebookModule))
        .split_for_parts();
    router.with_state(st)
}

fn permissions() -> Vec<pult_core::Permission> {
    let mut p = <CoreModule as Module<St>>::permissions(&CoreModule);
    p.extend(<PeopleModule as Module<St>>::permissions(&PeopleModule));
    p.extend(<PhonebookModule as Module<St>>::permissions(&PhonebookModule));
    p
}

/// Браузер: хранит cookie и отправляет CSRF-токен.
struct Browser {
    app: Router,
    cookies: HashMap<String, String>,
    /// Адрес клиента: по умолчанию из сети организации.
    ip: String,
}

impl Browser {
    fn new(app: Router) -> Self {
        Self { app, cookies: HashMap::new(), ip: "10.1.1.1:5000".into() }
    }

    async fn call(&mut self, method: Method, uri: &str, body: Option<Value>, csrf: bool) -> (StatusCode, Value) {
        let mut req = Request::builder().method(method).uri(uri);
        if !self.cookies.is_empty() {
            let c = self.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
            req = req.header(header::COOKIE, c);
        }
        if csrf && let Some(t) = self.cookies.get("__Host-pult_csrf") {
            req = req.header("x-csrf-token", t.clone());
        }
        let req = match body {
            Some(b) => req.header(header::CONTENT_TYPE, "application/json").body(Body::from(b.to_string())).unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        let mut req = req;
        req.extensions_mut().insert(ConnectInfo(self.ip.parse::<SocketAddr>().unwrap()));
        let resp = self.app.clone().oneshot(req).await.unwrap();
        for v in resp.headers().get_all(header::SET_COOKIE) {
            let s = v.to_str().unwrap();
            let (kv, _) = s.split_once(';').unwrap_or((s, ""));
            let (k, v) = kv.split_once('=').unwrap();
            if v.is_empty() {
                self.cookies.remove(k);
            } else {
                self.cookies.insert(k.to_string(), v.to_string());
            }
        }
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
        };
        (status, json)
    }

    async fn get(&mut self, uri: &str) -> (StatusCode, Value) {
        self.call(Method::GET, uri, None, false).await
    }

    async fn post(&mut self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.call(Method::POST, uri, Some(body), true).await
    }

    async fn put(&mut self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.call(Method::PUT, uri, Some(body), true).await
    }

    async fn delete(&mut self, uri: &str) -> (StatusCode, Value) {
        self.call(Method::DELETE, uri, None, true).await
    }

    async fn login(&mut self, login: &str, password: &str) -> (StatusCode, Value) {
        self.call(Method::POST, "/api/auth/login", Some(json!({"login": login, "password": password})), false).await
    }

    async fn login_code(&mut self, login: &str, password: &str, code: &str) -> (StatusCode, Value) {
        let body = json!({"login": login, "password": password, "code": code});
        self.call(Method::POST, "/api/auth/login", Some(body), false).await
    }
}

async fn db() -> Option<PgPool> {
    let Ok(url) = std::env::var("PULT_TEST_DATABASE_URL") else {
        // В CI база есть всегда: пропуск там означал бы, что приёмка этапа молча не проверялась.
        assert!(std::env::var("CI").is_err(), "в CI нужен PULT_TEST_DATABASE_URL");
        eprintln!("PULT_TEST_DATABASE_URL не задан — интеграционный тест пропущен");
        return None;
    };
    let pool = PgPool::connect(&url).await.expect("подключение к тестовой БД");
    sqlx::migrate!("../../migrations").run(&pool).await.expect("миграции");
    Some(pool)
}

fn names(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|p| format!("{} {}", p["last_name"].as_str().unwrap(), p["first_name"].as_str().unwrap()))
        .collect()
}

#[tokio::test]
async fn stage1_acceptance() {
    let Some(pool) = db().await else { return };
    let key = Secret::new("test-key-test-key-test-key-test-key-0");
    let core = Core {
        db: pool.clone(),
        secrets: Arc::new(SecretBox::new(&key)),
        cookies: CookiePolicy { secure: true },
        events: pult_core::events::Hub::default(),
        insecure_mail: false,
        org: Default::default(),
        permissions: Arc::new(permissions()),
    };
    pult_core::rbac::seed_defaults(&pool, &core.permissions).await.unwrap();

    // Каталог: трое сотрудников, служебная учётка без ФИО, уволенный (отключён с самого начала).
    let mut svc = FakeUser::person("svc-backup", "", "", 4);
    svc.sn = None;
    svc.given = None;
    svc.display = "svc-backup".into();
    // У Петровой начало UPN не совпадает с логином — так бывает в доменах.
    let mut petrova = FakeUser::person("petrova", "Петрова", "Анна", 2);
    petrova.upn = Some("anna.petrova@pult.test".into());
    let mut gone = FakeUser::person("uvolen", "Уволенный", "Олег", 5);
    gone.uac = 514;
    gone.status = Some("533");
    let dir = Directory::new(vec![
        FakeUser::person("ivanov", "Иванов", "Иван", 1),
        petrova,
        FakeUser::person("sidorov", "Сидоров", "Пётр", 3),
        svc,
        gone,
    ]);
    let ca = TestCa::new("Pult Test Domain CA");
    let (dc1, dc2) = (FakeDc::start(&ca, dir.clone()).await, FakeDc::start(&ca, dir.clone()).await);
    let sealed = core.secrets.encrypt(&Secret::new(SVC_PASS)).unwrap();
    sqlx::query(
        "INSERT INTO ad_domains (dns_name, base_dn, controllers, tls_mode, bind_dn, bind_secret, ca_cert_pem, user_filter, timeout_secs)
         VALUES ('pult.test', $1, $2, 'ldaps', $3, $4, $5, $6, 2)
         ON CONFLICT (dns_name) DO UPDATE SET controllers = EXCLUDED.controllers, bind_secret = EXCLUDED.bind_secret,
                ca_cert_pem = EXCLUDED.ca_cert_pem, sync_state = '{}', enabled = true, version = ad_domains.version + 1",
    )
    .bind(BASE_DN)
    .bind(vec![dc1.endpoint(), dc2.endpoint()])
    .bind(SVC_DN)
    .bind(sealed)
    .bind(ca.pem())
    .bind(USER_FILTER)
    .execute(&pool)
    .await
    .unwrap();

    let st = St {
        core: core.clone(),
        people: PeopleState { ad: Default::default(), initial_superadmin: Some("PULT\\ivanov".into()) },
    };
    let app = app(st);

    // --- Вход ---
    let mut admin = Browser::new(app.clone());
    let (code, body) = admin.login("ivanov", "wrong-password").await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["error"], "invalid_credentials");
    assert!(!body.to_string().contains("wrong-password"), "пароль не возвращается в ответе");

    let (code, body) = admin.login("PULT\\ivanov", USER_PASS).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{body}");
    assert!(admin.cookies.contains_key("__Host-pult_session") && admin.cookies.contains_key("__Host-pult_csrf"));
    let (code, me) = admin.get("/api/me").await;
    assert_eq!(code, StatusCode::OK, "{me}");
    assert_eq!(me["login"], "ivanov");
    assert_eq!(me["is_superadmin"], true, "первый суперадминистратор из PULT_INITIAL_SUPERADMIN: {me}");

    // CSRF: изменяющий запрос без токена отклоняется.
    let (code, body) = admin.call(Method::POST, "/api/notifications/read", Some(json!({"ids": []})), false).await;
    assert_eq!((code, body["error"].as_str()), (StatusCode::FORBIDDEN, Some("csrf")));
    let (code, _) = admin.post("/api/notifications/read", json!({"ids": []})).await;
    assert_eq!(code, StatusCode::NO_CONTENT);

    // Отключённая учётка: понятный ответ.
    let (code, body) = Browser::new(app.clone()).login("uvolen", USER_PASS).await;
    assert_eq!((code, body["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("account_disabled")), "{body}");

    // Лимит попыток: 5 неудач на логин — дальше 429.
    let ghost = format!("ghost{}", std::process::id());
    let mut b = Browser::new(app.clone());
    for _ in 0..5 {
        assert_eq!(b.login(&ghost, "x").await.0, StatusCode::UNAUTHORIZED);
    }
    let (code, body) = b.login(&ghost, "x").await;
    assert_eq!(code, StatusCode::TOO_MANY_REQUESTS, "{body}");

    // --- Синхронизация и связывание ---
    let (code, s) = admin.post("/api/admin/ad/sync", json!({"kind": "full"})).await;
    assert_eq!(code, StatusCode::OK, "{s}");
    assert_eq!(s["kind"], "full");
    let (_, all) = admin.get("/api/phonebook/persons").await;
    let n = names(&all);
    for who in ["Иванов Иван", "Петрова Анна", "Сидоров Пётр"] {
        assert!(n.contains(&who.to_string()), "{who} нет в справочнике: {n:?}");
    }
    assert!(!n.iter().any(|x| x.contains("svc-backup") || x.contains("Уволенный")), "{n:?}");

    // --- Поиск ---
    let (_, r) = admin.get("/api/phonebook/persons?q=bdfyjd").await;
    assert!(names(&r).contains(&"Иванов Иван".to_string()), "раскладка: {r}");
    let (_, r) = admin.get("/api/phonebook/persons?q=%D0%BF%D0%B5%D1%82%D1%80").await; // «петр» находит «Пётр»
    assert!(names(&r).contains(&"Сидоров Пётр".to_string()), "ё = е: {r}");

    // --- Правка редактором (суперадминистратор — тоже редактор) ---
    let (code, created) = admin
        .post(
            "/api/phonebook/persons",
            json!({"person": {"last_name": "Тестов", "first_name": "Тест", "work_phone": "45-12"}}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{created}");
    assert_eq!(created["internal_ext"], "4512");
    let id = created["id"].as_str().unwrap().to_string();
    let (_, r) = admin.get("/api/phonebook/persons?q=4512").await;
    assert!(names(&r).contains(&"Тестов Тест".to_string()), "по номеру: {r}");

    let v = created["version"].as_i64().unwrap();
    let edit = |version: i64, room: &str| json!({"version": version, "person": {"last_name": "Тестов", "first_name": "Тест", "work_phone": "45-12", "room": room}});
    let (code, upd) = admin.put(&format!("/api/phonebook/persons/{id}"), edit(v, "101")).await;
    assert_eq!(code, StatusCode::OK, "{upd}");
    let (code, conflict) = admin.put(&format!("/api/phonebook/persons/{id}"), edit(v, "102")).await;
    assert_eq!(code, StatusCode::CONFLICT, "устаревшая версия: {conflict}");
    assert_eq!(conflict["details"]["room"], "101", "в ответе — текущая запись");

    // Правку видит другой пользователь.
    let mut emp = Browser::new(app.clone());
    assert_eq!(emp.login("sidorov", USER_PASS).await.0, StatusCode::NO_CONTENT);
    let (code, card) = emp.get(&format!("/api/phonebook/persons/{id}")).await;
    assert_eq!((code, card["room"].as_str()), (StatusCode::OK, Some("101")));

    // И она есть в аудите с «было/стало».
    let (code, audit) = admin.get(&format!("/api/admin/audit?action=person.update&object_id={id}")).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(audit[0]["details"]["room"], json!({"before": null, "after": "101"}), "{audit}");

    // Поле из AD у связанного сотрудника в портале не меняется.
    let ivanov = names(&all).iter().position(|x| x == "Иванов Иван").unwrap();
    let iv = &all[ivanov];
    let (code, body) = admin
        .put(
            &format!("/api/phonebook/persons/{}", iv["id"].as_str().unwrap()),
            json!({"version": iv["version"], "person": {"last_name": "Иваненко", "first_name": "Иван", "email": iv["email"]}}),
        )
        .await;
    assert_eq!((code, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("field_from_ad")), "{body}");

    // --- Права: сотрудник не правит справочник и не видит аудит ---
    let (code, _) = emp.post("/api/phonebook/persons", json!({"person": {"last_name": "X", "first_name": "Y"}})).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    assert_eq!(emp.get("/api/admin/audit").await.0, StatusCode::FORBIDDEN);
    assert_eq!(emp.get("/api/admin/ad").await.0, StatusCode::FORBIDDEN);
    // Сохранённый пароль служебной учётки не уходит на другие контроллеры.
    let (_, ad) = admin.get("/api/admin/ad").await;
    let d = &ad["domain"];
    let mut controllers = d["controllers"].as_array().unwrap().clone();
    controllers.push(json!("rogue.pult.test:636"));
    let moved = json!({
        "netbios_name": d["netbios_name"], "base_dn": d["base_dn"], "controllers": controllers,
        "tls_mode": d["tls_mode"], "bind_dn": d["bind_dn"], "bind_password": null, "ca_cert_pem": null,
        "user_filter": d["user_filter"], "timeout_secs": d["timeout_secs"], "version": d["version"],
    });
    let (code, body) = admin.put("/api/admin/ad", moved).await;
    assert_eq!((code, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("secret_required")), "{body}");
    let same = json!({
        "netbios_name": d["netbios_name"], "base_dn": d["base_dn"], "controllers": d["controllers"],
        "tls_mode": d["tls_mode"], "bind_dn": d["bind_dn"], "bind_password": null, "ca_cert_pem": null,
        "user_filter": d["user_filter"], "timeout_secs": d["timeout_secs"], "version": d["version"],
    });
    assert_eq!(admin.put("/api/admin/ad", same).await.0, StatusCode::OK, "без смены адреса пароль не нужен");

    // --- Отключение в AD: доступ закрыт после ближайшей синхронизации, запись — в архиве ---
    let mut petrova = Browser::new(app.clone());
    assert_eq!(petrova.login("petrova", USER_PASS).await.0, StatusCode::NO_CONTENT);
    assert_eq!(petrova.get("/api/me").await.0, StatusCode::OK);
    dir.disable("petrova");
    let (code, s) = admin.post("/api/admin/ad/sync", json!({"kind": "incremental"})).await;
    assert_eq!(code, StatusCode::OK, "{s}");
    assert_eq!((s["kind"].as_str(), s["disabled"].as_i64()), (Some("incremental"), Some(1)), "{s}");
    assert_eq!(petrova.get("/api/me").await.0, StatusCode::UNAUTHORIZED, "сеанс закрыт");
    let (_, active) = admin.get("/api/phonebook/persons?q=%D0%BF%D0%B5%D1%82%D1%80%D0%BE%D0%B2%D0%B0").await;
    assert!(!names(&active).contains(&"Петрова Анна".to_string()), "{active}");
    let (_, archived) = admin.get("/api/phonebook/persons?archived=true&q=petrova").await;
    assert!(names(&archived).contains(&"Петрова Анна".to_string()), "в архиве: {archived}");
    // Снова включили — вернулась.
    dir.enable("petrova");
    let (_, s) = admin.post("/api/admin/ad/sync", json!({"kind": "incremental"})).await;
    assert_eq!(s["enabled"], 1, "{s}");
    assert_eq!(petrova.login("petrova", USER_PASS).await.0, StatusCode::NO_CONTENT);

    // --- Полная синхронизация с пустой выборкой (сломан фильтр, у служебной учётки пропали права):
    // никого не отключаем, запуск — ошибка с уведомлением, а не «успех» с закрытыми сеансами ---
    let everyone = dir.replace_users(Vec::new());
    let (code, s) = admin.post("/api/admin/ad/sync", json!({"kind": "full"})).await;
    assert_eq!((code, s["error"].as_str()), (StatusCode::BAD_REQUEST, Some("ad_sync_mass_disable")), "{s}");
    assert_eq!(petrova.get("/api/me").await.0, StatusCode::OK, "сеансы не закрыты");
    let status: String =
        sqlx::query_scalar("SELECT status FROM ad_sync_runs ORDER BY id DESC LIMIT 1").fetch_one(&pool).await.unwrap();
    assert_eq!(status, "error", "запуск отмечен как сбой");
    dir.replace_users(everyone);
    let (code, s) = admin.post("/api/admin/ad/sync", json!({"kind": "full"})).await;
    assert_eq!((code, s["disabled"].as_i64()), (StatusCode::OK, Some(0)), "{s}");
    // Следующая синхронизация не упирается в «уже идёт»: блокировка снята.
    let (code, s) = admin.post("/api/admin/ad/sync", json!({"kind": "incremental"})).await;
    assert_eq!(code, StatusCode::OK, "{s}");

    // --- Аудит: цепочка цела, изменить запись нельзя ---
    let (code, v) = admin.post("/api/admin/audit/verify", json!({})).await;
    assert_eq!(code, StatusCode::OK, "{v}");
    assert!(v["broken_at"].is_null() && v["total"].as_i64().unwrap() > 5, "{v}");
    assert!(sqlx::query("UPDATE audit_log SET action = 'x' WHERE id = 1").execute(&pool).await.is_err());
    assert!(sqlx::query("DELETE FROM audit_log").execute(&pool).await.is_err());
    let logged: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'auth.login_failed' AND details->>'login' = $1",
    )
    .bind(&ghost)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(logged, 5, "неудачные входы — в аудите");
    let leaked: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE details::text LIKE '%' || $1 || '%'")
        .bind(USER_PASS)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(leaked, 0, "пароль не попадает в журнал");

    checklist(&pool, &core, &dir, &mut admin, &mut emp, &app).await;
    second_factor(&mut admin, &app, &pool).await;
    kerberos(&core, &pool).await;
    push(&core, &mut admin, &mut emp, &pool).await;
    oidc(&mut admin, &app, &pool).await;

    // --- Выход ---
    assert_eq!(admin.post("/api/auth/logout", json!({})).await.0, StatusCode::NO_CONTENT);
    assert_eq!(admin.get("/api/me").await.0, StatusCode::UNAUTHORIZED);

    // --- Недоступность AD ---
    dc1.stop().await;
    dc2.stop().await;
    let (code, body) = Browser::new(app.clone()).login("sidorov", USER_PASS).await;
    assert_eq!((code, body["error"].as_str()), (StatusCode::SERVICE_UNAVAILABLE, Some("ad_unavailable")), "{body}");
    let ad = pult_people::domain::AdHandle::default();
    for i in 1..=3 {
        let h = pult_people::health::check(&core, &ad).await.unwrap();
        assert_eq!(h.suspended, i == 3, "политика block: приостановка после 3 неудач подряд, проверка {i}: {h:?}");
    }
    let (code, body) = emp.get("/api/me").await;
    assert_eq!(
        (code, body["error"].as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, Some("ad_unavailable")),
        "сеанс приостановлен: {body}"
    );
    pult_core::settings::save(&pool, &pult_core::settings::AdHealth::default(), None).await.unwrap();
    assert_eq!(emp.get("/api/me").await.0, StatusCode::OK, "связь вернулась — сеанс снова работает");
}

/// Остальные проверки: роли, площадки, справочники, связывание, импорт,
/// аудит, уведомления, почта, телефония, настройки безопасности.
async fn checklist(pool: &PgPool, core: &Core, dir: &Directory, admin: &mut Browser, emp: &mut Browser, app: &Router) {
    // --- Роли и матрица ---
    let (code, m) = admin.get("/api/admin/roles").await;
    assert_eq!(code, StatusCode::OK);
    let role = |m: &Value, c: &str| m["roles"].as_array().unwrap().iter().find(|r| r["code"] == c).cloned().unwrap();
    assert!(
        role(&m, "phonebook_editor")["permissions"].as_array().unwrap().contains(&json!("phonebook.edit")),
        "права по умолчанию: {m}"
    );
    assert!(role(&m, "auditor")["permissions"].as_array().unwrap().contains(&json!("core.audit.view")));
    let (code, _) = admin.put("/api/admin/roles/superadmin/permissions", json!({"permissions": []})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "у суперадминистратора права не настраиваются");
    let (code, _) =
        admin.put("/api/admin/roles/engineer/permissions", json!({"permissions": ["core.audit.view"]})).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let (_, m) = admin.get("/api/admin/roles").await;
    assert_eq!(role(&m, "engineer")["permissions"], json!(["core.audit.view"]), "матрица настраивается");
    // Новое разрешение не возвращается после снятия: повторная раздача прав по умолчанию ничего не добавляет.
    assert_eq!(pult_core::rbac::seed_defaults(pool, &core.permissions).await.unwrap(), 0);

    let (_, users) = admin.get("/api/admin/users?q=sidorov").await;
    let sid = users[0]["id"].as_str().unwrap().to_string();
    let (_, users) = admin.get("/api/admin/users?q=ivanov").await;
    let iv = users[0]["id"].as_str().unwrap().to_string();
    // Сотрудник не управляет ролями.
    assert_eq!(
        emp.put(&format!("/api/admin/users/{sid}/roles"), json!({"roles": ["superadmin"]})).await.0,
        StatusCode::FORBIDDEN
    );
    // Последнего суперадминистратора снять нельзя.
    let (code, body) = admin.put(&format!("/api/admin/users/{iv}/roles"), json!({"roles": []})).await;
    assert_eq!((code, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("last_superadmin")), "{body}");
    // Выданная роль действует сразу: инженер видит аудит (настроили выше).
    assert_eq!(emp.get("/api/admin/audit").await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        admin.put(&format!("/api/admin/users/{sid}/roles"), json!({"roles": ["engineer"]})).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(emp.get("/api/admin/audit").await.0, StatusCode::OK);
    let (_, audit) = admin.get("/api/admin/audit?action=user.roles").await;
    assert!(
        audit.as_array().unwrap().iter().any(|r| r["details"]["after"] == json!(["engineer"])),
        "назначение роли — в аудите"
    );
    // Без повышения прав: управляющий ролями (не суперадминистратор) раздаёт только то, что есть у него.
    let eng = |p: Value| json!({ "permissions": p });
    assert_eq!(
        admin
            .put("/api/admin/roles/engineer/permissions", eng(json!(["core.audit.view", "core.roles.manage"])))
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (code, body) = emp
        .put(
            "/api/admin/roles/engineer/permissions",
            eng(json!(["core.audit.view", "core.roles.manage", "people.ad.manage"])),
        )
        .await;
    assert_eq!((code, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("escalation")), "{body}");
    let (code, body) =
        emp.put(&format!("/api/admin/users/{sid}/roles"), json!({"roles": ["engineer", "phonebook_editor"]})).await;
    assert_eq!((code, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("escalation")), "{body}");
    assert_eq!(
        emp.put(&format!("/api/admin/users/{sid}/roles"), json!({"roles": ["engineer", "auditor"]})).await.0,
        StatusCode::NO_CONTENT,
        "роль с правами, которые у него есть, — можно"
    );
    assert_eq!(
        emp.put(&format!("/api/admin/users/{sid}/roles"), json!({"roles": ["engineer", "superadmin"]})).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        admin.put(&format!("/api/admin/users/{sid}/roles"), json!({"roles": ["engineer"]})).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        admin.put("/api/admin/roles/engineer/permissions", eng(json!(["core.audit.view"]))).await.0,
        StatusCode::NO_CONTENT
    );

    // --- Площадки и инженеры ---
    let site_name = format!("Площадка {}", pult_testkit::seq() + std::process::id() as usize);
    let (code, body) = admin
        .post(
            "/api/admin/sites",
            json!({"name": site_name, "address": null, "timezone": "Mars/Base", "is_active": true}),
        )
        .await;
    assert_eq!((code, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_timezone")));
    let (code, site) = admin
        .post(
            "/api/admin/sites",
            json!({"name": site_name, "address": "ул. Тестовая, 1", "timezone": "Europe/Moscow", "is_active": true}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{site}");
    let site_id = site["id"].as_str().unwrap().to_string();
    let (_, users) = admin.get("/api/admin/users?q=petrova").await;
    let pet = users[0]["id"].as_str().unwrap().to_string();
    let (code, body) =
        admin.put(&format!("/api/admin/sites/{site_id}/engineers"), json!({"l1": [pet], "l2": []})).await;
    assert_eq!(
        (code, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("not_engineer")),
        "ответственный — только инженер"
    );
    let (code, eng) =
        admin.put(&format!("/api/admin/sites/{site_id}/engineers"), json!({"l1": [sid], "l2": [sid, iv]})).await;
    assert_eq!(code, StatusCode::OK, "{eng}");
    assert_eq!(eng.as_array().unwrap().len(), 3, "L1 и L2 без ограничения числа: {eng}");
    let (code, conflict) = admin
        .put(
            &format!("/api/admin/sites/{site_id}"),
            json!({"name": site_name, "address": null, "timezone": "Europe/Moscow", "is_active": true, "version": 99}),
        )
        .await;
    assert_eq!(code, StatusCode::CONFLICT, "{conflict}");

    // --- Подразделения, должности, дубли, избранное ---
    let (code, dep) = admin
        .post("/api/phonebook/departments", json!({"name": format!("Отдел {site_name}"), "parent_id": null}))
        .await;
    assert_eq!(code, StatusCode::CREATED, "{dep}");
    let dep_id = dep["id"].as_str().unwrap().to_string();
    let (code, _) = admin
        .post("/api/phonebook/departments", json!({"name": format!("отдел {site_name}"), "parent_id": null}))
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "такое название уже есть");
    let (code, p) = admin
        .post("/api/phonebook/persons", json!({"person": {"last_name": "Дублёв", "first_name": "Дубль", "department_id": dep_id, "site_id": site_id, "mobile_phone": "+7 900 555-66-77"}}))
        .await;
    assert_eq!(code, StatusCode::CREATED, "{p}");
    assert_eq!(p["site_name"].as_str(), Some(site_name.as_str()), "площадка сотрудника");
    let (code, body) = admin.delete(&format!("/api/phonebook/departments/{dep_id}")).await;
    assert_eq!(
        (code, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("in_use")),
        "используемое — только в архив"
    );
    let (code, d) = admin
        .post(
            &format!("/api/phonebook/departments/{dep_id}/archive"),
            json!({"version": dep["version"], "archived": true}),
        )
        .await;
    assert_eq!((code, d["is_archived"].as_bool()), (StatusCode::OK, Some(true)));
    let (_, pos) = admin
        .post("/api/phonebook/positions", json!({"name": format!("Должность {site_name}"), "parent_id": null}))
        .await;
    assert_eq!(
        admin.delete(&format!("/api/phonebook/positions/{}", pos["id"].as_str().unwrap())).await.0,
        StatusCode::NO_CONTENT,
        "неиспользуемое удаляется"
    );
    let (_, dups) = admin
        .post(
            "/api/phonebook/duplicates",
            json!({"person": {"last_name": "ДУБЛЁВ", "first_name": "дубль"}, "exclude_id": null}),
        )
        .await;
    assert_eq!(dups[0]["reason"], "name", "дубль по ФИО (ё = е, регистр): {dups}");
    let (_, dups) = admin.post("/api/phonebook/duplicates", json!({"person": {"last_name": "Другой", "first_name": "X", "mobile_phone": "8(900)5556677"}, "exclude_id": null})).await;
    assert!(dups.as_array().unwrap().iter().any(|d| d["reason"] == "mobile_phone"), "дубль по мобильному: {dups}");
    let pid = p["id"].as_str().unwrap();
    assert_eq!(emp.put(&format!("/api/phonebook/favorites/{pid}"), json!({})).await.0, StatusCode::NO_CONTENT);
    let (_, fav) = emp.get("/api/phonebook/persons?favorites=true").await;
    assert_eq!(names(&fav), vec!["Дублёв Дубль".to_string()], "избранное — своё у каждого");
    let (_, fav) = admin.get("/api/phonebook/persons?favorites=true").await;
    assert!(names(&fav).is_empty());

    // --- Источник полей ---
    let (code, fs) = admin.put("/api/admin/phonebook/field-sources", json!({"room": "ad"})).await;
    assert_eq!((code, fs["room"].as_str()), (StatusCode::OK, Some("ad")));
    let (_, cfg) = emp.get("/api/phonebook/config").await;
    assert_eq!(cfg["field_sources"]["room"], "ad");
    assert_eq!(
        admin.put("/api/admin/phonebook/field-sources", json!({"bogus": "ad"})).await.0,
        StatusCode::BAD_REQUEST
    );
    admin.put("/api/admin/phonebook/field-sources", json!({"room": "portal"})).await;

    // --- Связывание: email — сразу, ФИО — на подтверждение, без пары и без автосоздания — пропуск ---
    admin
        .post(
            "/api/phonebook/persons",
            json!({"person": {"last_name": "Кузнецов", "first_name": "Олег", "email": "kuznetsov@pult.test"}}),
        )
        .await;
    admin.post("/api/phonebook/persons", json!({"person": {"last_name": "Новикова", "first_name": "Ольга"}})).await;
    assert_eq!(
        admin.put("/api/admin/people/link-settings", json!({"auto_create_persons": false})).await.0,
        StatusCode::OK
    );
    dir.add(FakeUser::person("kuznetsov", "Кузнецов", "Олег", 20));
    let mut nov = FakeUser::person("novikova", "Новикова", "Ольга", 21);
    nov.mail = None;
    dir.add(nov);
    dir.add(FakeUser::person("noauto", "Безпары", "Иван", 22));
    let (_, s) = admin.post("/api/admin/ad/sync", json!({"kind": "incremental"})).await;
    assert_eq!(
        (s["linked"].as_i64(), s["candidates"].as_i64(), s["persons_created"].as_i64()),
        (Some(1), Some(1), Some(0)),
        "{s}"
    );
    let (_, k) = admin.get("/api/phonebook/persons?q=kuznetsov").await;
    assert_eq!(k[0]["ad_login"], "kuznetsov", "связан по email: {k}");
    let (_, c) = admin.get("/api/admin/people/link-candidates").await;
    let cand = c.as_array().unwrap().iter().find(|x| x["ad_login"] == "novikova").cloned().unwrap();
    assert_eq!(
        admin
            .post(
                &format!("/api/admin/people/link-candidates/{}", cand["id"].as_str().unwrap()),
                json!({"confirm": true})
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (_, n) = admin.get("/api/phonebook/persons?q=%D0%BD%D0%BE%D0%B2%D0%B8%D0%BA%D0%BE%D0%B2%D0%B0").await;
    assert_eq!(n[0]["ad_login"], "novikova", "пара подтверждена: {n}");
    let (_, b) = admin.get("/api/phonebook/persons?q=%D0%B1%D0%B5%D0%B7%D0%BF%D0%B0%D1%80%D1%8B").await;
    assert!(names(&b).is_empty(), "автосоздание выключено — сотрудник не заведён");

    // --- Связывание, когда в AD пусты mail, sn и givenName (есть только displayName) ---
    // Логин почты = логин AD и фамилия есть в displayName — сразу; фамилия другая — на подтверждение;
    // ФИО из displayName — на подтверждение; служебная учётка без пары — пропуск.
    for (last, first, email) in [
        ("Соколов", "Пётр", Some("psokolov@corp.example.test")),
        ("Морозова", "Анна", Some("amorozova@corp.example.test")),
        ("Волков", "Илья", None),
    ] {
        let (code, _) = admin
            .post("/api/phonebook/persons", json!({"person": {"last_name": last, "first_name": first, "email": email}}))
            .await;
        assert_eq!(code, StatusCode::CREATED, "{last}");
    }
    let bare = |sam: &str, display: &str, seed: u8| {
        let mut u = FakeUser::person(sam, "-", "-", seed);
        (u.sn, u.given, u.mail, u.display) = (None, None, None, display.to_string());
        u
    };
    dir.add(bare("psokolov", "Соколов Пётр Иванович", 41));
    dir.add(bare("amorozova", "Смирнова Анна", 42));
    dir.add(bare("ivolkov", "Волков Илья Сергеевич", 43));
    dir.add(bare("scanner1", "Сканер 2 этаж", 44));
    let (_, s) = admin.post("/api/admin/ad/sync", json!({"kind": "incremental"})).await;
    assert_eq!(
        (s["linked"].as_i64(), s["candidates"].as_i64(), s["persons_created"].as_i64()),
        (Some(1), Some(2), Some(0)),
        "{s}"
    );
    let (_, k) = admin.get("/api/phonebook/persons?q=psokolov").await;
    assert_eq!(k[0]["ad_login"], "psokolov", "связан по логину почты: {k}");
    let (_, audit) = admin.get("/api/admin/audit?action=person.link").await;
    assert!(
        audit.as_array().unwrap().iter().any(|r| r["details"]["by"] == "email_login"),
        "правило связи — в аудите: {audit}"
    );
    let (_, c) = admin.get("/api/admin/people/link-candidates").await;
    let reason = |login: &str| {
        c.as_array().unwrap().iter().find(|x| x["ad_login"] == login).map(|x| x["reason"].as_str().unwrap().to_string())
    };
    assert!(reason("amorozova").is_some_and(|r| r.contains("Логин почты")), "{c}");
    assert!(reason("ivolkov").is_some_and(|r| r.contains("displayName")), "{c}");
    assert!(reason("scanner1").is_none(), "{c}");

    // --- Импорт Phonebook: повторный запуск не дублирует, сотрудник из AD сливается по email ---
    admin.put("/api/admin/people/link-settings", json!({"auto_create_persons": true})).await;
    dir.add(FakeUser::person("merge1", "Слияниев", "Марк", 23));
    admin.post("/api/admin/ad/sync", json!({"kind": "incremental"})).await;
    let emp_row = |id: i32, last: &str, mail: &str| pult_phonebook::LegacyEmployee {
        id,
        last_name: last.into(),
        first_name: "Имя".into(),
        department_id: Some(9001),
        position_id: Some(9101),
        work_phone: "4512".into(),
        email: mail.into(),
        ..Default::default()
    };
    let snap = pult_phonebook::Snapshot {
        departments: vec![(9001, "Бухгалтерия (импорт)".into())],
        positions: vec![(9101, "Бухгалтер (импорт)".into())],
        employees: vec![emp_row(90001, "Импортов", ""), emp_row(90002, "Слияниев", "merge1@pult.test")],
        audit_records: 1,
        audit: vec![pult_phonebook::LegacyAudit { id: 1, action: "update".into(), ..Default::default() }],
        has_email_column: true,
        has_archive_columns: true,
        has_audit_table: true,
    };
    let actor = pult_core::audit::Actor::system();
    let dry = pult_phonebook::import::apply(pool, &snap, true, &actor, "test").await.unwrap();
    assert_eq!(dry.applied.persons_created, 2);
    let first = pult_phonebook::import::apply(pool, &snap, false, &actor, "test").await.unwrap();
    assert_eq!(
        (first.applied.persons_created, first.applied.persons_merged, first.applied.audit_imported),
        (2, 1, 1),
        "{:?}",
        first.applied
    );
    assert!(first.reconciliation.matches, "{:?}", first.reconciliation);
    let again = pult_phonebook::import::apply(pool, &snap, false, &actor, "test").await.unwrap();
    assert_eq!(
        (again.applied.persons_created, again.applied.persons_unchanged, again.applied.audit_imported),
        (0, 2, 0),
        "идемпотентно: {:?}",
        again.applied
    );
    let (_, m) = admin.get("/api/phonebook/persons?q=merge1").await;
    assert_eq!(
        (m.as_array().unwrap().len(), m[0]["ad_login"].as_str()),
        (1, Some("merge1")),
        "слияние: одна запись, связанная с AD: {m}"
    );

    // --- PhonebookAdmins → «Редактор справочника» однократно ---
    dir.add_group("PhonebookAdmins", &["petrova"]);
    let ad = pult_people::domain::AdHandle::default();
    let n =
        pult_people::sync::grant_role_to_group_once(core, &ad, "PhonebookAdmins", "phonebook_editor").await.unwrap();
    assert_eq!(n, Some(1));
    assert_eq!(
        pult_people::sync::grant_role_to_group_once(core, &ad, "PhonebookAdmins", "phonebook_editor").await.unwrap(),
        None,
        "только один раз"
    );
    let (_, users) = admin.get("/api/admin/users?q=petrova").await;
    assert!(users[0]["roles"].as_array().unwrap().contains(&json!("phonebook_editor")), "{users}");

    // --- Аудит: фильтры и CSV ---
    let (_, rows) = admin.get("/api/admin/audit?action=person&limit=500").await;
    assert!(
        rows.as_array().unwrap().iter().all(|r| r["action"].as_str().unwrap().starts_with("person")),
        "фильтр по действию"
    );
    let (code, csv) = admin.get("/api/admin/audit.csv?action=person.update").await;
    let csv = csv.as_str().unwrap_or_default().to_string();
    assert_eq!(code, StatusCode::OK);
    assert!(
        csv.starts_with("\u{feff}id;ts;actor;action") && csv.contains("person.update"),
        "{}",
        &csv[..csv.len().min(200)]
    );
    assert!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM audit_log WHERE action = 'audit.export'")
            .fetch_one(pool)
            .await
            .unwrap()
            >= 1,
        "выгрузка — в аудите"
    );

    // --- Уведомления ---
    pult_core::notify::notify_superadmins(pool, pult_core::notify::kind::ROLE_GRANTED, json!({"test": true}))
        .await
        .unwrap();
    let (_, nl) = admin.get("/api/notifications").await;
    assert!(nl["unread"].as_i64().unwrap() >= 1, "{nl}");
    assert_eq!(admin.post("/api/notifications/read", json!({"ids": []})).await.0, StatusCode::NO_CONTENT);
    let (_, nl) = admin.get("/api/notifications").await;
    assert_eq!(nl["unread"], 0);

    // --- Почта: настройки, очередь, повтор при сбое ---
    let mail = |enabled: bool, from: &str| json!({"settings": {"enabled": enabled, "host": "127.0.0.1", "port": 1, "security": "starttls", "username": "", "from_address": from, "from_name": "Pult"}, "password": "smtp-secret-xyz"});
    assert_eq!(admin.put("/api/admin/mail", mail(true, "not-an-email")).await.0, StatusCode::BAD_REQUEST);
    let (code, mv) = admin.put("/api/admin/mail", mail(true, "helpdesk@pult.test")).await;
    assert_eq!((code, mv["has_password"].as_bool()), (StatusCode::OK, Some(true)), "{mv}");
    assert!(!mv.to_string().contains("smtp-secret-xyz"), "пароль SMTP наружу не отдаётся");
    let mut moved = mail(true, "helpdesk@pult.test");
    moved["settings"]["host"] = json!("smtp.elsewhere.test");
    moved["password"] = Value::Null;
    let (code, body) = admin.put("/api/admin/mail", moved).await;
    assert_eq!(
        (code, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("secret_required")),
        "сохранённый пароль не уходит на другой сервер: {body}"
    );
    // Срок хранения: отправленное давно — удаляется, свежее и ещё не отправленное — остаётся.
    let old = pult_core::mail::enqueue(pool, &["old@pult.test".into()], "Старое", "Текст").await.unwrap();
    let fresh = pult_core::mail::enqueue(pool, &["new@pult.test".into()], "Свежее", "Текст").await.unwrap();
    sqlx::query("UPDATE outbox_emails SET status = 'sent', created_at = now() - interval '91 days' WHERE id = $1")
        .bind(old)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE outbox_emails SET status = 'sent' WHERE id = $1").bind(fresh).execute(pool).await.unwrap();
    assert!(pult_core::retention::prune(pool).await.unwrap() >= 1);
    let left: Vec<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM outbox_emails WHERE id = ANY($1)")
        .bind(vec![old, fresh])
        .fetch_all(pool)
        .await
        .unwrap();
    assert_eq!(left, vec![fresh], "удалено только старое отправленное");
    let id = pult_core::mail::enqueue(pool, &["ivanov@pult.test".into()], "Проверка", "Текст").await.unwrap();
    assert_eq!(pult_core::mail::send_due(pool, &core.secrets).await.unwrap(), 0);
    let (status, attempts, err): (String, i32, Option<String>) =
        sqlx::query_as("SELECT status, attempts, last_error FROM outbox_emails WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!((status.as_str(), attempts), ("queued", 1), "сбой SMTP — повтор позже");
    assert!(err.is_some_and(|e| !e.contains("smtp-secret-xyz")));
    let (_, ob) = admin.get("/api/admin/mail/outbox").await;
    assert!(ob.as_array().unwrap().iter().any(|r| r["id"] == json!(id)));
    admin.put("/api/admin/mail", mail(false, "helpdesk@pult.test")).await;

    // --- Телефония: выключена — кнопки нет и звонок не идёт; ошибки шлюза понятны ---
    let (_, cfg) = emp.get("/api/phonebook/config").await;
    assert_eq!(cfg["call_enabled"], false);
    let (_, t) = admin.get("/api/phonebook/persons?q=%D0%B8%D0%BC%D0%BF%D0%BE%D1%80%D1%82%D0%BE%D0%B2").await;
    let callee = t[0]["id"].as_str().unwrap().to_string();
    let (code, body) = emp.post("/api/phonebook/calls", json!({"person_id": callee})).await;
    assert_eq!((code, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("telephony_disabled")));
    let tel = |url: &str| json!({"settings": {"enabled": true, "provider": "callgateway", "base_url": url, "mail_template": "mailto:{email}"}, "token": "gw-token-xyz"});
    assert_eq!(
        admin.put("/api/admin/telephony", tel("http://127.0.0.1:1")).await.0,
        StatusCode::BAD_REQUEST,
        "только https"
    );
    let (code, tv) = admin.put("/api/admin/telephony", tel("https://127.0.0.1:1")).await;
    assert_eq!((code, tv["has_token"].as_bool()), (StatusCode::OK, Some(true)));
    assert!(!tv.to_string().contains("gw-token-xyz"));
    let mut moved = tel("https://127.0.0.2:1");
    moved["token"] = Value::Null;
    let (code, body) = admin.put("/api/admin/telephony", moved).await;
    assert_eq!(
        (code, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("secret_required")),
        "сохранённый токен не уходит на другой шлюз: {body}"
    );
    let (code, body) = emp.post("/api/phonebook/calls", json!({"person_id": callee})).await;
    assert_eq!((code, body["error"].as_str()), (StatusCode::SERVICE_UNAVAILABLE, Some("pbx_unavailable")), "{body}");
    let (code, body) = emp.post("/api/phonebook/calls", json!({"person_id": pid})).await;
    assert_eq!(
        (code, body["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("call_failed")),
        "без добавочного — не звоним: {body}"
    );
    // AMI (FreePBX): без адреса АТС не сохраняется; пароль не возвращается; звонок — понятная ошибка.
    let ami = |host: &str| {
        json!({"settings": {"enabled": true, "provider": "ami", "ami_host": host, "ami_port": 1, "ami_user": "pult",
            "ami_channel": "Local/{ext}@from-internal", "ami_context": "from-internal", "mail_template": "mailto:{email}"},
            "token": "ami-secret-xyz"})
    };
    assert_eq!(admin.put("/api/admin/telephony", ami("")).await.0, StatusCode::BAD_REQUEST, "без адреса АТС");
    let (code, tv) = admin.put("/api/admin/telephony", ami("127.0.0.1")).await;
    assert_eq!((code, tv["has_token"].as_bool()), (StatusCode::OK, Some(true)), "{tv}");
    assert!(!tv.to_string().contains("ami-secret-xyz"));
    let (code, body) = emp.post("/api/phonebook/calls", json!({"person_id": callee})).await;
    assert!(code == StatusCode::BAD_REQUEST || code == StatusCode::SERVICE_UNAVAILABLE, "{code}: {body}");
    assert!(matches!(body["error"].as_str(), Some("call_no_extension" | "pbx_unavailable")), "{body}");
    admin
        .put(
            "/api/admin/telephony",
            json!({"settings": {"enabled": false, "base_url": "", "mail_template": "mailto:{email}"}, "token": ""}),
        )
        .await;

    // --- Настройки безопасности ---
    let (_, sec) = admin.get("/api/admin/security").await;
    let mut bad = sec.clone();
    bad["session_idle_hours"] = json!(0);
    assert_eq!(admin.put("/api/admin/security", bad).await.0, StatusCode::BAD_REQUEST);
    let mut ok = sec.clone();
    ok["login_max_failures_per_ip"] = json!(25);
    assert_eq!(admin.put("/api/admin/security", ok).await.1["login_max_failures_per_ip"], 25);
    admin.put("/api/admin/security", sec).await;
    assert_eq!(emp.get("/api/admin/security").await.0, StatusCode::FORBIDDEN);

    // Личные настройки (колонки справочника).
    assert_eq!(
        emp.put("/api/me/settings", json!({"settings": {"phonebook": {"columns": ["email"]}}})).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(emp.get("/api/me/settings").await.1["settings"]["phonebook"]["columns"], json!(["email"]));
    profile(admin, emp, app, pool).await;
}

/// Профиль и личные предпочтения, как в GLPI: тема, формат даты, порядок ФИО; свои сеансы.
async fn profile(admin: &mut Browser, emp: &mut Browser, app: &Router, pool: &PgPool) {
    let (code, me) = emp.get("/api/me").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(me["prefs"]["theme"], "system", "по умолчанию — как в системе: {me}");
    assert_eq!(me["prefs"]["page_size"], 50);

    let prefs = json!({"theme": "dark", "density": "compact", "date_format": "ymd", "clock": "h12",
                       "name_order": "first_last", "page_size": 100, "start_page": "phonebook"});
    let (code, saved) = emp.put("/api/me/prefs", prefs.clone()).await;
    assert_eq!(code, StatusCode::OK, "{saved}");
    assert_eq!(saved, prefs);
    assert_eq!(emp.get("/api/me").await.1["prefs"], prefs, "предпочтения приходят в /api/me");
    let (code, err) = emp.put("/api/me/prefs", json!({"theme": "neon"})).await;
    assert_eq!((code, err["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_prefs")), "{err}");
    let (code, err) = emp.put("/api/me/prefs", json!({"page_size": 7})).await;
    assert_eq!((code, err["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_prefs")), "{err}");
    // Частичное тело — остальное по умолчанию (форма всегда шлёт всё).
    let (_, partial) = emp.put("/api/me/prefs", json!({"theme": "light"})).await;
    assert_eq!((partial["theme"].as_str(), partial["page_size"].as_u64()), (Some("light"), Some(50)));
    emp.put("/api/me/prefs", prefs.clone()).await;

    // Настройки справочника (PUT целиком) не затирают предпочтения.
    let body = json!({"settings": {"phonebook": {"columns": ["room"]}, "prefs": {"theme": "light"}}});
    assert_eq!(emp.put("/api/me/settings", body).await.0, StatusCode::NO_CONTENT);
    let (_, s) = emp.get("/api/me/settings").await;
    assert_eq!(s["settings"]["phonebook"]["columns"], json!(["room"]));
    assert_eq!(s["settings"]["prefs"], prefs, "prefs меняются только через /api/me/prefs");

    // Профиль: учётка, карточка справочника, роли на языке интерфейса.
    let (code, p) = emp.get("/api/me/profile").await;
    assert_eq!(code, StatusCode::OK, "{p}");
    assert_eq!(p["login"], "sidorov");
    assert_eq!(p["person"]["last_name"], "Сидоров", "{p}");
    assert!(p["roles"].as_array().unwrap().iter().any(|r| r["code"] == "employee" && r["name"] == "Сотрудник"), "{p}");
    assert!(p["last_login_at"].is_string());
    assert_eq!(p["prefs"], prefs);

    // Сеансы: второй браузер того же сотрудника; текущий помечен, его не закрыть по ссылке.
    let current = |v: Value| v.as_array().unwrap().iter().find(|s| s["current"] == true).unwrap()["handle"].clone();
    let mut other = Browser::new(app.clone());
    assert_eq!(other.login("sidorov", USER_PASS).await.0, StatusCode::NO_CONTENT);
    let (code, list) = emp.get("/api/me/sessions").await;
    assert_eq!(code, StatusCode::OK, "{list}");
    let arr = list.as_array().unwrap();
    assert!(arr.len() >= 2, "{list}");
    assert_eq!(arr.iter().filter(|s| s["current"] == true).count(), 1, "ровно один текущий: {list}");
    let mine = current(list.clone()).as_str().unwrap().to_string();
    assert_eq!(mine.len(), 32);
    let (code, err) = emp.delete(&format!("/api/me/sessions/{mine}")).await;
    assert_eq!((code, err["error"].as_str()), (StatusCode::BAD_REQUEST, Some("current_session")));
    assert_eq!(emp.delete("/api/me/sessions/zz").await.0, StatusCode::NOT_FOUND);
    // Сеанс другого пользователя по ссылке не закрыть.
    let admins = current(admin.get("/api/me/sessions").await.1);
    assert_eq!(emp.delete(&format!("/api/me/sessions/{}", admins.as_str().unwrap())).await.0, StatusCode::NOT_FOUND);
    assert_eq!(admin.get("/api/me").await.0, StatusCode::OK);
    let theirs = current(other.get("/api/me/sessions").await.1);
    let theirs = theirs.as_str().unwrap();
    assert_ne!(theirs, mine);
    assert_eq!(emp.delete(&format!("/api/me/sessions/{theirs}")).await.0, StatusCode::NO_CONTENT, "свой второй сеанс");
    // В аудите видно, какой сеанс закрыт: ссылка, адрес и браузер (не токен).
    // Тестовый клиент не шлёт User-Agent: проверяем, что поля есть, а не их значения.
    let (h, fields): (Option<String>, bool) = sqlx::query_as(
        "SELECT details->>'handle', details ?& array['ip', 'user_agent'] FROM audit_log
          WHERE action = 'auth.session_revoked' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(h.as_deref(), Some(theirs), "в аудите — ссылка на закрытый сеанс");
    assert!(fields, "адрес и браузер закрытого сеанса");
    assert_eq!(other.get("/api/me").await.0, StatusCode::UNAUTHORIZED, "закрытый сеанс не работает");
    assert_eq!(emp.get("/api/me").await.0, StatusCode::OK, "текущий жив");

    // Выйти на остальных устройствах.
    let mut third = Browser::new(app.clone());
    assert_eq!(third.login("sidorov", USER_PASS).await.0, StatusCode::NO_CONTENT);
    let (code, r) = emp.post("/api/me/sessions/revoke-others", json!({})).await;
    assert_eq!(code, StatusCode::OK, "{r}");
    assert!(r["closed"].as_u64().unwrap() >= 1);
    assert_eq!(third.get("/api/me").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(emp.get("/api/me").await.0, StatusCode::OK);
    assert_eq!(emp.get("/api/me/sessions").await.1.as_array().unwrap().len(), 1);

    // Токен API (только чтение) не открывает профиль и сеансы владельца — только браузерный вход.
    let token = "pult_profile_test_token_0123456789abcdef"; // SAFE: выдуманный токен только для теста
    sqlx::query(
        "INSERT INTO api_tokens (name, owner_user_id, token_hash)
         SELECT 'профиль', u.id, $1 FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id
          WHERE a.sam_account_name = 'sidorov'",
    )
    .bind(pult_core::crypto::sha256(token.as_bytes()))
    .execute(pool)
    .await
    .unwrap();
    let by_token = |uri: &str| {
        Request::get(uri).header(header::AUTHORIZATION, format!("Bearer {token}")).body(Body::empty()).unwrap()
    };
    assert_eq!(app.clone().oneshot(by_token("/api/me")).await.unwrap().status(), StatusCode::OK, "токен рабочий");
    for uri in ["/api/me/profile", "/api/me/sessions"] {
        assert_eq!(app.clone().oneshot(by_token(uri)).await.unwrap().status(), StatusCode::FORBIDDEN, "{uri}");
    }

    // Без входа — 401.
    let mut anon = Browser::new(app.clone());
    assert_eq!(anon.get("/api/me/profile").await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(anon.get("/api/me/sessions").await.0, StatusCode::UNAUTHORIZED);

    // Смена своего пароля AD: выключена по умолчанию; старый пароль и политику проверяет домен.
    let change = |old: &str, new: &str| json!({"old_password": old, "new_password": new});
    const NEW_PASS: &str = "New-Pass-789"; // SAFE: пароль поддельного DC
    assert_eq!(emp.get("/api/me/profile").await.1["can_change_password"], false);
    assert_eq!(emp.post("/api/me/password", change(USER_PASS, NEW_PASS)).await.0, StatusCode::FORBIDDEN);
    let mut sec: pult_core::settings::SecuritySettings = pult_core::settings::load(pool).await.unwrap();
    sec.ad_password_change = true;
    pult_core::settings::save(pool, &sec, None).await.unwrap();
    assert_eq!(emp.get("/api/me/profile").await.1["can_change_password"], true);
    let (code, e) = emp.post("/api/me/password", change("wrong-password", NEW_PASS)).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("wrong_password")), "{e}");
    let (code, e) = emp.post("/api/me/password", change(USER_PASS, "short")).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("password_policy")), "{e}");
    let mut fourth = Browser::new(app.clone());
    assert_eq!(fourth.login("sidorov", USER_PASS).await.0, StatusCode::NO_CONTENT);
    let (code, r) = emp.post("/api/me/password", change(USER_PASS, NEW_PASS)).await;
    assert_eq!(code, StatusCode::OK, "{r}");
    assert_eq!(r["closed_sessions"], 1, "{r}");
    assert_eq!(fourth.get("/api/me").await.0, StatusCode::UNAUTHORIZED, "сеансы на других устройствах закрыты");
    assert_eq!(emp.get("/api/me").await.0, StatusCode::OK, "текущий сеанс жив");
    let mut fifth = Browser::new(app.clone());
    assert_eq!(fifth.login("sidorov", USER_PASS).await.0, StatusCode::UNAUTHORIZED, "старый пароль не подходит");
    assert_eq!(fifth.login("sidorov", NEW_PASS).await.0, StatusCode::NO_CONTENT, "вход новым паролем");
    // В аудите — итог и причина отказа, паролей нет.
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT action, details::text FROM audit_log WHERE action LIKE 'auth.password_change%' ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let actions: Vec<&str> = rows.iter().map(|(a, _)| a.as_str()).collect();
    assert_eq!(actions, ["auth.password_change_failed", "auth.password_change_failed", "auth.password_changed"]);
    assert!(rows.iter().all(|(_, d)| !d.contains(NEW_PASS) && !d.contains(USER_PASS)), "{rows:?}");
    // Вернуть прежний пароль: дальше тесты входят с ним.
    assert_eq!(emp.post("/api/me/password", change(NEW_PASS, USER_PASS)).await.0, StatusCode::OK);
    sec.ad_password_change = false;
    pult_core::settings::save(pool, &sec, None).await.unwrap();
}

/// Уведомления на устройства: выключены по умолчанию, подписки — только к службам push, закрытый ключ VAPID
/// зашифрован, рассылка забирает каждое уведомление один раз.
async fn push(core: &Core, admin: &mut Browser, emp: &mut Browser, pool: &PgPool) {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let (code, c) = emp.get("/api/me/push").await;
    assert_eq!((code, c["enabled"].as_bool(), c["public_key"].is_null()), (StatusCode::OK, Some(false), true), "{c}");
    let mut p256 = vec![7u8; 65];
    p256[0] = 4;
    let sub = |endpoint: &str| json!({"endpoint": endpoint, "keys": {"p256dh": b64.encode(&p256), "auth": b64.encode([9u8; 16])}});
    const FCM: &str = "https://fcm.googleapis.com/fcm/send/stage1";
    assert_eq!(emp.post("/api/me/push", sub(FCM)).await.0, StatusCode::FORBIDDEN, "выключено");
    assert_eq!(emp.put("/api/admin/push", json!({"enabled": true})).await.0, StatusCode::FORBIDDEN);
    let (code, e) = admin.put("/api/admin/push", json!({"enabled": true, "contact": "it@pult.test"})).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_settings")), "{e}");
    let (code, s) = admin.put("/api/admin/push", json!({"enabled": true, "contact": "mailto:it@pult.test"})).await;
    assert_eq!(code, StatusCode::OK, "{s}");
    let key = emp.get("/api/me/push").await.1["public_key"].as_str().unwrap().to_string();
    assert_eq!(b64.decode(&key).unwrap().len(), 65, "открытый ключ VAPID — точка P-256");
    assert_eq!(emp.get("/api/me/push").await.1["public_key"], key.as_str(), "ключ постоянный");
    let private: String = sqlx::query_scalar("SELECT value->>'private' FROM settings WHERE key = 'push_keys'")
        .fetch_one(pool)
        .await
        .unwrap();
    let blob = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(private).unwrap();
    assert_ne!(blob.first(), Some(&0x30), "закрытый ключ хранится зашифрованным, а не DER");
    let (code, e) = emp.post("/api/me/push", sub("https://10.0.0.5/push")).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_subscription")), "{e}");
    assert_eq!(emp.post("/api/me/push", sub(FCM)).await.0, StatusCode::NO_CONTENT);
    assert_eq!(emp.get("/api/me/push").await.1["devices"], 1);
    assert_eq!(
        emp.call(Method::DELETE, "/api/me/push", Some(json!({"endpoint": FCM})), true).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(emp.get("/api/me/push").await.1["devices"], 0);
    // Рассылка забирает уведомление один раз (подписок нет — в сеть не ходит).
    let me = emp.get("/api/me").await.1["user_id"].as_str().unwrap().parse::<uuid::Uuid>().unwrap();
    pult_core::notify::notify(pool, me, "system.test", json!({})).await.unwrap();
    assert_eq!(pult_people::push::deliver(core).await.unwrap(), 0);
    let left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM notifications WHERE pushed_at IS NULL").fetch_one(pool).await.unwrap();
    assert_eq!(left, 0, "уведомление забрано рассылкой");
    assert_eq!(admin.put("/api/admin/push", json!({"enabled": false})).await.0, StatusCode::OK);
}

/// Второй фактор для входа из интернета: код проверяется до AD, подключается только изнутри сети,
/// один код — один вход, перебор снаружи не закрывает вход в офисе, сброс суперадминистратором.
async fn second_factor(admin: &mut Browser, app: &Router, pool: &PgPool) {
    use pult_core::totp;
    const INSIDE: &str = "10.1.1.1:5000";
    const OUTSIDE: &str = "203.0.113.7:4000";
    let now = || totp::step_at(u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp()).unwrap());
    let code = |key: &[u8], step: u64| format!("{:06}", totp::code_at(key, step));
    let reset_steps = || async {
        // «Прошло время»: следующий код новее последнего принятого.
        sqlx::query("UPDATE users SET totp_last_step = 0 WHERE totp_enabled_at IS NOT NULL")
            .execute(pool)
            .await
            .unwrap();
    };

    let mut inside = Browser::new(app.clone());
    assert_eq!(inside.login("petrova", USER_PASS).await.0, StatusCode::NO_CONTENT, "изнутри — без кода");
    assert_eq!(inside.get("/api/auth/context").await.1["external"], false);
    let mut out = Browser::new(app.clone());
    out.ip = OUTSIDE.into();
    assert_eq!(out.get("/api/auth/context").await.1["external"], true);

    // Код не подключён и роль его не требует — снаружи вход по паролю, как в офисе.
    let mut plain = Browser::new(app.clone());
    plain.ip = OUTSIDE.into();
    let (c, body) = plain.login("petrova", "wrong-password").await;
    assert_eq!(body["error"], "invalid_credentials", "{c} {body}");
    assert_eq!(plain.login("petrova", USER_PASS).await.0, StatusCode::NO_CONTENT, "снаружи без кода");
    sqlx::query("DELETE FROM login_attempts WHERE login = 'petrova' AND external").execute(pool).await.unwrap();

    // Роль обязывает: снаружи без кода — просьба ввести код; кода нет — тот же ответ, что неверный код.
    let (c, sec) = admin.get("/api/admin/security").await;
    assert_eq!(c, StatusCode::OK, "{sec}");
    assert_eq!(sec["totp_required_roles"], json!([]), "по умолчанию код никому не обязателен");
    let mut strict = sec.clone();
    strict["totp_required_roles"] = json!(["superadmin"]);
    assert_eq!(admin.put("/api/admin/security", strict).await.0, StatusCode::OK);
    assert_eq!(admin.get("/api/me/totp").await.1["required"], true);
    assert_eq!(inside.get("/api/me/totp").await.1["required"], false);
    let mut boss = Browser::new(app.clone());
    boss.ip = OUTSIDE.into();
    let (c, body) = boss.login("ivanov", USER_PASS).await;
    assert_eq!((c, body["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("otp_required")), "{body}");
    let (c, body) = boss.login_code("ivanov", USER_PASS, "123456").await;
    assert_eq!((c, body["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("invalid_otp")), "{body}");
    assert_eq!(admin.put("/api/admin/security", sec).await.0, StatusCode::OK);
    sqlx::query("DELETE FROM login_attempts WHERE login = 'ivanov' AND external").execute(pool).await.unwrap();

    // Подключить можно только из сети организации, даже с открытым сеансом.
    inside.ip = OUTSIDE.into();
    let (c, st) = inside.get("/api/me/totp").await;
    assert_eq!((c, st["enabled"].as_bool(), st["can_change"].as_bool()), (StatusCode::OK, Some(false), Some(false)));
    let (c, body) = inside.post("/api/me/totp/setup", json!({})).await;
    assert_eq!((c, body["error"].as_str()), (StatusCode::FORBIDDEN, Some("outside_network")), "{body}");
    inside.ip = INSIDE.into();
    let (c, setup) = inside.post("/api/me/totp/setup", json!({})).await;
    assert_eq!(c, StatusCode::OK, "{setup}");
    let secret = setup["secret"].as_str().unwrap().to_string();
    let uri = setup["uri"].as_str().unwrap();
    assert!(uri.starts_with("otpauth://totp/") && uri.contains(&secret) && uri.contains("petrova"), "{uri}");
    let key = totp::base32_decode(&secret).unwrap();
    let stored: Vec<u8> = sqlx::query_scalar(
        "SELECT u.totp_secret FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id WHERE a.sam_account_name = 'petrova'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert!(!stored.windows(secret.len()).any(|w| w == secret.as_bytes()), "секрет в базе зашифрован");
    let right = code(&key, now());
    let wrong = format!("{:06}", (right.parse::<u32>().unwrap() + 1) % 1_000_000);
    let (c, body) = inside.post("/api/me/totp/confirm", json!({"code": wrong})).await;
    assert_eq!((c, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_otp")), "{body}");
    assert_eq!(inside.get("/api/me/totp").await.1["enabled"], false, "без подтверждения не включён");
    assert_eq!(inside.post("/api/me/totp/confirm", json!({"code": right})).await.0, StatusCode::NO_CONTENT);
    assert_eq!(inside.get("/api/me/totp").await.1["enabled"], true);
    let (c, body) = inside.post("/api/me/totp/setup", json!({})).await;
    assert_eq!((c, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("totp_enabled")), "не перезаписать: {body}");
    let (_, users) = admin.get("/api/admin/users?q=petrova").await;
    assert_eq!(users[0]["totp"], true, "{users}");

    // Подключила — снаружи без кода уже не войти.
    let (c, body) = out.login("petrova", USER_PASS).await;
    assert_eq!((c, body["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("otp_required")), "{body}");
    // И по UPN, начало которого не совпадает с логином: код спрашивается до AD (UPN из зеркала)…
    let mut by_upn = Browser::new(app.clone());
    by_upn.ip = OUTSIDE.into();
    let (c, body) = by_upn.login("Anna.Petrova@pult.test", USER_PASS).await;
    assert_eq!((c, body["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("otp_required")), "{body}");
    // …и после AD, если зеркало UPN не знает: без кода сеанса нет.
    sqlx::query("UPDATE ad_accounts SET upn = NULL WHERE sam_account_name = 'petrova'").execute(pool).await.unwrap();
    let (c, body) = by_upn.login("anna.petrova@pult.test", USER_PASS).await;
    assert_eq!((c, body["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("otp_required")), "{body}");
    assert_eq!(by_upn.get("/api/me").await.0, StatusCode::UNAUTHORIZED, "сеанс не выдан");
    sqlx::query("UPDATE ad_accounts SET upn = 'anna.petrova@pult.test' WHERE sam_account_name = 'petrova'")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM login_attempts WHERE login = 'petrova' AND external").execute(pool).await.unwrap();
    let (_, body) = out.login_code("petrova", USER_PASS, &wrong).await;
    assert_eq!(body["error"], "invalid_otp", "{body}");

    // Код подтверждения второй раз не входит.
    let (_, body) = out.login_code("petrova", USER_PASS, &right).await;
    assert_eq!(body["error"], "invalid_otp", "повтор кода: {body}");
    // Неверный код с неверным паролем до AD не доходит: в домене неудачи не копятся.
    let (_, body) = out.login_code("petrova", "wrong-password", &wrong).await;
    assert_eq!(body["error"], "invalid_otp", "{body}");
    // Верный код и неверный пароль — отказ AD; код при этом израсходован.
    let next = code(&key, now() + 1);
    let (_, body) = out.login_code("petrova", "wrong-password", &next).await;
    assert_eq!(body["error"], "invalid_credentials", "{body}");
    let (_, body) = out.login_code("petrova", USER_PASS, &next).await;
    assert_eq!(body["error"], "invalid_otp", "{body}");
    // Пять неудач снаружи — окно закрыто и для верного кода; ждать 5 минут тест не будет.
    let (c, _) = out.login_code("petrova", USER_PASS, &code(&key, now())).await;
    assert_eq!(c, StatusCode::TOO_MANY_REQUESTS, "лимит попыток снаружи");
    sqlx::query("DELETE FROM login_attempts WHERE login = 'petrova' AND external").execute(pool).await.unwrap();
    reset_steps().await;
    let (c, body) = out.login_code("petrova", USER_PASS, &code(&key, now())).await;
    assert_eq!(c, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(out.get("/api/me").await.1["login"], "petrova");
    let external: Option<bool> = sqlx::query_scalar(
        "SELECT (details->>'external')::boolean FROM audit_log WHERE action = 'auth.login' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(external, Some(true), "в аудите видно, что вход из интернета");

    // Перебор паролей снаружи (5 неудач) закрывает вход снаружи, но не в офисе.
    let mut attacker = Browser::new(app.clone());
    attacker.ip = "198.51.100.9:4000".into();
    for _ in 0..5 {
        assert_eq!(attacker.login("sidorov", "x").await.1["error"], "invalid_credentials");
    }
    assert_eq!(attacker.login("sidorov", "x").await.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(Browser::new(app.clone()).login("sidorov", USER_PASS).await.0, StatusCode::NO_CONTENT, "в офисе входит");
    // За сутки 20 неудач по логину с разных адресов — снаружи закрыто даже с верным кодом.
    sqlx::query(
        "INSERT INTO login_attempts (login, ip, result, external, ts)
         SELECT 'petrova', '192.0.2.1', 'invalid_otp', true, now() - interval '1 hour' FROM generate_series(1, 20)",
    )
    .execute(pool)
    .await
    .unwrap();
    reset_steps().await;
    let mut late = Browser::new(app.clone());
    late.ip = "192.0.2.77:4000".into();
    let (c, body) = late.login_code("petrova", USER_PASS, &code(&key, now())).await;
    assert_eq!(c, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(Browser::new(app.clone()).login("petrova", USER_PASS).await.0, StatusCode::NO_CONTENT, "в офисе входит");

    // Отключить — только изнутри и с кодом.
    inside.ip = OUTSIDE.into();
    assert_eq!(inside.post("/api/me/totp/disable", json!({"code": code(&key, now())})).await.0, StatusCode::FORBIDDEN);
    inside.ip = INSIDE.into();
    let (c, body) = inside.post("/api/me/totp/disable", json!({"code": "12345"})).await;
    assert_eq!((c, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_otp")), "{body}");

    // Потерян телефон: сброс суперадминистратором закрывает и все сеансы, в том числе открытый снаружи.
    let id = users[0]["id"].as_str().unwrap();
    let mut emp = Browser::new(app.clone());
    assert_eq!(emp.login("sidorov", USER_PASS).await.0, StatusCode::NO_CONTENT);
    assert_eq!(emp.delete(&format!("/api/admin/users/{id}/totp")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(admin.delete(&format!("/api/admin/users/{id}/totp")).await.0, StatusCode::NO_CONTENT);
    assert_eq!(out.get("/api/me").await.0, StatusCode::UNAUTHORIZED, "сеанс с телефона закрыт");
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'auth.totp_reset' AND object_id = $1")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(n, 1);

    // Подключить заново и отключить своим кодом.
    let mut again = Browser::new(app.clone());
    assert_eq!(again.login("petrova", USER_PASS).await.0, StatusCode::NO_CONTENT);
    assert_eq!(again.get("/api/me/totp").await.1["enabled"], false);
    let (_, setup) = again.post("/api/me/totp/setup", json!({})).await;
    let key = totp::base32_decode(setup["secret"].as_str().unwrap()).unwrap();
    assert_eq!(again.post("/api/me/totp/confirm", json!({"code": code(&key, now())})).await.0, StatusCode::NO_CONTENT);
    let (c, body) = again.post("/api/me/totp/disable", json!({"code": code(&key, now() + 1)})).await;
    assert_eq!(c, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(again.get("/api/me/totp").await.1["enabled"], false);
    for action in ["auth.totp_enabled", "auth.totp_disabled"] {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = $1")
            .bind(action)
            .fetch_one(pool)
            .await
            .unwrap();
        assert!(n >= 1, "{action} в аудите");
    }

    // Неверные коды в профиле считаются: украденным сеансом код не перебрать. Две неудачи уже были выше.
    let (_, setup) = again.post("/api/me/totp/setup", json!({})).await;
    let key = totp::base32_decode(setup["secret"].as_str().unwrap()).unwrap();
    let valid: Vec<String> = (now() - 1..=now() + 2).map(|s| code(&key, s)).collect();
    let bad = (0..1_000_000u32).map(|n| format!("{n:06}")).find(|c| !valid.contains(c)).unwrap();
    for _ in 0..3 {
        let (c, body) = again.post("/api/me/totp/confirm", json!({"code": bad})).await;
        assert_eq!((c, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_otp")), "{body}");
    }
    let (c, _) = again.post("/api/me/totp/confirm", json!({"code": code(&key, now())})).await;
    assert_eq!(c, StatusCode::TOO_MANY_REQUESTS, "после 5 неверных кодов — пауза даже для верного");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'auth.totp_code_failed'")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(n, 5, "неверные коды — в аудите");
}

/// Вход через домен: билет уже проверил сервер (GSSAPI) — здесь путь от принципала до сеанса.
/// Поддельный провайдер OpenID Connect: что подписать в следующем ID-токене и какой PKCE ждать.
#[derive(Default)]
struct Idp {
    challenge: String,
    nonce: String,
    user: String,
    /// Свой `sub` вместо логина: другой пользователь провайдера с тем же логином.
    sub: Option<String>,
}

/// Запрос мимо `Browser`: нужны заголовки `Location` и `Set-Cookie` (как `имя=значение`).
async fn raw(app: &Router, uri: &str, cookie: Option<&str>, ip: &str) -> (StatusCode, String, Vec<String>) {
    let mut req = Request::builder().uri(uri);
    if let Some(c) = cookie {
        req = req.header(header::COOKIE, c);
    }
    let mut req = req.body(Body::empty()).unwrap();
    req.extensions_mut().insert(ConnectInfo(ip.parse::<SocketAddr>().unwrap()));
    let r = app.clone().oneshot(req).await.unwrap();
    let loc = r.headers().get(header::LOCATION).map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
    let cookies = r
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string())
        .collect();
    (r.status(), loc, cookies)
}

/// Вход через провайдера целиком: начало, «согласие» у провайдера, возврат. Ответ — куда отправил портал.
async fn oidc_round(
    app: &Router,
    idp: &std::sync::Mutex<Idp>,
    user: &str,
    ip: &str,
    tamper: bool,
) -> (String, Vec<String>) {
    let (code, loc, cookies) = raw(app, "/api/auth/oidc/start", None, ip).await;
    assert_eq!(code, StatusCode::SEE_OTHER);
    let Ok(url) = reqwest::Url::parse(&loc) else { return (loc, cookies) };
    let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!((q["code_challenge_method"].as_str(), q["client_id"].as_str()), ("S256", "pult"), "{loc}");
    {
        let mut i = idp.lock().unwrap();
        i.challenge = q["code_challenge"].clone();
        i.nonce = q["nonce"].clone();
        i.user = user.to_string();
    }
    let state = if tamper { "forged" } else { q["state"].as_str() };
    let jar = cookies.join("; ");
    let (code, loc, cookies) =
        raw(app, &format!("/api/auth/oidc/callback?code=good&state={state}"), Some(&jar), ip).await;
    assert_eq!(code, StatusCode::SEE_OTHER);
    (loc, cookies)
}

async fn oidc(admin: &mut Browser, app: &Router, pool: &PgPool) {
    use axum::{
        Form,
        routing::{get, post},
    };
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use pult_core::settings::{self, SecuritySettings};
    use ring::{
        rand::SystemRandom,
        signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
    };
    use std::sync::{Arc, Mutex};

    const IN: &str = "10.1.1.9:5000";
    const OUT: &str = "203.0.113.5:5000";
    assert_eq!(raw(app, "/api/auth/oidc/start", None, IN).await.0, StatusCode::NOT_FOUND, "выключено по умолчанию");

    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
    let kp = Arc::new(EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap());
    let pk = kp.public_key().as_ref().to_vec();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let idp = Arc::new(Mutex::new(Idp::default()));
    let meta = json!({"issuer": base, "authorization_endpoint": format!("{base}/auth"),
        "token_endpoint": format!("{base}/token"), "jwks_uri": format!("{base}/jwks")});
    let jwks = json!({"keys": [{"kty": "EC", "crv": "P-256", "kid": "k1", "use": "sig",
        "x": URL_SAFE_NO_PAD.encode(&pk[1..33]), "y": URL_SAFE_NO_PAD.encode(&pk[33..65])}]});
    let token = {
        let (idp, kp, iss) = (idp.clone(), kp.clone(), base.clone());
        move |Form(f): Form<HashMap<String, String>>| {
            let (idp, kp, iss) = (idp.clone(), kp.clone(), iss.clone());
            async move {
                let i = idp.lock().unwrap();
                let field = |k: &str| f.get(k).map(String::as_str).unwrap_or_default();
                let pkce = URL_SAFE_NO_PAD
                    .encode(ring::digest::digest(&ring::digest::SHA256, field("code_verifier").as_bytes()));
                if field("code") != "good"
                    || pkce != i.challenge
                    || field("client_secret") != "s3cret"
                    || field("client_id") != "pult"
                {
                    return (StatusCode::BAD_REQUEST, axum::Json(json!({"error": "invalid_grant"})));
                }
                let now = time::OffsetDateTime::now_utc().unix_timestamp();
                let sub = i.sub.clone().unwrap_or_else(|| i.user.to_lowercase());
                let claims = json!({"iss": iss, "aud": "pult", "sub": sub, "exp": now + 300, "iat": now,
                    "nonce": i.nonce, "preferred_username": i.user});
                let msg = format!(
                    "{}.{}",
                    URL_SAFE_NO_PAD.encode(r#"{"alg":"ES256","kid":"k1"}"#),
                    URL_SAFE_NO_PAD.encode(claims.to_string())
                );
                let sig = kp.sign(&SystemRandom::new(), msg.as_bytes()).unwrap();
                let id_token = format!("{msg}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()));
                (StatusCode::OK, axum::Json(json!({"id_token": id_token, "token_type": "Bearer"})))
            }
        }
    };
    let server = axum::Router::new()
        .route("/.well-known/openid-configuration", get(move || std::future::ready(axum::Json(meta.clone()))))
        .route("/jwks", get(move || std::future::ready(axum::Json(jwks.clone()))))
        .route("/token", post(token));
    tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });

    // Настройка: адрес возврата проверяется, секрет не показывается.
    let mut s = json!({"enabled": true, "issuer": base, "client_id": "pult",
        "redirect_uri": "https://pult.test/", "username_claim": "preferred_username"});
    let (code, b) = admin.put("/api/admin/oidc", json!({"settings": s})).await;
    assert_eq!((code, b["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_settings")), "{b}");
    s["redirect_uri"] = json!("https://pult.test/api/auth/oidc/callback");
    let (code, b) = admin.put("/api/admin/oidc", json!({"settings": s, "client_secret": "s3cret"})).await;
    assert_eq!((code, b["has_secret"].as_bool()), (StatusCode::OK, Some(true)), "{b}");
    assert!(!admin.get("/api/admin/oidc").await.1.to_string().contains("s3cret"));
    let (_, t) = admin.post("/api/admin/oidc/test", json!({})).await;
    assert_eq!((t["ok"].as_bool(), t["keys"].as_i64()), (Some(true), Some(1)), "{t}");
    let mut other = s.clone();
    other["client_id"] = json!("other");
    let (code, b) = admin.put("/api/admin/oidc", json!({"settings": other})).await;
    assert_eq!((code, b["error"].as_str()), (StatusCode::BAD_REQUEST, Some("secret_required")), "{b}");

    // Вход: на главную с cookie сеанса; сеанс — «oidc».
    let (loc, cookies) = oidc_round(app, &idp, "Ivanov", IN, false).await;
    assert_eq!(loc, "/", "{cookies:?}");
    let mut b = Browser::new(app.clone());
    for c in &cookies {
        let (k, v) = c.split_once('=').unwrap();
        if !v.is_empty() {
            b.cookies.insert(k.into(), v.into());
        }
    }
    assert!(!b.cookies.contains_key("__Host-pult_oidc"), "cookie входа снята");
    let (code, me) = b.get("/api/me").await;
    assert_eq!((code, me["login"].as_str()), (StatusCode::OK, Some("ivanov")), "{me}");
    let method: String = sqlx::query_scalar(
        "SELECT s.auth_method FROM sessions s JOIN users u ON u.id = s.user_id JOIN ad_accounts a ON a.id = u.ad_account_id
          WHERE a.sam_account_name = 'ivanov' ORDER BY s.created_at DESC LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(method, "oidc");
    // UPN тоже логин; чужой state, отключённая и неизвестная учётки — отказ.
    assert_eq!(oidc_round(app, &idp, "anna.petrova@pult.test", IN, false).await.0, "/");
    assert_eq!(oidc_round(app, &idp, "ivanov", IN, true).await.0, "/?oidc_error=state");
    assert_eq!(oidc_round(app, &idp, "uvolen", IN, false).await.0, "/?oidc_error=account_disabled");
    assert_eq!(oidc_round(app, &idp, "ghost", IN, false).await.0, "/?oidc_error=no_account");
    // Переименовался у провайдера в чужой логин: `sub` уже привязан к другому пользователю.
    idp.lock().unwrap().sub = Some("someone-else".into());
    assert_eq!(oidc_round(app, &idp, "ivanov", IN, false).await.0, "/?oidc_error=identity_mismatch");
    idp.lock().unwrap().sub = None;
    let failed: i64 = sqlx::query_scalar("SELECT count(*) FROM login_attempts WHERE result = 'identity_mismatch'")
        .fetch_one(pool)
        .await
        .unwrap();
    assert_eq!(failed, 0, "отказы провайдера не копятся в лимитах подбора пароля");
    let (_, loc, _) = raw(app, "/api/auth/oidc/callback?code=good&state=x", None, IN).await;
    assert_eq!(loc, "/?oidc_error=state", "без cookie входа");
    // Из интернета — только если разрешено, и не тем, кому снаружи нужен код.
    assert_eq!(oidc_round(app, &idp, "ivanov", OUT, false).await.0, "/?oidc_error=external");
    s["allow_external"] = json!(true);
    assert_eq!(admin.put("/api/admin/oidc", json!({"settings": s})).await.0, StatusCode::OK);
    let mut sec: SecuritySettings = settings::load(pool).await.unwrap();
    let before = sec.totp_required_roles.clone();
    sec.totp_required_roles = vec![pult_core::rbac::ROLE_SUPERADMIN.into()];
    settings::save(pool, &sec, None).await.unwrap();
    assert_eq!(oidc_round(app, &idp, "ivanov", OUT, false).await.0, "/?oidc_error=otp_required");
    assert_eq!(oidc_round(app, &idp, "sidorov", OUT, false).await.0, "/", "без обязательного кода");
    sec.totp_required_roles = before;
    settings::save(pool, &sec, None).await.unwrap();
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'auth.login' AND details->>'method' = 'oidc'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(audited, 3);
    let (_, v) = admin.get("/api/admin/oidc").await;
    assert_eq!(v["identities"], 3, "{v}");
    let (code, _) = admin.delete("/api/admin/oidc/identities").await;
    assert_eq!(code, StatusCode::OK);
    let (_, v) = admin.get("/api/admin/oidc").await;
    assert_eq!(v["identities"], 0, "{v}");
    s["enabled"] = json!(false);
    assert_eq!(admin.put("/api/admin/oidc", json!({"settings": s})).await.0, StatusCode::OK);
}

async fn kerberos(core: &Core, pool: &PgPool) {
    use axum::response::IntoResponse;
    use pult_core::settings::{self, SecuritySettings};
    let parts = |ip: &str| {
        let mut req = Request::builder().uri("/api/auth/kerberos").body(()).unwrap();
        req.extensions_mut().insert(ConnectInfo(ip.parse::<SocketAddr>().unwrap()));
        req.into_parts().0
    };
    let status = |r: Result<axum::response::Response, pult_core::ApiError>| match r {
        Ok(r) => r.status(),
        Err(e) => e.into_response().status(),
    };
    let inside = parts("10.1.1.9:5000");
    let login = |p: &'static str| pult_people::auth::kerberos_login(core, &inside, p);
    assert_eq!(status(login("ivanov@PULT.TEST").await), StatusCode::NOT_FOUND, "выключено по умолчанию");
    let mut sec: SecuritySettings = settings::load(pool).await.unwrap();
    sec.kerberos_login = true;
    settings::save(pool, &sec, None).await.unwrap();
    let r = login("Ivanov@PULT.TEST").await.unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    assert!(r.headers().get_all(header::SET_COOKIE).iter().count() >= 2, "cookie сеанса и CSRF");
    let method: String = sqlx::query_scalar(
        "SELECT s.auth_method FROM sessions s JOIN users u ON u.id = s.user_id JOIN ad_accounts a ON a.id = u.ad_account_id
          WHERE a.sam_account_name = 'ivanov' ORDER BY s.created_at DESC LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(method, "kerberos");
    for p in ["ghost@PULT.TEST", "ivanov@OTHER.TEST", "HTTP/portal.pult.test@PULT.TEST", "ivanov", "@PULT.TEST"] {
        assert_eq!(status(login(p).await), StatusCode::FORBIDDEN, "{p}");
    }
    assert_eq!(status(login("uvolen@PULT.TEST").await), StatusCode::BAD_REQUEST, "отключённая учётка");
    // Чужой UPN с тем же началом, что логин Иванова: неясно, чей билет, — отказ, а не вход за Иванова.
    let old_upn: Option<String> = sqlx::query_scalar("SELECT upn FROM ad_accounts WHERE sam_account_name = 'petrova'")
        .fetch_one(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE ad_accounts SET upn = 'Ivanov@pult.test' WHERE sam_account_name = 'petrova'")
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(status(login("ivanov@PULT.TEST").await), StatusCode::FORBIDDEN, "имя совпадает у двух учёток");
    sqlx::query("UPDATE ad_accounts SET upn = $1 WHERE sam_account_name = 'petrova'")
        .bind(&old_upn)
        .execute(pool)
        .await
        .unwrap();
    let outside = parts("203.0.113.5:5000");
    assert_eq!(
        status(pult_people::auth::kerberos_login(core, &outside, "ivanov@PULT.TEST").await),
        StatusCode::BAD_REQUEST,
        "из интернета вход через домен закрыт"
    );
    let audited: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'auth.login' AND details->>'method' = 'kerberos'",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);
    sec.kerberos_login = false;
    settings::save(pool, &sec, None).await.unwrap();
}
