//! Сквозные сценарии изменений в AD и запросов доступа на настоящей PostgreSQL и поддельном DC.
//! Нужен `PULT_TEST_DATABASE_URL` (в CI задан). Тест создаёт отдельную базу.
//!
//! Сценарий плана: сотрудник запрашивает доступ к «Бухгалтерия — общая папка» (группа AD), исполнитель
//! отправляет на согласование, владелец согласует, «Выполнить» добавляет в группу (проверено повторным
//! чтением), всё в заявке и аудите; без разрешения операция невозможна ни в интерфейсе, ни через API;
//! повторная доставка задачи не дублирует операцию; инженер разблокирует учётку заявителя из карточки.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, FromRef},
    http::{Method, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use pult_core::{Core, Module, Secret, api::CoreModule, crypto::SecretBox, session::CookiePolicy};
use pult_directory::DirectoryModule;
use pult_helpdesk::{HelpdeskState, api::HelpdeskModule};
use pult_people::{PeopleModule, PeopleState};
use pult_testkit::{
    BASE_DN, Directory, FakeDc, FakeUser, SVC_DN, SVC_PASS, TestCa, USER_FILTER, USER_PASS, WRITER_DN, WRITER_PASS,
};
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, PgPool};
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, atomic::Ordering},
};
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
    p.extend(pult_directory::permissions());
    p
}

struct Browser {
    app: Router,
    cookies: HashMap<String, String>,
}

impl Browser {
    async fn send(&mut self, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
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
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    async fn get(&mut self, uri: &str) -> (StatusCode, Value) {
        self.send(Method::GET, uri, None).await
    }

    async fn post(&mut self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.send(Method::POST, uri, Some(body)).await
    }

    async fn put(&mut self, uri: &str, body: Value) -> (StatusCode, Value) {
        self.send(Method::PUT, uri, Some(body)).await
    }

    async fn try_login(app: &Router, login: &str, password: &str) -> (StatusCode, Self) {
        let mut b = Self { app: app.clone(), cookies: HashMap::new() };
        let (code, _) = b.post("/api/auth/login", json!({"login": login, "password": password})).await;
        (code, b)
    }

    async fn login(app: &Router, login: &str) -> Self {
        let (code, b) = Self::try_login(app, login, USER_PASS).await;
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
    let name = format!("pult_stage4_{}", std::process::id());
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

async fn comments(pool: &PgPool, n: i64) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT c.body FROM ticket_comments c JOIN tickets t ON t.id = c.ticket_id WHERE t.number = $1 ORDER BY c.created_at",
    )
    .bind(n)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn audit(pool: &PgPool, action: &str) -> Vec<Value> {
    sqlx::query_scalar("SELECT details FROM audit_log WHERE action = $1 ORDER BY id")
        .bind(action)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn stage4_acceptance() {
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
        FakeUser::person("boss", "Главбухова", "Анна", 4),
        FakeUser::person("emp", "Сотрудников", "Иван", 5),
        FakeUser::person("emp2", "Другой", "Пётр", 6),
        FakeUser::person("newbie", "Новиков", "Нил", 7).disabled(),
        // Администратор домена в той же OU: AdminSDHolder, `adminCount = 1`.
        FakeUser::person("domadm", "Доменов", "Админ", 8).protected(),
    ]);
    dir.add_group("Бухгалтерия-папка", &[]);
    dir.add_group("Администраторы домена", &[]);
    let accounting = Directory::group_dn("Бухгалтерия-папка");
    let admins_group = Directory::group_dn("Администраторы домена");
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
        helpdesk: HelpdeskState { files_dir: std::env::temp_dir().join(format!("pult-stage4-{}", std::process::id())) },
    };
    let (router, _) = utoipa_axum::router::OpenApiRouter::new()
        .merge(<CoreModule as Module<St>>::routes(&CoreModule))
        .merge(<PeopleModule as Module<St>>::routes(&PeopleModule))
        .merge(<HelpdeskModule as Module<St>>::routes(&HelpdeskModule))
        .merge(<DirectoryModule as Module<St>>::routes(&DirectoryModule))
        .split_for_parts();
    let app: Router = router.with_state(st.clone());
    // Фоновая задача «выполнить операцию каталога» — так же, как её вызывает очередь.
    let run = |op: &Value| {
        let core = st.core.clone();
        let op: Uuid = op["id"].as_str().unwrap().parse().unwrap();
        async move { pult_helpdesk::access::run_operation(&core, op).await.unwrap() }
    };

