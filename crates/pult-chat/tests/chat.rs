//! Сквозной сценарий модуля «Коммуникации» на настоящей PostgreSQL и поддельном DC.
//! Нужен `PULT_TEST_DATABASE_URL` (в CI задан). Тест создаёт отдельную базу.
//!
//! Личный диалог (закрыт даже от суперадминистратора, текст в базе зашифрован, правка 15 минут с версиями),
//! групповой чат (состав ведут владелец и администраторы, уведомления о добавлении и исключении, история
//! для бывших участников только для чтения, скрытие и закрепление), отключение учёток в AD (передача
//! владения, блокировка чата, назначение владельца суперадминистратором) и обезличенная статистика.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, FromRef},
    http::{Method, Request, StatusCode, header},
    middleware,
};
use http_body_util::BodyExt;
use pult_chat::ChatModule;
use pult_core::{Core, Module, Secret, api::CoreModule, crypto::SecretBox, session::CookiePolicy};
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
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
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
    async fn delete(&mut self, uri: &str) -> (StatusCode, Value) {
        self.send(Method::DELETE, uri, None).await
    }
    async fn login(app: &Router, login: &str) -> Self {
        let mut b = Self { app: app.clone(), cookies: HashMap::new() };
        let (code, _) =
            b.send(Method::POST, "/api/auth/login", Some(json!({"login": login, "password": USER_PASS}))).await;
        assert_eq!(code, StatusCode::NO_CONTENT, "вход {login}");
        b
    }
    async fn me(&mut self) -> String {
        self.get("/api/me").await.1["user_id"].as_str().unwrap().to_string()
    }
    async fn kinds(&mut self) -> Vec<String> {
        let (_, n) = self.get("/api/notifications").await;
        n["items"].as_array().unwrap().iter().map(|x| x["kind"].as_str().unwrap().to_string()).collect()
    }
    async fn chats(&mut self, hidden: bool) -> Vec<Value> {
        let (code, v) = self.get(&format!("/api/chat?hidden={hidden}")).await;
        assert_eq!(code, StatusCode::OK, "{v}");
        v.as_array().unwrap().clone()
    }
    async fn bodies(&mut self, chat: &str) -> Vec<String> {
        let (code, v) = self.get(&format!("/api/chat/{chat}/messages")).await;
        assert_eq!(code, StatusCode::OK, "{v}");
        v.as_array().unwrap().iter().map(|m| m["body"].as_str().unwrap().to_string()).collect()
    }
}

