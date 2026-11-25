//! Сквозные сценарии заявок на настоящей PostgreSQL и поддельных DC.
//! Нужен `PULT_TEST_DATABASE_URL` (в CI задан). Тест создаёт отдельную базу, чтобы не зависеть от других.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, FromRef},
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use pult_core::{Core, Module, Secret, api::CoreModule, crypto::SecretBox, session::CookiePolicy};
use pult_helpdesk::{HelpdeskState, api::HelpdeskModule, tickets};
use pult_people::{PeopleModule, PeopleState};
use pult_testkit::{
    BASE_DN, Directory, FakeComputer, FakeDc, FakeUser, SVC_DN, SVC_PASS, TestCa, USER_FILTER, USER_PASS,
};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, PgPool};
use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};
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
    ip: String,
}

impl Browser {
    fn new(app: &Router) -> Self {
        Self { app: app.clone(), cookies: HashMap::new(), ip: "10.1.1.1:5000".into() }
    }

    async fn send(
        &mut self,
        method: Method,
        uri: &str,
        ctype: &str,
        body: Vec<u8>,
    ) -> (StatusCode, Value, axum::http::HeaderMap) {
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
        let mut req = req.header(header::CONTENT_TYPE, ctype).body(Body::from(body)).unwrap();
        req.extensions_mut().insert(ConnectInfo(self.ip.parse::<SocketAddr>().unwrap()));
        let resp = self.app.clone().oneshot(req).await.unwrap();
        for v in resp.headers().get_all(header::SET_COOKIE) {
            let s = v.to_str().unwrap();
            let (kv, _) = s.split_once(';').unwrap_or((s, ""));
            let (k, v) = kv.split_once('=').unwrap();
            self.cookies.insert(k.to_string(), v.to_string());
        }
        let (status, headers) = (resp.status(), resp.headers().clone());
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
        (status, json, headers)
    }

    async fn get(&mut self, uri: &str) -> (StatusCode, Value) {
        let (s, j, _) = self.send(Method::GET, uri, "application/json", Vec::new()).await;
        (s, j)
    }

    async fn post(&mut self, uri: &str, body: Value) -> (StatusCode, Value) {
        let (s, j, _) = self.send(Method::POST, uri, "application/json", body.to_string().into_bytes()).await;
        (s, j)
    }

    async fn put(&mut self, uri: &str, body: Value) -> (StatusCode, Value) {
        let (s, j, _) = self.send(Method::PUT, uri, "application/json", body.to_string().into_bytes()).await;
        (s, j)
    }

    async fn login(app: &Router, login: &str) -> Self {
        let mut b = Self::new(app);
        let (code, body) = b.post("/api/auth/login", json!({"login": login, "password": USER_PASS})).await;
        assert_eq!(code, StatusCode::NO_CONTENT, "{login}: {body}");
        b
    }