    let mut admin = Browser::login(&app, "admin").await;
    let (code, s) = admin.post("/api/admin/ad/sync", json!({"kind": "full"})).await;
    assert_eq!(code, StatusCode::OK, "{s}");
    let mut eng1 = Browser::login(&app, "eng1").await;
    let mut eng2 = Browser::login(&app, "eng2").await;
    let mut boss = Browser::login(&app, "boss").await;
    let mut emp = Browser::login(&app, "emp").await;
    let mut emp2 = Browser::login(&app, "emp2").await;
    let one = |q: &'static str, login: &'static str| {
        let pool = pool.clone();
        async move {
            let id: Uuid = sqlx::query_scalar(q).bind(login).fetch_one(&pool).await.unwrap();
            id
        }
    };
    const USER: &str =
        "SELECT u.id FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id WHERE a.sam_account_name = $1";
    const PERSON: &str =
        "SELECT p.id FROM persons p JOIN ad_accounts a ON a.id = p.ad_account_id WHERE a.sam_account_name = $1";
    let (u1, u2) = (one(USER, "eng1").await, one(USER, "eng2").await);
    for u in [u1, u2] {
        let (code, b) = admin.put(&format!("/api/admin/users/{u}/roles"), json!({"roles": ["engineer"]})).await;
        assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
    }
    // Оператор каталога: инженер eng2 с разрешениями записи; eng1 — обычный инженер, только чтение.
    // Роли заводятся в админке ролей; здесь — сразу в базе, разрешения — через API.
    sqlx::query("INSERT INTO roles (code, name) VALUES ('directory_operator', 'Оператор каталога AD')")
        .execute(&pool)
        .await
        .unwrap();
    let (code, b) = admin
        .put(
            "/api/admin/roles/directory_operator/permissions",
            json!({"permissions": ["directory.group.add_member", "directory.user.unlock",
                                   "directory.user.enable", "directory.user.reset_password"]}),
        )
        .await;
    assert!(code.is_success(), "{b}");
    let (code, b) =
        admin.put(&format!("/api/admin/users/{u2}/roles"), json!({"roles": ["engineer", "directory_operator"]})).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "{b}");
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
    let (_, cats) = emp.get("/api/helpdesk/categories").await;
    let cat = |name: &str| {
        cats.as_array().unwrap().iter().find(|c| c["name"] == name).unwrap_or_else(|| panic!("{name}: {cats}"))["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (access_cat, printers) = (cat("Запрос доступа"), cat("Принтеры и сканеры"));

    // --- Запись в AD выключена, пока её не настроили: по умолчанию только чтение ---
    let emp_person = one(PERSON, "emp").await;
    let n0 = {
        let (code, c) = emp
            .post(
                "/api/helpdesk/tickets",
                json!({"title": "Не могу войти", "description": "Заблокировало", "site_id": site, "category_id": printers}),
            )
            .await;
        assert_eq!(code, StatusCode::CREATED, "{c}");
        c["number"].as_i64().unwrap()
    };
    let unlock = |key: &str, n: i64| {
        json!({"kind": "user.unlock", "person_id": emp_person, "basis": "Сотрудник позвонил, учётка заблокирована",
               "ticket_number": n, "idempotency_key": key, "confirm": true})
    };
    let (code, e) = eng2.post("/api/directory/operations", unlock("unlock-0000", n0)).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "учётка записи не настроена: {e}");

    let (code, e) = eng2.put("/api/admin/directory", json!({})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "настройки записи — только с directory.manage: {e}");
    let (code, cfg) = admin
        .put(
            "/api/admin/directory",
            json!({"writer_bind_dn": WRITER_DN, "writer_password": WRITER_PASS,
                   "user_ous": [format!("OU=Users,{BASE_DN}")], "groups": [accounting], "require_ticket": true}),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{cfg}");
    assert_eq!(cfg["has_writer_password"], true);
    assert!(!cfg.to_string().contains(WRITER_PASS), "пароль учётки записи не возвращается");
    // Сменили контроллеры (или CA) — пароль учётки записи сброшен: иначе первая же операция каталога
    // отправила бы его на новый, возможно чужой, LDAP-сервер. Учётка записи остаётся, нужен только пароль.
    let (_, ad) = admin.get("/api/admin/ad").await;
    let d = ad["domain"].clone();
    let put_ad = |controllers: Value, version: &Value| {
        json!({"netbios_name": d["netbios_name"], "base_dn": d["base_dn"], "controllers": controllers,
               "tls_mode": d["tls_mode"], "bind_dn": d["bind_dn"], "bind_password": SVC_PASS, "ca_cert_pem": null,
               "user_filter": d["user_filter"], "timeout_secs": d["timeout_secs"], "version": version})
    };
    let mut moved = d["controllers"].as_array().unwrap().clone();
    moved.push(json!("rogue.pult.test:636"));
    let (code, b) = admin.put("/api/admin/ad", put_ad(json!(moved), &d["version"])).await;
    assert_eq!(code, StatusCode::OK, "{b}");
    let (_, cfg) = admin.get("/api/admin/directory").await;
    assert_eq!(
        (cfg["has_writer_password"].as_bool(), cfg["writer_bind_dn"].as_str()),
        (Some(false), Some(WRITER_DN)),
        "пароль записи сброшен, учётка осталась: {cfg}"
    );
    let (_, ad) = admin.get("/api/admin/ad").await;
    let (code, b) = admin.put("/api/admin/ad", put_ad(d["controllers"].clone(), &ad["domain"]["version"])).await;
    assert_eq!(code, StatusCode::OK, "{b}");
    let (code, cfg) = admin
        .put(
            "/api/admin/directory",
            json!({"writer_bind_dn": WRITER_DN, "writer_password": WRITER_PASS,
                   "user_ous": [format!("OU=Users,{BASE_DN}")], "groups": [accounting], "require_ticket": true}),
        )
        .await;
    assert_eq!((code, cfg["has_writer_password"].as_bool()), (StatusCode::OK, Some(true)), "{cfg}");

    // --- Ресурсы и владельцы ---
    let boss_person = one(PERSON, "boss").await;
    let (code, e) = eng1
        .post(
            "/api/admin/helpdesk/access/resources",
            json!({"kind": "ad_group", "name": "x", "group_dn": accounting, "owner_id": boss_person}),
        )
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN, "ресурсы заводит администратор: {e}");
    let (code, res) = admin
        .post(
            "/api/admin/helpdesk/access/resources",
            json!({"kind": "ad_group", "name": "Бухгалтерия — общая папка", "description": "\\\\fs\\buh",
                   "group_dn": accounting, "owner_id": boss_person}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{res}");
    let resource = id(&res);
    let (code, b) = admin
        .post(
            "/api/admin/helpdesk/access/resources",
            json!({"kind": "ad_group", "name": "Без группы", "owner_id": boss_person}),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "у группы AD нужен DN: {b}");
    let (_, list) = emp.get("/api/helpdesk/access/resources").await;
    assert!(list.as_array().unwrap().iter().any(|r| r["id"] == resource && r["owner_name"].is_string()), "{list}");

    // --- Запрос доступа: заявка с ресурсом ---
    let (code, b) =
        emp.post("/api/helpdesk/tickets", json!({"title": "Доступ", "site_id": site, "category_id": access_cat})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "без ресурса заявку «Запрос доступа» не создать: {b}");
    let (code, c) = emp
        .post(
            "/api/helpdesk/tickets",
            json!({"title": "Нужен доступ к папке бухгалтерии", "description": "Для квартального отчёта",
                   "site_id": site, "category_id": access_cat, "resource_id": resource}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{c}");
    let n = c["number"].as_i64().unwrap();
    let (code, req) = emp.get(&format!("/api/helpdesk/tickets/{n}/access")).await;
    assert_eq!((code, req["status"].as_str()), (StatusCode::OK, Some("new")), "{req}");
    let (_, mine) = emp.get("/api/helpdesk/access/mine").await;
    assert_eq!(mine.as_array().unwrap().len(), 1, "«Мои доступы»: {mine}");
    assert_eq!(mine[0]["resource_name"], "Бухгалтерия — общая папка");

    let (code, e) = emp.post(&format!("/api/helpdesk/tickets/{n}/access/send"), json!({})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "на согласование отправляет исполнитель: {e}");
    let (code, e) = eng1.post(&format!("/api/helpdesk/tickets/{n}/access/execute"), json!({})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "до согласования «Выполнить» недоступно: {e}");
    let (code, req) = eng1.post(&format!("/api/helpdesk/tickets/{n}/access/send"), json!({})).await;
    assert_eq!(code, StatusCode::OK, "{req}");
    assert_eq!(
        (req["status"].as_str(), req["approver_id"].as_str()),
        (Some("pending"), Some(boss_person.to_string().as_str()))
    );
    let bells: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM notifications WHERE user_id = (SELECT id FROM users WHERE person_id = $1) AND kind = 'helpdesk.approval'",
    )
    .bind(boss_person)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bells, 1, "владелец получил уведомление");

    // --- Согласования ---
    let (_, approvals) = emp2.get("/api/helpdesk/approvals").await;
    assert_eq!(approvals.as_array().unwrap().len(), 0, "чужие согласования не видны: {approvals}");
    let approval = id(&req);
    let (code, e) = emp2.post(&format!("/api/helpdesk/approvals/{approval}"), json!({"approve": true})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "согласует только владелец: {e}");
    let (code, e) = eng1.post(&format!("/api/helpdesk/approvals/{approval}"), json!({"approve": true})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "исполнитель тоже не согласует за владельца: {e}");
    let (_, approvals) = boss.get("/api/helpdesk/approvals").await;
    let a = approvals.as_array().unwrap();
    assert_eq!(a.len(), 1, "{approvals}");
    assert_eq!((a[0]["ticket_number"].as_i64(), a[0]["person_name"].is_string()), (Some(n), true));
    let (code, req) = boss
        .post(
            &format!("/api/helpdesk/approvals/{approval}"),
            json!({"approve": true, "comment": "Согласую до конца года"}),
        )
        .await;
    assert_eq!((code, req["status"].as_str()), (StatusCode::OK, Some("approved")), "{req}");
    let (code, _) = boss.post(&format!("/api/helpdesk/approvals/{approval}"), json!({"approve": false})).await;
    assert_eq!(code, StatusCode::CONFLICT, "решение принимается один раз");
    assert!(comments(&pool, n).await.iter().any(|c| c.contains("Согласую до конца года")), "решение — в ленте заявки");

    // --- «Выполнить»: без разрешения — ни в интерфейсе, ни через API ---
    let (code, e) = eng1.post(&format!("/api/helpdesk/tickets/{n}/access/execute"), json!({})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "инженер без directory.group.add_member: {e}");
    let (code, e) = eng1
        .post(
            "/api/directory/operations",
            json!({"kind": "group.add_member", "person_id": emp_person, "group_dn": accounting,
                   "basis": "в обход согласования", "ticket_number": n, "idempotency_key": "bypass-0001", "confirm": true}),
        )
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN, "и напрямую через API: {e}");
    let (code, e) = eng2
        .post(
            "/api/directory/operations",
            json!({"kind": "group.add_member", "person_id": emp_person, "group_dn": admins_group,
                   "basis": "хочу", "ticket_number": n, "idempotency_key": "zone-00001", "confirm": true}),
        )
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN, "группа вне разрешённых: {e}");
    let (code, e) = eng2
        .post(
            "/api/directory/operations",
            json!({"kind": "group.add_member", "person_id": emp_person, "group_dn": accounting,
                   "basis": "без заявки", "idempotency_key": "noticket-1", "confirm": true}),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "основание без заявки по настройке нельзя: {e}");
    let (code, e) = eng2
        .post(
            "/api/directory/operations",
            json!({"kind": "group.add_member", "person_id": emp_person, "group_dn": accounting,
                   "basis": "x", "ticket_number": n, "idempotency_key": "noconfirm1"}),
        )
        .await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "без подтверждения не выполняется: {e}");

    let writes = dc.stats.modifies.load(Ordering::Relaxed);
    let (code, op) = eng2.post(&format!("/api/helpdesk/tickets/{n}/access/execute"), json!({})).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{op}");
    assert_eq!(op["status"], "queued");
    let (code, again) = eng2.post(&format!("/api/helpdesk/tickets/{n}/access/execute"), json!({})).await;
    assert_eq!(
        (code, again["id"].clone()),
        (StatusCode::ACCEPTED, op["id"].clone()),
        "повторное нажатие — та же операция"
    );
    let jobs: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'directory.operation'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(jobs, 1, "одна задача в очереди");
    let done = run(&op).await;
    assert_eq!(done["status"], "done", "{done}");
    let emp_dn = FakeUser::person("emp", "Сотрудников", "Иван", 5).dn();
    assert!(dir.members("Бухгалтерия-папка").iter().any(|m| m.eq_ignore_ascii_case(&emp_dn)), "сотрудник в группе");
    assert_eq!(dc.stats.modifies.load(Ordering::Relaxed), writes + 1, "одна запись в AD");
    let (_, view) = eng2.get(&format!("/api/directory/operations/{}", op["id"].as_str().unwrap())).await;
    assert_eq!(
        (view["status"].as_str(), view["verified"].as_bool()),
        (Some("done"), Some(true)),
        "проверено чтением: {view}"
    );
    assert!(view["after"]["groups"].to_string().contains("Бухгалтерия-папка"), "{view}");
    // Повторная доставка задачи: операция не выполняется второй раз.
    let again = run(&op).await;
    assert_eq!(again["status"], "done");
    assert_eq!(dc.stats.modifies.load(Ordering::Relaxed), writes + 1, "повтор задачи не пишет в AD");
    let (_, req) = emp.get(&format!("/api/helpdesk/tickets/{n}/access")).await;
    assert_eq!(req["status"], "done", "{req}");
    let (_, mine) = emp.get("/api/helpdesk/access/mine").await;
    assert_eq!(mine[0]["status"], "done");
    assert!(mine[0]["granted_at"].is_string(), "{mine}");
    let notes = comments(&pool, n).await;
    assert!(
        notes.iter().filter(|c| c.contains("Бухгалтерия-папка")).count() == 1,
        "итог операции — в ленте один раз: {notes:?}"
    );
    let log = audit(&pool, "directory.group.add_member").await;
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(
        log[0]["before"].is_object() && log[0]["after"].is_object() && log[0]["ticket"] == n,
        "было/стало: {:?}",
        log[0]
    );

    // --- Карточка заявки: состояние учётки заявителя и «Разблокировать» ---
    dir.lock("emp");
    let (code, acc) = eng1.get(&format!("/api/directory/persons/{emp_person}/account")).await;
    assert_eq!(code, StatusCode::OK, "{acc}");
    assert_eq!((acc["enabled"].as_bool(), acc["locked"].as_bool()), (Some(true), Some(true)), "{acc}");
    assert!(acc["password_expires_at"].is_string() && acc["last_logon_at"].is_string(), "{acc}");
    let (code, _) = emp.get(&format!("/api/directory/persons/{emp_person}/account")).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "сотрудник не смотрит состояние учёток");
    let (code, _) = Browser::try_login(&app, "emp", USER_PASS).await;
    assert_ne!(code, StatusCode::NO_CONTENT, "заблокированная учётка не входит");
    let (code, e) = eng1.post("/api/directory/operations", unlock("unlock-0001", n0)).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "без directory.user.unlock: {e}");
    let (code, op) = eng2.post("/api/directory/operations", unlock("unlock-0002", n0)).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{op}");
    let (code, same) = eng2.post("/api/directory/operations", unlock("unlock-0002", n0)).await;
    assert_eq!((code, same["id"].clone()), (StatusCode::ACCEPTED, op["id"].clone()), "тот же ключ — та же операция");
    let (code, e) = eng1.post("/api/directory/operations", unlock("unlock-0002", n0)).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "чужой ключ не открывает чужую операцию: {e}");
    let done = run(&op).await;
    assert_eq!((done["status"].as_str(), done["verified"].as_bool()), (Some("done"), Some(true)), "{done}");
    let (_, acc) = eng1.get(&format!("/api/directory/persons/{emp_person}/account")).await;
    assert_eq!(acc["locked"], false, "{acc}");
    Browser::login(&app, "emp").await;
    assert!(comments(&pool, n0).await.iter().any(|c| c.contains("разблокирована")), "результат — в ленте заявки");
    let (_, ops) = eng1.get(&format!("/api/directory/operations?ticket={n0}")).await;
    assert_eq!(ops.as_array().unwrap().len(), 1, "операции заявки видны в карточке: {ops}");
    assert_eq!(audit(&pool, "directory.user.unlock").await.len(), 1);

    // --- Сброс пароля: только по LDAPS, временный пароль показывается один раз ---
    let (code, op) = eng2
        .post(
            "/api/directory/operations",
            json!({"kind": "user.reset_password", "person_id": emp_person, "basis": "Забыл пароль",
                   "ticket_number": n0, "idempotency_key": "reset-00001", "confirm": true}),
        )
        .await;
    assert_eq!(code, StatusCode::ACCEPTED, "{op}");
    assert!(run(&op).await["verified"].as_bool().unwrap());
    let op_id = op["id"].as_str().unwrap();
    let (code, e) = eng1.post(&format!("/api/directory/operations/{op_id}/password"), json!({})).await;
    assert_eq!(code, StatusCode::FORBIDDEN, "пароль видит только запросивший: {e}");
    let (code, p) = eng2.post(&format!("/api/directory/operations/{op_id}/password"), json!({})).await;
    assert_eq!(code, StatusCode::OK, "{p}");
    let temp = p["password"].as_str().unwrap().to_string();
    assert!(temp.len() >= 12, "временный пароль достаточной длины");
    let (code, _) = eng2.post(&format!("/api/directory/operations/{op_id}/password"), json!({})).await;
    assert_eq!(code, StatusCode::GONE, "второй раз пароль не показывается");
    let (code, _) = Browser::try_login(&app, "emp", &temp).await;
    assert_eq!(code, StatusCode::NO_CONTENT, "вход с временным паролем");
    let (_, acc) = eng1.get(&format!("/api/directory/persons/{emp_person}/account")).await;
    assert_eq!(acc["must_change_password"], true, "сменить при следующем входе: {acc}");
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM directory_operations WHERE secret IS NOT NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, 0, "временный пароль после показа стёрт");
    for action in ["directory.user.reset_password", "directory.password.shown"] {
        let details = audit(&pool, action).await;
        assert_eq!(details.len(), 1, "{action}: одна запись аудита");
        assert!(!details.iter().any(|d| d.to_string().contains(&temp)), "{action}: пароль не попал в аудит");
    }

    // --- Владелец сменился: ожидающие согласования переходят новому ---
    let (_, c2) = emp2
        .post(
            "/api/helpdesk/tickets",
            json!({"title": "Тоже доступ", "site_id": site, "category_id": access_cat, "resource_id": resource}),
        )
        .await;
    let n2 = c2["number"].as_i64().unwrap();
    let (code, _) = eng1.post(&format!("/api/helpdesk/tickets/{n2}/access/send"), json!({})).await;
    assert_eq!(code, StatusCode::OK);
    let admin_person = one(PERSON, "admin").await;
    let (code, b) = admin
        .put(
            &format!("/api/admin/helpdesk/access/resources/{resource}"),
            json!({"kind": "ad_group", "name": "Бухгалтерия — общая папка", "group_dn": accounting, "owner_id": admin_person, "is_active": true}),
        )
        .await;
    assert_eq!(code, StatusCode::OK, "{b}");
    let (_, approvals) = boss.get("/api/helpdesk/approvals").await;
    assert!(
        approvals.as_array().unwrap().iter().all(|a| a["status"] != "pending"),
        "у прежнего владельца нет ожидающих: {approvals}"
    );
    let (_, approvals) = admin.get("/api/helpdesk/approvals").await;
    assert!(
        approvals.as_array().unwrap().iter().any(|a| a["ticket_number"] == n2 && a["status"] == "pending"),
        "{approvals}"
    );

    // --- «Новый сотрудник»: учётка включается пунктом чек-листа через каталог ---
    let (_, tpls) = eng1.get("/api/helpdesk/templates").await;
    let tpl = tpls.as_array().unwrap().iter().find(|t| t["name"] == "Новый сотрудник").unwrap()["id"].clone();
    let (code, c3) = admin
        .post(
            "/api/helpdesk/tickets",
            json!({"title": "", "site_id": site, "category_id": printers, "template_id": tpl}),
        )
        .await;
    assert_eq!(code, StatusCode::CREATED, "{c3}");
    let n3 = c3["number"].as_i64().unwrap();
    let (_, card) = eng2.get(&format!("/api/helpdesk/tickets/{n3}")).await;
    let item =
        card["checklist"].as_array().unwrap().iter().find(|i| i["action"] == "directory.enable").unwrap()["id"].clone();
    let (code, e) = eng1
        .post(
            &format!("/api/helpdesk/tickets/{n3}/checklist/{}/run", item.as_str().unwrap()),
            json!({"login": "newbie"}),
        )
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN, "{e}");
    let (code, op) = eng2
        .post(
            &format!("/api/helpdesk/tickets/{n3}/checklist/{}/run", item.as_str().unwrap()),
            json!({"login": "newbie"}),
        )
        .await;
    assert_eq!(code, StatusCode::ACCEPTED, "{op}");
    assert_eq!(run(&op).await["status"], "done");
    Browser::login(&app, "newbie").await;
    let (_, card) = eng2.get(&format!("/api/helpdesk/tickets/{n3}")).await;
    let it = card["checklist"].as_array().unwrap().iter().find(|i| i["id"] == item).unwrap().clone();
    assert!(it["done_at"].is_string(), "пункт отмечен выполненным: {it}");

    // ================= Проверки безопасности =================

    // --- Свою учётку оператор не меняет: ни напрямую, ни пунктом чек-листа (разделение обязанностей) ---
    let eng2_person = one(PERSON, "eng2").await;
    let (code, e) = eng2
        .post(
            "/api/directory/operations",
            json!({"kind": "user.reset_password", "person_id": eng2_person, "basis": "сам себе",
                   "ticket_number": n0, "idempotency_key": "self-00001", "confirm": true}),
        )
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN, "свою учётку оператор не меняет: {e}");
    let (code, e) = eng2
        .post(&format!("/api/helpdesk/tickets/{n3}/checklist/{}/run", item.as_str().unwrap()), json!({"login": "eng2"}))
        .await;
    assert_eq!(code, StatusCode::FORBIDDEN, "и пунктом чек-листа: {e}");

    // --- Привилегированная учётка (adminCount = 1) в разрешённой OU: Pult её не меняет ---
    let domadm = one(PERSON, "domadm").await;
    let writes = dc.stats.modifies.load(Ordering::Relaxed);
    let (code, op) = eng2
        .post(
            "/api/directory/operations",
            json!({"kind": "user.reset_password", "person_id": domadm, "basis": "Забыл пароль",
                   "ticket_number": n0, "idempotency_key": "protected-1", "confirm": true}),
        )
        .await;
    assert_eq!(code, StatusCode::ACCEPTED, "{op}");
    let done = run(&op).await;
    assert_eq!(done["status"], "failed", "{done}");
    assert!(done["error"].as_str().unwrap_or_default().contains("adminCount"), "причина — защищённая учётка: {done}");
    assert_eq!(dc.stats.modifies.load(Ordering::Relaxed), writes, "AD не меняли");
    Browser::login(&app, "domadm").await;

    // --- Заявка отменена: ожидающий запрос не согласовать и не виден владельцу ---
    let (_, req2) = emp2.get(&format!("/api/helpdesk/tickets/{n2}/access")).await;
    let req2 = id(&req2);
    let (code, e) = emp2.post(&format!("/api/helpdesk/tickets/{n2}/cancel"), json!({})).await;
    assert!(code.is_success(), "{e}");
    let (_, approvals) = admin.get("/api/helpdesk/approvals").await;
    assert!(
        !approvals.as_array().unwrap().iter().any(|a| a["ticket_number"] == n2 && a["status"] == "pending"),
        "запрос отменённой заявки не ждёт решения: {approvals}"
    );
    let (code, e) = admin.post(&format!("/api/helpdesk/approvals/{req2}"), json!({"approve": true})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "отменённую заявку не согласовать: {e}");

    // --- Согласованный запрос: ресурс выключен или сменил группу — «Выполнить» не выдаёт доступ ---
    async fn approve(engineer: &mut Browser, owner: &mut Browser, n: i64) {
        let (code, r) = engineer.post(&format!("/api/helpdesk/tickets/{n}/access/send"), json!({})).await;
        assert_eq!(code, StatusCode::OK, "{r}");
        let (code, r) = owner.post(&format!("/api/helpdesk/approvals/{}", id(&r)), json!({"approve": true})).await;
        assert_eq!((code, r["status"].as_str()), (StatusCode::OK, Some("approved")), "{r}");
    }
    let set_resource = |group: &str, active: bool| {
        json!({"kind": "ad_group", "name": "Бухгалтерия — общая папка", "group_dn": group,
               "owner_id": admin_person, "is_active": active})
    };
    let n4 = {
        let (code, c) = emp2
            .post(
                "/api/helpdesk/tickets",
                json!({"title": "Доступ к папке ещё раз", "site_id": site, "category_id": access_cat,
                       "resource_id": resource}),
            )
            .await;
        assert_eq!(code, StatusCode::CREATED, "{c}");
        c["number"].as_i64().unwrap()
    };
    approve(&mut eng1, &mut admin, n4).await;
    let res_uri = format!("/api/admin/helpdesk/access/resources/{resource}");
    let (code, b) = admin.put(&res_uri, set_resource(&accounting, false)).await;
    assert_eq!(code, StatusCode::OK, "{b}");
    let (code, e) = eng2.post(&format!("/api/helpdesk/tickets/{n4}/access/execute"), json!({})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "ресурс выключен: {e}");
    let (code, b) = admin.put(&res_uri, set_resource(&admins_group, true)).await;
    assert_eq!(code, StatusCode::OK, "{b}");
    let (_, r4) = emp2.get(&format!("/api/helpdesk/tickets/{n4}/access")).await;
    assert_eq!(r4["status"], "new", "владелец согласовывал другую группу — согласование снято: {r4}");
    let (code, e) = eng2.post(&format!("/api/helpdesk/tickets/{n4}/access/execute"), json!({})).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "без нового согласования не выполнить: {e}");
    let (code, b) = admin.put(&res_uri, set_resource(&accounting, true)).await;
    assert_eq!(code, StatusCode::OK, "{b}");

    // --- Неудачное выполнение можно повторить; второй инженер видит ту же операцию, а не 403 ---
    approve(&mut eng1, &mut admin, n4).await;
    let dir_cfg = |groups: Vec<String>| {
        json!({"writer_bind_dn": WRITER_DN, "user_ous": [format!("OU=Users,{BASE_DN}")], "groups": groups,
               "require_ticket": true})
    };
    let (code, op1) = eng2.post(&format!("/api/helpdesk/tickets/{n4}/access/execute"), json!({})).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{op1}");
    let (code, b) = admin.put("/api/admin/directory", dir_cfg(vec![])).await;
    assert_eq!(code, StatusCode::OK, "{b}");
    assert_eq!(run(&op1).await["status"], "failed", "группу убрали из разрешённых до выполнения");
    let (code, b) = admin.put("/api/admin/directory", dir_cfg(vec![accounting.clone()])).await;
    assert_eq!((code, b["has_writer_password"].as_bool()), (StatusCode::OK, Some(true)), "{b}");
    let (code, op2) = eng2.post(&format!("/api/helpdesk/tickets/{n4}/access/execute"), json!({})).await;
    assert_eq!(code, StatusCode::ACCEPTED, "{op2}");
    assert_ne!(op2["id"], op1["id"], "после ошибки — новая операция");
    let (code, same) = admin.post(&format!("/api/helpdesk/tickets/{n4}/access/execute"), json!({})).await;
    assert_eq!(
        (code, same["id"].clone()),
        (StatusCode::ACCEPTED, op2["id"].clone()),
        "другой инженер — та же операция"
    );
    assert_eq!(run(&op2).await["status"], "done");
    let emp2_dn = FakeUser::person("emp2", "Другой", "Пётр", 6).dn();
    assert!(dir.members("Бухгалтерия-папка").iter().any(|m| m.eq_ignore_ascii_case(&emp2_dn)), "доступ выдан");
    let (_, r4) = emp2.get(&format!("/api/helpdesk/tickets/{n4}/access")).await;
    assert_eq!(r4["status"], "done", "{r4}");

    dc.stop().await;
}
