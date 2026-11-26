//! Сквозные сценарии карточки заявки, SLA и базы знаний на настоящей PostgreSQL и поддельном DC.
//! Нужен `PULT_TEST_DATABASE_URL` (в CI задан). Тест создаёт отдельную базу.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, FromRef},
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use pult_core::{Core, Module, Secret, api::CoreModule, crypto::SecretBox, session::CookiePolicy};
use pult_helpdesk::{HelpdeskState, api::HelpdeskModule};
use pult_people::{PeopleModule, PeopleState};
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
    p
}

struct Browser {
    app: Router,
    cookies: HashMap<String, String>,
}

impl Browser {
    async fn send(&mut self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value, Vec<u8>) {
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
        let bytes = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json, bytes)
    }

    async fn get(&mut self, uri: &str) -> (StatusCode, Value) {
        let (s, j, _) = self.send(Method::GET, uri, None).await;
        (s, j)
    }

    async fn post(&mut self, uri: &str, body: Value) -> (StatusCode, Value) {
        let (s, j, _) = self.send(Method::POST, uri, Some(body)).await;
        (s, j)
    }

    async fn put(&mut self, uri: &str, body: Value) -> (StatusCode, Value) {
        let (s, j, _) = self.send(Method::PUT, uri, Some(body)).await;
        (s, j)
    }

    async fn delete(&mut self, uri: &str) -> StatusCode {
        self.send(Method::DELETE, uri, None).await.0
    }

    async fn login(app: &Router, login: &str) -> Self {
        let mut b = Self { app: app.clone(), cookies: HashMap::new() };
        let (code, body) = b.post("/api/auth/login", json!({"login": login, "password": USER_PASS})).await;
        assert_eq!(code, StatusCode::NO_CONTENT, "{login}: {body}");
        b
    }
}

/// Запрос с токеном API вместо сессии.
async fn bearer(app: &Router, method: Method, uri: &str, token: &str) -> StatusCode {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{}"))
        .unwrap();
    req.extensions_mut().insert(ConnectInfo("10.1.1.2:5000".parse::<SocketAddr>().unwrap()));
    app.clone().oneshot(req).await.unwrap().status()
}

