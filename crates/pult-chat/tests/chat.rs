//! Сквозной сценарий модуля «Коммуникации» на настоящей PostgreSQL и поддельном DC.
//! Нужен `PULT_TEST_DATABASE_URL` (в CI задан). Тест создаёт отдельную базу.
//!
//! Личный диалог (закрыт даже от суперадминистратора, текст в базе зашифрован, правка 15 минут с версиями),
//! групповой чат (состав ведут владелец и администраторы, уведомления о добавлении и исключении, история
//! для бывших участников только для чтения, скрытие и закрепление), отключение учёток в AD (передача
//! владения, блокировка чата, назначение владельца суперадминистратором), вложения (типы, шифрование,
//! доступ только участникам) и обезличенная статистика.

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
    /// Запрос с готовым телом: (код, байты ответа, заголовки).
    async fn raw(
        &mut self,
        method: Method,
        uri: &str,
        ctype: &str,
        body: Vec<u8>,
    ) -> (StatusCode, Vec<u8>, header::HeaderMap) {
        let mut req = Request::builder().method(method.clone()).uri(uri);
        let c = self.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ");
        req = req.header(header::COOKIE, c);
        if method != Method::GET
            && let Some(t) = self.cookies.get("__Host-pult_csrf")
        {
            req = req.header("x-csrf-token", t.clone());
        }
        let mut req = req.header(header::CONTENT_TYPE, ctype).body(Body::from(body)).unwrap();
        req.extensions_mut().insert(ConnectInfo("10.1.1.1:5000".parse::<SocketAddr>().unwrap()));
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let (status, headers) = (resp.status(), resp.headers().clone());
        (status, resp.into_body().collect().await.unwrap().to_bytes().to_vec(), headers)
    }
    async fn upload(&mut self, chat: &str, name: &str, data: &[u8]) -> (StatusCode, Value) {
        let b = "XBOUNDARYX";
        let mut body = format!("--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
        let ctype = format!("multipart/form-data; boundary={b}");
        let (code, bytes, _) = self.raw(Method::POST, &format!("/api/chat/{chat}/files"), &ctype, body).await;
        (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
    async fn upload_gif(&mut self, name: &str, data: &[u8]) -> (StatusCode, Value) {
        let b = "XBOUNDARYX";
        let mut body = format!("--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: image/gif\r\n\r\n").into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
        let ctype = format!("multipart/form-data; boundary={b}");
        let (code, bytes, _) = self.raw(Method::POST, "/api/chat/gifs", &ctype, body).await;
        (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
    async fn download(&mut self, file: &str) -> (StatusCode, Vec<u8>, header::HeaderMap) {
        self.raw(Method::GET, &format!("/api/chat/files/{file}"), "application/json", Vec::new()).await
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

    // Вложения: те же типы, что у заявок; содержимое и имя в базе зашифрованы; видят только участники.
    const PDF: &[u8] = "%PDF-1.4 секретный отчёт".as_bytes();
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";
    let (code, e) = iv.upload(&direct, "вирус.exe", b"MZ\x90").await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_file")), "{e}");
    assert_eq!(iv.upload(&direct, "поддельный.png", b"<html>").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(admin.upload(&direct, "a.pdf", PDF).await.0, StatusCode::NOT_FOUND, "чужой чат");
    let (code, pdf) = iv.upload(&direct, "Отчёт за ноябрь.pdf", PDF).await;
    assert_eq!(
        (code, pdf["image"].as_bool(), pdf["size"].as_i64()),
        (StatusCode::OK, Some(false), Some(PDF.len() as i64)),
        "{pdf}"
    );
    let (_, png) = iv.upload(&direct, "скрин.png", PNG).await;
    assert_eq!(png["image"], true);
    let raw: Vec<(Vec<u8>, Vec<u8>)> =
        sqlx::query_as("SELECT name, data FROM chat_files").fetch_all(&pool).await.unwrap();
    assert!(
        raw.iter()
            .all(|(n, d)| !String::from_utf8_lossy(n).contains("Отчёт") && !String::from_utf8_lossy(d).contains("PDF"))
    );
    // Неотправленное видит только загрузивший.
    assert_eq!(pe.download(&id(&pdf)).await.0, StatusCode::NOT_FOUND);
    assert_eq!(iv.download(&id(&pdf)).await.0, StatusCode::OK);
    // Сообщение только из файлов: текст не нужен.
    let (code, fm) =
        iv.post(&format!("/api/chat/{direct}/messages"), json!({"body": "", "files": [id(&pdf), id(&png)]})).await;
    assert_eq!(
        (code, fm["body"].as_str(), fm["files"].as_array().map(Vec::len)),
        (StatusCode::CREATED, Some(""), Some(2)),
        "{fm}"
    );
    assert_eq!(fm["files"][0]["name"], "Отчёт за ноябрь.pdf");
    let (code, e) = iv.post(&format!("/api/chat/{direct}/messages"), json!({"body": "ещё", "files": [id(&pdf)]})).await;
    assert_eq!(
        (code, e["error"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_files")),
        "второй раз не прикрепить"
    );
    let (code, data, h) = pe.download(&id(&pdf)).await;
    assert_eq!((code, data.as_slice()), (StatusCode::OK, PDF));
    assert_eq!(h[header::CONTENT_TYPE], "application/pdf");
    assert!(h[header::CONTENT_DISPOSITION].to_str().unwrap().starts_with("attachment;"));
    assert_eq!(h[header::CACHE_CONTROL], "no-store");
    assert!(pe.download(&id(&png)).await.2[header::CONTENT_DISPOSITION].to_str().unwrap().starts_with("inline;"));
    assert_eq!(admin.download(&id(&pdf)).await.0, StatusCode::NOT_FOUND, "суперадминистратор не читает");
    let (_, msgs) = pe.get(&format!("/api/chat/{direct}/messages")).await;
    assert_eq!(msgs.as_array().unwrap().last().unwrap()["files"].as_array().unwrap().len(), 2);
    let list = pe.chats(false).await;
    assert_eq!((list[0]["preview"].is_null(), list[0]["preview_files"].as_i64()), (true, Some(2)), "{list:?}");
    let many: Vec<String> = (0..11).map(|_| uuid::Uuid::now_v7().to_string()).collect();
    let (code, e) = iv.post(&format!("/api/chat/{direct}/messages"), json!({"body": "x", "files": many})).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("too_many_files")));
    // Неотправленное за сутки убирает фоновая задача.
    let (_, stale) = iv.upload(&direct, "забытый.txt", "черновик".as_bytes()).await;
    sqlx::query("UPDATE chat_files SET created_at = now() - interval '2 days' WHERE id = $1::uuid")
        .bind(id(&stale))
        .execute(&pool)
        .await
        .unwrap();
    pult_chat::sweep_blocked(&pool).await.unwrap();
    assert_eq!(iv.download(&id(&stale)).await.0, StatusCode::NOT_FOUND);
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM chat_files").fetch_one(&pool).await.unwrap();
    assert_eq!(left, 2, "отправленные остаются");

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
    let (_, late) = iv.upload(&group, "после.txt", "после исключения".as_bytes()).await;
    let (code, _) =
        iv.post(&format!("/api/chat/{group}/messages"), json!({"body": "Без Козлова", "files": [id(&late)]})).await;
    assert_eq!(code, StatusCode::CREATED);
    assert_eq!(ko.download(&id(&late)).await.0, StatusCode::NOT_FOUND, "вложение после исключения не видно");
    assert_eq!(ko.upload(&group, "a.txt", b"x").await.0, StatusCode::FORBIDDEN, "бывший участник не загружает");
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

    // 3а. Как в мессенджерах: ответ, реакции, закрепление, «прочитано», пересылка, поиск, файлы чата.
    // Владелец «Новостей» — Сидоров, участник — Петров (Иванова дальше отключают в AD — его здесь нет).
    let (_, n) = si.post("/api/chat/groups", json!({"title": "Новости", "members": [pe_id]})).await;
    let news = id(&n);
    let (_, q1) = si.post(&format!("/api/chat/{news}/messages"), json!({"body": "Обновление 1С вечером"})).await;
    let (code, r1) =
        pe.post(&format!("/api/chat/{news}/messages"), json!({"body": "Во сколько?", "reply_to": id(&q1)})).await;
    assert_eq!((code, r1["reply"]["preview"].as_str()), (StatusCode::CREATED, Some("Обновление 1С вечером")), "{r1}");
    let (code, e) = pe.post(&format!("/api/chat/{news}/messages"), json!({"body": "x", "reply_to": id(&gm1)})).await;
    assert_eq!((code, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("invalid_reply")), "ответ — в своём чате");
    let react = format!("/api/chat/{news}/messages/{}/reactions", id(&q1));
    assert_eq!(pe.put(&react, json!({"emoji": "👍"})).await.0, StatusCode::NO_CONTENT);
    assert_eq!(si.put(&react, json!({"emoji": "👍"})).await.0, StatusCode::NO_CONTENT);
    assert_eq!(si.put(&react, json!({"emoji": "💩"})).await.0, StatusCode::BAD_REQUEST, "только из списка");
    let (_, msgs) = si.get(&format!("/api/chat/{news}/messages")).await;
    let r = &msgs[0]["reactions"][0];
    assert_eq!(
        (r["emoji"].as_str(), r["count"].as_i64(), r["mine"].as_bool(), r["names"].as_array().map(Vec::len)),
        (Some("👍"), Some(2), Some(true), Some(2)),
        "{msgs}"
    );
    assert_eq!(si.put(&react, json!({"emoji": "👍"})).await.0, StatusCode::NO_CONTENT, "повторно — снять");
    let (_, msgs) = si.get(&format!("/api/chat/{news}/messages")).await;
    assert_eq!(msgs[0]["reactions"][0]["count"], 1, "{msgs}");
    // Закрепляют в группе владелец и администраторы.
    let pin = format!("/api/chat/{news}/pin");
    assert_eq!(pe.put(&pin, json!({"message": id(&q1)})).await.0, StatusCode::FORBIDDEN);
    assert_eq!(si.put(&pin, json!({"message": id(&gm1)})).await.0, StatusCode::NOT_FOUND, "сообщение другого чата");
    assert_eq!(si.put(&pin, json!({"message": id(&q1)})).await.0, StatusCode::NO_CONTENT);
    let (_, d) = pe.get(&format!("/api/chat/{news}")).await;
    assert_eq!(
        (d["pinned_message"]["preview"].as_str(), d["can_pin"].as_bool()),
        (Some("Обновление 1С вечером"), Some(false)),
        "{d}"
    );
    assert_eq!(pe.post(&format!("/api/chat/{news}/read"), json!({})).await.0, StatusCode::NO_CONTENT);
    let (_, d) = si.get(&format!("/api/chat/{news}")).await;
    assert!(d["read_upto"].is_string(), "собеседник прочитал: {d}");
    assert_eq!(pe.post(&format!("/api/chat/{news}/typing"), json!({})).await.0, StatusCode::NO_CONTENT);
    // Пересылка: текст и вложения копируются, видно автора исходного сообщения.
    let (_, dm) = pe.get(&format!("/api/chat/{direct}/messages")).await;
    let with_files = dm.as_array().unwrap().iter().find(|m| m["files"].as_array().unwrap().len() == 2).unwrap();
    let forward = format!("/api/chat/{direct}/messages/{}/forward", id(with_files));
    let (code, fw) = pe.post(&forward, json!({"to": news})).await;
    assert_eq!(
        (code, fw["forwarded_from_name"].is_string(), fw["files"].as_array().map(Vec::len)),
        (StatusCode::CREATED, true, Some(2)),
        "{fw}"
    );
    let copy = fw["files"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(si.download(&copy).await.0, StatusCode::OK, "копию видят участники того чата");
    assert_eq!(si.download(&id(&pdf)).await.0, StatusCode::NOT_FOUND, "а исходник — нет");
    assert_eq!(ko.post(&forward, json!({"to": news})).await.0, StatusCode::NOT_FOUND, "из чужого чата не переслать");
    // Поиск по зашифрованному тексту («во сколько») и файлы чата.
    let (_, found) =
        si.get(&format!("/api/chat/{news}/search?q=%D0%B2%D0%BE%20%D1%81%D0%BA%D0%BE%D0%BB%D1%8C%D0%BA%D0%BE")).await;
    assert_eq!(found.as_array().map(Vec::len), Some(1), "{found}");
    let (_, files) = si.get(&format!("/api/chat/{news}/files")).await;
    assert_eq!(files.as_array().map(Vec::len), Some(2), "{files}");
    assert_eq!(si.put(&format!("/api/chat/{news}/prefs"), json!({"muted": true})).await.0, StatusCode::NO_CONTENT);
    assert!(si.chats(false).await.iter().any(|c| c["id"] == news.as_str() && c["muted"] == true));
    // «Мои GIF»: личная подборка, отправка копии в чат, сохранение из переписки.
    const GIF: &[u8] =
        b"GIF89a\x01\0\x01\0\x80\0\0\0\0\0\xff\xff\xff!\xf9\x04\x01\0\0\0\0,\0\0\0\0\x01\0\x01\0\0\x02\x02D\x01\0;";
    let (code, g) = si.upload_gif("смешной.gif", GIF).await;
    assert_eq!(code, StatusCode::OK, "{g}");
    let (_, again) = si.upload_gif("ещё раз.gif", GIF).await;
    assert_eq!(id(&again), id(&g), "тот же GIF второй раз не добавляется");
    assert_eq!(si.upload_gif("картинка.gif", PNG).await.0, StatusCode::BAD_REQUEST, "не GIF");
    let (_, mine) = si.get("/api/chat/gifs").await;
    assert_eq!(mine.as_array().map(Vec::len), Some(1), "{mine}");
    let gif_url = format!("/api/chat/gifs/{}", id(&g));
    let (code, data, headers) = si.raw(Method::GET, &gif_url, "application/json", Vec::new()).await;
    assert_eq!(
        (code, data.as_slice(), headers[header::CONTENT_TYPE].to_str().unwrap()),
        (StatusCode::OK, GIF, "image/gif")
    );
    assert_eq!(pe.get(&gif_url).await.0, StatusCode::NOT_FOUND, "чужая подборка не видна");
    let send_gif = format!("/api/chat/{news}/gifs/{}", id(&g));
    assert_eq!(pe.post(&send_gif, json!({})).await.0, StatusCode::NOT_FOUND, "чужой GIF не отправить");
    assert_eq!(
        si.post(&format!("/api/chat/{direct}/gifs/{}", id(&g)), json!({})).await.0,
        StatusCode::NOT_FOUND,
        "не участник"
    );
    let (code, sent) = si.post(&send_gif, json!({"reply_to": id(&q1)})).await;
    assert_eq!(
        (code, sent["files"][0]["mime"].as_str(), sent["body"].as_str(), sent["reply"]["id"].as_str()),
        (StatusCode::CREATED, Some("image/gif"), Some(""), Some(id(&q1).as_str())),
        "{sent}"
    );
    let sent_file = sent["files"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(pe.download(&sent_file).await.1, GIF, "копия в чате — тот же GIF");
    let (code, saved) = pe.post("/api/chat/gifs/from-file", json!({"file": sent_file})).await;
    assert_eq!(code, StatusCode::OK, "{saved}");
    assert_eq!(
        ko.post("/api/chat/gifs/from-file", json!({"file": sent_file})).await.0,
        StatusCode::NOT_FOUND,
        "из чужого чата"
    );
    assert_eq!(
        pe.post("/api/chat/gifs/from-file", json!({"file": id(&png)})).await.0,
        StatusCode::BAD_REQUEST,
        "PNG — не GIF"
    );
    let pe_gif = format!("/api/chat/gifs/{}", id(&saved));
    assert_eq!(pe.delete(&pe_gif).await.0, StatusCode::NO_CONTENT);
    assert_eq!(pe.delete(&pe_gif).await.0, StatusCode::NOT_FOUND);
    assert_eq!(si.get("/api/chat/gifs").await.1.as_array().map(Vec::len), Some(1), "у отправителя подборка на месте");
    assert_eq!(pe.download(&sent_file).await.0, StatusCode::OK, "отправленная копия остаётся");
    // Срок хранения вложений: содержимое стирается, отметка в переписке остаётся.
    let retention = "/api/admin/chats/settings";
    assert_eq!(si.put(retention, json!({"file_retention_days": 30})).await.0, StatusCode::FORBIDDEN);
    assert_eq!(admin.put(retention, json!({"file_retention_days": 30})).await.0, StatusCode::NO_CONTENT);
    // Пересланная копия считает срок от оригинала (kept_since): пересылка не продлевает хранение.
    let kept: Option<String> = sqlx::query_scalar("SELECT kept_since::text FROM chat_files WHERE id = $1::uuid")
        .bind(&copy)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(kept.is_some(), "у копии нет kept_since");
    sqlx::query("UPDATE chat_files SET kept_since = now() - interval '40 days' WHERE id = $1::uuid")
        .bind(&copy)
        .execute(&pool)
        .await
        .unwrap();
    pult_chat::sweep_blocked(&pool).await.unwrap();
    assert_eq!(si.download(&copy).await.0, StatusCode::GONE);
    let (_, msgs) = si.get(&format!("/api/chat/{news}/messages")).await;
    assert!(msgs.to_string().contains("\"expired\":true"), "{msgs}");
    let (_, st) = admin.get("/api/admin/chats/storage").await;
    assert_eq!((st["expired_files"].as_i64(), st["file_retention_days"].as_i64()), (Some(1), Some(30)), "{st}");
    assert_eq!(admin.put(retention, json!({"file_retention_days": 0})).await.0, StatusCode::NO_CONTENT);

    // 4. Учётка владельца отключена в AD: владение — самому давнему администратору, уведомление — только ему.
    sqlx::query("UPDATE users SET is_active = false WHERE id = $1::uuid").bind(&iv_id).execute(&pool).await.unwrap();
    assert_eq!(pult_chat::sweep_blocked(&pool).await.unwrap(), 1, "только групповой чат; личный диалог не трогается");
    let (_, detail) = pe.get(&format!("/api/chat/{group}")).await;
    assert_eq!(detail["members"][0]["role"], "owner", "{detail}");
    assert_eq!(detail["members"][0]["user_id"], pe_id.as_str());
    assert!(pe.kinds().await.contains(&"chat.owner".to_string()));
    assert!(!si.kinds().await.contains(&"chat.owner".to_string()));
    assert_eq!(
        pe.bodies(&direct).await,
        ["Привет!", ""],
        "переписка с отключённым сохраняется (второе — только файлы)"
    );
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