    async fn upload(&mut self, name: &str, data: &[u8]) -> (StatusCode, Value) {
        let b = "XBOUNDARYX";
        let mut body = format!("--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
        let (s, j, _) =
            self.send(Method::POST, "/api/helpdesk/files", &format!("multipart/form-data; boundary={b}"), body).await;
        (s, j)
    }
}

/// Своя база для теста.
async fn db() -> Option<PgPool> {
    let Ok(url) = std::env::var("PULT_TEST_DATABASE_URL") else {
        assert!(std::env::var("CI").is_err(), "в CI нужен PULT_TEST_DATABASE_URL");
        eprintln!("PULT_TEST_DATABASE_URL не задан — интеграционный тест пропущен");
        return None;
    };
    let name = format!("pult_stage2_{}", std::process::id());
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

const PNG: &[u8] = b"\x89PNG\r\n\x1a\nfake-image-content";

fn new_ticket(title: &str, site: &str, cat: &str) -> Value {
    json!({"title": title, "description": "Описание", "site_id": site, "category_id": cat})
}

async fn scalar_str(pool: &PgPool, sql: &str, n: i64) -> Option<String> {
    sqlx::query_scalar(sql).bind(n).fetch_optional(pool).await.unwrap().flatten()
}

async fn status_of(pool: &PgPool, n: i64) -> String {
    scalar_str(pool, "SELECT status FROM tickets WHERE number = $1", n).await.unwrap()
}

async fn outbox_for(pool: &PgPool, n: i64) -> Vec<(Vec<String>, String)> {
    sqlx::query_as(
        "SELECT to_addrs, body_text FROM outbox_emails WHERE subject LIKE '[#' || $1::text || ']%' ORDER BY created_at",
    )
    .bind(n.to_string())
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Токен из ссылки «Да / Нет» последнего письма о решении.
async fn token_of(pool: &PgPool, n: i64) -> String {
    let mails = outbox_for(pool, n).await;
    let text = mails.iter().rev().find(|(_, b)| b.contains("/ticket-response/")).expect("письмо о решении").1.clone();
    let tail = text.split("/ticket-response/").nth(1).unwrap();
    tail.split('?').next().unwrap().to_string()
}

/// Обычный сотрудник (без ролей инженера) запрашивает список компьютеров.
async fn emp2_search(app: &Router) -> StatusCode {
    Browser::login(app, "emp3").await.get("/api/helpdesk/computers?q=ws").await.0
}

#[tokio::test]
async fn stage2_acceptance() {
    let Some(pool) = db().await else { return };
    let files_dir = std::env::temp_dir().join(format!("pult-stage2-{}", std::process::id()));
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
    core.events.spawn_listener(pool.clone());
    tokio::time::sleep(Duration::from_millis(500)).await;

    let dir = Directory::new(vec![
        FakeUser::person("admin", "Админов", "Адам", 1),
        FakeUser::person("eng1", "Инженеров", "Первый", 2),
        FakeUser::person("eng2", "Техников", "Второй", 3),
        FakeUser::person("eng3", "Вторлиниев", "Третий", 4),
        FakeUser::person("emp", "Сотрудников", "Иван", 5),
        FakeUser::person("emp2", "Другой", "Пётр", 6),
        FakeUser::person("emp3", "Третьяков", "Семён", 7),
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
        helpdesk: HelpdeskState { files_dir },
    };
    let (router, _) = utoipa_axum::router::OpenApiRouter::new()
        .merge(<CoreModule as Module<St>>::routes(&CoreModule))
        .merge(<PeopleModule as Module<St>>::routes(&PeopleModule))
        .merge(<HelpdeskModule as Module<St>>::routes(&HelpdeskModule))
        .split_for_parts();
    let app: Router = router.with_state(st.clone());

    let mut admin = Browser::login(&app, "admin").await;
    let (code, s) = admin.post("/api/admin/ad/sync", json!({"kind": "full"})).await;
    assert_eq!(code, StatusCode::OK, "{s}");
    let mut eng1 = Browser::login(&app, "eng1").await;
    let eng2 = Browser::login(&app, "eng2").await;
    let mut eng3 = Browser::login(&app, "eng3").await;
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
    let (u1, u2, u3) = (uid("eng1").await, uid("eng2").await, uid("eng3").await);
    for u in [u1, u2, u3] {
        let (code, b) = admin.put(&format!("/api/admin/users/{u}/roles"), json!({"roles": ["engineer"]})).await;
        assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    }
    let (_, site_a) =
        admin.post("/api/admin/sites", json!({"name": "Офис А", "timezone": "Europe/Moscow", "is_active": true})).await;
    let (_, site_b) = admin
        .post(
            "/api/admin/sites",
            json!({"name": "Офис Б", "timezone": "Europe/Moscow", "is_active": true, "emergency_phone": "100"}),
        )
        .await;
    let (a, b) = (site_a["id"].as_str().unwrap().to_string(), site_b["id"].as_str().unwrap().to_string());
    let (code, r) = admin.put(&format!("/api/admin/sites/{a}/engineers"), json!({"l1": [u1, u2], "l2": [u3]})).await;
    assert!(code.is_success(), "{r}");
    // Сотрудники — площадки А. Письма шлём им по адресу справочника.
    sqlx::query(
        "UPDATE persons SET site_id = $1::uuid, email = a.mail FROM ad_accounts a WHERE a.id = persons.ad_account_id",
    )
    .bind(&a)
    .execute(&pool)
    .await
    .unwrap();
    // Рабочее время — круглые сутки, кроме проверки нерабочего времени ниже.
    let work_always = json!({"portal_url": "https://pult.example.test", "work_start": "00:00", "work_end": "23:59", "work_days": [1,2,3,4,5,6,7],
        "timezone": "Europe/Moscow", "auto_close_days": 3, "max_file_mb": 1, "login_help_subnets": ["10.0.0.0/8"], "emergency_phone": ""});
    assert_eq!(admin.put("/api/admin/helpdesk/settings", work_always.clone()).await.0, StatusCode::OK);
    let (_, cats) = emp.get("/api/helpdesk/categories").await;
    let cat = cats.as_array().unwrap().iter().find(|c| c["name"] == "Принтеры и сканеры").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        cats.as_array().unwrap().iter().all(|c| c["name"] != "Не указана"),
        "служебная категория в форме не показывается"
    );

    // Адрес портала для писем не запоминается из запроса сотрудника: заголовок Host можно подделать,
    // и ссылки «Да / Нет» с токенами в письмах вели бы на чужой сайт.
    let mut no_url = work_always.clone();
    no_url["portal_url"] = json!("");
    assert_eq!(admin.put("/api/admin/helpdesk/settings", no_url).await.0, StatusCode::OK);
    let (code, c) = emp.post("http://evil.example/api/helpdesk/tickets", new_ticket("Проверка адреса", &a, &cat)).await;
    assert_eq!(code, StatusCode::CREATED, "{c}");
    let (_, cfg) = admin.get("/api/admin/helpdesk/settings").await;
    assert_eq!(cfg["portal_url"], "", "адрес из запроса сотрудника не сохранён: {cfg}");
    let (code, _) = emp.post(&format!("/api/helpdesk/tickets/{}/cancel", c["number"]), json!({})).await;
    assert!(code.is_success());
    assert_eq!(admin.put("/api/admin/helpdesk/settings", work_always.clone()).await.0, StatusCode::OK);

    // --- Сотрудник создаёт заявку со скриншотом: появляется у наименее загруженного без перезагрузки ---
    let mut events = core.events.subscribe();
    let (code, up) = emp.upload("скрин.png", PNG).await;
    assert_eq!(code, StatusCode::OK, "{up}");
    assert_eq!(emp.upload("вирус.exe", b"MZ\x90").await.0, StatusCode::BAD_REQUEST, "опасный тип");
    assert_eq!(emp.upload("поддельный.png", b"<html><script>").await.0, StatusCode::BAD_REQUEST, "содержимое не то");
    assert_eq!(
        emp.upload("большой.png", &[PNG, &vec![0u8; 2 * 1024 * 1024]].concat()).await.0,
        StatusCode::BAD_REQUEST,
        "лимит размера"
    );
    // Квота: загруженные и не прикреплённые файлы не копятся без предела (диск портала общий).
    let mut accepted = 0;
    let quota = loop {
        let (code, r) = emp.upload(&format!("квота-{accepted}.txt"), format!("заметка {accepted}").as_bytes()).await;
        if code != StatusCode::OK || accepted > 60 {
            break (code, r);
        }
        accepted += 1;
    };
    assert_eq!(
        (quota.0, quota.1["error"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("too_many_requests")),
        "{}",
        quota.1
    );
    assert!((45..=50).contains(&accepted), "не больше 50 неприкреплённых файлов: {accepted}");
    sqlx::query("DELETE FROM files WHERE original_name LIKE 'квота-%'").execute(&pool).await.unwrap();
    let mut body = new_ticket("Не печатает принтер", &a, &cat);
    body["files"] = json!([up["id"]]);
    let (code, c1) = emp.post("/api/helpdesk/tickets", body).await;
    assert_eq!(code, StatusCode::CREATED, "{c1}");
    assert_eq!(c1["status"], "assigned");
    assert!(c1["assignee_name"].is_string());
    let n1 = c1["number"].as_i64().unwrap();
    let id1: Uuid =
        sqlx::query_scalar("SELECT id FROM tickets WHERE number = $1").bind(n1).fetch_one(&pool).await.unwrap();
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let e = events.recv().await.unwrap();
            if e.ticket == Some(id1) {
                return e;
            }
        }
    })
    .await
    .expect("событие SSE за 5 с");
    assert_eq!(got.permission.as_deref(), Some("helpdesk.ticket.work"), "инженеры видят новую заявку сразу");
    let (_, list) = eng1.get("/api/helpdesk/tickets?scope=all").await;
    assert!(list.as_array().unwrap().iter().any(|t| t["number"] == n1 && t["unread"] == true), "{list}");
    let first = sqlx::query_scalar::<_, Uuid>("SELECT assignee_id FROM tickets WHERE number = $1")
        .bind(n1)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(first == u1 || first == u2, "только инженер L1 площадки");

    // Письмо-подтверждение заявителю и письмо исполнителю.
    let mails = outbox_for(&pool, n1).await;
    assert!(
        mails.iter().any(|(to, b)| to == &vec!["emp@pult.test".to_string()] && b.contains("Заявка принята")),
        "{mails:?}"
    );

    // Следующая — другому: первый уже занят.
    let (_, c2) = emp.post("/api/helpdesk/tickets", new_ticket("Вторая", &a, &cat)).await;
    let n2 = c2["number"].as_i64().unwrap();
    let second = sqlx::query_scalar::<_, Uuid>("SELECT assignee_id FROM tickets WHERE number = $1")
        .bind(n2)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_ne!(first, second, "наименее загруженный");

    // «Взять»: «В работе» и время реакции.
    let (mut owner, other_u) = if first == u1 { (eng1, u2) } else { (eng2, u1) };
    let mut other = Browser::login(&app, if first == u1 { "eng2" } else { "eng1" }).await;
    assert_eq!(owner.post(&format!("/api/helpdesk/tickets/{n1}/take"), json!({})).await.0, StatusCode::NO_CONTENT);
    assert_eq!(status_of(&pool, n1).await, "in_progress");
    let taken: bool = sqlx::query_scalar("SELECT taken_at IS NOT NULL FROM tickets WHERE number = $1")
        .bind(n1)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(taken, "время реакции записано");

    // Конкурентно: 20 заявок не перекашивают нагрузку.
    let mut tasks = Vec::new();
    for i in 0..20 {
        let mut b = Browser::login(&app, "emp").await;
        let (a, cat) = (a.clone(), cat.clone());
        tasks.push(tokio::spawn(async move {
            b.post("/api/helpdesk/tickets", new_ticket(&format!("Параллельная {i}"), &a, &cat)).await.0
        }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap(), StatusCode::CREATED);
    }
    let loads: Vec<i64> = sqlx::query_scalar("SELECT count(*) FROM tickets WHERE assignee_id = ANY($1) AND status IN ('assigned', 'in_progress') GROUP BY assignee_id ORDER BY 1")
        .bind(vec![u1, u2])
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(loads.len(), 2);
    assert!(loads[1] - loads[0] <= 1, "нагрузка поровну: {loads:?}");

    // Отсутствующий инженер новых заявок не получает.
    let now = time::OffsetDateTime::now_utc();
    let fmt = |t: time::OffsetDateTime| t.format(&time::format_description::well_known::Rfc3339).unwrap();
    let (code, ab) = admin.post("/api/admin/helpdesk/absences", json!({"user_id": other_u, "starts_at": fmt(now - time::Duration::hours(1)), "ends_at": fmt(now + time::Duration::hours(1))})).await;
    assert_eq!(code, StatusCode::OK, "{ab}");
    for i in 0..3 {
        let (_, c) = emp.post("/api/helpdesk/tickets", new_ticket(&format!("При отсутствии {i}"), &a, &cat)).await;
        let got: Uuid = sqlx::query_scalar("SELECT assignee_id FROM tickets WHERE number = $1")
            .bind(c["number"].as_i64().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(got, first, "отсутствующий пропущен");
    }
    sqlx::query("DELETE FROM engineer_absences").execute(&pool).await.unwrap();

    // Площадка без инженеров — очередь и уведомление суперадминистратору; любой инженер берёт.
    let (_, q) = emp.post("/api/helpdesk/tickets", new_ticket("Без инженеров", &b, &cat)).await;
    assert_eq!(q["status"], "new");
    let nq = q["number"].as_i64().unwrap();
    let (_, n) = admin.get("/api/notifications").await;
    assert!(
        n["items"].to_string().contains("helpdesk.unassigned") || n.to_string().contains("helpdesk.unassigned"),
        "{n}"
    );
    let (_, un) = other.get("/api/helpdesk/tickets?scope=unassigned").await;
    assert!(un.as_array().unwrap().iter().any(|t| t["number"] == nq));
    assert_eq!(other.post(&format!("/api/helpdesk/tickets/{nq}/take"), json!({})).await.0, StatusCode::NO_CONTENT);

    // --- Инженер, не исполнитель, отвечает и решает; автор виден; исполнитель уведомлён ---
    let (code, r) = other
        .post(&format!("/api/helpdesk/tickets/{n1}/comments"), json!({"body": "Посмотрел, ответ от коллеги"}))
        .await;
    assert_eq!(code, StatusCode::OK, "{r}");
    let (_, note) = other
        .post(&format!("/api/helpdesk/tickets/{n1}/comments"), json!({"body": "СЕКРЕТНАЯ заметка", "internal": true}))
        .await;
    assert_eq!(note["follow_up"], false);
    let (_, d) = owner.get(&format!("/api/helpdesk/tickets/{n1}")).await;
    let feed = d["feed"].as_array().unwrap();
    assert!(
        feed.iter()
            .any(|f| f["body"] == "Посмотрел, ответ от коллеги" && f["author"].as_str().is_some_and(|a| !a.is_empty())),
        "{feed:?}"
    );
    let (_, bell) = owner.get("/api/notifications").await;
    assert!(bell.to_string().contains("helpdesk.changed_by_other"), "исполнитель уведомлён: {bell}");

    // --- Права: чужой сотрудник не видит ничего ---
    let (_, l) = emp2.get("/api/helpdesk/tickets?scope=all&status=all").await;
    assert!(l.as_array().unwrap().is_empty(), "чужого нет в списке: {l}");
    let (_, l) =
        emp2.get("/api/helpdesk/tickets?scope=all&status=all&q=%D0%BF%D1%80%D0%B8%D0%BD%D1%82%D0%B5%D1%80").await;
    assert!(l.as_array().unwrap().is_empty(), "и в поиске");
    assert_eq!(emp2.get(&format!("/api/helpdesk/tickets/{n1}")).await.0, StatusCode::NOT_FOUND, "и по прямой ссылке");
    assert_eq!(
        emp2.post(&format!("/api/helpdesk/tickets/{n1}/comments"), json!({"body": "x"})).await.0,
        StatusCode::NOT_FOUND
    );
    let fid = up["id"].as_str().unwrap();
    assert_eq!(emp2.get(&format!("/api/helpdesk/files/{fid}")).await.0, StatusCode::NOT_FOUND, "и вложение");
    assert_eq!(emp.get(&format!("/api/helpdesk/files/{fid}")).await.0, StatusCode::OK);
    let (_, pre, h) =
        emp.send(Method::GET, &format!("/api/helpdesk/files/{fid}"), "application/json", Vec::new()).await;
    drop(pre);
    assert_eq!(h.get(header::CONTENT_TYPE).unwrap(), "image/png");
    assert_eq!(h.get(header::CACHE_CONTROL).unwrap(), "no-store", "вложение не остаётся в кэше браузера");
    assert_eq!(emp2.post(&format!("/api/helpdesk/tickets/{n1}/take"), json!({})).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        emp.post(&format!("/api/helpdesk/tickets/{n1}/take"), json!({})).await.0,
        StatusCode::FORBIDDEN,
        "сотрудник не работает с заявками"
    );
    assert_eq!(
        emp.post(&format!("/api/helpdesk/tickets/{n1}/comments"), json!({"body": "x", "internal": true})).await.0,
        StatusCode::FORBIDDEN
    );
    // Заявитель не видит внутреннюю заметку, её вложений и она не уходит в письма.
    let (_, photo) = other.upload("внутреннее.png", PNG).await;
    other
        .post(
            &format!("/api/helpdesk/tickets/{n1}/comments"),
            json!({"body": "заметка с файлом", "internal": true, "files": [photo["id"]]}),
        )
        .await;
    let (_, mine) = emp.get(&format!("/api/helpdesk/tickets/{n1}")).await;
    assert!(!mine.to_string().contains("СЕКРЕТНАЯ"), "{mine}");
    assert_eq!(
        emp.get(&format!("/api/helpdesk/files/{}", photo["id"].as_str().unwrap())).await.0,
        StatusCode::NOT_FOUND,
        "вложение заметки"
    );
    assert!(outbox_for(&pool, n1).await.iter().all(|(_, b)| !b.contains("СЕКРЕТНАЯ")), "заметка не в письмах");
    // Журнал скачивания чужого вложения.
    other.get(&format!("/api/helpdesk/files/{fid}")).await;
    let (_, aud) = admin.get("/api/admin/audit?action=ticket.file_download").await;
    assert!(!aud.as_array().unwrap().is_empty(), "скачивание инженером не своей заявки — в аудите");

    // --- Решение и «Да / Нет» ---
    assert_eq!(
        owner.post(&format!("/api/helpdesk/tickets/{n1}/resolve"), json!({"comment": "  "})).await.0,
        StatusCode::BAD_REQUEST,
        "комментарий обязателен"
    );
    let (_, foto) = owner.upload("фото.jpg", b"\xFF\xD8\xFFdata").await;
    let (code, r) = owner
        .post(
            &format!("/api/helpdesk/tickets/{n1}/resolve"),
            json!({"comment": "Заменил картридж", "files": [foto["id"]], "closure_code": "solved"}),
        )
        .await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{r}");
    assert_eq!(status_of(&pool, n1).await, "resolved");
    let token = token_of(&pool, n1).await;
    let mut anon = Browser::new(&app);
    let (_, info) = anon.get(&format!("/api/public/ticket-response/{token}")).await;
    assert_eq!(info["valid"], true, "{info}");
    assert_eq!(status_of(&pool, n1).await, "resolved", "открытие ссылки ничего не меняет");
    assert_eq!(
        anon.post(&format!("/api/public/ticket-response/{token}"), json!({"accepted": false})).await.0,
        StatusCode::BAD_REQUEST,
        "нужен комментарий"
    );
    assert_eq!(
        anon.post("/api/public/ticket-response/bad-token", json!({"accepted": true})).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        anon.post(
            &format!("/api/public/ticket-response/{token}"),
            json!({"accepted": false, "comment": "Всё ещё не печатает"})
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let (st_, count, assignee): (String, i32, Uuid) =
        sqlx::query_as("SELECT status, reopened_count, assignee_id FROM tickets WHERE number = $1")
            .bind(n1)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((st_.as_str(), count, assignee), ("in_progress", 1, first), "вернулась тому же исполнителю, счётчик +1");
    assert_eq!(
        anon.post(&format!("/api/public/ticket-response/{token}"), json!({"accepted": true})).await.0,
        StatusCode::BAD_REQUEST,
        "ссылка одноразовая"
    );
    let (_, info) = anon.get(&format!("/api/public/ticket-response/{token}")).await;
    assert_eq!(info["valid"], false);

    // Снова решили; через 3 рабочих дня закрывается сама.
    owner
        .post(
            &format!("/api/helpdesk/tickets/{n1}/resolve"),
            json!({"comment": "Теперь точно", "closure_code": "solved"}),
        )
        .await;
    let days: f64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM auto_close_at - now())::float8 / 86400 FROM tickets WHERE number = $1",
    )
    .bind(n1)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!((2.9..=3.1).contains(&days), "3 дня при графике без выходных: {days}");
    assert_eq!(tickets::auto_close(&core).await.unwrap(), 0, "срок не вышел");
    sqlx::query("UPDATE tickets SET auto_close_at = now() - interval '1 minute' WHERE number = $1")
        .bind(n1)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(tickets::auto_close(&core).await.unwrap(), 1);
    assert_eq!(status_of(&pool, n1).await, "closed");
    assert!(outbox_for(&pool, n1).await.iter().any(|(_, b)| b.contains("Заявка закрыта")));

    // Закрытую открывает только суперадминистратор; ответ заявителя — новая связанная заявка.
    assert_eq!(owner.post(&format!("/api/helpdesk/tickets/{n1}/reopen"), json!({})).await.0, StatusCode::FORBIDDEN);
    assert_eq!(
        owner.post(&format!("/api/helpdesk/tickets/{n1}/comments"), json!({"body": "x"})).await.0,
        StatusCode::BAD_REQUEST
    );
    let (code, f) = emp.post(&format!("/api/helpdesk/tickets/{n1}/comments"), json!({"body": "Снова сломался"})).await;
    assert_eq!((code, f["follow_up"].as_bool()), (StatusCode::OK, Some(true)), "{f}");
    let fu = f["number"].as_i64().unwrap();
    let (title, to): (String, Uuid) = sqlx::query_as("SELECT title, assignee_id FROM tickets WHERE number = $1")
        .bind(fu)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((title, to), (format!("Повторное обращение по #{n1}"), first), "тому же исполнителю");
    // Второй ответ на ту же закрытую — в уже открытое повторное обращение, а не ещё одна заявка.
    let (code, f2) = emp.post(&format!("/api/helpdesk/tickets/{n1}/comments"), json!({"body": "И ещё деталь"})).await;
    assert_eq!(
        (code, f2["number"].as_i64(), f2["follow_up"].as_bool()),
        (StatusCode::OK, Some(fu), Some(true)),
        "{f2}"
    );
    assert_eq!(admin.post(&format!("/api/helpdesk/tickets/{n1}/reopen"), json!({})).await.0, StatusCode::NO_CONTENT);
    assert_eq!(status_of(&pool, n1).await, "in_progress");

    // «Ждёт ответа»: ответ заявителя возвращает в работу.
    owner.post(&format!("/api/helpdesk/tickets/{n2}/waiting"), json!({"comment": "Какая модель?"})).await;
    let w = status_of(&pool, n2).await;
    assert_eq!(w, "waiting");
    emp.post(&format!("/api/helpdesk/tickets/{n2}/comments"), json!({"body": "HP 1020"})).await;
    assert_eq!(status_of(&pool, n2).await, "in_progress");

    // Отмена заявителем — пока инженер не ответил.
    assert_eq!(emp.post(&format!("/api/helpdesk/tickets/{n2}/cancel"), json!({})).await.0, StatusCode::BAD_REQUEST);
    let (_, c3) = emp.post("/api/helpdesk/tickets", new_ticket("Передумал", &a, &cat)).await;
    let n3 = c3["number"].as_i64().unwrap();
    assert_eq!(emp.post(&format!("/api/helpdesk/tickets/{n3}/cancel"), json!({})).await.0, StatusCode::NO_CONTENT);
    assert_eq!(status_of(&pool, n3).await, "canceled");

    // --- Передача на L2 ---
    assert_eq!(
        owner
            .post(
                &format!("/api/helpdesk/tickets/{n2}/transfer"),
                json!({"assignee_id": u3, "line": "l2", "comment": " "})
            )
            .await
            .0,
        StatusCode::BAD_REQUEST,
        "нужна причина"
    );
    assert_eq!(
        owner
            .post(
                &format!("/api/helpdesk/tickets/{n2}/transfer"),
                json!({"assignee_id": u3, "line": "l2", "comment": "Нужен L2"})
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (line, st2): (String, String) =
        sqlx::query_as("SELECT line, status FROM tickets WHERE number = $1").bind(n2).fetch_one(&pool).await.unwrap();
    assert_eq!((line.as_str(), st2.as_str()), ("l2", "assigned"));
    let (_, b3) = eng3.get("/api/notifications").await;
    assert!(b3.to_string().contains("helpdesk.assigned"), "{b3}");
    let (_, engs) = owner.get(&format!("/api/helpdesk/engineers?site_id={a}")).await;
    assert_eq!(engs.as_array().unwrap().len(), 3, "{engs}");

    // --- Правка с конфликтом версий ---
    let (_, d) = owner.get(&format!("/api/helpdesk/tickets/{n2}")).await;
    let v = d["ticket"]["version"].as_i64().unwrap();
    assert_eq!(
        owner.put(&format!("/api/helpdesk/tickets/{n2}"), json!({"version": v, "priority": "high"})).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        other.put(&format!("/api/helpdesk/tickets/{n2}"), json!({"version": v, "priority": "low"})).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        owner.put(&format!("/api/helpdesk/tickets/{n2}"), json!({"version": v + 1, "priority": "оч высокий"})).await.0,
        StatusCode::BAD_REQUEST
    );

    // --- Заявка за сотрудника без учётки AD ---
    let person: Uuid = sqlx::query_scalar("INSERT INTO persons (last_name, first_name, email, site_id, source) VALUES ('Безучёткин', 'Пётр', 'nologin@pult.test', $1::uuid, 'manual') RETURNING id").bind(&a).fetch_one(&pool).await.unwrap();
    let mut body = new_ticket("По звонку", &a, &cat);
    body["on_behalf_of"] = json!(person);
    body["channel"] = json!("phone");
    assert_eq!(emp.post("/api/helpdesk/tickets", body.clone()).await.0, StatusCode::FORBIDDEN, "сотруднику нельзя");
    let (code, oc) = owner.post("/api/helpdesk/tickets", body).await;
    assert_eq!(code, StatusCode::CREATED, "{oc}");
    let nb = oc["number"].as_i64().unwrap();
    let (_, d) = owner.get(&format!("/api/helpdesk/tickets/{nb}")).await;
    assert_eq!(d["ticket"]["channel"], "phone");
    assert!(d["ticket"]["created_by_name"].is_string(), "виден регистратор: {d}");
    assert!(d["ticket"]["requester_name"].as_str().unwrap().contains("Безучёткин"));
    assert!(
        outbox_for(&pool, nb).await.iter().any(|(to, _)| to == &vec!["nologin@pult.test".to_string()]),
        "письмо сотруднику"
    );

    // --- «Не могу войти» ---
    let help = |name: &str| json!({"name": name, "phone": "4512", "room": "101", "issue": "locked"});
    let mut outside = Browser::new(&app);
    outside.ip = "192.0.2.50:4000".into();
    assert_eq!(
        outside.post("/api/public/login-help", help("Иванов")).await.0,
        StatusCode::FORBIDDEN,
        "только внутренние сети"
    );
    let mut lh = Browser::new(&app);
    let (c1_, r1) = lh.post("/api/public/login-help", help("Сотрудников Иван")).await;
    let (c2_, r2) = lh.post("/api/public/login-help", help("Такого Нет Человека")).await;
    assert_eq!((c1_, r1.clone()), (c2_, r2), "ответ одинаковый для любых ФИО");
    assert_eq!(c1_, StatusCode::ACCEPTED, "{r1}");
    lh.post("/api/public/login-help", help("Третий")).await;
    assert_eq!(lh.post("/api/public/login-help", help("Четвёртый")).await.0, StatusCode::TOO_MANY_REQUESTS, "3 в час");
    let (_, l) = owner.get("/api/helpdesk/tickets?scope=unassigned&q=%D0%A1%D0%BE%D1%82%D1%80%D1%83%D0%B4%D0%BD%D0%B8%D0%BA%D0%BE%D0%B2").await;
    let t = l.as_array().unwrap().iter().find(|t| t["channel"] == "login_help").expect("заявка есть").clone();
    assert_eq!(t["unverified"], true, "пометка «Личность не подтверждена»");
    let nh = t["number"].as_i64().unwrap();
    let (_, bl) = eng3.get("/api/notifications").await;
    assert!(bl.to_string().contains("helpdesk.login_help"), "инженеры уведомлены");
    let emp_person: Uuid = sqlx::query_scalar("SELECT person_id FROM users WHERE id = $1")
        .bind(uid("emp").await)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        owner.post(&format!("/api/helpdesk/tickets/{nh}/verify-identity"), json!({"person_id": emp_person})).await.0,
        StatusCode::NO_CONTENT
    );
    let (_, d) = emp.get(&format!("/api/helpdesk/tickets/{nh}")).await;
    assert_eq!(d["ticket"]["unverified"], false, "{d}");

    // --- Нерабочее время ---
    let mut closed = work_always.clone();
    closed["work_start"] = json!("00:00");
    closed["work_end"] = json!("00:01");
    admin.put("/api/admin/helpdesk/settings", closed).await;
    let (_, c) = emp.post("/api/helpdesk/tickets", new_ticket("Ночью", &b, &cat)).await;
    assert!(c["after_hours"]["next_open"].is_string(), "{c}");
    assert_eq!(c["after_hours"]["emergency_phone"], "100", "номер площадки");
    assert!(
        outbox_for(&pool, c["number"].as_i64().unwrap())
            .await
            .iter()
            .any(|(_, b)| b.contains("нерабочее время") && b.contains("звоните 100"))
    );
    admin.put("/api/admin/helpdesk/settings", work_always.clone()).await;

    // --- Поля категории: обязательные проверяет сервер ---
    let (_, c_) = admin
        .post(
            "/api/admin/helpdesk/categories",
            json!({"name": "С полями", "kind": "incident", "default_priority": "high", "is_active": true, "sort": 1,
        "description": "  Модель и серийный номер  ",
        "form_schema": [{"key": "model", "label": "Модель", "type": "text", "required": true}]}),
        )
        .await;
    let fc = c_["id"].as_str().unwrap().to_string();
    // Каталог услуг: описание категории видят сотрудники; длинное — отказ.
    let (_, cats) = emp.get("/api/helpdesk/categories").await;
    let mine = cats.as_array().unwrap().iter().find(|c| c["id"] == fc.as_str()).unwrap();
    assert_eq!(mine["description"], "Модель и серийный номер", "{mine}");
    let (code, r) = admin
        .post(
            "/api/admin/helpdesk/categories",
            json!({"name": "Длинная", "kind": "incident", "default_priority": "normal", "is_active": true, "sort": 1,
                   "description": "я".repeat(501)}),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{r}");
    let (code, r) = emp.post("/api/helpdesk/tickets", new_ticket("Форма", &a, &fc)).await;
    assert_eq!((code, r["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_ticket")), "{r}");
    let mut body = new_ticket("Форма", &a, &fc);
    body["form_answers"] = json!({"model": "HP", "лишнее": 1});
    let (_, c) = emp.post("/api/helpdesk/tickets", body).await;
    let (answers, prio): (Value, String) =
        sqlx::query_as("SELECT form_answers, priority FROM tickets WHERE number = $1")
            .bind(c["number"].as_i64().unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((answers, prio.as_str()), (json!({"model": "HP"}), "high"));

    // --- Массовый сбой: объявление на главной, пять заявок решаются одним действием ---
    let (_, m) = emp.post("/api/helpdesk/tickets", new_ticket("Не работает 1С", &a, &cat)).await;
    let major = m["number"].as_i64().unwrap();
    assert_eq!(
        owner
            .post(
                &format!("/api/helpdesk/tickets/{major}/major"),
                json!({"title": "Сбой 1С", "body": "Чиним, ориентир 15:00"})
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    let (_, an) = emp.get("/api/helpdesk/announcements").await;
    assert_eq!(an[0]["title"], "Сбой 1С", "{an}");
    let mut kids = Vec::new();
    for i in 0..3 {
        let (_, c) = emp.post("/api/helpdesk/tickets", new_ticket(&format!("Тоже 1С {i}"), &a, &cat)).await;
        let k = c["number"].as_i64().unwrap();
        assert_eq!(
            owner.post(&format!("/api/helpdesk/tickets/{k}/link"), json!({"parent": major})).await.0,
            StatusCode::NO_CONTENT
        );
        kids.push(k);
    }
    for i in 0..2 {
        let mut body = new_ticket(&format!("У меня то же самое {i}"), &a, &cat);
        body["same_as"] = json!(major);
        let (code, c) = emp2.post("/api/helpdesk/tickets", body).await;
        assert_eq!(code, StatusCode::CREATED, "{c}");
        kids.push(c["number"].as_i64().unwrap());
    }
    let (_, r) = owner
        .post(
            &format!("/api/helpdesk/tickets/{major}/comments"),
            json!({"body": "Промежуточно: чиним", "broadcast": true}),
        )
        .await;
    assert_eq!(r["follow_up"], false);
    assert!(
        outbox_for(&pool, kids[4]).await.iter().any(|(_, b)| b.contains("Промежуточно")),
        "ответ разослан в привязанные"
    );
    assert_eq!(
        owner
            .post(
                &format!("/api/helpdesk/tickets/{major}/resolve"),
                json!({"comment": "1С снова работает", "closure_code": "solved"})
            )
            .await
            .0,
        StatusCode::NO_CONTENT
    );
    for k in &kids {
        assert_eq!(status_of(&pool, *k).await, "resolved", "#{k}");
        assert!(outbox_for(&pool, *k).await.iter().any(|(_, b)| b.contains("1С снова работает")));
    }
    let (_, an) = emp.get("/api/helpdesk/announcements").await;
    assert!(an.as_array().unwrap().is_empty(), "объявление снято: {an}");
    // К решённой главной заявке новые не привязываются.
    let (_, c) = emp.post("/api/helpdesk/tickets", new_ticket("Опять 1С", &a, &cat)).await;
    let late = c["number"].as_i64().unwrap();
    let (code, r) = owner.post(&format!("/api/helpdesk/tickets/{late}/link"), json!({"parent": major})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{r}");
    // Отмена главной: объявление снимается, привязанные остаются открытыми и отвязываются.
    let (_, m) = emp.post("/api/helpdesk/tickets", new_ticket("Не работает почта", &a, &cat)).await;
    let major2 = m["number"].as_i64().unwrap();
    let (code, _) =
        owner.post(&format!("/api/helpdesk/tickets/{major2}/major"), json!({"title": "Сбой почты", "body": ""})).await;
    assert_eq!(code, StatusCode::NO_CONTENT);
    assert_eq!(
        owner.post(&format!("/api/helpdesk/tickets/{late}/link"), json!({"parent": major2})).await.0,
        StatusCode::NO_CONTENT
    );
    let (code, r) =
        owner.post(&format!("/api/helpdesk/tickets/{major2}/cancel"), json!({"reason": "Ложная тревога"})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{r}");
    let (_, an) = emp.get("/api/helpdesk/announcements").await;
    assert!(an.as_array().unwrap().is_empty(), "объявление отменённой снято: {an}");
    let (late_status, parent): (String, Option<Uuid>) =
        sqlx::query_as("SELECT status, parent_ticket_id FROM tickets WHERE number = $1")
            .bind(late)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(late_status != "canceled" && parent.is_none(), "{late_status} {parent:?}");

    // --- Почта только в одну сторону: уведомление с номером и цепочкой, просьба не отвечать ---
    let (_, c) = emp.post("/api/helpdesk/tickets", new_ticket("Сломалась мышь", &a, &cat)).await;
    let mailn = c["number"].as_i64().unwrap();
    owner.post(&format!("/api/helpdesk/tickets/{mailn}/comments"), json!({"body": "Замените батарейку"})).await;
    let (subject, text, headers): (String, String, Value) = sqlx::query_as(
        "SELECT subject, body_text, headers FROM outbox_emails WHERE body_text LIKE '%Замените батарейку%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(subject.starts_with(&format!("[#{mailn}]")), "{subject}");
    assert!(text.contains("отвечать на него не нужно"), "{text}");
    assert!(headers["references"].as_str().unwrap().contains("ticket-"), "{headers}");

    // --- Отключённый инженер: его заявки — в очередь, суперадминистратору — уведомление ---
    let open: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets WHERE assignee_id = $1 AND status = ANY($2)")
        .bind(u3)
        .bind(tickets::OPEN_STATUSES)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(open >= 1);
    dir.disable("eng3");
    let (code, _) = admin.post("/api/admin/ad/sync", json!({"kind": "incremental"})).await;
    assert_eq!(code, StatusCode::OK);
    assert!(tickets::release_orphans(&core).await.unwrap() >= 1);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets WHERE assignee_id = $1 AND status = ANY($2)")
        .bind(u3)
        .bind(tickets::OPEN_STATUSES)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
    assert_eq!(status_of(&pool, n2).await, "new");

    // Учётку удалили и создали заново (новый objectGUID, тот же логин): синхронизация не падает,
    // права прежнего владельца новому объекту не переходят.
    let mut prior = Browser::login(&app, "eng2").await;
    dir.remove("eng2");
    dir.add(FakeUser::person("eng2", "Техников", "Новый", 77));
    let (code, s) = admin.post("/api/admin/ad/sync", json!({"kind": "full"})).await;
    assert_eq!(code, StatusCode::OK, "пересозданная учётка не останавливает синхронизацию: {s}");
    assert_eq!(prior.get("/api/me").await.0, StatusCode::UNAUTHORIZED, "сеанс прежнего владельца закрыт");
    let mut fresh = Browser::login(&app, "eng2").await;
    let (_, me) = fresh.get("/api/me").await;
    assert!(!me["permissions"].to_string().contains("helpdesk.ticket.work"), "роль не унаследована: {me}");
    let (running, ok): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status = 'running'), count(*) FILTER (WHERE status = 'ok') FROM ad_sync_runs",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(running, 0, "незавершённых запусков нет");
    assert!(ok >= 1);

    // Назначение инженера одним действием: учётка из AD, роль выдаётся сама.
    let (_, c) = admin.get("/api/admin/engineers/candidates?q=emp2").await;
    let account = c[0]["ad_account_id"].as_str().unwrap().to_string();
    assert_eq!(emp.get("/api/admin/engineers/candidates?q=emp2").await.0, StatusCode::FORBIDDEN);
    let (code, list) =
        admin.post(&format!("/api/admin/sites/{b}/engineers"), json!({"ad_account_id": account, "line": "l1"})).await;
    assert_eq!(code, StatusCode::OK, "{list}");
    let (_, me) = emp2.get("/api/me").await;
    assert!(me["permissions"].to_string().contains("helpdesk.ticket.work"), "роль выдана сразу: {me}");
    let e2 = list[0]["user_id"].as_str().unwrap().to_string();
    let (code, list) = admin
        .send(Method::DELETE, &format!("/api/admin/sites/{b}/engineers/{e2}?line=l1"), "application/json", Vec::new())
        .await
        .0
        .as_u16()
        .eq(&200)
        .then_some((StatusCode::OK, Value::Null))
        .unwrap();
    assert_eq!((code, list), (StatusCode::OK, Value::Null));

    // Компьютеры домена: имя ПК по прямым записям DNS, подсказки — только инженерам.
    dir.add_computer(FakeComputer::new("WS-ONE", Some("Бухгалтерия, кабинет 5"), 1));
    dir.add_computer(FakeComputer::new("WS-TWO", None, 2));
    dir.add_computer(FakeComputer::new("WS-OLD", None, 3));
    dir.add_computer(FakeComputer::new("WS-OFF", None, 4).disabled());
    dir.add_computer(FakeComputer::new("LAB-WS-ONE2", None, 5));
    let ad = st.people.ad.clone();
    async fn fake_dns(host: String) -> Vec<std::net::IpAddr> {
        let found: &[&str] = match host.as_str() {
            "ws-one.pult.test" => &["10.2.0.5", "8.8.8.8"],
            "ws-two.pult.test" | "ws-old.pult.test" => &["10.2.0.6"],
            "ws-off.pult.test" => &["10.2.0.7"],
            _ => &[],
        };
        found.iter().map(|a| a.parse().unwrap()).collect()
    }
    let s = pult_people::computers::sync(&core, &ad, fake_dns).await.unwrap();
    assert_eq!((s.seen, s.resolved), (5, 4));
    let client = |a: &str| Some(a.parse::<std::net::IpAddr>().unwrap());
    let of = pult_people::computers::of_client;
    assert_eq!(of(&pool, client("10.2.0.5")).await.unwrap().as_deref(), Some("WS-ONE"));
    assert_eq!(of(&pool, client("::ffff:10.2.0.5")).await.unwrap().as_deref(), Some("WS-ONE"));
    assert_eq!(of(&pool, client("10.2.0.6")).await.unwrap(), None, "два ПК на адресе — имя не подставляем");
    // Адрес отключённого ПК уходит в обратную зону DNS — у раннера CI на неё может быть ответ
    // (`ip-10-2-0-7`), поэтому проверяется только то, что имя отключённого ПК не подставлено.
    assert_ne!(of(&pool, client("10.2.0.7")).await.unwrap().as_deref(), Some("WS-OFF"), "отключённый ПК");
    assert_eq!(of(&pool, client("8.8.8.8")).await.unwrap(), None, "внешний адрес");
    emp.ip = "10.2.0.5:5000".into();
    let (_, ctx) = emp.get("/api/helpdesk/context").await;
    assert_eq!(ctx["computer_name"], "WS-ONE", "{ctx}");

    let mut eng = Browser::login(&app, "eng1").await;
    let (code, found) = eng.get("/api/helpdesk/computers?q=ws-one").await;
    assert_eq!(code, StatusCode::OK, "{found}");
    let names: Vec<&str> = found.as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["WS-ONE", "LAB-WS-ONE2"], "сначала точное совпадение");
    assert_eq!(found[0]["addresses"], json!(["10.2.0.5"]), "внешний адрес не хранится");
    let (_, by_text) = eng.get("/api/helpdesk/computers?q=%D0%B1%D1%83%D1%85%D0%B3%D0%B0%D0%BB").await;
    assert_eq!(by_text[0]["name"], "WS-ONE", "по описанию, без учёта регистра: {by_text}");
    let (_, by_ip) = eng.get("/api/helpdesk/computers?q=10.2.0.5").await;
    assert_eq!(by_ip[0]["name"], "WS-ONE", "{by_ip}");
    let (_, off) = eng.get("/api/helpdesk/computers?q=ws-off").await;
    assert_eq!(off, json!([]), "отключённых в подсказках нет");
    let (_, wild) = eng.get("/api/helpdesk/computers?q=%25").await;
    assert_eq!(wild, json!([]), "процент — обычный символ");
    assert_eq!(emp2_search(&app).await, StatusCode::FORBIDDEN, "сотруднику список машин не показываем");

    // Компьютер удалили из AD — пропал и у нас. Пустой ответ AD список не стирает.
    dir.remove_computer("WS-TWO");
    pult_people::computers::sync(&core, &ad, fake_dns).await.unwrap();
    assert_eq!(of(&pool, client("10.2.0.6")).await.unwrap().as_deref(), Some("WS-OLD"));
    dir.clear_computers();
    let s = pult_people::computers::sync(&core, &ad, fake_dns).await.unwrap();
    assert_eq!(s.seen, 0);
    assert_eq!(of(&pool, client("10.2.0.5")).await.unwrap().as_deref(), Some("WS-ONE"));

    // SSE: без сеанса поток не открыть; цепочка аудита цела.
    assert_eq!(Browser::new(&app).get("/api/events").await.0, StatusCode::UNAUTHORIZED);
    let (code, v) = admin.post("/api/admin/audit/verify", json!({})).await;
    assert_eq!(code, StatusCode::OK, "{v}");
    assert!(v["broken_at"].is_null(), "{v}");
}