/// Оповещение мониторинга с токеном источника.
async fn hook_call(app: &Router, uri: &str, token: &str, body: &Value) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    req.extensions_mut().insert(ConnectInfo("10.1.1.3:5000".parse::<SocketAddr>().unwrap()));
    let res = app.clone().oneshot(req).await.unwrap();
    let code = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn db() -> Option<PgPool> {
    let Ok(url) = std::env::var("PULT_TEST_DATABASE_URL") else {
        assert!(std::env::var("CI").is_err(), "в CI нужен PULT_TEST_DATABASE_URL");
        eprintln!("PULT_TEST_DATABASE_URL не задан — интеграционный тест пропущен");
        return None;
    };
    let name = format!("pult_stage3_{}", std::process::id());
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

async fn ticket(b: &mut Browser, n: i64) -> Value {
    let (code, mut t) = b.get(&format!("/api/helpdesk/tickets/{n}")).await;
    assert_eq!(code, StatusCode::OK, "{t}");
    // Поля заголовка (`ticket`) — рядом с чек-листом, наблюдателями и объектами: так короче проверки.
    if let Some(head) = t["ticket"].as_object().cloned() {
        for (k, v) in head {
            t[k] = v;
        }
    }
    t
}

async fn create(b: &mut Browser, body: Value) -> i64 {
    let (code, c) = b.post("/api/helpdesk/tickets", body).await;
    assert_eq!(code, StatusCode::CREATED, "{c}");
    c["number"].as_i64().unwrap()
}

#[tokio::test]
async fn stage3_acceptance() {
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
    let files_dir = std::env::temp_dir().join(format!("pult-stage3-{}", std::process::id()));
    let st = St {
        core: core.clone(),
        people: PeopleState { ad: Default::default(), initial_superadmin: Some("admin".into()) },
        helpdesk: HelpdeskState { files_dir },
    };
    let (router, _) = utoipa_axum::router::OpenApiRouter::new()
        .merge(<CoreModule as Module<St>>::routes(&CoreModule))
        .merge(<PeopleModule as Module<St>>::routes(&PeopleModule))
        .merge(<HelpdeskModule as Module<St>>::routes(&HelpdeskModule))
        .split_for_parts();
    let app: Router = router.with_state(st).layer(axum::middleware::from_fn(pult_core::l10n::scope));

    let mut admin = Browser::login(&app, "admin").await;
    let (code, s) = admin.post("/api/admin/ad/sync", json!({"kind": "full"})).await;
    assert_eq!(code, StatusCode::OK, "{s}");
    let mut eng1 = Browser::login(&app, "eng1").await;
    let mut eng2 = Browser::login(&app, "eng2").await;
    let mut emp = Browser::login(&app, "emp").await;
    let mut emp2 = Browser::login(&app, "emp2").await;

    let uid = |login: &'static str| {
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
    let pid = |login: &'static str| {
        let pool = pool.clone();
        async move {
            let id: Uuid = sqlx::query_scalar(
                "SELECT p.id FROM persons p JOIN ad_accounts a ON a.id = p.ad_account_id WHERE a.sam_account_name = $1",
            )
            .bind(login)
            .fetch_one(&pool)
            .await
            .unwrap();
            id
        }
    };
    let (u1, u2) = (uid("eng1").await, uid("eng2").await);
    for u in [u1, u2] {
        let (code, b) = admin.put(&format!("/api/admin/users/{u}/roles"), json!({"roles": ["engineer"]})).await;
        assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    }
    let (_, site) =
        admin.post("/api/admin/sites", json!({"name": "Офис А", "timezone": "Europe/Moscow", "is_active": true})).await;
    let site = id(&site);
    let (code, r) = admin.put(&format!("/api/admin/sites/{site}/engineers"), json!({"l1": [u1, u2], "l2": []})).await;
    assert!(code.is_success(), "{r}");
    sqlx::query(
        "UPDATE persons SET site_id = $1::uuid, email = a.mail FROM ad_accounts a WHERE a.id = persons.ad_account_id",
    )
    .bind(&site)
    .execute(&pool)
    .await
    .unwrap();
    let settings = json!({"portal_url": "https://pult.example.test", "work_start": "00:00", "work_end": "23:59", "work_days": [1,2,3,4,5,6,7],
        "timezone": "Europe/Moscow", "auto_close_days": 3, "max_file_mb": 1, "login_help_subnets": ["10.0.0.0/8"], "emergency_phone": ""});
    let (code, b) = admin.put("/api/admin/helpdesk/settings", settings).await;
    assert_eq!(code, StatusCode::OK, "{b}");
    let (_, cats) = emp.get("/api/helpdesk/categories").await;
    let cat = cats.as_array().unwrap().iter().find(|c| c["name"] == "Принтеры и сканеры").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let new = |title: &str| json!({"title": title, "description": "Описание", "site_id": site, "category_id": cat});

    // --- Календарь и SLA ---
    let week: Vec<Value> = (1..=7).map(|d| json!({"day": d, "start": "00:00", "end": "23:59"})).collect();
    let (code, cal) =
        admin.post("/api/admin/helpdesk/calendars", json!({"name": "Круглосуточно", "hours": week})).await;
    assert_eq!(code, StatusCode::CREATED, "{cal}");
    let cal = id(&cal);
    let (code, b) = admin
        .put(
            &format!("/api/admin/helpdesk/calendars/{cal}/days"),
            json!([{"day": "2026-12-31", "note": "Предпраздничный"}, {"day": "2027-01-03", "start": "10:00", "end": "15:00"}]),
        )
        .await;
    assert!(code.is_success(), "{b}");
    let (code, days) = admin.get(&format!("/api/admin/helpdesk/calendars/{cal}/days")).await;
    assert_eq!((code, days.as_array().map(Vec::len)), (StatusCode::OK, Some(2)), "{days}");
    let (code, b) = admin.put(&format!("/api/admin/sites/{site}/calendar"), json!({"calendar_id": cal})).await;
    assert!(code.is_success(), "{b}");
    // Праздник месяц назад тоже учитывается: сроки и паузы старых заявок считаются по их датам.
    let holiday: time::Date = sqlx::query_scalar(
        "INSERT INTO calendar_days (calendar_id, day, note) VALUES ($1::uuid, current_date - 30, 'Праздник') RETURNING day",
    )
    .bind(&cal)
    .fetch_one(&pool)
    .await
    .unwrap();
    let cfg: pult_helpdesk::settings::HelpdeskSettings = pult_core::settings::load(&pool).await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    let (sch, _) = pult_helpdesk::schedule::for_site(&mut tx, Some(site.parse().unwrap()), &cfg).await.unwrap();
    tx.rollback().await.unwrap();
    let noon = time::PrimitiveDateTime::new(holiday, time::Time::from_hms(12, 0, 0).unwrap());
    assert!(!sch.is_open(noon), "праздник месяц назад — нерабочий день");
    sqlx::query("DELETE FROM calendar_days WHERE calendar_id = $1::uuid AND day = $2")
        .bind(&cal)
        .bind(holiday)
        .execute(&pool)
        .await
        .unwrap();
    let (code, sla) = admin
        .post("/api/admin/helpdesk/sla", json!({"name": "Общий", "response_minutes": 60, "resolve_minutes": 480}))
        .await;
    assert_eq!(code, StatusCode::CREATED, "{sla}");
    let (code, b) = admin
        .post(
            "/api/admin/helpdesk/sla",
            json!({"name": "Критичные", "priority": "critical", "response_minutes": 15, "resolve_minutes": 120}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{b}");

    // --- Карточка: SLA, назначение, наблюдатели, отложено, чек-лист, код закрытия, трудозатраты ---
    let n1 = create(&mut emp, new("Не печатает принтер")).await;
    let t = ticket(&mut eng1, n1).await;
    assert_eq!(t["sla_policy_name"], "Общий", "{t}");
    assert_eq!(t["sla_state"], "ok");
    assert!(t["resolve_due_at"].is_string() && t["response_due_at"].is_string());
    let (code, b) = admin.post(&format!("/api/helpdesk/tickets/{n1}/assign"), json!({"assignee_id": u2})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "суперадмин назначает исполнителя: {b}");
    let t = ticket(&mut admin, n1).await;
    assert_eq!(t["assignee_id"], json!(u2));
    let emp2_person = pid("emp2").await;
    let (code, b) = eng2.post(&format!("/api/helpdesk/tickets/{n1}/watchers"), json!({"person_id": emp2_person})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (_, list) = emp2.get("/api/helpdesk/tickets?scope=watching").await;
    assert!(list.as_array().unwrap().iter().any(|t| t["number"] == n1), "наблюдатель видит заявку: {list}");
    let (code, _) = emp2.get(&format!("/api/helpdesk/tickets/{n1}")).await;
    assert_eq!(code, StatusCode::OK);
    let (code, b) = eng2
        .post(&format!("/api/helpdesk/tickets/{n1}/hold"), json!({"comment": "Ждём картридж", "minutes": 10}))
        .await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let t = ticket(&mut eng2, n1).await;
    assert_eq!(t["status"], "on_hold");
    assert_eq!(t["sla_state"], "paused");
    let (code, b) = eng2
        .post(
            &format!("/api/helpdesk/tickets/{n1}/checklist"),
            json!({"items": [{"title": "Заменить картридж", "assignee_id": u1}, {"title": "Проверить печать"}]}),
        )
        .await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let items = ticket(&mut eng2, n1).await["checklist"].as_array().unwrap().clone();
    assert_eq!(items.len(), 2);
    let (code, b) = eng2
        .post(&format!("/api/helpdesk/tickets/{n1}/resolve"), json!({"comment": "Готово", "closure_code": "solved"}))
        .await;
    assert_eq!((code, b["error"].as_str()), (StatusCode::BAD_REQUEST, Some("checklist_open")), "{b}");
    for it in &items {
        let (code, b) = eng2
            .put(&format!("/api/helpdesk/tickets/{n1}/checklist/{}", it["id"].as_str().unwrap()), json!({"done": true}))
            .await;
        assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    }
    let (code, b) = eng2.post(&format!("/api/helpdesk/tickets/{n1}/resolve"), json!({"comment": "Готово"})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "код закрытия обязателен: {b}");
    let (code, b) = eng2
        .post(
            &format!("/api/helpdesk/tickets/{n1}/resolve"),
            json!({"comment": "Заменили картридж", "closure_code": "solved", "minutes": 30}),
        )
        .await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let t = ticket(&mut eng2, n1).await;
    assert_eq!((t["closure_code"].as_str(), t["work_minutes"].as_i64()), (Some("solved"), Some(40)), "{t}");
    // Заявителю и наблюдателю — без внутренней кухни: чек-лист инженеров, трудозатраты, политика SLA, VIP.
    for b in [&mut emp, &mut emp2] {
        let t = ticket(b, n1).await;
        assert_eq!(
            (
                t["checklist"].as_array().map(Vec::len),
                t["work_minutes"].as_i64(),
                t["sla_policy_name"].is_null(),
                t["requester_vip"].as_bool()
            ),
            (Some(0), Some(0), true, Some(false)),
            "{t}"
        );
    }
    let (code, b) = emp2.post(&format!("/api/helpdesk/tickets/{n1}/rating"), json!({"value": "up"})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "оценивает только заявитель: {b}");
    let (code, b) =
        emp.post(&format!("/api/helpdesk/tickets/{n1}/rating"), json!({"value": "down", "comment": "Долго"})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    assert_eq!(ticket(&mut emp, n1).await["rating"], "down");
    // Подробный опрос: баллы 1–5 необязательны, вне диапазона — отказ (средние — в отчётах ниже).
    let (code, b) = emp.post(&format!("/api/helpdesk/tickets/{n1}/rating"), json!({"value": "down", "speed": 6})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{b}");
    let (code, b) = emp
        .post(
            &format!("/api/helpdesk/tickets/{n1}/rating"),
            json!({"value": "down", "comment": "Долго", "speed": 2, "quality": 4}),
        )
        .await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let t = ticket(&mut emp, n1).await;
    assert_eq!(
        (t["rating_speed"].as_i64(), t["rating_quality"].as_i64(), t["rating_courtesy"].is_null()),
        (Some(2), Some(4), true),
        "{t}"
    );

    // --- Матрица приоритета и VIP ---
    let n2 = create(&mut emp, new("Не работает VPN")).await;
    let v = ticket(&mut eng1, n2).await["version"].clone();
    let (code, b) = eng1
        .put(
            &format!("/api/helpdesk/tickets/{n2}"),
            json!({"version": v, "impact": "organization", "urgency": "blocked"}),
        )
        .await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let t = ticket(&mut eng1, n2).await;
    assert_eq!((t["priority"].as_str(), t["sla_policy_name"].as_str()), (Some("critical"), Some("Критичные")), "{t}");
    sqlx::query("UPDATE persons SET is_vip = true WHERE id = $1").bind(pid("emp2").await).execute(&pool).await.unwrap();
    let n3 = create(&mut emp2, new("Не открывается почта")).await;
    let t = ticket(&mut eng1, n3).await;
    assert_eq!((t["priority"].as_str(), t["requester_vip"].as_bool()), (Some("high"), Some(true)), "VIP: {t}");

    // --- Группы, правила, шаблоны, регламентные ---
    let (code, g) =
        admin.post("/api/admin/helpdesk/groups", json!({"name": "Сети", "members": [u1], "auto_assign": true})).await;
    assert_eq!(code, StatusCode::CREATED, "{g}");
    let g = id(&g);
    let (code, b) = admin
        .post(
            "/api/admin/helpdesk/rules",
            json!({"name": "VPN — в сети", "trigger": "created",
                   "conditions": [{"field": "text", "op": "contains", "value": "впн"}],
                   "actions": [{"type": "set_group", "value": g}, {"type": "add_note", "value": "Правило: сети"}]}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{b}");
    let n4 = create(&mut emp, new("ВПН отваливается")).await;
    let t = ticket(&mut eng1, n4).await;
    assert_eq!(t["group_id"], json!(g), "правило назначило группу: {t}");
    let (_, list) = eng1.get("/api/helpdesk/tickets?scope=group").await;
    assert!(list.as_array().unwrap().iter().any(|t| t["number"] == n4), "{list}");
    // Заявка из очереди, переведённая в группу с автоназначением, достаётся участнику группы.
    let n7 = create(&mut emp, new("Не работает сканер")).await;
    let (code, b) = admin.post(&format!("/api/helpdesk/tickets/{n7}/assign"), json!({"assignee_id": null})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let t = ticket(&mut eng1, n7).await;
    assert!(t["assignee_id"].is_null(), "в очереди: {t}");
    let (code, b) =
        eng1.put(&format!("/api/helpdesk/tickets/{n7}"), json!({"version": t["version"], "group_id": g})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    assert_eq!(ticket(&mut eng1, n7).await["assignee_id"], json!(u1), "автоназначение в группе");
    // Правка не исполнителем не считается реакцией: статус и время реакции не трогаются.
    let n9 = create(&mut emp, new("Не открывается почта")).await;
    let t = ticket(&mut admin, n9).await;
    assert_eq!(t["status"], "assigned", "{t}");
    let (code, b) =
        admin.put(&format!("/api/helpdesk/tickets/{n9}"), json!({"version": t["version"], "priority": "high"})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let t = ticket(&mut admin, n9).await;
    assert_eq!((t["status"].as_str(), t["priority"].as_str()), (Some("assigned"), Some("high")), "{t}");
    // Правило ссылается на удалённую группу: действие не выполняется, но заявка создаётся
    // (ошибка SQL в действии не должна ломать всю транзакцию создания).
    let (_, tmp) = admin.post("/api/admin/helpdesk/groups", json!({"name": "Временная", "members": [u1]})).await;
    let tmp = id(&tmp);
    let (code, b) = admin
        .post(
            "/api/admin/helpdesk/rules",
            json!({"name": "Битая группа", "trigger": "created",
                   "conditions": [{"field": "text", "op": "contains", "value": "плоттер"}],
                   "actions": [{"type": "set_group", "value": tmp}, {"type": "add_note", "value": "Правило: плоттер"}]}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{b}");
    assert_eq!(admin.delete(&format!("/api/admin/helpdesk/groups/{tmp}")).await, StatusCode::NO_CONTENT);
    let n10 = create(&mut emp, new("Плоттер не печатает")).await;
    let t = ticket(&mut eng1, n10).await;
    assert!(t["group_id"].is_null(), "{t}");
    // Нечитаемые условия (испорчены в базе) — правило пропускается, а не срабатывает на всё.
    sqlx::query(
        "INSERT INTO automation_rules (name, trigger, conditions, actions)
         VALUES ('Испорченное', 'created', '{\"bad\": 1}', '[{\"type\": \"set_priority\", \"value\": \"critical\"}]')",
    )
    .execute(&pool)
    .await
    .unwrap();
    let n11 = create(&mut emp, new("Мышка не работает")).await;
    assert_eq!(ticket(&mut eng1, n11).await["priority"], "normal", "испорченное правило не сработало");
    sqlx::query("DELETE FROM automation_rules WHERE name IN ('Испорченное', 'Битая группа')")
        .execute(&pool)
        .await
        .unwrap();
    let (_, templates) = eng1.get("/api/helpdesk/templates").await;
    let onboarding = templates.as_array().unwrap().iter().find(|t| t["name"] == "Новый сотрудник").expect("шаблон");
    let text = onboarding["description"].as_str().unwrap();
    assert!(text.contains('\n') && !text.contains("\\n"), "переводы строк, а не «\\n»: {text:?}");
    let mut body = new("");
    body["template_id"] = onboarding["id"].clone();
    body["title"] = json!("Новый сотрудник: Петров");
    let n5 = create(&mut eng1, body).await;
    assert_eq!(ticket(&mut eng1, n5).await["checklist"].as_array().unwrap().len(), 6);
    // Шаблон устарел: исполнитель пункта больше не инженер — заявка создаётся, пункт без исполнителя.
    let stale = uid("emp").await;
    sqlx::query("UPDATE ticket_templates SET checklist = jsonb_set(checklist, '{0,assignee_id}', to_jsonb($1::text)) WHERE id = $2")
        .bind(stale.to_string())
        .bind(onboarding["id"].as_str().unwrap().parse::<Uuid>().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let mut body = new("");
    body["template_id"] = onboarding["id"].clone();
    body["title"] = json!("Новый сотрудник: Сидоров");
    let n12 = create(&mut eng1, body).await;
    let items = ticket(&mut eng1, n12).await["checklist"].clone();
    assert_eq!(items.as_array().unwrap().len(), 6, "{items}");
    assert!(items[0]["assignee_id"].is_null(), "{items}");
    sqlx::query("UPDATE ticket_templates SET checklist = checklist #- '{0,assignee_id}' WHERE id = $1")
        .bind(onboarding["id"].as_str().unwrap().parse::<Uuid>().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let (_, templates) = emp.get("/api/helpdesk/templates").await;
    assert!(templates.as_array().unwrap().iter().all(|t| t["name"] != "Новый сотрудник"), "шаблон инженеров");
    let (code, rec) = admin
        .post(
            "/api/admin/helpdesk/recurring",
            json!({"name": "Проверка бэкапов", "template_id": onboarding["id"], "schedule": "weekly", "site_id": site}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{rec}");
    let rec = id(&rec);
    let (code, r) = admin.post(&format!("/api/admin/helpdesk/recurring/{rec}/run"), json!({})).await;
    assert_eq!(code, StatusCode::OK, "{r}");
    assert!(r["number"].as_i64().is_some());
    sqlx::query("UPDATE recurring_tickets SET next_run_at = now() - interval '1 minute'").execute(&pool).await.unwrap();
    assert_eq!(pult_helpdesk::templates::run_due(&core).await.unwrap(), 1);
    assert_eq!(pult_helpdesk::templates::run_due(&core).await.unwrap(), 0, "следующий запуск — через неделю");

    // --- Объекты, оборудование, связи, слияние, массовые действия ---
    let (_, types) = eng1.get("/api/helpdesk/asset-types").await;
    let printer = types.as_array().unwrap().first().expect("типы оборудования")["id"].clone();
    let (code, asset) = eng1
        .post(
            "/api/helpdesk/assets",
            json!({"type_id": printer, "name": "МФУ 3 этаж", "inventory_number": "INV-001", "status": "in_use", "site_id": site}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{asset}");
    let asset = id(&asset);
    let (code, b) = eng1
        .post(
            "/api/helpdesk/assets",
            json!({"type_id": printer, "name": "Дубль", "inventory_number": "inv-001", "status": "in_use"}),
        )
        .await;
    assert!(code.is_client_error(), "инвентарный номер уникален: {b}");
    let (_, found) = eng1.get("/api/helpdesk/objects?q=INV-001").await;
    assert!(found.as_array().unwrap().iter().any(|o| o["kind"] == "asset"), "{found}");
    let (code, b) =
        eng1.post(&format!("/api/helpdesk/tickets/{n2}/objects"), json!({"kind": "asset", "object_id": asset})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (code, b) = eng1
        .post(&format!("/api/helpdesk/tickets/{n2}/objects"), json!({"kind": "person", "object_id": pid("emp").await}))
        .await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    assert_eq!(ticket(&mut eng1, n2).await["objects"].as_array().unwrap().len(), 2);
    let (code, card) = eng1.get(&format!("/api/helpdesk/objects/asset/{asset}")).await;
    assert_eq!(code, StatusCode::OK, "{card}");
    assert!(card["tickets"].as_array().unwrap().iter().any(|t| t["number"] == n2), "история объекта: {card}");

    // --- Оборудование при создании заявки ---
    // Сотрудник выбирает только своё оборудование (он ответственный), инженер — любое из реестра.
    // Кабинет и телефон в форму не подставляются — их вводит человек.
    let (emp_person, emp2_person) = (pid("emp").await, pid("emp2").await);
    let asset_of = |name: &str, person: Uuid, status: &str| json!({"type_id": printer, "name": name, "status": status, "responsible_person_id": person});
    let mut add_asset = async |body: Value| {
        let (code, a) = eng1.post("/api/helpdesk/assets", body).await;
        assert_eq!(code, StatusCode::CREATED, "{a}");
        id(&a)
    };
    let mine = add_asset(asset_of("Ноутбук сотрудника", emp_person, "in_use")).await;
    let foreign = add_asset(asset_of("Ноутбук коллеги", emp2_person, "in_use")).await;
    let retired = add_asset(asset_of("Старый ноутбук", emp_person, "retired")).await;
    let (code, ctx) = emp.get("/api/helpdesk/context").await;
    assert_eq!(code, StatusCode::OK, "{ctx}");
    assert!(ctx.get("phone").is_none() && ctx.get("room").is_none(), "кабинет и телефон не подставляются: {ctx}");
    let ids: Vec<&str> = ctx["assets"].as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [mine.as_str()], "в форме — только своё действующее оборудование: {ctx}");
    assert_eq!(ctx["assets"][0]["kind"], "asset");
    let with = |title: &str, assets: Vec<&str>| {
        let mut b = new(title);
        b["asset_ids"] = json!(assets);
        b
    };
    for bad in [&foreign, &retired] {
        let (code, b) = emp.post("/api/helpdesk/tickets", with("Чужое", vec![bad])).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "сотрудник не привязывает чужое или списанное: {b}");
    }
    let many: Vec<String> = (0..11).map(|_| Uuid::now_v7().to_string()).collect();
    let (code, _) = emp.post("/api/helpdesk/tickets", with("Много", many.iter().map(String::as_str).collect())).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "не больше 10 единиц");
    let n7 = create(&mut emp, with("Не включается ноутбук", vec![&mine, &mine])).await;
    let t7 = ticket(&mut emp, n7).await;
    let objs = t7["objects"].as_array().unwrap();
    assert_eq!(objs.len(), 1, "заявитель видит привязанное оборудование, дубль не повторяется: {t7}");
    assert_eq!((objs[0]["kind"].as_str(), objs[0]["object_id"].as_str()), (Some("asset"), Some(mine.as_str())));
    let (code, _) =
        emp.post(&format!("/api/helpdesk/tickets/{n7}/objects"), json!({"kind": "asset", "object_id": foreign})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "менять привязки заявитель не может");
    // Инженер за сотрудника — любое оборудование из реестра; список оборудования заявителя — подсказка.
    let (code, list) = eng1.get(&format!("/api/helpdesk/persons/{emp2_person}/assets")).await;
    assert_eq!(code, StatusCode::OK, "{list}");
    let ids: Vec<&str> = list.as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [foreign.as_str()], "{list}");
    assert_eq!(emp.get(&format!("/api/helpdesk/persons/{emp2_person}/assets")).await.0, StatusCode::FORBIDDEN);

    // --- Выдача, возврат, передача: история оборудования и «Моё оборудование» ---
    let hand = |a: &str| format!("/api/helpdesk/assets/{a}/handover");
    let (code, b) = eng1.post(&hand(&asset), json!({"action": "return"})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "вернуть можно только выданное: {b}");
    let (code, b) = eng1.post(&hand(&asset), json!({"action": "issue"})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "выдать — кому-то: {b}");
    let (code, b) = emp.post(&hand(&asset), json!({"action": "issue", "person_id": emp_person})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "сотрудник не выдаёт оборудование: {b}");
    let (code, b) =
        eng1.post(&hand(&asset), json!({"action": "issue", "person_id": emp_person, "comment": "под роспись"})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (code, b) = eng1.post(&hand(&asset), json!({"action": "issue", "person_id": emp2_person})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "выданное не выдаётся второй раз: {b}");
    let (code, b) = eng1.post(&hand(&asset), json!({"action": "transfer", "person_id": emp_person})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "передать тому же сотруднику нельзя: {b}");
    let (code, mine_list) = emp.get("/api/helpdesk/my/assets").await;
    assert_eq!(code, StatusCode::OK, "{mine_list}");
    let names: Vec<&str> = mine_list.as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"МФУ 3 этаж") && names.contains(&"Ноутбук сотрудника"), "{mine_list}");
    assert!(!names.contains(&"Старый ноутбук"), "списанное не показывается: {mine_list}");
    let mfu = mine_list.as_array().unwrap().iter().find(|a| a["name"] == "МФУ 3 этаж").unwrap();
    assert!(mfu["since"].is_string(), "дата выдачи: {mfu}");
    assert!(mfu.get("open_tickets").is_none() && mfu.get("notes").is_none(), "сотруднику — без служебных полей: {mfu}");
    let (code, b) = eng1.post(&hand(&asset), json!({"action": "transfer", "person_id": emp2_person})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (_, mine_list) = emp.get("/api/helpdesk/my/assets").await;
    assert!(
        !mine_list.as_array().unwrap().iter().any(|a| a["name"] == "МФУ 3 этаж"),
        "передано — ушло из списка: {mine_list}"
    );
    let (code, b) = eng1.post(&hand(&asset), json!({"action": "return", "status": "retired"})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "после возврата — склад или ремонт: {b}");
    let (code, b) = eng1.post(&hand(&asset), json!({"action": "return", "status": "repair"})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (_, a) = eng1.get(&format!("/api/helpdesk/assets/{asset}")).await;
    assert_eq!((a["status"].as_str(), a["responsible_person_id"].as_str()), (Some("repair"), None), "{a}");
    // Правка карточки — отдельная запись с полями «было → стало».
    let mut edited = a.clone();
    edited["room"] = json!("305");
    let (code, b) = eng1.put(&format!("/api/helpdesk/assets/{asset}"), edited).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (_, card) = eng1.get(&format!("/api/helpdesk/objects/asset/{asset}")).await;
    let kinds: Vec<&str> = card["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["updated", "returned", "transferred", "issued", "created"], "история, новые сверху: {card}");
    let ev = &card["events"];
    assert_eq!(ev[0]["changes"]["room"], json!([null, "305"]), "{ev}");
    assert_eq!(ev[1]["changes"]["status"], json!(["in_use", "repair"]), "{ev}");
    assert_eq!(ev[3]["comment"], "под роспись", "{ev}");
    assert!(ev[2]["from_name"].is_string() && ev[2]["to_name"].is_string(), "имена сохраняются: {ev}");
    assert_eq!(
        emp.get(&format!("/api/helpdesk/objects/asset/{asset}")).await.0,
        StatusCode::FORBIDDEN,
        "история — только ИТ"
    );
    let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'asset.handover'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(audited, 3, "выдача, передача, возврат — в журнале аудита");
    // Списанное не выдаётся.
    let (code, b) = eng1.post(&hand(&retired), json!({"action": "issue", "person_id": emp_person})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{b}");

    // --- Импорт из CSV: проверка без записи, импорт; известные инвентарные номера пропускаются ---
    let csv = "\u{feff}Название;Тип;Инв. номер;Площадка;Ответственный;IP-адрес;status\r\n\
               Ноутбук бухгалтера;компьютер;INV-100;офис а;emp;10.0.0.5;\r\n\
               Дубль МФУ;Принтер / МФУ;inv-001;;;;\r\n\
               Холодильник;Бытовая техника;;;;;\r\n\
               Чужой;Компьютер;INV-101;;nobody;;\r\n\
               Странный;Компьютер;INV-102;;;;lost\r\n\
               \"Склад; резерв\";Компьютер;INV-103;;;;\r\n\
               Повтор;Компьютер;INV-103;;;;\r\n";
    let import = "/api/helpdesk/assets/import";
    let (code, b) = emp.post(import, json!({"csv": csv, "dry_run": true})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "импорт — только ИТ: {b}");
    let (code, b) = eng1.post(import, json!({"csv": "", "dry_run": true})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "пустой файл: {b}");
    let (code, b) = eng1.post(import, json!({"csv": "name;room\nx;1\n", "dry_run": true})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "без колонки типа: {b}");
    let (code, b) = eng1.post(import, json!({"csv": "name;Название;type\nx;y;Компьютер\n", "dry_run": true})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "две колонки названия: {b}");
    let (code, r) = eng1.post(import, json!({"csv": csv, "dry_run": true})).await;
    assert_eq!(code, StatusCode::OK, "{r}");
    assert_eq!((r["created"].as_u64(), r["skipped"].as_u64()), (Some(2), Some(2)), "{r}");
    let rows: Vec<u64> = r["errors"].as_array().unwrap().iter().map(|e| e["row"].as_u64().unwrap()).collect();
    assert_eq!(rows, [4, 5, 6], "тип, сотрудник, состояние: {r}");
    let (_, found) = eng1.get("/api/helpdesk/objects?q=INV-100").await;
    assert!(found.as_array().unwrap().is_empty(), "проверка ничего не записывает: {found}");
    let (code, r) = eng1.post(import, json!({"csv": csv})).await;
    assert_eq!(code, StatusCode::OK, "{r}");
    assert_eq!((r["created"].as_u64(), r["skipped"].as_u64()), (Some(2), Some(2)), "{r}");
    let (_, found) = eng1.get("/api/helpdesk/objects?q=INV-100").await;
    let imported = found.as_array().unwrap().iter().find(|o| o["kind"] == "asset").expect("импортировано").clone();
    let imported = imported["id"].as_str().unwrap().to_string();
    let (_, a) = eng1.get(&format!("/api/helpdesk/assets/{imported}")).await;
    assert_eq!(a["status"], "in_use", "с ответственным — в работе: {a}");
    assert_eq!(a["responsible_person_id"], json!(emp_person), "ответственный по логину AD: {a}");
    assert_eq!(a["site_id"], json!(site), "площадка по названию без учёта регистра: {a}");
    assert_eq!(a["attributes"]["ip"], "10.0.0.5", "колонка по подписи поля типа: {a}");
    let (_, card) = eng1.get(&format!("/api/helpdesk/objects/asset/{imported}")).await;
    assert_eq!(
        (card["events"][0]["kind"].as_str(), card["events"][0]["comment"].as_str()),
        (Some("created"), Some("CSV"))
    );
    let (_, found) = eng1.get("/api/helpdesk/objects?q=INV-103").await;
    assert!(
        found.as_array().unwrap().iter().any(|o| o["label"].as_str().is_some_and(|l| l.contains("Склад; резерв"))),
        "{found}"
    );
    let (_, r) = eng1.post(import, json!({"csv": csv})).await;
    assert_eq!(
        (r["created"].as_u64(), r["skipped"].as_u64()),
        (Some(0), Some(4)),
        "повторный импорт ничего не меняет: {r}"
    );
    let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'asset.import'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(audited, 2, "проверка без записи в журнал не попадает");

    // --- ПК из агента: принять в реестр, отклонить, вернуть ---
    let device = |host: &'static str, ip: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Uuid>(
                "INSERT INTO devices (hostname, os, os_version, ip_addrs) VALUES ($1, 'Windows 11 Pro', '24H2', ARRAY[$2::inet]) RETURNING id",
            )
            .bind(host)
            .bind(ip)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let (ws42, ws43) = (device("WS-042", "10.0.0.42").await, device("WS-043", "10.0.0.43").await);
    let hosts = async |b: &mut Browser, rejected: bool| -> Vec<String> {
        let (code, list) = b.get(&format!("/api/helpdesk/assets/discovered?rejected={rejected}")).await;
        assert_eq!(code, StatusCode::OK, "{list}");
        list.as_array().unwrap().iter().map(|d| d["hostname"].as_str().unwrap().to_string()).collect()
    };
    let pending = hosts(&mut eng1, false).await;
    assert!(pending.contains(&"WS-042".into()) && pending.contains(&"WS-043".into()), "{pending:?}");
    assert_eq!(emp.get("/api/helpdesk/assets/discovered").await.0, StatusCode::FORBIDDEN);
    let (_, types) = eng1.get("/api/helpdesk/asset-types").await;
    let computer =
        types.as_array().unwrap().iter().find(|t| t["name"] == "Компьютер").expect("тип «Компьютер»")["id"].clone();
    let accept = |d: Uuid| format!("/api/helpdesk/assets/discovered/{d}/accept");
    let body = json!({"type_id": computer, "inventory_number": "INV-200", "responsible_person_id": emp_person});
    let (code, b) = emp.post(&accept(ws42), body.clone()).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "{b}");
    let (code, b) = eng1.post(&accept(ws42), body.clone()).await;
    assert_eq!(code, StatusCode::CREATED, "{b}");
    let ws42_asset = id(&b);
    let (code, b) = eng1.post(&accept(ws42), body).await;
    assert_eq!(code, StatusCode::NOT_FOUND, "второй раз не принимается: {b}");
    let (_, a) = eng1.get(&format!("/api/helpdesk/assets/{ws42_asset}")).await;
    assert_eq!(a["name"], "WS-042", "название — имя ПК: {a}");
    assert_eq!(a["status"], "in_use", "{a}");
    assert_eq!(
        a["attributes"],
        json!({"os": "Windows 11 Pro 24H2", "ip": "10.0.0.42"}),
        "ОС и IP из отчёта агента: {a}"
    );
    let (code, b) = eng1.post(&format!("/api/helpdesk/assets/discovered/{ws43}/reject"), json!({})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let pending = hosts(&mut eng1, false).await;
    assert!(!pending.contains(&"WS-042".into()) && !pending.contains(&"WS-043".into()), "{pending:?}");
    assert!(hosts(&mut eng1, true).await.contains(&"WS-043".into()));
    let (code, b) = eng1.post(&format!("/api/helpdesk/assets/discovered/{ws43}/restore"), json!({})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    assert!(hosts(&mut eng1, false).await.contains(&"WS-043".into()), "вернулся в список");
    let (code, _) = eng1.post(&format!("/api/helpdesk/assets/discovered/{ws42}/restore"), json!({})).await;
    assert_eq!(code, StatusCode::NOT_FOUND, "принятый не возвращается");

    let mut behalf = with("МФУ не печатает", vec![&asset, &foreign]);
    behalf["on_behalf_of"] = json!(emp2_person);
    let n8 = create(&mut eng1, behalf).await;
    let objs = ticket(&mut eng1, n8).await["objects"].clone();
    let mut got: Vec<&str> = objs.as_array().unwrap().iter().map(|o| o["object_id"].as_str().unwrap()).collect();
    got.sort_unstable();
    let mut want = vec![asset.as_str(), foreign.as_str()];
    want.sort_unstable();
    assert_eq!(got, want, "{objs}");
    let (code, b) =
        eng1.post(&format!("/api/helpdesk/tickets/{n2}/links"), json!({"number": n3, "kind": "related"})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    assert_eq!(ticket(&mut eng1, n3).await["links"].as_array().unwrap().len(), 1, "связь видна с обеих сторон");
    let n6 = create(&mut emp2, new("VPN не подключается")).await;
    let (code, b) = eng1.post(&format!("/api/helpdesk/tickets/{n6}/merge"), json!({"into": n2})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let dup = ticket(&mut eng1, n6).await;
    assert_eq!((dup["status"].as_str(), dup["closure_code"].as_str()), (Some("closed"), Some("duplicate")), "{dup}");
    let main = ticket(&mut eng1, n2).await;
    assert!(main["watchers"].as_array().unwrap().iter().any(|w| w["person_id"] == json!(emp2_person)), "{main}");
    let (code, res) = eng1
        .post(
            "/api/helpdesk/tickets/bulk",
            json!({"action": "priority", "priority": "low", "numbers": [n3, n4, 999999]}),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{res}");
    let oks: Vec<bool> = res.as_array().unwrap().iter().map(|r| r["ok"].as_bool().unwrap()).collect();
    assert_eq!(oks, [true, true, false], "{res}");
    assert_eq!(
        emp.post("/api/helpdesk/tickets/bulk", json!({"action": "take", "numbers": [n3]})).await.0,
        StatusCode::FORBIDDEN
    );

    // --- Шаблоны ответов и база знаний ---
    let (_, answers) = eng1.get("/api/helpdesk/response-templates").await;
    let a = answers.as_array().unwrap().first().expect("шаблоны ответов")["id"].as_str().unwrap().to_string();
    let (code, r) = eng1.get(&format!("/api/helpdesk/tickets/{n2}/response-templates/{a}")).await;
    assert_eq!(code, StatusCode::OK, "{r}");
    assert!(!r["body"].as_str().unwrap().contains("{{"), "подстановки заполнены: {r}");
    let (code, sec) = admin.post("/api/kb/sections", json!({"name": "Сеть"})).await;
    assert_eq!(code, StatusCode::CREATED, "{sec}");
    let (code, draft) = eng1.get(&format!("/api/helpdesk/tickets/{n1}/kb-draft")).await;
    assert_eq!(code, StatusCode::OK, "{draft}");
    let (code, art) = admin
        .post(
            "/api/kb/articles",
            json!({"title": "Как подключить VPN", "body": "Откройте клиент VPN и войдите.", "section_id": id(&sec),
                   "audience": "all", "status": "published", "source_ticket_number": n2}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{art}");
    let (code, b) = admin
        .post(
            "/api/kb/articles",
            json!({"title": "Пароль от роутера", "body": "Секрет", "audience": "engineers", "status": "published"}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{b}");
    let (_, arts) = emp.get("/api/kb/articles").await;
    let titles: Vec<&str> = arts.as_array().unwrap().iter().map(|a| a["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Как подключить VPN"], "сотрудник не видит статьи для инженеров");
    let (_, sug) = emp.get("/api/kb/suggest?q=vpn").await;
    assert!(!sug.as_array().unwrap().is_empty(), "подсказка в форме заявки: {sug}");
    let (code, _) = emp
        .post("/api/kb/articles", json!({"title": "x", "body": "y", "audience": "all", "status": "published"}))
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN);

    // --- Напоминания ожидания ---
    let (code, b) = eng1.post(&format!("/api/helpdesk/tickets/{n3}/take"), json!({})).await;
    assert!(code.is_success(), "{b}");
    let (code, b) =
        eng1.post(&format!("/api/helpdesk/tickets/{n3}/waiting"), json!({"comment": "Пришлите скриншот"})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    sqlx::query("UPDATE tickets SET waiting_since = now() - interval '5 days' WHERE number = $1")
        .bind(n3)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(pult_helpdesk::service::waiting_reminders(&core).await.unwrap(), 1, "напоминание");
    sqlx::query("UPDATE tickets SET reminded_at = now() - interval '5 days' WHERE number = $1")
        .bind(n3)
        .execute(&pool)
        .await
        .unwrap();
    pult_helpdesk::service::waiting_reminders(&core).await.unwrap();
    let t = ticket(&mut eng1, n3).await;
    assert_eq!((t["status"].as_str(), t["closure_code"].as_str()), (Some("resolved"), Some("no_response")), "{t}");

    // --- SLA: предупреждение и просрочка ---
    sqlx::query(
        "UPDATE tickets SET resolve_due_at = now() - interval '1 minute', resolve_warn_at = now() - interval '1 hour',
                response_due_at = now() - interval '1 hour', response_warn_at = now() - interval '2 hour' WHERE number = $1",
    )
    .bind(n4)
    .execute(&pool)
    .await
    .unwrap();
    assert!(pult_helpdesk::sla::check(&core).await.unwrap() >= 1);
    let t = ticket(&mut eng1, n4).await;
    assert_eq!(t["sla_state"], "breached", "{t}");
    let (_, list) = eng1.get("/api/helpdesk/tickets?scope=all&sla=breached").await;
    assert!(list.as_array().unwrap().iter().any(|t| t["number"] == n4), "{list}");

    // --- Отчёты ---
    let (from, to): (String, String) = sqlx::query_as(
        "SELECT to_char(now() - interval '1 day', 'YYYY-MM-DD'), to_char(now() + interval '1 day', 'YYYY-MM-DD')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let period = format!("from={from}&to={to}");
    let (code, rep) = admin.get(&format!("/api/helpdesk/reports?{period}")).await;
    assert_eq!(code, StatusCode::OK, "{rep}");
    assert!(rep["summary"]["created"].as_i64().unwrap() >= 6, "{rep}");
    assert_eq!(rep["summary"]["rating_down"], 1);
    let sm = &rep["summary"];
    assert_eq!((sm["survey_count"].as_i64(), sm["avg_speed"].as_f64()), (Some(1), Some(2.0)), "{rep}");
    let (code, _, csv) =
        admin.send(Method::GET, &format!("/api/helpdesk/reports/export?format=csv&{period}"), None).await;
    assert_eq!(code, StatusCode::OK);
    assert!(csv.starts_with("\u{feff}".as_bytes()), "CSV с BOM для Excel");
    let (code, _, xlsx) =
        admin.send(Method::GET, &format!("/api/helpdesk/reports/export?format=xlsx&{period}"), None).await;
    assert_eq!(code, StatusCode::OK);
    assert!(xlsx.starts_with(b"PK\x03\x04"), "XLSX — ZIP");
    assert_eq!(emp.get(&format!("/api/helpdesk/reports?{period}")).await.0, StatusCode::FORBIDDEN);

    // --- Токены API: только чтение и только права создателя ---
    let (code, tok) = admin
        .post("/api/admin/api-tokens", json!({"name": "BI", "permissions": ["helpdesk.reports"], "days": 30}))
        .await;
    assert_eq!(code, StatusCode::CREATED, "{tok}");
    let token = tok["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("pult_"));
    assert_eq!(bearer(&app, Method::GET, &format!("/api/helpdesk/reports?{period}"), &token).await, StatusCode::OK);
    assert!(
        bearer(&app, Method::POST, "/api/helpdesk/tickets/bulk", &token).await.is_client_error(),
        "запись токеном запрещена"
    );
    assert_eq!(
        bearer(&app, Method::GET, "/api/admin/api-tokens", &token).await,
        StatusCode::FORBIDDEN,
        "только выданные права"
    );
    assert_eq!(
        bearer(&app, Method::GET, &format!("/api/helpdesk/reports?{period}"), "pult_wrong").await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(admin.delete(&format!("/api/admin/api-tokens/{}", id(&tok))).await, StatusCode::NO_CONTENT);
    assert_eq!(
        bearer(&app, Method::GET, &format!("/api/helpdesk/reports?{period}"), &token).await,
        StatusCode::UNAUTHORIZED,
        "отозван"
    );
    let (code, _) = eng1.post("/api/admin/api-tokens", json!({"name": "x", "permissions": ["helpdesk.reports"]})).await;
    assert_eq!(code, StatusCode::FORBIDDEN);

    // --- Расширения: манифест (файлом, по адресу, из каталога), токен и вебхук, ссылки в меню ---
    let manifest = json!({
        "manifest_version": 1, "id": "com.example.sla", "name": "SLA-отчёт", "version": "1.0.0",
        "publisher": "Example", "permissions": ["helpdesk.reports", "helpdesk.ticket.work"],
        "webhook": {"url": "https://ext.example/hook", "events": ["created"]},
        "links": [{"title": "Отчёт SLA", "url": "https://ext.example/sla", "permission": "helpdesk.reports"},
                  {"title": "Справка", "url": "https://ext.example/help"}]
    });
    let served = manifest.clone();
    let ext_srv = Router::new()
        .route("/m.json", axum::routing::get(move || { let m = served.clone(); async move { axum::Json(m) } }))
        .route("/catalog.json", axum::routing::get(|| async {
            axum::Json(json!([{"manifest_version": 1, "id": "com.example.kb", "name": "KB", "version": "2.0"}, {"broken": true}]))
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ext_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, ext_srv).await.unwrap() });
    assert_eq!(
        eng1.post("/api/admin/extensions/preview", json!({"manifest": manifest})).await.0,
        StatusCode::FORBIDDEN
    );
    let (code, pv) = admin.post("/api/admin/extensions/preview", json!({"url": format!("{ext_url}/m.json")})).await;
    assert_eq!(code, StatusCode::OK, "{pv}");
    assert_eq!(
        (pv["permissions"][0]["code"].as_str(), pv["already_installed"].as_bool()),
        (Some("helpdesk.reports"), Some(false))
    );
    let mut bad_m = manifest.clone();
    bad_m["permissions"] = json!(["helpdesk.nope"]);
    let (code, b) = admin.post("/api/admin/extensions", json!({"manifest": bad_m})).await;
    assert_eq!((code, b["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_manifest")), "{b}");
    // События заявок — только расширению, которое просит право работать с ними; ссылка — только https.
    let mut bad_m = manifest.clone();
    bad_m["permissions"] = json!(["helpdesk.reports"]);
    let (code, b) = admin.post("/api/admin/extensions", json!({"manifest": bad_m})).await;
    assert_eq!((code, b["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_manifest")), "{b}");
    let mut bad_m = manifest.clone();
    bad_m["links"] = json!([{"title": "x", "url": "HTTP://localhost/x"}]);
    let (code, b) = admin.post("/api/admin/extensions", json!({"manifest": bad_m})).await;
    assert_eq!((code, b["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_manifest")), "{b}");
    let (code, inst) = admin.post("/api/admin/extensions", json!({"manifest": manifest})).await;
    assert_eq!(code, StatusCode::CREATED, "{inst}");
    let ext_token = inst["token"].as_str().unwrap().to_string();
    assert!(inst["webhook_secret"].as_str().is_some_and(|s| s.len() >= 24));
    assert_eq!(bearer(&app, Method::GET, &format!("/api/helpdesk/reports?{period}"), &ext_token).await, StatusCode::OK);
    assert_eq!(
        admin.post("/api/admin/extensions", json!({"manifest": manifest})).await.1["error"],
        "already_installed"
    );
    let hooks: i64 =
        sqlx::query_scalar("SELECT count(*) FROM webhooks WHERE url = 'https://ext.example/hook' AND is_active")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(hooks, 1);
    // Ссылки: «Отчёт SLA» — у кого есть право на отчёты, «Справка» — всем.
    let titles =
        |v: &Value| v.as_array().unwrap().iter().map(|l| l["title"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(titles(&admin.get("/api/extensions/links").await.1), ["Отчёт SLA", "Справка"]);
    assert_eq!(titles(&emp.get("/api/extensions/links").await.1), ["Справка"]);
    // Выключение отзывает токен и прячет ссылки; включение выдаёт новый токен.
    let ext = format!("/api/admin/extensions/{}", id(&inst));
    admin.put(&ext, json!({"enabled": false})).await;
    assert_eq!(
        bearer(&app, Method::GET, &format!("/api/helpdesk/reports?{period}"), &ext_token).await,
        StatusCode::UNAUTHORIZED
    );
    assert!(emp.get("/api/extensions/links").await.1.as_array().unwrap().is_empty());
    let (_, again) = admin.put(&ext, json!({"enabled": true})).await;
    let new_token = again["token"].as_str().unwrap();
    assert_eq!(bearer(&app, Method::GET, &format!("/api/helpdesk/reports?{period}"), new_token).await, StatusCode::OK);
    // Каталог: негодный манифест пропускается.
    admin.put("/api/admin/extensions/settings", json!({"catalog_url": format!("{ext_url}/catalog.json")})).await;
    let (code, catalog) = admin.get("/api/admin/extensions/catalog").await;
    assert_eq!((code, catalog.as_array().map(Vec::len)), (StatusCode::OK, Some(1)), "{catalog}");
    assert_eq!(catalog[0]["manifest"]["id"], "com.example.kb");
    // Удаление: токен отозван, вебхука нет, всё в аудите.
    assert_eq!(admin.delete(&ext).await, StatusCode::NO_CONTENT);
    assert_eq!(
        bearer(&app, Method::GET, &format!("/api/helpdesk/reports?{period}"), new_token).await,
        StatusCode::UNAUTHORIZED
    );
    let hooks: i64 = sqlx::query_scalar("SELECT count(*) FROM webhooks WHERE url = 'https://ext.example/hook'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(hooks, 0);
    let audited: Vec<String> =
        sqlx::query_scalar("SELECT action FROM audit_log WHERE action LIKE 'extension.%' ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        audited,
        ["extension.install", "extension.disable", "extension.enable", "extension.settings", "extension.remove"]
    );

    // --- Мониторинг: источник с токеном, оповещения, заявка, «решено» ---
    let (code, b) = admin.post("/api/admin/monitoring/sources", json!({"name": "", "min_severity": "warning"})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "без названия: {b}");
    let (code, b) = eng1.post("/api/admin/monitoring/sources", json!({"name": "Z", "min_severity": "warning"})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "{b}");
    let (code, src) = admin
        .post(
            "/api/admin/monitoring/sources",
            json!({"name": "Zabbix", "create_tickets": true, "min_severity": "warning", "site_id": site}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{src}");
    let mon = src["token"].as_str().unwrap().to_string();
    assert!(mon.starts_with("pult_mon_"));
    let hook = "/api/helpdesk/monitoring/alerts";
    let fire = json!({"alerts": [
        {"status": "firing", "fingerprint": "disk-db1", "labels": {"alertname": "DiskFull", "severity": "critical", "instance": "db1"},
         "annotations": {"summary": "Диск заполнен", "description": "95% занято"}},
        {"status": "firing", "fingerprint": "info-1", "labels": {"alertname": "Backup done", "severity": "info"}}
    ]});
    let (code, _) = hook_call(&app, hook, "pult_mon_wrong", &fire).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED);
    let (code, _) = hook_call(&app, hook, &token, &fire).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "токен API — не токен мониторинга");
    let (code, r) = hook_call(&app, hook, &mon, &fire).await;
    assert_eq!(code, StatusCode::OK, "{r}");
    assert_eq!(
        (r["opened"].as_u64(), r["tickets"].as_u64()),
        (Some(2), Some(1)),
        "заявка — от важности источника: {r}"
    );
    let (_, r) = hook_call(&app, hook, &mon, &fire).await;
    assert_eq!(
        (r["opened"].as_u64(), r["repeated"].as_u64(), r["tickets"].as_u64()),
        (Some(0), Some(2), Some(0)),
        "повтор: {r}"
    );
    let (code, open) = eng1.get("/api/helpdesk/monitoring/alerts?open=true").await;
    assert_eq!(code, StatusCode::OK, "{open}");
    let disk = open.as_array().unwrap().iter().find(|a| a["title"] == "Диск заполнен").expect("оповещение").clone();
    assert_eq!((disk["severity"].as_str(), disk["host"].as_str()), (Some("critical"), Some("db1")), "{disk}");
    let number = disk["ticket_number"].as_i64().expect("номер заявки");
    let t = ticket(&mut eng1, number).await;
    assert_eq!((t["channel"].as_str(), t["title"].as_str()), (Some("monitoring"), Some("Диск заполнен")), "{t}");
    assert!(t["description"].as_str().unwrap().contains("db1"), "узел в описании: {t}");
    assert_eq!(emp.get(hook).await.0, StatusCode::FORBIDDEN, "оповещения — только ИТ");
    let bells: i64 = sqlx::query_scalar("SELECT count(*) FROM notifications WHERE kind = 'monitoring.alert'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(bells >= 1, "суперадминистраторам — уведомление");
    let (_, r) = hook_call(
        &app,
        hook,
        &mon,
        &json!({"status": "resolved", "alerts": [{"fingerprint": "disk-db1", "labels": {"alertname": "DiskFull"}}]}),
    )
    .await;
    assert_eq!(r["resolved"].as_u64(), Some(1), "{r}");
    let feed: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM ticket_events e JOIN tickets t ON t.id = e.ticket_id WHERE t.number = $1 AND e.kind = 'alert_resolved'",
    )
    .bind(number)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(feed, 1, "«решено» — в ленте заявки");
    let (_, open) = eng1.get("/api/helpdesk/monitoring/alerts?open=true").await;
    assert!(!open.as_array().unwrap().iter().any(|a| a["title"] == "Диск заполнен"), "{open}");
    // Снова сработало — новое оповещение и новая заявка.
    let (_, r) = hook_call(&app, hook, &mon, &fire).await;
    assert_eq!((r["opened"].as_u64(), r["tickets"].as_u64()), (Some(1), Some(1)), "{r}");
    let (_, list) = admin.get("/api/admin/monitoring/sources").await;
    assert_eq!(list[0]["open_alerts"], 2, "{list}");
    // Заявку в прошлый раз создать не удалось — её создаёт повтор того же оповещения.
    sqlx::query("UPDATE alerts SET ticket_id = NULL WHERE fingerprint = 'disk-db1' AND status = 'firing'")
        .execute(&pool)
        .await
        .unwrap();
    let (_, r) = hook_call(&app, hook, &mon, &fire).await;
    assert_eq!((r["repeated"].as_u64(), r["tickets"].as_u64()), (Some(2), Some(1)), "{r}");
    let (_, r) = hook_call(&app, hook, &mon, &fire).await;
    assert_eq!(r["tickets"].as_u64(), Some(0), "заявка уже есть: {r}");
    // NUL и переводы строк в названии база не получит.
    let (code, r) =
        hook_call(&app, hook, &mon, &json!({"title": "Сбой\u{0}\nслужбы", "severity": "info", "id": "nul-1"})).await;
    assert_eq!((code, r["opened"].as_u64()), (StatusCode::OK, Some(1)), "{r}");
    let title: String =
        sqlx::query_scalar("SELECT title FROM alerts WHERE fingerprint = 'nul-1'").fetch_one(&pool).await.unwrap();
    assert_eq!(title, "Сбой службы");
    let off = json!({"name": "Zabbix", "min_severity": "warning", "is_active": false});
    let (code, b) = admin.put(&format!("/api/admin/monitoring/sources/{}", id(&src)), off).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    assert_eq!(hook_call(&app, hook, &mon, &fire).await.0, StatusCode::UNAUTHORIZED, "выключенный источник");
    assert_eq!(admin.delete(&format!("/api/admin/monitoring/sources/{}", id(&src))).await, StatusCode::NO_CONTENT);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM alerts").fetch_one(&pool).await.unwrap();
    assert_eq!(left, 0, "оповещения удалены вместе с источником");

    // --- Вебхуки ---
    let (code, b) = admin
        .post(
            "/api/admin/webhooks",
            json!({"name": "Bad", "url": "http://example.test/hook", "secret": "0123456789abcdef"}),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "только https: {b}");
    let (code, wh) = admin
        .post(
            "/api/admin/webhooks",
            json!({"name": "Чат", "url": "https://example.test/hook", "secret": "0123456789abcdef", "events": []}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{wh}");
    let before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'helpdesk.webhook'").fetch_one(&pool).await.unwrap();
    create(&mut emp, new("Сломалась мышь")).await;
    let after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'helpdesk.webhook'").fetch_one(&pool).await.unwrap();
    assert!(after > before, "событие заявки ставит доставку вебхука");
    // Одна правка — несколько событий: подписчик на «priority» получает своё, даже если оно не последнее.
    let (code, only) = admin
        .post(
            "/api/admin/webhooks",
            json!({"name": "Приоритет", "url": "https://example.test/p", "secret": "0123456789abcdef", "events": ["priority"]}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{only}");
    let n8 = create(&mut emp, new("Не печатает принтер")).await;
    let t = ticket(&mut eng1, n8).await;
    let (code, b) = eng1
        .put(
            &format!("/api/helpdesk/tickets/{n8}"),
            json!({"version": t["version"], "priority": "low", "title": "Не печатает принтер в бухгалтерии"}),
        )
        .await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let got: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = 'helpdesk.webhook' AND payload->>'webhook' = $1 AND payload->>'event' = 'priority'",
    )
    .bind(id(&only))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(got, 1, "доставка события priority");
    let (_, hooks) = admin.get("/api/admin/webhooks").await;
    assert!(!hooks.to_string().contains("0123456789abcdef"), "секрет не возвращается");
    // Мессенджеры: входящий вебхук чата без секрета, бот Telegram — токен и чат.
    let (code, chat) = admin
        .post(
            "/api/admin/webhooks",
            json!({"name": "Mattermost", "format": "chat", "url": "https://chat.example.test/hooks/x"}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "чату секрет не нужен: {chat}");
    let bot = "123456789:AAHdqTcvCH1vGWJxfSeofSAs0K5PALDsaw";
    for (body, why) in [
        (json!({"name": "TG", "format": "telegram", "url": "", "secret": bot}), "нет чата"),
        (
            json!({"name": "TG", "format": "telegram", "url": "", "chat_id": "-100123", "secret": "abc"}),
            "не токен бота",
        ),
        (json!({"name": "TG", "format": "icq", "url": "https://example.test/x", "secret": bot}), "нет такого формата"),
    ] {
        let (code, b) = admin.post("/api/admin/webhooks", body).await;
        assert_eq!(code, StatusCode::BAD_REQUEST, "{why}: {b}");
    }
    let (code, tg) = admin
        .post(
            "/api/admin/webhooks",
            json!({"name": "Telegram", "format": "telegram", "url": "", "chat_id": "-1001234567890", "secret": bot, "events": ["created"]}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{tg}");
    let (_, hooks) = admin.get("/api/admin/webhooks").await;
    let tg_row = hooks.as_array().unwrap().iter().find(|h| h["name"] == "Telegram").unwrap();
    assert_eq!((tg_row["format"].as_str(), tg_row["chat_id"].as_str()), (Some("telegram"), Some("-1001234567890")));
    assert!(!hooks.to_string().contains("AAHdqTcvCH1"), "токен бота не возвращается: {hooks}");
    // Сменить формат без нового секрета нельзя: подпись не станет токеном бота.
    let (code, b) = admin
        .put(
            &format!("/api/admin/webhooks/{}", id(&chat)),
            json!({"name": "Mattermost", "format": "telegram", "url": "", "chat_id": "-100123"}),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{b}");
    let (code, b) = admin
        .put(&format!("/api/admin/webhooks/{}", id(&tg)), json!({"name": "Telegram 2", "format": "telegram", "url": "", "chat_id": "@pult_alerts", "events": ["created"]}))
        .await;
    assert_eq!(code, StatusCode::NO_CONTENT, "тот же формат — токен остаётся: {b}");
    let before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'helpdesk.webhook' AND payload->>'webhook' = $1")
            .bind(id(&tg))
            .fetch_one(&pool)
            .await
            .unwrap();
    create(&mut emp, new("Не открывается почта")).await;
    let after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'helpdesk.webhook' AND payload->>'webhook' = $1")
            .bind(id(&tg))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(after, before + 1, "новая заявка — сообщение в Telegram");
    for h in [&chat, &tg] {
        assert_eq!(admin.delete(&format!("/api/admin/webhooks/{}", id(h))).await, StatusCode::NO_CONTENT);
    }

    // Еженедельный отчёт без адресатов — ничего не шлёт.
    assert_eq!(pult_helpdesk::reports::weekly(&core).await.unwrap(), 0);

    // --- Найдено ревью 17.11.2026 ---
    // Заметка коллеги не берёт заявку «В работу» за исполнителя и не останавливает время реакции.
    let nr = create(&mut emp, new("Ревью: заметка коллеги")).await;
    let t = ticket(&mut admin, nr).await;
    let (owner, other) = if t["assignee_id"] == json!(u1) { (u1, &mut eng2) } else { (u2, &mut eng1) };
    assert_eq!(t["status"], "assigned", "{t}");
    let (code, b) =
        other.post(&format!("/api/helpdesk/tickets/{nr}/comments"), json!({"body": "Заметка", "internal": true})).await;
    assert_eq!(code, StatusCode::OK, "{b}");
    let t = ticket(&mut admin, nr).await;
    assert_eq!((t["status"].as_str(), t["assignee_id"].clone()), (Some("assigned"), json!(owner)), "{t}");
    // Передача ожидающей ответа заявки не снимает ожидание.
    let owner_b = if owner == u1 { &mut eng1 } else { &mut eng2 };
    let (code, b) =
        owner_b.post(&format!("/api/helpdesk/tickets/{nr}/waiting"), json!({"comment": "Пришлите модель"})).await;
    assert!(code.is_success(), "{b}");
    let next = if owner == u1 { u2 } else { u1 };
    let (code, b) = admin.post(&format!("/api/helpdesk/tickets/{nr}/assign"), json!({"assignee_id": next})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let t = ticket(&mut admin, nr).await;
    assert_eq!((t["status"].as_str(), t["assignee_id"].clone()), (Some("waiting"), json!(next)), "{t}");
    // Неизвестная категория в правке — 400, а не 500.
    let v = t["version"].clone();
    let (code, b) =
        admin.put(&format!("/api/helpdesk/tickets/{nr}"), json!({"version": v, "category_id": Uuid::now_v7()})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{b}");
    // «Мои заявки» заявителя: решённые (ждут подтверждения) — среди открытых.
    let (code, b) = admin
        .post(&format!("/api/helpdesk/tickets/{nr}/resolve"), json!({"comment": "Готово", "closure_code": "solved"}))
        .await;
    assert!(code.is_success(), "{b}");
    let (_, mine) = emp.get("/api/helpdesk/tickets?status=active").await;
    assert!(mine.as_array().unwrap().iter().any(|r| r["number"] == nr), "решённая видна заявителю: {mine}");
    let (_, open) = emp.get("/api/helpdesk/tickets?status=open").await;
    assert!(!open.as_array().unwrap().iter().any(|r| r["number"] == nr));

    // Ответ на закрытую заявку, когда у категории появилось обязательное поле, — повторная заявка с теми же ответами.
    sqlx::query("UPDATE ticket_categories SET form_schema = $2 WHERE id = $1::uuid")
        .bind(&cat)
        .bind(json!([{"key": "model", "label": "Модель", "type": "text", "required": true}]))
        .execute(&pool)
        .await
        .unwrap();
    let mut body = new("Ревью: повторное обращение");
    body["form_answers"] = json!({"model": "HP 400"});
    let nf = create(&mut emp, body).await;
    sqlx::query("UPDATE tickets SET status = 'closed', resolved_at = now(), closed_at = now() WHERE number = $1")
        .bind(nf)
        .execute(&pool)
        .await
        .unwrap();
    let (code, f) =
        emp.post(&format!("/api/helpdesk/tickets/{nf}/comments"), json!({"body": "Снова не печатает"})).await;
    assert_eq!(code, StatusCode::OK, "{f}");
    assert_eq!(f["follow_up"], true, "{f}");
    let nf2 = f["number"].as_i64().unwrap();
    assert_ne!(nf2, nf);
    let answers: Value = sqlx::query_scalar("SELECT form_answers FROM tickets WHERE number = $1")
        .bind(nf2)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(answers["model"], "HP 400");
    // Категорию выключили — повторная заявка всё равно создаётся, без категории.
    let nf3 = create(&mut emp, {
        let mut b = new("Ревью: категория выключена");
        b["form_answers"] = json!({"model": "Canon"});
        b
    })
    .await;
    sqlx::query("UPDATE tickets SET status = 'closed', resolved_at = now(), closed_at = now() WHERE number = $1")
        .bind(nf3)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE ticket_categories SET is_active = false WHERE id = $1::uuid")
        .bind(&cat)
        .execute(&pool)
        .await
        .unwrap();
    let (code, f) = emp.post(&format!("/api/helpdesk/tickets/{nf3}/comments"), json!({"body": "Опять"})).await;
    assert_eq!(code, StatusCode::OK, "{f}");
    let cat_of: Option<Uuid> = sqlx::query_scalar("SELECT category_id FROM tickets WHERE number = $1")
        .bind(f["number"].as_i64().unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(cat_of, None);
    sqlx::query("UPDATE ticket_categories SET is_active = true, form_schema = '[]' WHERE id = $1::uuid")
        .bind(&cat)
        .execute(&pool)
        .await
        .unwrap();

    // Инженер без роли не получает заявки, его открытые возвращаются в очередь.
    let (code, b) = admin.put(&format!("/api/admin/users/{u2}/roles"), json!({"roles": []})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    for i in 0..3 {
        let n = create(&mut emp, new(&format!("Ревью: без роли {i}"))).await;
        let t = ticket(&mut admin, n).await;
        assert_ne!(t["assignee_id"], json!(u2), "снятому с роли заявки не назначаются: {t}");
    }
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets WHERE assignee_id = $1 AND status = ANY($2)")
        .bind(u2)
        .bind(pult_helpdesk::tickets::OPEN_STATUSES)
        .fetch_one(&pool)
        .await
        .unwrap();
    let released = pult_helpdesk::tickets::release_orphans(&core).await.unwrap();
    assert!(released as i64 >= held, "{released} < {held}");
    let held: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets WHERE assignee_id = $1 AND status = ANY($2)")
        .bind(u2)
        .bind(pult_helpdesk::tickets::OPEN_STATUSES)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(held, 0);
    let (code, b) = admin.put(&format!("/api/admin/users/{u2}/roles"), json!({"roles": ["engineer"]})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");

    // --- Проблемы и изменения ---
    let inc1 = create(&mut emp, new("Не печатает принтер 1")).await;
    let inc2 = create(&mut emp, new("Не печатает принтер 2")).await;
    let (code, _) = emp.post("/api/helpdesk/itil", json!({"kind": "problem", "title": "Своя проблема"})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "проблемы и изменения заводит только ИТ");
    let (code, p) = eng1
        .post(
            "/api/helpdesk/itil",
            json!({"kind": "problem", "title": "Сбой драйвера печати", "category_id": cat, "site_id": site, "incident": inc1}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{p}");
    let pn = p["number"].as_i64().unwrap();
    let (code, b) = eng1.post(&format!("/api/helpdesk/itil/{pn}/incidents"), json!({"number": inc2})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (code, _) = eng1.post(&format!("/api/helpdesk/itil/{pn}/incidents"), json!({"number": pn})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "проблема — не инцидент");
    // Проблема — не обращение: в общем списке её нет, в своём — есть; SLA нет; исполнитель — автор.
    let (_, all) = eng1.get("/api/helpdesk/tickets?scope=all&status=all&limit=500").await;
    assert!(all.as_array().unwrap().iter().all(|t| t["number"] != pn && t["kind"] != "problem"), "{all}");
    let (_, probs) = eng1.get("/api/helpdesk/tickets?scope=all&kind=problem").await;
    let probs: Vec<i64> = probs.as_array().unwrap().iter().map(|t| t["number"].as_i64().unwrap()).collect();
    assert_eq!(probs, vec![pn]);
    let (_, theirs) = emp.get("/api/helpdesk/tickets?kind=problem&status=all").await;
    assert!(theirs.as_array().unwrap().iter().all(|t| t["kind"] != "problem"), "{theirs}");
    let (code, _) = emp.get(&format!("/api/helpdesk/itil/{pn}")).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let pt = ticket(&mut eng1, pn).await;
    assert_eq!((pt["kind"].as_str(), &pt["assignee_id"]), (Some("problem"), &json!(u1)), "{pt}");
    assert!(pt["resolve_due_at"].is_null(), "у проблемы нет срока SLA: {pt}");
    assert_eq!(ticket(&mut eng1, inc1).await["problem_number"], pn);
    let (_, r) = eng1.get(&format!("/api/helpdesk/itil/{pn}")).await;
    assert_eq!(r["problem"]["incidents"].as_array().map(Vec::len), Some(2), "{r}");
    let (code, _) = eng1
        .put(&format!("/api/helpdesk/itil/{pn}"), json!({"cause": "Обновление драйвера", "known_error": true}))
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "известная ошибка — только с обходным решением");
    let (code, b) = eng1
        .put(
            &format!("/api/helpdesk/itil/{pn}"),
            json!({"cause": "Обновление драйвера", "workaround": "Откатить драйвер", "known_error": true}),
        )
        .await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (_, r) = eng1.get(&format!("/api/helpdesk/itil/{pn}")).await;
    assert_eq!(
        (r["problem"]["known_error"].as_bool(), r["problem"]["workaround"].as_str()),
        (Some(true), Some("Откатить драйвер"))
    );
    // Без кода закрытия — отказ сразу, ни один инцидент не тронут.
    let (code, _) =
        eng1.post(&format!("/api/helpdesk/itil/{pn}/resolve-incidents"), json!({"body": "Откатили драйвер"})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST);
    assert_ne!(ticket(&mut emp, inc2).await["status"], "resolved");
    // Все открытые инциденты проблемы — одним ответом, каждый как обычное решение.
    let (code, r) = eng1
        .post(
            &format!("/api/helpdesk/itil/{pn}/resolve-incidents"),
            json!({"body": "Откатили драйвер", "closure_code": "solved"}),
        )
        .await;
    assert_eq!((code, r["resolved"].as_i64()), (StatusCode::OK, Some(2)), "{r}");
    assert_eq!(ticket(&mut emp, inc2).await["status"], "resolved");
    assert_eq!(eng1.delete(&format!("/api/helpdesk/itil/{pn}/incidents/{inc1}")).await, StatusCode::NO_CONTENT);
    assert!(ticket(&mut eng1, inc1).await["problem_number"].is_null());

    let (code, c) = eng1
        .post(
            "/api/helpdesk/itil",
            json!({"kind": "change", "title": "Обновить драйвер печати", "category_id": cat, "site_id": site, "risk": "medium"}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{c}");
    let cn = c["number"].as_i64().unwrap();
    let submit = format!("/api/helpdesk/itil/{cn}/submit");
    let (code, _) = eng1.post(&submit, json!({})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "без плана на согласование не отправить");
    let mut plan = json!({"risk": "medium", "plan": "Установить драйвер 2.0", "rollback": "",
        "planned_start": "2026-12-01T10:00:00Z", "planned_end": "2026-12-01T11:00:00Z"});
    let (code, b) = eng1.put(&format!("/api/helpdesk/itil/{cn}"), plan.clone()).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (code, _) = eng1.post(&submit, json!({})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "средний риск — с планом отката");
    plan["rollback"] = json!("Вернуть 1.9");
    eng1.put(&format!("/api/helpdesk/itil/{cn}"), plan.clone()).await;
    let resolve = format!("/api/helpdesk/tickets/{cn}/resolve");
    let (code, _) = eng1.post(&resolve, json!({"comment": "Готово", "closure_code": "solved"})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "несогласованное изменение не решить");
    // Правила: по категории с среднего риска — сотрудник-руководитель; для всех с высокого — второй инженер;
    // для всех с низкого — сам автор (своё он не согласует).
    let boss = uid("emp").await;
    for (c, r, who) in [(json!(cat), "medium", "emp"), (Value::Null, "high", "eng2"), (Value::Null, "low", "eng1")] {
        let p = pid(who).await;
        let (code, b) =
            admin.post("/api/helpdesk/change-rules", json!({"category_id": c, "min_risk": r, "person_id": p})).await;
        assert_eq!(code, StatusCode::CREATED, "{b}");
    }
    let p = pid("emp").await;
    let (code, _) = admin
        .post("/api/helpdesk/change-rules", json!({"category_id": cat, "min_risk": "medium", "person_id": p}))
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "такое правило уже есть");
    let (code, _) = eng1.get("/api/helpdesk/change-rules").await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let (code, s) = eng1.post(&submit, json!({})).await;
    assert_eq!(
        (code, s["approval"].as_str(), s["approvers"].as_i64()),
        (StatusCode::OK, Some("pending"), Some(1)),
        "{s}"
    );
    assert_eq!(ticket(&mut eng1, cn).await["status"], "waiting_approval");
    let bells: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM notifications WHERE user_id = $1 AND kind = 'helpdesk.change_approval'",
    )
    .bind(boss)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bells, 1);
    // Согласующему не нужен доступ к заявкам: всё нужное — в его списке.
    let pending = |list: &Value| -> String {
        id(list
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["number"] == cn && a["status"] == "pending")
            .expect("ждёт решения"))
    };
    let (_, mine) = emp.get("/api/helpdesk/change-approvals").await;
    let a1 = pending(&mine);
    assert_eq!(mine[0]["plan"], "Установить драйвер 2.0", "{mine}");
    let (code, _) = emp2.post(&format!("/api/helpdesk/change-approvals/{a1}"), json!({"approve": true})).await;
    assert_eq!(code, StatusCode::NOT_FOUND, "чужое согласование");
    let (code, _) = emp.post(&format!("/api/helpdesk/change-approvals/{a1}"), json!({"approve": false})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "отказ — с причиной");
    let (code, b) = emp
        .post(&format!("/api/helpdesk/change-approvals/{a1}"), json!({"approve": false, "comment": "Нет окна"}))
        .await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (_, r) = eng1.get(&format!("/api/helpdesk/itil/{cn}")).await;
    assert_eq!(r["change"]["approval"], "rejected", "{r}");
    assert_eq!(ticket(&mut eng1, cn).await["status"], "in_progress");
    let (code, _) = emp.post(&format!("/api/helpdesk/change-approvals/{a1}"), json!({"approve": true})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "решение прошлого круга не меняется");
    // Второй круг — согласовано; правка согласованного начинает согласование заново.
    let (code, _) = eng1.post(&submit, json!({})).await;
    assert_eq!(code, StatusCode::OK);
    let (_, mine) = emp.get("/api/helpdesk/change-approvals").await;
    let a2 = pending(&mine);
    let (code, b) = emp.post(&format!("/api/helpdesk/change-approvals/{a2}"), json!({"approve": true})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (_, r) = eng1.get(&format!("/api/helpdesk/itil/{cn}")).await;
    assert_eq!((r["change"]["approval"].as_str(), r["change"]["round"].as_i64()), (Some("approved"), Some(2)), "{r}");
    plan["risk"] = json!("high");
    let (code, _) = eng1.put(&format!("/api/helpdesk/itil/{cn}"), plan.clone()).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    let (_, r) = eng1.get(&format!("/api/helpdesk/itil/{cn}")).await;
    assert_eq!(r["change"]["approval"], "draft", "{r}");
    // Высокий риск — оба правила; согласовано, когда решили все.
    let (_, s) = eng1.post(&submit, json!({})).await;
    assert_eq!(s["approvers"], 2, "{s}");
    let (_, mine) = eng2.get("/api/helpdesk/change-approvals").await;
    let a3 = pending(&mine);
    eng2.post(&format!("/api/helpdesk/change-approvals/{a3}"), json!({"approve": true})).await;
    let (_, r) = eng1.get(&format!("/api/helpdesk/itil/{cn}")).await;
    assert_eq!(r["change"]["approval"], "pending", "ждёт второго согласующего");
    let (_, mine) = emp.get("/api/helpdesk/change-approvals").await;
    let a4 = pending(&mine);
    emp.post(&format!("/api/helpdesk/change-approvals/{a4}"), json!({"approve": true, "comment": "Согласовано"})).await;
    let (code, b) = eng1.post(&resolve, json!({"comment": "Драйвер обновлён", "closure_code": "solved"})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (code, _) = eng1.post(&format!("/api/helpdesk/itil/{cn}/resolve-incidents"), json!({"body": "x"})).await;
    assert_eq!(code, StatusCode::NOT_FOUND, "изменение — не проблема");
    // Отменённое изменение не висит у согласующего.
    let (_, c2) = eng1
        .post(
            "/api/helpdesk/itil",
            json!({"kind": "change", "title": "Сменить картридж", "category_id": cat, "risk": "medium"}),
        )
        .await;
    let cn2 = c2["number"].as_i64().unwrap();
    let (_, r) = eng1.get(&format!("/api/helpdesk/itil/{cn2}")).await;
    assert_eq!(r["change"]["risk"], "medium", "риск записан при создании: {r}");
    let plan2 = json!({"risk": "medium", "plan": "Заменить", "rollback": "Вернуть старый"});
    eng1.put(&format!("/api/helpdesk/itil/{cn2}"), plan2).await;
    let (_, s) = eng1.post(&format!("/api/helpdesk/itil/{cn2}/submit"), json!({})).await;
    assert_eq!(s["approval"], "pending", "{s}");
    let (code, b) = eng1.post(&format!("/api/helpdesk/tickets/{cn2}/cancel"), json!({"comment": "Не нужно"})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (_, mine) = emp.get("/api/helpdesk/change-approvals").await;
    assert!(mine.as_array().unwrap().iter().all(|a| a["number"] != cn2 || a["status"] == "skipped"), "{mine}");
    // Своё не согласовать, даже если отправил другой; после смены категории — согласование заново.
    let (_, c3) = eng1
        .post(
            "/api/helpdesk/itil",
            json!({"kind": "change", "title": "Перезапустить сервер печати", "category_id": cat, "risk": "low"}),
        )
        .await;
    let cn3 = c3["number"].as_i64().unwrap();
    eng1.put(&format!("/api/helpdesk/itil/{cn3}"), json!({"risk": "low", "plan": "Перезапустить", "rollback": ""}))
        .await;
    let (code, s) = eng2.post(&format!("/api/helpdesk/itil/{cn3}/submit"), json!({})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "согласующий по правилу — автор изменения: {s}");
    let plan3 = json!({"risk": "medium", "plan": "Перезапустить", "rollback": "Запустить вручную"});
    eng1.put(&format!("/api/helpdesk/itil/{cn3}"), plan3).await;
    let (_, s) = eng1.post(&format!("/api/helpdesk/itil/{cn3}/submit"), json!({})).await;
    assert_eq!((s["approval"].as_str(), s["approvers"].as_i64()), (Some("pending"), Some(1)), "{s}");
    let (_, mine) = emp.get("/api/helpdesk/change-approvals").await;
    let a5 = id(mine.as_array().unwrap().iter().find(|a| a["number"] == cn3 && a["status"] == "pending").unwrap());
    emp.post(&format!("/api/helpdesk/change-approvals/{a5}"), json!({"approve": true})).await;
    let (_, r) = eng1.get(&format!("/api/helpdesk/itil/{cn3}")).await;
    assert_eq!(r["change"]["approval"], "approved", "{r}");
    let other =
        cats.as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).find(|c| *c != cat).unwrap();
    let v = ticket(&mut eng1, cn3).await["version"].clone();
    let (code, b) =
        eng1.put(&format!("/api/helpdesk/tickets/{cn3}"), json!({"version": v, "category_id": other})).await;
    assert!(code.is_success(), "{code} {b}");
    let (_, r) = eng1.get(&format!("/api/helpdesk/itil/{cn3}")).await;
    assert_eq!(r["change"]["approval"], "draft", "согласовывали в другой категории: {r}");
    let (_, rules) = admin.get("/api/helpdesk/change-rules").await;
    for r in rules.as_array().unwrap() {
        assert_eq!(admin.delete(&format!("/api/helpdesk/change-rules/{}", id(r))).await, StatusCode::NO_CONTENT);
    }

    // --- Хранилище секретов ---
    let (code, _) = emp.get("/api/helpdesk/vault").await;
    assert_eq!(code, StatusCode::FORBIDDEN, "сотруднику хранилище не положено");
    let (code, b) = eng1
        .post(
            "/api/helpdesk/vault",
            json!({"name": "Zabbix", "login": "svc_zbx", "url": "https://zabbix.local", "value": "Zbx-Pa55-1"}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{b}");
    let sid = id(&b);
    let vault = |b: &Value| -> Vec<(String, String)> {
        b.as_array()
            .unwrap()
            .iter()
            .map(|s| (s["name"].as_str().unwrap().to_string(), s["level"].as_str().unwrap().to_string()))
            .collect()
    };
    let (_, mine) = eng1.get("/api/helpdesk/vault").await;
    assert_eq!(vault(&mine), vec![("Zabbix".to_string(), "manage".to_string())]);
    assert!(mine[0].get("value").is_none() && !mine.to_string().contains("Zbx-Pa55-1"), "в списке значения нет");
    let (_, theirs) = eng2.get("/api/helpdesk/vault").await;
    assert!(vault(&theirs).is_empty(), "чужой секрет не виден");
    let (code, _) = eng2.get(&format!("/api/helpdesk/vault/{sid}")).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    let (_, all) = admin.get("/api/helpdesk/vault").await;
    assert_eq!(vault(&all).len(), 1, "суперадминистратор видит все: {all}");
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM vault_secrets WHERE position(convert_to('Zbx-Pa55-1', 'UTF8') in sealed) > 0",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(leaked, 0, "в базе значение только зашифрованным");
    // Доступ: второму инженеру — только карточка.
    let access = format!("/api/helpdesk/vault/{sid}/access");
    let (_, b) = eng1.get(&access).await;
    assert_eq!(b["version"], 1, "{b}");
    let (code, b) = eng1
        .put(
            &access,
            json!({"version": 1, "entries": [{"user_id": u1, "level": "manage"}, {"person_id": pid("eng2").await, "level": "view"}]}),
        )
        .await;
    assert_eq!(
        (code, b["entries"].as_array().map(Vec::len), b["version"].as_i64()),
        (StatusCode::OK, Some(2), Some(2)),
        "{b}"
    );
    let (code, _) = eng1.put(&access, json!({"version": 1, "entries": [{"user_id": u1, "level": "manage"}]})).await;
    assert_eq!(code, StatusCode::CONFLICT, "правка поверх чужой");
    let (code, b) = eng1.put(&access, json!({"version": 2, "entries": [{"user_id": u1, "level": "view"}]})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "без управляющего нельзя: {b}");
    let (code, _) =
        eng1.put(&access, json!({"version": 2, "entries": [{"role": "engineer", "level": "manage"}]})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "управляющий — сотрудник, а не только роль");
    let (code, _) = eng1
        .put(
            &access,
            json!({"version": 2, "entries": [{"role": "nope", "level": "view"}, {"user_id": u1, "level": "manage"}]}),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "нет такой роли");
    let (_, theirs) = eng2.get("/api/helpdesk/vault").await;
    assert_eq!(vault(&theirs), vec![("Zabbix".to_string(), "view".to_string())]);
    let reveal = format!("/api/helpdesk/vault/{sid}/reveal");
    let (code, _) = eng2.post(&reveal, json!({"reason": "проверка"})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "карточка — не значение");
    let (code, _) = eng2.get(&access).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    // Роль «Инженер» — со значением.
    let (code, b) = eng1
        .put(&access, json!({"version": 2, "entries": [{"user_id": u1, "level": "manage"}, {"role": "engineer", "level": "reveal"}]}))
        .await;
    assert_eq!(code, StatusCode::OK, "{b}");
    let (code, v) = eng2.post(&reveal, json!({"reason": "Настройка шаблона"})).await;
    assert_eq!((code, v["value"].as_str()), (StatusCode::OK, Some("Zbx-Pa55-1")), "{v}");
    let (code, _) = emp.post(&reveal, json!({})).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    for empty in [json!({}), json!({"reason": "  "}), json!({"reason": "."})] {
        let (code, v) = eng2.post(&reveal, empty).await;
        assert_eq!((code, v["error"].as_str()), (StatusCode::BAD_REQUEST, Some("reason_required")), "{v}");
    }
    let shown: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE action = 'vault.reveal' AND details->>'reason' = 'Настройка шаблона'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(shown, 1, "показ значения — в журнале");
    // Правка: только управляющий, по версии; новое значение меняет дату смены.
    let (_, card) = eng1.get(&format!("/api/helpdesk/vault/{sid}")).await;
    let upd = json!({"name": "Zabbix (прод)", "login": "svc_zbx", "url": "https://zabbix.local", "version": card["version"], "value": "Zbx-Pa55-2"});
    let (code, _) = eng2.put(&format!("/api/helpdesk/vault/{sid}"), upd.clone()).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let (code, b) = eng1.put(&format!("/api/helpdesk/vault/{sid}"), upd.clone()).await;
    assert_eq!((code, b["name"].as_str()), (StatusCode::OK, Some("Zabbix (прод)")), "{b}");
    let (code, _) = eng1.put(&format!("/api/helpdesk/vault/{sid}"), upd).await;
    assert_eq!(code, StatusCode::CONFLICT, "устаревшая версия");
    let (_, v) = eng2.post(&reveal, json!({"reason": "после смены"})).await;
    assert_eq!(v["value"], "Zbx-Pa55-2");
    let leaked: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE details::text LIKE '%Zbx-Pa55%'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(leaked, 0, "значения нет в журнале");
    // Шифротекст привязан к секрету: переставленный в другую запись — не расшифруется.
    let (_, b2) = eng1.post("/api/helpdesk/vault", json!({"name": "Второй", "value": "other"})).await;
    let sid2 = id(&b2);
    sqlx::query(
        "UPDATE vault_secrets SET sealed = (SELECT sealed FROM vault_secrets WHERE id = $1::uuid) WHERE id = $2::uuid",
    )
    .bind(&sid)
    .bind(&sid2)
    .execute(&pool)
    .await
    .unwrap();
    let (code, _) = eng1.post(&format!("/api/helpdesk/vault/{sid2}/reveal"), json!({"reason": "проверка"})).await;
    assert_eq!(code, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(eng2.delete(&format!("/api/helpdesk/vault/{sid}")).await, StatusCode::FORBIDDEN);
    for s in [&sid, &sid2] {
        assert_eq!(eng1.delete(&format!("/api/helpdesk/vault/{s}")).await, StatusCode::NO_CONTENT);
    }
    let (_, theirs) = eng2.get("/api/helpdesk/vault").await;
    assert!(vault(&theirs).is_empty());

    // --- Помощник: своя модель за OpenAI-совместимым API (здесь — поддельная) ---
    type Seen = Arc<std::sync::Mutex<Vec<(Option<String>, Value)>>>;
    let seen: Seen = Arc::default();
    let llm = {
        let seen = seen.clone();
        Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |headers: axum::http::HeaderMap, axum::Json(b): axum::Json<Value>| {
                let seen = seen.clone();
                async move {
                    let auth = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).map(str::to_string);
                    let system = b["messages"][0]["content"].as_str().unwrap_or_default().to_string();
                    seen.lock().unwrap().push((auth, b));
                    let content = if system.starts_with("Choose the best category") {
                        "1"
                    } else {
                        "<think>рассуждение</think>Ответ помощника [KB-1]"
                    };
                    axum::Json(json!({"choices": [{"message": {"role": "assistant", "content": content}}]}))
                }
            }),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let llm_url = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, llm).await.unwrap() });
    let ask = json!({"messages": [{"role": "user", "content": "Как подключить VPN и где пароль от роутера?"}]});
    let (_, st) = emp.get("/api/helpdesk/assist").await;
    assert_eq!(st["enabled"], false, "{st}");
    let (code, _) = emp.post("/api/helpdesk/assist/chat", ask.clone()).await;
    assert_eq!(code, StatusCode::NOT_FOUND, "выключенного помощника нет");
    let settings = |url: &str| json!({"enabled": true, "base_url": url, "model": "test-model", "timeout_secs": 10, "max_tokens": 256});
    let (code, _) = eng1.put("/api/admin/assist", json!({"settings": settings(&llm_url)})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "настраивает только суперадминистратор");
    let (code, b) = admin.put("/api/admin/assist", json!({"settings": settings("ftp://llm.local")})).await;
    assert_eq!((code, b["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_settings")), "{b}");
    let (code, b) =
        admin.put("/api/admin/assist", json!({"settings": settings(&llm_url), "api_key": "llm-key-1"})).await;
    assert_eq!((code, b["has_key"].as_bool()), (StatusCode::OK, Some(true)), "{b}");
    assert!(!b.to_string().contains("llm-key-1"), "ключ не возвращается");
    let (code, b) = admin.put("/api/admin/assist", json!({"settings": settings("http://127.0.0.1:9/v1")})).await;
    assert_eq!(
        (code, b["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("key_required")),
        "ключ не уходит на другой адрес: {b}"
    );
    let (code, b) = admin.post("/api/admin/assist/test", json!({})).await;
    assert_eq!((code, b["ok"].as_bool()), (StatusCode::OK, Some(true)), "{b}");
    let (_, st) = emp.get("/api/helpdesk/assist").await;
    assert_eq!(st["enabled"], true);
    // Сотрудник: ответ без рассуждений модели; в контексте — только статьи «для всех».
    seen.lock().unwrap().clear();
    let (code, a) = emp.post("/api/helpdesk/assist/chat", ask.clone()).await;
    assert_eq!((code, a["text"].as_str()), (StatusCode::OK, Some("Ответ помощника [KB-1]")), "{a}");
    assert_eq!(a["sources"][0]["title"], "Как подключить VPN", "{a}");
    {
        let seen = seen.lock().unwrap();
        let (auth, req) = seen.last().unwrap();
        assert_eq!(auth.as_deref(), Some("Bearer llm-key-1"));
        assert_eq!(req["model"], "test-model");
        let system = req["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("Как подключить VPN") && !system.contains("Пароль от роутера"), "{system}");
        assert!(system.contains("Answer in Russian"), "{system}");
    }
    let (_, a) = eng1.post("/api/helpdesk/assist/chat", ask.clone()).await;
    assert!(
        a["sources"].as_array().unwrap().iter().any(|s| s["title"] == "Пароль от роутера"),
        "инженеру — и статьи для инженеров: {a}"
    );
    let (code, _) =
        emp.post("/api/helpdesk/assist/chat", json!({"messages": [{"role": "assistant", "content": "привет"}]})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "последним — вопрос пользователя");
    // Инженер: черновик ответа и краткое содержание; сотруднику — нельзя. Внутренняя заметка в черновик
    // (его увидит заявитель) не уходит, в краткое содержание — уходит.
    let at = create(&mut emp, new("Не печатает принтер 3")).await;
    let (code, b) = eng1
        .post(&format!("/api/helpdesk/tickets/{at}/comments"), json!({"body": "Заметка-для-своих", "internal": true}))
        .await;
    assert!(code.is_success(), "{code} {b}");
    let last_prompt =
        |seen: &Seen| seen.lock().unwrap().last().unwrap().1["messages"][1]["content"].as_str().unwrap().to_string();
    let (code, d) = eng1.post(&format!("/api/helpdesk/assist/tickets/{at}/draft"), json!({})).await;
    assert_eq!(code, StatusCode::OK, "{d}");
    assert!(
        d["sources"].as_array().unwrap().iter().all(|s| s["kind"] == "kb" && s["title"] != "Пароль от роутера"),
        "в черновик — только статьи для всех: {d}"
    );
    let prompt = last_prompt(&seen);
    assert!(prompt.contains("Не печатает принтер 3") && !prompt.contains("Заметка-для-своих"), "{prompt}");
    let (code, _) = eng1.post(&format!("/api/helpdesk/assist/tickets/{at}/summary"), json!({})).await;
    assert_eq!(code, StatusCode::OK);
    assert!(last_prompt(&seen).contains("Заметка-для-своих"));
    let (code, _) = emp.post(&format!("/api/helpdesk/assist/tickets/{inc1}/draft"), json!({})).await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    let (code, _) = eng1.post("/api/helpdesk/assist/tickets/999999/summary", json!({})).await;
    assert_eq!(code, StatusCode::NOT_FOUND);
    // Категория: номер из списка, который видит модель.
    let (code, c) = emp.post("/api/helpdesk/assist/category", json!({"title": "Не печатает принтер"})).await;
    assert_eq!(code, StatusCode::OK, "{c}");
    let (_, cats) = emp.get("/api/helpdesk/categories").await;
    assert!(cats.as_array().unwrap().iter().any(|x| x["id"] == c["category_id"]), "{c}");
    // Сервер модели недоступен — 503, портал работает.
    let (code, _) =
        admin.put("/api/admin/assist", json!({"settings": settings("http://127.0.0.1:9/v1"), "api_key": ""})).await;
    assert_eq!(code, StatusCode::OK);
    let (code, b) = emp.post("/api/helpdesk/assist/chat", ask).await;
    assert_eq!((code, b["error"].as_str()), (StatusCode::SERVICE_UNAVAILABLE, Some("assist_unavailable")), "{b}");
    let (code, _) = admin
        .put("/api/admin/assist", json!({"settings": {"enabled": false, "base_url": "", "model": "", "timeout_secs": 60, "max_tokens": 1024}}))
        .await;
    assert_eq!(code, StatusCode::OK);

    // --- CMDB: связи, влияние, карта, предложения, жизненный цикл, лицензии ---
    let (_, types) = eng1.get("/api/helpdesk/asset-types").await;
    let type_id = |name: &str| {
        types.as_array().unwrap().iter().find(|t| t["name"] == name).unwrap_or_else(|| panic!("тип {name}"))["id"]
            .clone()
    };
    let (server_t, service_t) = (type_id("Сервер"), type_id("Сервис / ИС"));
    let mk = async |b: &mut Browser, ty: &Value, name: &str, room: &str| -> String {
        let body = json!({"type_id": ty, "name": name, "status": "in_use", "room": room, "attributes": {}});
        let (code, r) = b.post("/api/helpdesk/assets", body).await;
        assert_eq!(code, StatusCode::CREATED, "{r}");
        id(&r)
    };
    let srv = mk(&mut eng1, &server_t, "SRV-DB-01", "Серверная").await;
    let crm = mk(&mut eng1, &service_t, "CRM", "").await;
    let portal = mk(&mut eng1, &service_t, "Портал продаж", "").await;
    let ups = mk(&mut eng1, &server_t, "SRV-APP-02", "Серверная").await;
    let (code, rtypes) = eng1.get("/api/helpdesk/asset-relation-types").await;
    assert_eq!(code, StatusCode::OK, "{rtypes}");
    let rtype = |name: &str| {
        rtypes.as_array().unwrap().iter().find(|t| t["name"] == name).unwrap_or_else(|| panic!("связь {name}"))["id"]
            .clone()
    };
    let (depends, part_of) = (rtype("Зависит от"), rtype("Входит в"));
    let rel = |a: &str| format!("/api/helpdesk/assets/{a}/relations");
    // CRM зависит от сервера БД; портал продаж зависит от CRM (задано со стороны CRM — «нужен для»).
    let (code, b) = emp.post(&rel(&crm), json!({"type_id": depends, "other_id": srv})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "сотрудник не ведёт CMDB: {b}");
    let (code, b) = eng1.post(&rel(&crm), json!({"type_id": depends, "other_id": srv})).await;
    assert_eq!(code, StatusCode::CREATED, "{b}");
    let (code, b) = eng1.post(&rel(&crm), json!({"type_id": depends, "other_id": portal, "direction": "in"})).await;
    assert_eq!(code, StatusCode::CREATED, "{b}");
    let (code, b) = eng1.post(&rel(&crm), json!({"type_id": depends, "other_id": srv})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "повтор: {b}");
    let (code, b) = eng1.post(&rel(&crm), json!({"type_id": depends, "other_id": crm})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "сам с собой: {b}");
    // Связь без влияния в анализ не попадает.
    let (code, b) = eng1.post(&rel(&ups), json!({"type_id": part_of, "other_id": srv})).await;
    assert_eq!(code, StatusCode::CREATED, "{b}");
    let (code, list) = eng1.get(&rel(&crm)).await;
    assert_eq!(code, StatusCode::OK, "{list}");
    let labels: Vec<(String, String)> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["label"].as_str().unwrap().to_string(), r["other_name"].as_str().unwrap().to_string()))
        .collect();
    assert!(labels.contains(&("Зависит от".into(), "SRV-DB-01".into())), "{list}");
    assert!(labels.contains(&("Нужен для".into(), "Портал продаж".into())), "{list}");
    // Отказ сервера БД затронет CRM напрямую и портал через CRM; SRV-APP-02 («входит в») — нет.
    let (code, hit) = eng1.get(&format!("/api/helpdesk/assets/{srv}/impact")).await;
    assert_eq!(code, StatusCode::OK, "{hit}");
    let hit: Vec<(String, i64)> = hit
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (x["name"].as_str().unwrap().to_string(), x["depth"].as_i64().unwrap()))
        .collect();
    assert_eq!(hit, vec![("CRM".to_string(), 1), ("Портал продаж".to_string(), 2)]);
    assert_eq!(emp.get(&format!("/api/helpdesk/assets/{srv}/impact")).await.0, StatusCode::FORBIDDEN);
    // Цикл не зацикливает анализ.
    let (code, b) = eng1.post(&rel(&srv), json!({"type_id": depends, "other_id": portal})).await;
    assert_eq!(code, StatusCode::CREATED, "{b}");
    let (code, hit) = eng1.get(&format!("/api/helpdesk/assets/{srv}/impact")).await;
    assert_eq!(code, StatusCode::OK, "{hit}");
    assert_eq!(hit.as_array().unwrap().len(), 2, "{hit}");
    let (code, map) = eng1.get(&format!("/api/helpdesk/assets/{crm}/map")).await;
    assert_eq!(code, StatusCode::OK, "{map}");
    assert_eq!(map["nodes"].as_array().unwrap().len(), 4, "CRM, сервер, портал и SRV-APP-02 в двух шагах: {map}");
    assert_eq!(map["edges"].as_array().unwrap().len(), 4, "{map}");
    // Мониторинг ↔ CMDB: авария сервера БД (узел — FQDN) делает красными CRM и портал продаж.
    let src: Uuid = sqlx::query_scalar(
        "INSERT INTO alert_sources (name, token_hash) VALUES ('cmdb-test', sha256('cmdb-test'::bytea)) RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO alerts (source_id, fingerprint, title, severity, host)
         VALUES ($1, 'db-down', 'PostgreSQL недоступен', 'critical', 'srv-db-01.corp.local'),
                ($1, 'other', 'Чужой узел', 'warning', 'nobody.corp.local')",
    )
    .bind(src)
    .execute(&pool)
    .await
    .unwrap();
    let (code, own) = eng1.get(&format!("/api/helpdesk/assets/{srv}/alerts")).await;
    assert_eq!(code, StatusCode::OK, "{own}");
    assert_eq!(own.as_array().unwrap().len(), 1, "оповещение по FQDN — у сервера: {own}");
    let (_, own) = eng1.get(&format!("/api/helpdesk/assets/{crm}/alerts")).await;
    assert_eq!(own, json!([]), "у CRM своих оповещений нет");
    let (code, svc) = eng1.get("/api/helpdesk/monitoring/services").await;
    assert_eq!(code, StatusCode::OK, "{svc}");
    let state = |name: &str| {
        svc.as_array().unwrap().iter().find(|s| s["name"] == name).unwrap_or_else(|| panic!("{name}: {svc}")).clone()
    };
    let (c, p) = (state("CRM"), state("Портал продаж"));
    assert_eq!(c["state"], "critical", "{c}");
    assert_eq!(p["state"], "critical", "через CRM: {p}");
    assert_eq!(c["causes"][0]["asset_name"], "SRV-DB-01", "{c}");
    assert!(svc.as_array().unwrap().iter().all(|s| s["name"] != "SRV-DB-01"), "сервер — не сервис: {svc}");
    assert_eq!(emp.get("/api/helpdesk/monitoring/services").await.0, StatusCode::FORBIDDEN);
    sqlx::query("UPDATE alerts SET status = 'resolved', resolved_at = now() WHERE source_id = $1")
        .bind(src)
        .execute(&pool)
        .await
        .unwrap();
    let (_, svc) = eng1.get("/api/helpdesk/monitoring/services").await;
    assert!(svc.as_array().unwrap().iter().all(|s| s["state"] == "ok"), "решено — всё работает: {svc}");
    // Предложения: тот же кабинет; уже связанные не предлагаются.
    let other = mk(&mut eng1, &server_t, "SRV-FILE-03", "Серверная").await;
    let (code, sug) = eng1.get(&format!("/api/helpdesk/assets/{srv}/relation-suggestions")).await;
    assert_eq!(code, StatusCode::OK, "{sug}");
    let sug: Vec<&str> = sug.as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(sug, vec!["SRV-FILE-03"], "SRV-APP-02 уже связан");
    // Удаление связи — только в аудите.
    let first = list.as_array().unwrap().iter().find(|r| r["other_name"] == "SRV-DB-01").unwrap()["id"].clone();
    let first = first.as_str().unwrap();
    assert_eq!(eng1.delete(&format!("/api/helpdesk/assets/{crm}/relations/{first}")).await, StatusCode::NO_CONTENT);
    assert_eq!(eng1.delete(&format!("/api/helpdesk/assets/{crm}/relations/{first}")).await, StatusCode::NOT_FOUND);
    let removed: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'asset.relation.remove'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(removed, 1);
    // Жизненный цикл: у серверов нет «в ремонте», списать — только со склада.
    let life = format!("/api/admin/helpdesk/asset-types/{}/lifecycle", server_t.as_str().unwrap());
    let body = json!({"statuses": ["in_use", "in_stock", "retired"], "transitions": [["in_stock", "in_use"], ["in_use", "in_stock"], ["in_stock", "retired"]]});
    assert_eq!(eng1.put(&life, body.clone()).await.0, StatusCode::FORBIDDEN);
    let (code, b) = admin.put(&life, json!({"statuses": ["in_use"], "transitions": [["in_use", "retired"]]})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "переход в состояние, которого нет: {b}");
    let (code, b) = admin.put(&life, body).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let edit = |status: &str| json!({"type_id": server_t, "name": "SRV-FILE-03", "status": status, "room": "Серверная", "attributes": {}});
    let (code, b) = eng1.put(&format!("/api/helpdesk/assets/{other}"), edit("repair")).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "у типа нет «в ремонте»: {b}");
    let (code, b) = eng1.put(&format!("/api/helpdesk/assets/{other}"), edit("retired")).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "из работы сразу не списать: {b}");
    let (code, b) = eng1.put(&format!("/api/helpdesk/assets/{other}"), edit("in_stock")).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (code, b) = eng1.put(&format!("/api/helpdesk/assets/{other}"), edit("retired")).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    let (_, types) = eng1.get("/api/helpdesk/asset-types").await;
    let srv_type = types.as_array().unwrap().iter().find(|t| t["name"] == "Сервер").unwrap();
    assert_eq!(srv_type["lifecycle"]["statuses"], json!(["in_use", "in_stock", "retired"]), "{srv_type}");
    // Лицензии: установки — по программам из отчётов агента.
    for (host, prog) in [
        ("LIC-PC-1", "Microsoft Office Professional 2021"),
        ("LIC-PC-2", "Microsoft Office Standard 2019"),
        ("LIC-PC-3", "7-Zip 23.01"),
    ] {
        let dev: Uuid = sqlx::query_scalar("INSERT INTO devices (hostname) VALUES ($1) RETURNING id")
            .bind(host)
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO device_software (device_id, name, version) VALUES ($1, $2, '1.0')")
            .bind(dev)
            .bind(prog)
            .execute(&pool)
            .await
            .unwrap();
    }
    let lic = json!({"name": "Office", "vendor": "Microsoft", "pattern": "microsoft office", "seats": 1, "expires_on": "2020-01-31"});
    let (code, b) = emp.post("/api/helpdesk/licenses", lic.clone()).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "{b}");
    let (code, b) = eng1.post("/api/helpdesk/licenses", json!({"name": "x", "pattern": "o"})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "короткая строка поиска: {b}");
    let (code, b) = eng1
        .post("/api/helpdesk/licenses", json!({"name": "x", "pattern": "office", "expires_on": "31.01.2020"}))
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "дата — ГГГГ-ММ-ДД: {b}");
    let (code, b) = eng1.post("/api/helpdesk/licenses", lic).await;
    assert_eq!(code, StatusCode::CREATED, "{b}");
    let lic_id = id(&b);
    let (code, list) = eng1.get("/api/helpdesk/licenses").await;
    assert_eq!(code, StatusCode::OK, "{list}");
    let office = list.as_array().unwrap().iter().find(|l| l["name"] == "Office").unwrap();
    assert_eq!(office["installed"], 2, "{office}");
    assert_eq!(office["seats"], 1, "{office}");
    assert_eq!(office["expires_on"], "2020-01-31", "{office}");
    let (code, d) = eng1.get(&format!("/api/helpdesk/licenses/{lic_id}")).await;
    assert_eq!(code, StatusCode::OK, "{d}");
    let hosts: Vec<&str> = d["devices"].as_array().unwrap().iter().map(|x| x["hostname"].as_str().unwrap()).collect();
    assert_eq!(hosts, vec!["LIC-PC-1", "LIC-PC-2"]);
    assert_eq!(eng1.delete(&format!("/api/helpdesk/licenses/{lic_id}")).await, StatusCode::NO_CONTENT);

    // --- Инвентарь из NetBox (здесь — поддельный): синхронизация, проверка инженером ---
    let edge_gone = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let netbox = {
        let gone = edge_gone.clone();
        let devices = move |headers: axum::http::HeaderMap,
                            axum::extract::Query(q): axum::extract::Query<
            std::collections::HashMap<String, String>,
        >| {
            let gone = gone.clone();
            async move {
                let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
                if headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) != Some("Token nb-token") {
                    return (StatusCode::FORBIDDEN, axum::Json(json!({"detail": "Invalid token"})));
                }
                let page = if q.get("offset").map(String::as_str) == Some("2") {
                    let mut rest = vec![];
                    if !gone.load(std::sync::atomic::Ordering::SeqCst) {
                        rest.push(json!({"id": 3, "name": "edge-rt-01", "device_type": {"model": "MX204", "manufacturer": {"name": "Juniper"}}}));
                    }
                    json!({"count": 3, "next": null, "results": rest})
                } else {
                    json!({"count": 3, "next": format!("http://{host}/api/dcim/devices/?limit=250&offset=2"), "results": [
                        {"id": 1, "name": "core-sw-01", "serial": "FOC123", "device_type": {"model": "C9300", "manufacturer": {"name": "Cisco"}},
                         "primary_ip": {"address": "10.0.0.2/24"}, "site": {"name": "HQ"}, "role": {"name": "Core switch"}},
                        {"id": 2, "name": "srv-esx-01", "device_role": {"name": "Hypervisor"}, "platform": {"name": "VMware ESXi 8"}}
                    ]})
                };
                (StatusCode::OK, axum::Json(page))
            }
        };
        Router::new().route("/api/dcim/devices/", axum::routing::get(devices)).route(
            "/api/virtualization/virtual-machines/",
            axum::routing::get(|| async {
                axum::Json(json!({"count": 1, "next": null, "results": [{"id": 9, "name": "vm-1c-01", "primary_ip": {"address": "10.0.1.5/24"}}]}))
            }),
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let nb_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, netbox).await.unwrap() });
    let nb = |url: &str| json!({"enabled": true, "base_url": url, "import_vms": true});
    assert_eq!(eng1.get("/api/admin/inventory/netbox").await.0, StatusCode::FORBIDDEN);
    let (code, b) = admin.put("/api/admin/inventory/netbox", json!({"settings": nb(&nb_url)})).await;
    assert_eq!((code, b["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_settings")), "без токена: {b}");
    let (code, b) =
        admin.put("/api/admin/inventory/netbox", json!({"settings": nb(&nb_url), "token": "nb-token"})).await;
    assert_eq!((code, b["has_token"].as_bool()), (StatusCode::OK, Some(true)), "{b}");
    assert!(!b.to_string().contains("nb-token"), "токен не возвращается");
    let (code, st) = admin.post("/api/admin/inventory/netbox/sync", json!({})).await;
    assert_eq!(code, StatusCode::OK, "{st}");
    assert_eq!(
        (st["fetched"].as_i64(), st["added"].as_i64(), st["missing"].as_i64()),
        (Some(4), Some(4), Some(0)),
        "{st}"
    );
    assert!(st["last_error"].is_null(), "{st}");
    assert_eq!(emp.get("/api/helpdesk/inventory/external").await.0, StatusCode::FORBIDDEN);
    let found = async |b: &mut Browser, rejected: bool| -> Vec<Value> {
        let (code, list) = b.get(&format!("/api/helpdesk/inventory/external?rejected={rejected}")).await;
        assert_eq!(code, StatusCode::OK, "{list}");
        list.as_array().unwrap().clone()
    };
    let list = found(&mut eng1, false).await;
    assert_eq!(list.len(), 4, "{list:?}");
    let by = |list: &[Value], name: &str| {
        list.iter().find(|x| x["name"] == name).unwrap_or_else(|| panic!("{name}")).clone()
    };
    let sw = by(&list, "core-sw-01");
    assert_eq!(
        (sw["ip"].as_str(), sw["model"].as_str(), sw["role"].as_str()),
        (Some("10.0.0.2"), Some("C9300"), Some("Core switch"))
    );
    assert_eq!(by(&list, "srv-esx-01")["role"], "Hypervisor", "NetBox 3.x: device_role");
    assert_eq!(by(&list, "vm-1c-01")["kind"], "vm");
    let net_t = type_id("Сетевое оборудование");
    let acc = |id: &Value| format!("/api/helpdesk/inventory/external/{}/accept", id.as_str().unwrap());
    let (code, b) = eng1.post(&acc(&sw["id"]), json!({"type_id": net_t, "inventory_number": "NET-001"})).await;
    assert_eq!(code, StatusCode::CREATED, "{b}");
    let (_, a) = eng1.get(&format!("/api/helpdesk/assets/{}", id(&b))).await;
    assert_eq!(a["attributes"], json!({"model": "Cisco C9300", "ip": "10.0.0.2"}), "{a}");
    assert_eq!((a["serial_number"].as_str(), a["status"].as_str()), (Some("FOC123"), Some("in_use")), "{a}");
    assert_eq!(
        eng1.post(&acc(&sw["id"]), json!({"type_id": net_t})).await.0,
        StatusCode::NOT_FOUND,
        "второй раз не принимается"
    );
    let vm = by(&list, "vm-1c-01")["id"].as_str().unwrap().to_string();
    assert_eq!(
        eng1.post(&format!("/api/helpdesk/inventory/external/{vm}/reject"), json!({})).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(found(&mut eng1, false).await.len(), 2);
    assert_eq!(found(&mut eng1, true).await.len(), 1);
    assert_eq!(
        eng1.post(&format!("/api/helpdesk/inventory/external/{vm}/restore"), json!({})).await.0,
        StatusCode::NO_CONTENT
    );
    // Повторная синхронизация: принятое не трогается, пропавшее в NetBox уходит из списка.
    edge_gone.store(true, std::sync::atomic::Ordering::SeqCst);
    let (_, st) = admin.post("/api/admin/inventory/netbox/sync", json!({})).await;
    assert_eq!(
        (st["fetched"].as_i64(), st["added"].as_i64(), st["missing"].as_i64()),
        (Some(3), Some(0), Some(1)),
        "{st}"
    );
    let names: Vec<String> =
        found(&mut eng1, false).await.iter().map(|x| x["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(!names.contains(&"edge-rt-01".to_string()) && !names.contains(&"core-sw-01".to_string()), "{names:?}");
    // Неверный токен: ошибка сохраняется в состоянии, найденное остаётся.
    let (code, _) = admin.put("/api/admin/inventory/netbox", json!({"settings": nb(&nb_url), "token": "wrong"})).await;
    assert_eq!(code, StatusCode::OK);
    let (_, st) = admin.post("/api/admin/inventory/netbox/sync", json!({})).await;
    assert!(st["last_error"].as_str().is_some_and(|e| e.contains("403")), "{st}");
    assert_eq!(found(&mut eng1, false).await.len(), 2);
    let (code, b) = admin.put("/api/admin/inventory/netbox", json!({"settings": nb("http://127.0.0.1:9")})).await;
    assert_eq!(
        (code, b["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("key_required")),
        "токен не уходит на другой адрес: {b}"
    );

    // Язык: ответы сервера — на языке интерфейса, выбор хранится в учётке.
    emp.cookies.insert("pult_lang".into(), "en".into());
    let (code, e) = emp.get("/api/admin/locale").await;
    assert_eq!(code, StatusCode::FORBIDDEN);
    assert_eq!(e["message"], "insufficient permissions", "{e}");
    let (code, e) = emp.put("/api/me/locale", json!({"locale": "de"})).await;
    assert_eq!((code, e["message"].as_str()), (StatusCode::BAD_REQUEST, Some("Language: ru, en or zh.")));
    let (code, e) = emp.put("/api/me/locale", json!({"locale": "en"})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{e}");
    let (_, me) = emp.get("/api/me").await;
    assert_eq!(me["locale"], "en");
    emp.cookies.remove("pult_lang");
    let (_, e) = emp.get("/api/admin/locale").await;
    assert_eq!(e["message"], "недостаточно прав", "без выбора — русский");
    let (_, codes) = eng1.get("/api/helpdesk/close-codes").await;
    eng1.cookies.insert("pult_lang".into(), "en".into());
    let (_, codes_en) = eng1.get("/api/helpdesk/close-codes").await;
    assert_ne!(codes, codes_en, "стартовые коды закрытия переводятся");
    assert!(!codes_en.to_string().chars().any(pult_core::l10n::is_cyrillic), "{codes_en}");
    // Язык установки — для новых учёток.
    let (code, l) = admin.put("/api/admin/locale", json!({"default_locale": "en"})).await;
    assert_eq!(code, StatusCode::OK, "{l}");
    assert_eq!(pult_core::l10n::default_lang(&pool).await, pult_core::l10n::Lang::En);
    dir.add(FakeUser::person("newbie", "Новиков", "Нил", 40));
    Browser::login(&app, "newbie").await;
    let locale: String = sqlx::query_scalar(
        "SELECT u.locale FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id WHERE a.sam_account_name = 'newbie'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(locale, "en");
}
