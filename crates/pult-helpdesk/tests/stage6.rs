//! Сквозной сценарий: настройка организации в мастере установки.
//! Нужен `PULT_TEST_DATABASE_URL` (в CI задан). Тест создаёт отдельную базу.
//!
//! Сценарий: новая установка — мастер не пройден, все модули включены; суперадминистратор задаёт
//! название и логотип, выключает модуль (его API отвечает 404 и снова работает после включения),
//! переводит стартовые данные на язык установки (изменённое администратором не трогается),
//! завершает мастер. Сотрудник ничего из этого сделать не может.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, FromRef},
    http::{HeaderMap, Method, Request, StatusCode, header},
    middleware,
};
use base64::Engine as _;
use http_body_util::BodyExt;
use pult_core::{Core, Module, Secret, api::CoreModule, crypto::SecretBox, l10n::Lang, session::CookiePolicy};
use pult_helpdesk::{HelpdeskState, api::HelpdeskModule};
use pult_people::{PeopleModule, PeopleState};
use pult_testkit::{BASE_DN, Directory, FakeDc, FakeUser, SVC_DN, SVC_PASS, TestCa, USER_FILTER, USER_PASS};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, PgPool};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tower::ServiceExt;

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
    p
}

struct Browser {
    app: Router,
    cookies: HashMap<String, String>,
}

impl Browser {
    async fn raw(&mut self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, HeaderMap, Vec<u8>) {
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
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let mut req = req.header(header::CONTENT_TYPE, "application/json").body(Body::from(body)).unwrap();
        req.extensions_mut().insert(ConnectInfo("10.1.1.1:5000".parse::<SocketAddr>().unwrap()));
        let resp = self.app.clone().oneshot(req).await.unwrap();
        for v in resp.headers().get_all(header::SET_COOKIE) {
            let s = v.to_str().unwrap();
            let (kv, _) = s.split_once(';').unwrap_or((s, ""));
            let (k, v) = kv.split_once('=').unwrap();
            self.cookies.insert(k.to_string(), v.to_string());
        }
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
        (status, headers, bytes)
    }

