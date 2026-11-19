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