async fn db() -> Option<PgPool> {
    let Ok(url) = std::env::var("PULT_TEST_DATABASE_URL") else {
        assert!(std::env::var("CI").is_err(), "в CI нужен PULT_TEST_DATABASE_URL");
        eprintln!("PULT_TEST_DATABASE_URL не задан — интеграционный тест пропущен");
        return None;
    };
    let name = format!("pult_chat_{}", std::process::id());
    let mut c = PgConnection::connect(&url).await.expect("подключение");
    sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)")).execute(&mut c).await.unwrap();
    sqlx::query(&format!("CREATE DATABASE {name}")).execute(&mut c).await.unwrap();
    let (base, _) = url.rsplit_once('/').unwrap();
    let pool = PgPool::connect(&format!("{base}/{name}")).await.expect("подключение к тестовой базе");
    sqlx::migrate!("../../migrations").run(&pool).await.expect("миграции");
    Some(pool)
}

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn chat_acceptance() {
    let Some(pool) = db().await else { return };
    let mut permissions = <CoreModule as Module<St>>::permissions(&CoreModule);
    permissions.extend(<PeopleModule as Module<St>>::permissions(&PeopleModule));
    let core = Core {
        db: pool.clone(),
        secrets: Arc::new(SecretBox::new(&Secret::new("test-key-test-key-test-key-test-key-0"))),
        cookies: CookiePolicy { secure: true },
        permissions: Arc::new(permissions),
        events: pult_core::events::Hub::default(),
        insecure_mail: false,
        org: Default::default(),
    };
    pult_core::rbac::seed_defaults(&pool, &core.permissions).await.unwrap();
    let dir = Directory::new(vec![
        FakeUser::person("admin", "Админов", "Адам", 1),
        FakeUser::person("ivanov", "Иванов", "Иван", 2),
        FakeUser::person("petrov", "Петров", "Пётр", 3),
        FakeUser::person("sidorov", "Сидоров", "Сидор", 4),
        FakeUser::person("kozlov", "Козлов", "Кузьма", 5),
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
    };
    let (router, _) = utoipa_axum::router::OpenApiRouter::new()
        .merge(<CoreModule as Module<St>>::routes(&CoreModule))
        .merge(<PeopleModule as Module<St>>::routes(&PeopleModule))
        .merge(<ChatModule as Module<St>>::routes(&ChatModule))
        .split_for_parts();
    let app: Router = router
        .layer(middleware::from_fn_with_state(core.clone(), pult_core::org::gate))
        .with_state(st.clone())
        .layer(middleware::from_fn(pult_core::l10n::scope));

    let mut admin = Browser::login(&app, "admin").await;
    let mut iv = Browser::login(&app, "ivanov").await;
    let mut pe = Browser::login(&app, "petrov").await;
    let mut si = Browser::login(&app, "sidorov").await;
    let mut ko = Browser::login(&app, "kozlov").await;
    let (iv_id, pe_id, si_id, ko_id) = (iv.me().await, pe.me().await, si.me().await, ko.me().await);

    // 1. Модуль выключен, пока его не включит суперадминистратор.
    assert_eq!(iv.get("/api/chat").await.0, StatusCode::NOT_FOUND);
    let on = json!({"helpdesk": true, "phonebook": true, "directory": true, "remote": true, "telephony": true, "chat": true});
    let (code, b) = admin.put("/api/admin/org", json!({"name": "Ромашка", "modules": on, "setup_done": true})).await;
    assert_eq!(code, StatusCode::OK, "{b}");
    assert!(iv.chats(false).await.is_empty());

    // 2. Личный диалог: один на пару, самому себе нельзя.
    let (_, found) = iv.get("/api/chat/people?q=петр").await;
    assert_eq!(found[0]["id"], pe_id.as_str(), "{found}");
    assert_eq!(iv.post("/api/chat/direct", json!({"user_id": iv_id})).await.0, StatusCode::BAD_REQUEST);
    let (code, d) = iv.post("/api/chat/direct", json!({"user_id": pe_id})).await;
    assert_eq!(code, StatusCode::OK, "{d}");
    let direct = id(&d);
    assert_eq!(id(&pe.post("/api/chat/direct", json!({"user_id": iv_id})).await.1), direct, "тот же диалог");
    let (code, m1) =
        iv.post(&format!("/api/chat/{direct}/messages"), json!({"body": "Привет, секретный пароль от Wi-Fi"})).await;
    assert_eq!(code, StatusCode::CREATED, "{m1}");
    assert_eq!(m1["editable"], true);
    assert_eq!(
        iv.post(&format!("/api/chat/{direct}/messages"), json!({"body": "   "})).await.0,
        StatusCode::BAD_REQUEST
    );
    // В базе текст зашифрован.
    let raw: Vec<Vec<u8>> = sqlx::query_scalar("SELECT body FROM chat_messages").fetch_all(&pool).await.unwrap();
    assert!(raw.iter().all(|b| !String::from_utf8_lossy(b).contains("Wi-Fi")));
    let list = pe.chats(false).await;
    assert_eq!((list[0]["title"].as_str(), list[0]["unread"].as_i64()), (Some("Иванов Иван"), Some(1)), "{list:?}");
    assert_eq!(list[0]["preview"], "Привет, секретный пароль от Wi-Fi");
    assert_eq!(pe.post(&format!("/api/chat/{direct}/read"), json!({})).await.0, StatusCode::NO_CONTENT);
    assert_eq!(pe.chats(false).await[0]["unread"], 0);
    // Личную переписку не читает никто, кроме участников, — и суперадминистратор тоже.
    assert_eq!(admin.get(&format!("/api/chat/{direct}")).await.0, StatusCode::NOT_FOUND);
    assert_eq!(admin.get(&format!("/api/chat/{direct}/messages")).await.0, StatusCode::NOT_FOUND);
    // Правка: только автор, версии видят оба.
    let m1_url = format!("/api/chat/{direct}/messages/{}", id(&m1));
    assert_eq!(pe.put(&m1_url, json!({"body": "чужое"})).await.0, StatusCode::FORBIDDEN);
    let (code, e) = iv.put(&m1_url, json!({"body": "Привет!"})).await;
    assert_eq!((code, e["version"].as_i64(), e["edited_at"].is_string()), (StatusCode::OK, Some(2), true), "{e}");
    let (_, versions) = pe.get(&format!("{m1_url}/versions")).await;
    assert_eq!(versions[0]["body"], "Привет, секретный пароль от Wi-Fi", "{versions}");
    // Через 15 минут править нельзя; удалить нельзя никогда (маршрута нет).
    sqlx::query("UPDATE chat_messages SET created_at = now() - interval '20 minutes'").execute(&pool).await.unwrap();
    let (code, e) = iv.put(&m1_url, json!({"body": "ещё раз"})).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::FORBIDDEN, Some("edit_expired")), "{e}");
    assert_eq!(iv.delete(&m1_url).await.0, StatusCode::METHOD_NOT_ALLOWED);
    // Личный диалог не скрывается, но закрепляется.
    assert_eq!(iv.put(&format!("/api/chat/{direct}/prefs"), json!({"hidden": true})).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(iv.put(&format!("/api/chat/{direct}/prefs"), json!({"pinned": true})).await.0, StatusCode::NO_CONTENT);

    // 3. Групповой чат: создатель — владелец, добавленные получают уведомление.
    let (code, g) = iv.post("/api/chat/groups", json!({"title": "  Отдел ИТ ", "members": [pe_id, si_id]})).await;
    assert_eq!(code, StatusCode::CREATED, "{g}");
    let group = id(&g);
    assert!(pe.kinds().await.contains(&"chat.added".to_string()));
    let (_, detail) = pe.get(&format!("/api/chat/{group}")).await;
    assert_eq!(
        (detail["chat"]["title"].as_str(), detail["members"].as_array().unwrap().len()),
        (Some("Отдел ИТ"), 3),
        "{detail}"
    );
    assert_eq!((detail["can_write"].as_bool(), detail["can_manage"].as_bool()), (Some(true), Some(false)));
    // Обычный участник состав не ведёт.
    assert_eq!(
        pe.post(&format!("/api/chat/{group}/members"), json!({"user_ids": [ko_id]})).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        iv.put(&format!("/api/chat/{group}/members/{pe_id}/role"), json!({"role": "admin"})).await.0,
        StatusCode::NO_CONTENT
    );
    // Администратор добавляет, но владельца не трогает.
    assert_eq!(
        pe.post(&format!("/api/chat/{group}/members"), json!({"user_ids": [ko_id]})).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(pe.delete(&format!("/api/chat/{group}/members/{iv_id}")).await.0, StatusCode::FORBIDDEN);
    let (_, gm1) = iv.post(&format!("/api/chat/{group}/messages"), json!({"body": "Собрание в 10"})).await;
    assert_eq!(ko.bodies(&group).await, ["Собрание в 10"], "новый участник видит всю историю");
    // Исключение: уведомление, история до исключения — только для чтения.
    assert_eq!(pe.delete(&format!("/api/chat/{group}/members/{ko_id}")).await.0, StatusCode::NO_CONTENT);
    assert!(ko.kinds().await.contains(&"chat.removed".to_string()));
    iv.post(&format!("/api/chat/{group}/messages"), json!({"body": "Без Козлова"})).await;
    let former = ko.chats(false).await.into_iter().find(|c| c["id"] == group.as_str()).unwrap();
    assert_eq!(
        (former["active"].as_bool(), former["preview"].as_str()),
        (Some(false), Some("Собрание в 10")),
        "{former}"
    );
    assert_eq!(ko.bodies(&group).await, ["Собрание в 10"]);
    let (code, e) = ko.post(&format!("/api/chat/{group}/messages"), json!({"body": "я тут"})).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::FORBIDDEN, Some("read_only")));
    assert_eq!(ko.put(&format!("/api/chat/{group}/prefs"), json!({"pinned": true})).await.0, StatusCode::BAD_REQUEST);
    // Бывший чат можно скрыть; при возвращении он снова в списке, с полной историей.
    assert_eq!(ko.put(&format!("/api/chat/{group}/prefs"), json!({"hidden": true})).await.0, StatusCode::NO_CONTENT);
    assert!(ko.chats(false).await.iter().all(|c| c["id"] != group.as_str()));
    assert_eq!(ko.chats(true).await.len(), 1);
    assert_eq!(
        iv.post(&format!("/api/chat/{group}/members"), json!({"user_ids": [ko_id]})).await.0,
        StatusCode::NO_CONTENT
    );
    assert!(ko.chats(false).await.iter().any(|c| c["id"] == group.as_str() && c["active"] == true));
    assert_eq!(ko.bodies(&group).await, ["Собрание в 10", "Без Козлова"]);
    // Действующий чат не скрывается; владелец не выходит, участник — выходит.
    assert_eq!(iv.put(&format!("/api/chat/{group}/prefs"), json!({"hidden": true})).await.0, StatusCode::BAD_REQUEST);
    let (code, e) = iv.post(&format!("/api/chat/{group}/leave"), json!({})).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("owner_cannot_leave")));
    assert_eq!(ko.post(&format!("/api/chat/{group}/leave"), json!({})).await.0, StatusCode::NO_CONTENT);
    // Версии в группе: автор, владелец и администраторы; обычный участник — нет.
    let gm1_url = format!("/api/chat/{group}/messages/{}", id(&gm1));
    iv.put(&gm1_url, json!({"body": "Собрание в 11"})).await;
    assert_eq!(si.get(&format!("{gm1_url}/versions")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(pe.get(&format!("{gm1_url}/versions")).await.1[0]["body"], "Собрание в 10");
    // История состава — владельцу и администраторам; в ленте её нет.
    assert_eq!(si.get(&format!("/api/chat/{group}/member-events")).await.0, StatusCode::FORBIDDEN);
    let (_, ev) = pe.get(&format!("/api/chat/{group}/member-events")).await;
    let actions: Vec<&str> = ev.as_array().unwrap().iter().map(|e| e["action"].as_str().unwrap()).collect();
    assert_eq!(actions, ["left", "added", "removed", "added", "admin", "added", "added", "created"], "{ev}");
    // Чужой групповой чат не виден и суперадминистратору.
    assert_eq!(admin.get(&format!("/api/chat/{group}/messages")).await.0, StatusCode::NOT_FOUND);

    // 4. Учётка владельца отключена в AD: владение — самому давнему администратору, уведомление — только ему.
    sqlx::query("UPDATE users SET is_active = false WHERE id = $1::uuid").bind(&iv_id).execute(&pool).await.unwrap();
    assert_eq!(pult_chat::sweep_blocked(&pool).await.unwrap(), 1, "только групповой чат; личный диалог не трогается");
    let (_, detail) = pe.get(&format!("/api/chat/{group}")).await;
    assert_eq!(detail["members"][0]["role"], "owner", "{detail}");
    assert_eq!(detail["members"][0]["user_id"], pe_id.as_str());
    assert!(pe.kinds().await.contains(&"chat.owner".to_string()));
    assert!(!si.kinds().await.contains(&"chat.owner".to_string()));
    assert_eq!(pe.bodies(&direct).await, ["Привет!"], "переписка с отключённым сохраняется");
    // Отключён и новый владелец, администраторов нет — чат заблокирован, суперадминистратору — уведомление.
    sqlx::query("UPDATE users SET is_active = false WHERE id = $1::uuid").bind(&pe_id).execute(&pool).await.unwrap();
    pult_chat::sweep_blocked(&pool).await.unwrap();
    assert!(admin.kinds().await.contains(&"chat.locked".to_string()));
    let locked = si.chats(false).await.into_iter().find(|c| c["id"] == group.as_str()).unwrap();
    assert_eq!((locked["locked"].as_bool(), locked["preview"].is_null()), (Some(true), true), "{locked}");
    let (code, e) = si.get(&format!("/api/chat/{group}/messages")).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::FORBIDDEN, Some("chat_locked")));
    // Суперадминистратор видит список групповых чатов без сообщений и назначает владельца.
    assert_eq!(si.get("/api/admin/chats").await.0, StatusCode::FORBIDDEN);
    let (_, all) = admin.get("/api/admin/chats").await;
    assert_eq!((all[0]["locked"].as_bool(), all[0]["owner_name"].is_null()), (Some(true), true), "{all}");
    let owner = format!("/api/admin/chats/{group}/owner");
    assert_eq!(admin.post(&owner, json!({"user_id": si_id, "previous": "x"})).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(admin.post(&owner, json!({"user_id": si_id, "previous": "member"})).await.0, StatusCode::NO_CONTENT);
    assert!(si.kinds().await.contains(&"chat.unlocked".to_string()));
    assert_eq!(si.bodies(&group).await, ["Собрание в 11", "Без Козлова"]);
    // Себя — тоже можно; это в аудите.
    let admin_id = admin.me().await;
    assert_eq!(admin.post(&owner, json!({"user_id": admin_id, "previous": "admin"})).await.0, StatusCode::NO_CONTENT);
    assert_eq!(admin.bodies(&group).await.len(), 2);
    assert!(si.kinds().await.contains(&"chat.owner_changed".to_string()), "участники знают о новом владельце");
    assert!(!admin.kinds().await.contains(&"chat.owner_changed".to_string()));
    let audited: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'chat.owner.assign'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(audited, 2);
    let (_, detail) = si.get(&format!("/api/chat/{group}")).await;
    assert_eq!(detail["can_manage"], true, "прежний владелец стал администратором: {detail}");

    // 5. Статистика — только обезличенная и только суперадминистратору; малые числа скрыты.
    assert_eq!(si.get("/api/admin/chats/stats").await.0, StatusCode::FORBIDDEN);
    let (code, s) = admin.get("/api/admin/chats/stats").await;
    assert_eq!(code, StatusCode::OK, "{s}");
    assert!(s["direct_chats"].is_null() && s["group_chats"].is_null(), "{s}");
    assert!(!s.to_string().contains("Иван") && !s.to_string().contains(&group));
}