    async fn send(&mut self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        let (code, _, bytes) = self.raw(method, uri, body).await;
        (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn get(&mut self, uri: &str) -> (StatusCode, Value) {
        self.send(Method::GET, uri, None).await
    }

    async fn put(&mut self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.send(Method::PUT, uri, Some(body)).await
    }

    async fn anonymous(app: &Router) -> Self {
        Self { app: app.clone(), cookies: HashMap::new() }
    }

    async fn login(app: &Router, login: &str) -> Self {
        let mut b = Self::anonymous(app).await;
        let (code, _) =
            b.send(Method::POST, "/api/auth/login", Some(json!({"login": login, "password": USER_PASS}))).await;
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
    let name = format!("pult_stage6_{}", std::process::id());
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

async fn names(pool: &PgPool, sql: &str) -> Vec<String> {
    sqlx::query_scalar(sql).fetch_all(pool).await.unwrap()
}

/// Самый маленький PNG: 1×1, прозрачный.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49,
    0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
];

#[tokio::test]
async fn stage6_acceptance() {
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
        FakeUser::person("emp", "Сотрудников", "Иван", 5),
    ]);
    let ca = TestCa::new("Pult Test CA");
    let dc = FakeDc::start(&ca, dir.clone()).await;
    sqlx::query(
        "INSERT INTO ad_domains (dns_name, base_dn, controllers, tls_mode, bind_dn, bind_secret, ca_cert_pem, user_filter, timeout_secs)
         VALUES ('pult.test', $1, $2, 'ldaps', $3, $4, $5, $6, 2)",
    )
    .bind(BASE_DN)
    .bind(vec![dc.endpoint()])
    .bind(SVC_DN)
    .bind(core.secrets.encrypt(&Secret::new(SVC_PASS)).unwrap())
    .bind(ca.pem())
    .bind(USER_FILTER)
    .execute(&pool)
    .await
    .unwrap();
    let st = St {
        core: core.clone(),
        people: PeopleState { ad: Default::default(), initial_superadmin: Some("admin".into()) },
        helpdesk: HelpdeskState { files_dir: std::env::temp_dir().join(format!("pult-stage6-{}", std::process::id())) },
    };
    let (router, _) = utoipa_axum::router::OpenApiRouter::new()
        .merge(<CoreModule as Module<St>>::routes(&CoreModule))
        .merge(<PeopleModule as Module<St>>::routes(&PeopleModule))
        .merge(<HelpdeskModule as Module<St>>::routes(&HelpdeskModule))
        .split_for_parts();
    // Как в pult-server: выключенные модули закрывает `org::gate`, язык — `l10n::scope`.
    let app: Router = router
        .layer(middleware::from_fn_with_state(core.clone(), pult_core::org::gate))
        .with_state(st.clone())
        .layer(middleware::from_fn(pult_core::l10n::scope));

    // 1. Новая установка: мастер не пройден, всё включено. Читать можно до входа (шапка и меню).
    let mut anon = Browser::anonymous(&app).await;
    let (code, org) = anon.get("/api/org").await;
    assert_eq!(code, StatusCode::OK, "{org}");
    assert_eq!(org["setup_done"], false);
    assert_eq!(org["name"], "");
    assert_eq!(org["logo"], Value::Null);
    for m in ["helpdesk", "phonebook", "directory", "remote", "telephony"] {
        assert_eq!(org["modules"][m], true, "{m}");
    }

    let mut admin = Browser::login(&app, "admin").await;
    let mut emp = Browser::login(&app, "emp").await;
    let all_on = json!({"helpdesk": true, "phonebook": true, "directory": true, "remote": true, "telephony": true});

    // 2. Сотрудник настройки не меняет.
    let (code, _) = emp.put("/api/admin/org", json!({"name": "X", "modules": all_on, "setup_done": true})).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let (code, _) =
        emp.put("/api/admin/org/logo", json!({"data": base64::engine::general_purpose::STANDARD.encode(PNG)})).await;
    assert_eq!(code, StatusCode::FORBIDDEN);

    // 3. Проверки: телефония без справочника, название в несколько строк.
    let mut bad = all_on.clone();
    bad["phonebook"] = json!(false);
    let (code, e) = admin.put("/api/admin/org", json!({"name": "Ромашка", "modules": bad, "setup_done": false})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{e}");
    let (code, _) = admin.put("/api/admin/org", json!({"name": "a\nb", "modules": all_on, "setup_done": false})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    let (code, _) =
        admin.put("/api/admin/org", json!({"name": "я".repeat(101), "modules": all_on, "setup_done": false})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);

    // 4. Название и выключенный helpdesk: его API для всех — 404, остальное работает.
    let mut off = all_on.clone();
    off["helpdesk"] = json!(false);
    let (code, org) =
        admin.put("/api/admin/org", json!({"name": "  ООО «Ромашка»  ", "modules": off, "setup_done": false})).await;
    assert_eq!(code, StatusCode::OK, "{org}");
    assert_eq!(org["name"], "ООО «Ромашка»");
    for uri in ["/api/helpdesk/categories", "/api/kb/articles", "/api/admin/helpdesk/categories"] {
        let (code, _) = admin.get(uri).await;
        assert_eq!(code, StatusCode::NOT_FOUND, "{uri}");
    }
    let (code, _) = emp.get("/api/me").await;
    assert_eq!(code, StatusCode::OK);
    let (_, org) = anon.get("/api/org").await;
    assert_eq!(org["modules"]["helpdesk"], false);
    let audit: Vec<Value> = sqlx::query_scalar(
        "SELECT details FROM audit_log WHERE action = 'settings.update' AND object_id = 'org' ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(audit.len(), 1, "неудачные попытки в аудит не пишутся");
    assert!(audit[0].to_string().contains("helpdesk"), "{}", audit[0]);

    // 5. Включили обратно — сразу работает (кэш сброшен), данные на месте.
    let (code, _) =
        admin.put("/api/admin/org", json!({"name": "ООО «Ромашка»", "modules": all_on, "setup_done": false})).await;
    assert_eq!(code, StatusCode::OK);
    let (code, cats) = emp.get("/api/helpdesk/categories").await;
    assert_eq!(code, StatusCode::OK, "{cats}");
    assert!(cats.to_string().contains("Компьютер и периферия"), "{cats}");

    // 6. Логотип: только PNG/JPEG/WebP до 256 КБ; отдаётся без входа; удаляется.
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>";
    for data in [b64(svg), "не base64".into(), b64(&[PNG, &vec![0u8; 256 * 1024]].concat())] {
        let (code, e) = admin.put("/api/admin/org/logo", json!({"data": data})).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{e}");
        assert_eq!(e["error"], "invalid_logo");
    }
    let (code, org) = admin.put("/api/admin/org/logo", json!({"data": b64(PNG)})).await;
    assert_eq!(code, StatusCode::OK, "{org}");
    let version = org["logo"].as_str().unwrap().to_string();
    assert_eq!(org["name"], "ООО «Ромашка»", "логотип не сбрасывает название");
    let (code, h, body) = anon.raw(Method::GET, &format!("/api/org/logo?v={version}"), None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(h[header::CONTENT_TYPE], "image/png");
    assert_eq!(body, PNG);
    let (_, org) = anon.get("/api/org").await;
    assert_eq!(org["logo"], version.as_str());
    // Название не сбрасывает логотип.
    let (_, org) =
        admin.put("/api/admin/org", json!({"name": "Ромашка", "modules": all_on, "setup_done": false})).await;
    assert_eq!(org["logo"], version.as_str());
    let (code, org) = admin.send(Method::DELETE, "/api/admin/org/logo", None).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(org["logo"], Value::Null);
    let (code, _, _) = anon.raw(Method::GET, "/api/org/logo", None).await;
    assert_eq!(code, StatusCode::NOT_FOUND);

    // 7. Стартовые данные на языке установки. Переименованное администратором остаётся как есть.
    sqlx::query("UPDATE ticket_categories SET name = 'Почта и календарь' WHERE name = 'Почта'")
        .execute(&pool)
        .await
        .unwrap();
    // Свой тип «other» уже есть: стартовый «Прочее» не переименовывается в занятое имя и не роняет перевод.
    sqlx::query("INSERT INTO asset_types (name) VALUES ('other')").execute(&pool).await.unwrap();
    let n = pult_helpdesk::starter::localize(&mut pool.acquire().await.unwrap(), Lang::En).await.unwrap();
    assert!(n > 20, "переведено {n}");
    assert_eq!(
        pult_helpdesk::starter::localize(&mut pool.acquire().await.unwrap(), Lang::En).await.unwrap(),
        0,
        "повтор ничего не меняет"
    );
    let cats = names(&pool, "SELECT name FROM ticket_categories ORDER BY sort").await;
    assert!(cats.contains(&"Computer and peripherals".to_string()), "{cats:?}");
    assert!(cats.contains(&"Почта и календарь".to_string()), "{cats:?}");
    assert!(cats.contains(&"Access request".to_string()), "{cats:?}");
    let types: Vec<Value> =
        sqlx::query_scalar("SELECT jsonb_build_object('name', name, 'fields', fields) FROM asset_types")
            .fetch_all(&pool)
            .await
            .unwrap();
    let types = Value::Array(types).to_string();
    assert!(types.contains("Printer / MFP") && types.contains("\"Model\"") && !types.contains("Модель"), "{types}");
    assert!(types.contains("Прочее") && types.contains("\"other\""), "занятое имя не трогаем: {types}");
    let tpl: Value = sqlx::query_scalar("SELECT to_jsonb(t) FROM ticket_templates t WHERE name = 'New employee'")
        .fetch_one(&pool)
        .await
        .unwrap();
    let items = tpl["checklist"].as_array().unwrap();
    assert!(items.iter().any(|i| i["title"] == "AD account" && i["action"] == "directory.enable"), "{tpl}");
    assert!(tpl["description"].as_str().unwrap().starts_with("Name:\nDepartment"), "{tpl}");
    let bodies = names(&pool, "SELECT body FROM response_templates ORDER BY title").await;
    assert!(bodies.iter().any(|b| b.contains("{{requester.first_name}}") && b.starts_with("Hello")), "{bodies:?}");

    // Китайский, затем обратно русский — с любого языка.
    pult_helpdesk::starter::localize(&mut pool.acquire().await.unwrap(), Lang::Zh).await.unwrap();
    let cats = names(&pool, "SELECT name FROM ticket_categories ORDER BY sort").await;
    assert!(cats.contains(&"电脑及外设".to_string()), "{cats:?}");
    pult_helpdesk::starter::localize(&mut pool.acquire().await.unwrap(), Lang::Ru).await.unwrap();
    let cats = names(&pool, "SELECT name FROM ticket_categories ORDER BY sort").await;
    assert!(
        cats.contains(&"Компьютер и периферия".to_string()) && cats.contains(&"Почта и календарь".to_string()),
        "{cats:?}"
    );

    // 8. Системные роли — на языке интерфейса.
    admin.cookies.insert("pult_lang".into(), "en".into());
    let (code, roles) = admin.get("/api/admin/roles").await;
    assert_eq!(code, StatusCode::OK);
    assert!(
        roles["roles"].as_array().unwrap().iter().any(|r| r["code"] == "engineer" && r["name"] == "Engineer"),
        "{roles}"
    );
    admin.cookies.insert("pult_lang".into(), "zh".into());
    let (_, roles) = admin.get("/api/admin/roles").await;
    assert!(
        roles["roles"].as_array().unwrap().iter().any(|r| r["code"] == "superadmin" && r["name"] == "超级管理员"),
        "{roles}"
    );
    admin.cookies.remove("pult_lang");

    // 9. Мастер пройден.
    let (code, org) =
        admin.put("/api/admin/org", json!({"name": "Ромашка", "modules": all_on, "setup_done": true})).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(org["setup_done"], true);
    let (_, org) = anon.get("/api/org").await;
    assert_eq!(org["setup_done"], true);

    dc.stop().await;
}
