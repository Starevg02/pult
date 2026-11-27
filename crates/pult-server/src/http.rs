//! Роутер: `/health`, `/metrics`, учёт запросов, заголовки безопасности.

use axum::{
    Json, Router,
    extract::{ConnectInfo, MatchedPath, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use metrics_exporter_prometheus::PrometheusHandle;
use serde::Serialize;
use sqlx::{PgPool, types::ipnetwork::IpNetwork};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tower_http::{set_header::SetResponseHeaderLayer, trace::TraceLayer};

/// Сколько клиент может слать заголовки запроса (защита от «медленных» соединений, slowloris).
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Ограничение медленных клиентов для HTTPS-сервера. В hyper время чтения заголовков задано по умолчанию,
/// но без таймера оно молча не действует, а axum-server таймер не ставит. Рукопожатие TLS axum-server
/// ограничивает сам (10 с).
pub fn limit_slow_clients(
    b: &mut hyper_util::server::conn::auto::Builder<hyper_util::rt::TokioExecutor>,
    header: Duration,
) {
    b.http1().timer(hyper_util::rt::TokioTimer::new()).header_read_timeout(header);
    b.http2().timer(hyper_util::rt::TokioTimer::new());
}

#[derive(Clone)]
pub struct AppState {
    /// `None` — БД не настроена (только в тестах и `pult check`).
    pub db: Option<PgPool>,
    pub metrics: PrometheusHandle,
    /// Токен для `/metrics` не с самого сервера; без него — только loopback.
    pub metrics_token: Option<pult_core::Secret>,
    /// Обратные прокси, которым верим в `X-Forwarded-For`.
    pub trusted_proxies: Arc<Vec<IpNetwork>>,
}

/// Политика CSP без внешних адресов.
const CSP: &str = "default-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

/// Политика CSP с поиском GIF в KLIPY: картинки с серверов KLIPY, запросы к их API (только браузером).
fn csp_with_klipy() -> String {
    format!(
        "{CSP}; img-src 'self' {}; connect-src 'self' {}",
        pult_chat::api::KLIPY_MEDIA.map(|u| u.trim_end_matches('/')).join(" "),
        pult_chat::api::KLIPY_API
    )
}

/// CSP на каждом ответе: адреса KLIPY пропускаются, только пока в настройках «Сообщений» есть ключ.
async fn content_security_policy(State(core): State<Option<pult_core::Core>>, req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let klipy = match &core {
        Some(core) => pult_chat::api::klipy_enabled(core).await,
        None => false,
    };
    let value = if klipy { HeaderValue::from_str(&csp_with_klipy()).ok() } else { None };
    resp.headers_mut().insert(header::CONTENT_SECURITY_POLICY, value.unwrap_or(HeaderValue::from_static(CSP)));
    resp
}

pub fn router(state: AppState, api: Option<crate::api::ApiState>) -> Router {
    let trusted = state.trusted_proxies.clone();
    let csp_core = api.as_ref().map(|a| a.core.clone());
    let modules = match api {
        // Выключенные в мастере модули отвечают 404 (`pult_core::org::gate`).
        Some(api) => crate::api::modules()
            .split_for_parts()
            .0
            .layer(middleware::from_fn_with_state(api.core.clone(), pult_core::org::gate))
            .with_state(api),
        None => Router::new(),
    };
    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_endpoint))
        .route("/api/openapi.json", get(openapi_json))
        .route("/api/auth/negotiate", get(crate::negotiate::negotiate))
        .with_state(state)
        .merge(modules)
        // Неизвестный путь под /api — 404 JSON, а не страница SPA.
        .route("/api/{*rest}", axum::routing::any(api_not_found))
        .fallback(crate::static_files::serve)
        .layer(middleware::from_fn(track_requests))
        .layer(middleware::from_fn(request_deadline))
        .layer(middleware::from_fn(no_store_api))
        .layer(middleware::from_fn_with_state(trusted, real_client_ip))
        .layer(TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<_>| {
            tracing::debug_span!("request", method = %req.method(), path = %req.uri().path())
        }))
        .layer(SetResponseHeaderLayer::overriding(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")))
        .layer(SetResponseHeaderLayer::overriding(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer")))
        .layer(middleware::from_fn_with_state(csp_core, content_security_policy))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::HeaderName::from_static("permissions-policy"),
            HeaderValue::from_static("camera=(), microphone=(), geolocation=(), payment=(), usb=()"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            axum::http::HeaderName::from_static("cross-origin-opener-policy"),
            HeaderValue::from_static("same-origin"),
        ))
        // Язык ответа (cookie `pult_lang` или Accept-Language) — для сообщений, отчётов, описаний прав.
        .layer(middleware::from_fn(pult_core::l10n::scope))
}

#[derive(Debug, Serialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    Up,
    Down,
    NotConfigured,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Health {
    pub status: &'static str,
    pub version: &'static str,
    pub database: Check,
}

/// Состояние зависимостей. Сведения о сервере (ЦП, память, PID) не раскрываются.
#[utoipa::path(get, path = "/health", tag = "system",
    responses((status = 200, description = "Все зависимости доступны", body = Health),
               (status = 503, description = "Есть недоступные зависимости", body = Health)))]
pub async fn health(State(state): State<AppState>) -> (StatusCode, Json<Health>) {
    let database = match &state.db {
        None => Check::NotConfigured,
        Some(pool) => {
            let ping = sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(pool);
            match tokio::time::timeout(Duration::from_secs(2), ping).await {
                Ok(Ok(_)) => Check::Up,
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "health: БД недоступна");
                    Check::Down
                }
                Err(_) => {
                    tracing::warn!("health: БД не ответила за 2 с");
                    Check::Down
                }
            }
        }
    };
    let ok = database == Check::Up;
    let code = if ok { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE };
    (code, Json(Health { status: if ok { "ok" } else { "degraded" }, version: env!("CARGO_PKG_VERSION"), database }))
}

async fn api_not_found() -> pult_core::ApiError {
    pult_core::ApiError::NotFound
}

/// Схема API для генерации TS-клиента.
async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    Json(crate::openapi::spec())
}

/// Метрики Prometheus: с самого сервера — свободно, с других адресов — только с токеном.
/// Маршруты и частота ошибок — сведения для разведки, наружу без проверки их не отдаём.
async fn metrics_endpoint(State(state): State<AppState>, req: Request) -> Response {
    let headers = req.headers();
    // С самого сервера — только напрямую: запрос через прокси на этом же сервере приходит с 127.0.0.1.
    let local = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .is_some_and(|c| c.0.ip().is_loopback())
        && !proxied(headers);
    let by_token = state.metrics_token.as_ref().is_some_and(|t| {
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|given| pult_core::crypto::constant_time_eq(given, t.expose()))
    });
    if !local && !by_token {
        return StatusCode::NOT_FOUND.into_response();
    }
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], state.metrics.render()).into_response()
}

/// Клиент за доверенным прокси: `X-Forwarded-For` справа налево до первого адреса не из прокси.
/// Левая часть заголовка — то, что прислал сам клиент, ей верить нельзя.
fn forwarded_client(headers: &HeaderMap, trusted: &[IpNetwork]) -> Option<IpAddr> {
    let list = headers.get_all("x-forwarded-for").iter().filter_map(|v| v.to_str().ok()).collect::<Vec<_>>().join(",");
    for item in list.rsplit(',') {
        let ip: IpAddr = item.trim().parse().ok()?;
        if !trusted.iter().any(|n| n.contains(ip)) {
            return Some(ip);
        }
    }
    None
}

/// Запрос прошёл через прокси (есть заголовки адреса клиента).
fn proxied(headers: &HeaderMap) -> bool {
    ["x-forwarded-for", "forwarded", "x-real-ip"].iter().any(|h| headers.contains_key(*h))
}

/// Подменяет адрес соединения настоящим адресом клиента — для лимитов входа, аудита и `/metrics`.
async fn real_client_ip(State(trusted): State<Arc<Vec<IpNetwork>>>, mut req: Request, next: Next) -> Response {
    let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
    if let Some(peer) = peer.filter(|p| trusted.iter().any(|n| n.contains(p.ip())))
        && let Some(ip) = forwarded_client(req.headers(), &trusted)
    {
        req.extensions_mut().insert(ConnectInfo(SocketAddr::new(ip, peer.port())));
    } else if proxied(req.headers()) || peer.is_some_and(|p| trusted.iter().any(|n| n.contains(p.ip()))) {
        // Адрес клиента скрыт прокси, которому не доверяем, или доверенный прокси его не передал:
        // для входа такой запрос — из интернета.
        req.extensions_mut().insert(pult_core::session::UnverifiedForwarding);
        if trusted.is_empty()
            && peer.is_some_and(|p| p.ip().is_loopback())
            && !PROXY_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            // Все клиенты видны как 127.0.0.1: общий лимит входа на всех и неверный адрес в аудите.
            tracing::warn!("запросы идут через прокси на этом сервере, а PULT_TRUSTED_PROXIES не задан");
        }
    }
    next.run(req).await
}

static PROXY_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Сколько длится запрос целиком, с чтением тела: медленное тело по байту держало бы соединение вечно
/// (время на заголовки ограничивает `HEADER_READ_TIMEOUT`). Поток событий (SSE) не задевает: ответ уже отдан.
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(60);
/// Загрузка вложения (до 100 МБ) или пакета агента на медленном канале филиала.
pub const UPLOAD_DEADLINE: Duration = Duration::from_secs(600);
/// Маршруты загрузки: тело читают только после проверки входа и прав.
const UPLOAD_PATHS: [&str; 2] = ["/api/helpdesk/files", "/api/admin/remote/packages"];
/// Запрос к помощнику ждёт модель до `timeout_secs` из настроек (до 300 с) плюс поиск контекста: на сервере
/// без видеокарты ответ идёт дольше минуты. Тело маленькое, вход проверяется до чтения тела, а число
/// одновременных запросов к модели ограничено самим помощником.
pub const ASSIST_DEADLINE: Duration =
    Duration::from_secs(pult_core::settings::AssistSettings::MAX_TIMEOUT_SECS as u64 + 20);

fn is_assist_call(req: &Request) -> bool {
    let path = req.uri().path();
    req.method() == axum::http::Method::POST
        && (path.starts_with("/api/helpdesk/assist/") || path == "/api/admin/assist/test")
}

/// Загрузка файла: вложение заявки или сообщения, пакет агента, аватар (GIF до 15 МБ).
fn is_upload(req: &Request) -> bool {
    let path = req.uri().path();
    UPLOAD_PATHS.contains(&path)
        || (req.method() == axum::http::Method::PUT && path == "/api/me/avatar")
        || (req.method() == axum::http::Method::POST
            && path
                .strip_prefix("/api/chat/")
                .and_then(|p| p.strip_suffix("/files"))
                .is_some_and(|id| !id.contains('/')))
}

fn deadline_for(req: &Request) -> Duration {
    let len =
        req.headers().get(header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    if is_upload(req) && len.is_some_and(|l| l > 2 * 1024 * 1024) {
        UPLOAD_DEADLINE
    } else if is_assist_call(req) {
        ASSIST_DEADLINE
    } else {
        REQUEST_DEADLINE
    }
}

async fn request_deadline(req: Request, next: Next) -> Response {
    let limit = deadline_for(&req);
    cut_after(limit, next.run(req)).await
}

async fn cut_after(limit: Duration, handling: impl Future<Output = Response>) -> Response {
    match tokio::time::timeout(limit, handling).await {
        Ok(resp) => resp,
        Err(_) => {
            let message = pult_core::l10n::tr("Запрос не уложился во время. Повторите.").into_owned();
            (StatusCode::REQUEST_TIMEOUT, Json(serde_json::json!({"error": "request_timeout", "message": message})))
                .into_response()
        }
    }
}

/// Ответы `/api/` не кэшируются браузером (общие ПК, кнопка «назад» после выхода), если обработчик
/// не задал своё правило (логотип организации кэшируется).
async fn no_store_api(req: Request, next: Next) -> Response {
    let api = req.uri().path().starts_with("/api/");
    let mut resp = next.run(req).await;
    if api {
        resp.headers_mut().entry(header::CACHE_CONTROL).or_insert(HeaderValue::from_static("no-store"));
    }
    resp
}

/// Число и длительность запросов. Метка — шаблон маршрута, а не путь (иначе метки множатся).
async fn track_requests(req: Request, next: Next) -> Response {
    let start = Instant::now();
    let method = req.method().to_string();
    let route =
        req.extensions().get::<MatchedPath>().map(|p| p.as_str().to_string()).unwrap_or_else(|| "unmatched".into());
    let resp = next.run(req).await;
    let status = resp.status().as_u16().to_string();
    let labels = [("method", method), ("route", route), ("status", status)];
    metrics::counter!("pult_http_requests_total", &labels).increment(1);
    metrics::histogram!("pult_http_request_duration_seconds", &labels).record(start.elapsed().as_secs_f64());
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn slow_client_is_dropped_but_normal_one_served() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let addr = l.local_addr().unwrap();
        let mut server = axum_server::from_tcp(l).unwrap();
        limit_slow_clients(server.http_builder(), Duration::from_millis(300));
        let app = Router::new().route("/", axum::routing::get(|| async { "ok" }));
        tokio::spawn(server.serve(app.into_make_service()));

        let mut ok = tokio::net::TcpStream::connect(addr).await.unwrap();
        ok.write_all(b"GET / HTTP/1.1\r\nHost: t\r\nConnection: close\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), ok.read_to_end(&mut buf)).await.unwrap().unwrap();
        assert!(buf.starts_with(b"HTTP/1.1 200"));

        // Заголовки не дописаны: соединение закрывается по таймауту, а не висит.
        let mut slow = tokio::net::TcpStream::connect(addr).await.unwrap();
        slow.write_all(b"GET / HTTP/1.1\r\nHost: t\r\n").await.unwrap();
        let mut buf = Vec::new();
        let closed = tokio::time::timeout(Duration::from_secs(3), slow.read_to_end(&mut buf)).await;
        assert!(closed.is_ok(), "медленное соединение не закрыто");
    }

    fn state() -> AppState {
        AppState {
            db: None,
            metrics: crate::telemetry::metrics_handle(),
            metrics_token: None,
            trusted_proxies: Arc::default(),
        }
    }

    async fn get(app: Router, uri: &str) -> (StatusCode, axum::http::HeaderMap, String) {
        get_from(app, uri, "127.0.0.1:50000", None).await
    }

    async fn get_from(
        app: Router,
        uri: &str,
        from: &str,
        bearer: Option<&str>,
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let mut req = Request::get(uri).body(Body::empty()).unwrap();
        req.extensions_mut().insert(axum::extract::ConnectInfo(from.parse::<std::net::SocketAddr>().unwrap()));
        if let Some(b) = bearer {
            req.headers_mut().insert(header::AUTHORIZATION, format!("Bearer {b}").parse().unwrap());
        }
        let resp = app.oneshot(req).await.unwrap();
        let (parts, body) = resp.into_parts();
        let bytes = body.collect().await.unwrap().to_bytes();
        (parts.status, parts.headers, String::from_utf8(bytes.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn health_without_db_is_503() {
        let (code, headers, body) = get(router(state(), None), "/health").await;
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert!(body.contains("\"database\":\"not_configured\""), "{body}");
        assert!(!body.contains("memory") && !body.contains("pid"), "{body}");
        assert_eq!(headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");
        let csp = headers.get(header::CONTENT_SECURITY_POLICY).unwrap().to_str().unwrap();
        assert!(csp.contains("object-src 'none'") && csp.contains("frame-ancestors 'none'"), "{csp}");
        assert!(headers.contains_key("permissions-policy"));
    }

    #[test]
    fn long_deadline_only_for_big_uploads() {
        let with = |path: &str, len: Option<u64>| {
            let mut b = Request::post(path);
            if let Some(l) = len {
                b = b.header(header::CONTENT_LENGTH, l);
            }
            deadline_for(&b.body(Body::empty()).unwrap())
        };
        assert_eq!(with("/api/helpdesk/files", Some(50 << 20)), UPLOAD_DEADLINE);
        assert_eq!(with("/api/admin/remote/packages", Some(30 << 20)), UPLOAD_DEADLINE);
        assert_eq!(with("/api/helpdesk/files", Some(1000)), REQUEST_DEADLINE, "маленький файл — обычный срок");
        assert_eq!(with("/api/helpdesk/files", None), REQUEST_DEADLINE);
        assert_eq!(with("/api/chat/0190a0a0-0000-7000-8000-000000000000/files", Some(50 << 20)), UPLOAD_DEADLINE);
        assert_eq!(with("/api/chat/a/b/files", Some(50 << 20)), REQUEST_DEADLINE);
        let avatar =
            Request::put("/api/me/avatar").header(header::CONTENT_LENGTH, 10u64 << 20).body(Body::empty()).unwrap();
        assert_eq!(deadline_for(&avatar), UPLOAD_DEADLINE);
        // Заявленная большая длина на вход не продлевает срок: медленное тело держало бы соединение 10 минут.
        assert_eq!(with("/api/auth/login", Some(100 << 20)), REQUEST_DEADLINE);
    }

    #[test]
    fn assist_calls_wait_for_the_model() {
        let post = |path: &str| deadline_for(&Request::post(path).body(Body::empty()).unwrap());
        assert_eq!(post("/api/helpdesk/assist/chat"), ASSIST_DEADLINE);
        assert_eq!(post("/api/helpdesk/assist/tickets/1001/draft"), ASSIST_DEADLINE);
        assert_eq!(post("/api/helpdesk/assist/category"), ASSIST_DEADLINE);
        assert_eq!(post("/api/admin/assist/test"), ASSIST_DEADLINE);
        assert!(ASSIST_DEADLINE > Duration::from_secs(300), "срок запроса длиннее самого долгого ожидания модели");
        // Чтение настроек и всё остальное — обычный срок.
        assert_eq!(deadline_for(&Request::get("/api/helpdesk/assist").body(Body::empty()).unwrap()), REQUEST_DEADLINE);
        assert_eq!(post("/api/admin/assist"), REQUEST_DEADLINE);
        assert_eq!(post("/api/helpdesk/tickets"), REQUEST_DEADLINE);
    }

    #[tokio::test]
    async fn slow_request_is_cut_off() {
        // Тело читается внутри обработчика, поэтому медленное тело упирается в тот же срок.
        let slow = async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            "поздно".into_response()
        };
        let resp = cut_after(Duration::from_millis(20), slow).await;
        assert_eq!(resp.status(), StatusCode::REQUEST_TIMEOUT);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&body).contains("\"error\":\"request_timeout\""));
        let quick = cut_after(Duration::from_secs(5), async { "ok".into_response() }).await;
        assert_eq!(quick.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn metrics_through_local_proxy_is_not_local() {
        // nginx на том же сервере без PULT_TRUSTED_PROXIES: соединение с 127.0.0.1, но запрос — чужой.
        let mut req = Request::get("/metrics").header("x-forwarded-for", "10.1.2.3").body(Body::empty()).unwrap();
        req.extensions_mut().insert(axum::extract::ConnectInfo("127.0.0.1:50000".parse::<SocketAddr>().unwrap()));
        let resp = router(state(), None).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn untrusted_proxy_hides_client_from_login() {
        // Прокси без PULT_TRUSTED_PROXIES: виден его внутренний адрес. Вход из интернета через него
        // прошёл бы без второго фактора, поэтому запрос помечается.
        let probe = |trusted: Vec<IpNetwork>| {
            Router::new()
                .route(
                    "/p",
                    axum::routing::get(|req: Request| async move {
                        let marked = req.extensions().get::<pult_core::session::UnverifiedForwarding>().is_some();
                        let ip = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip().to_string());
                        format!("{marked} {}", ip.unwrap_or_default())
                    }),
                )
                .layer(middleware::from_fn_with_state(Arc::new(trusted), real_client_ip))
        };
        let call = |app: Router, from: &'static str, xff: Option<&'static str>| async move {
            let mut req = Request::get("/p");
            if let Some(x) = xff {
                req = req.header("x-forwarded-for", x);
            }
            let mut req = req.body(Body::empty()).unwrap();
            req.extensions_mut().insert(ConnectInfo(from.parse::<SocketAddr>().unwrap()));
            let resp = app.oneshot(req).await.unwrap();
            String::from_utf8(resp.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
        };
        assert_eq!(call(probe(vec![]), "10.0.0.9:1", None).await, "false 10.0.0.9");
        assert_eq!(call(probe(vec![]), "10.0.0.9:1", Some("203.0.113.5")).await, "true 10.0.0.9");
        let trusted = vec!["10.0.0.9/32".parse().unwrap()];
        assert_eq!(call(probe(trusted.clone()), "10.0.0.9:1", Some("203.0.113.5")).await, "false 203.0.113.5");
        assert_eq!(call(probe(trusted.clone()), "10.0.0.8:1", Some("10.1.1.1")).await, "true 10.0.0.8", "чужой прокси");
        assert_eq!(
            call(probe(trusted), "10.0.0.9:1", None).await,
            "true 10.0.0.9",
            "доверенный прокси без адреса клиента"
        );
    }

    #[tokio::test]
    async fn api_answers_are_not_cached() {
        // Ответы API (ФИО, телефоны, сеансы) не остаются в кэше браузера на общем ПК.
        let (code, headers, _) = get(router(state(), None), "/api/nope").await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "no-store");
        // Свой заголовок обработчика не перебивается.
        let app = Router::new().route(
            "/api/logo",
            axum::routing::get(|| async { ([(header::CACHE_CONTROL, "public, max-age=86400")], "x") }),
        );
        let app = app.layer(middleware::from_fn(no_store_api));
        let (_, headers, _) = get(app, "/api/logo").await;
        assert_eq!(headers.get(header::CACHE_CONTROL).unwrap(), "public, max-age=86400");
    }

    #[tokio::test]
    async fn metrics_counts_requests_by_route() {
        let st = state();
        let _ = get(router(st.clone(), None), "/health").await;
        let (code, headers, body) = get(router(st, None), "/metrics").await;
        assert_eq!(code, StatusCode::OK);
        assert!(headers.get(header::CONTENT_TYPE).unwrap().to_str().unwrap().starts_with("text/plain"));
        assert!(body.contains("pult_http_requests_total"), "{body}");
        assert!(body.contains("route=\"/health\""), "{body}");
    }

    #[test]
    fn forwarded_client_ignores_what_the_client_wrote() {
        let trusted: Vec<IpNetwork> = vec!["10.0.0.0/24".parse().unwrap()];
        let with = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert("x-forwarded-for", v.parse().unwrap());
            h
        };
        let ip = |s: &str| Some(s.parse::<IpAddr>().unwrap());
        assert_eq!(forwarded_client(&with("192.0.2.7"), &trusted), ip("192.0.2.7"));
        assert_eq!(
            forwarded_client(&with("1.1.1.1, 192.0.2.7, 10.0.0.5"), &trusted),
            ip("192.0.2.7"),
            "подделка слева"
        );
        assert_eq!(forwarded_client(&with("10.0.0.5"), &trusted), None);
        assert_eq!(forwarded_client(&with("мусор"), &trusted), None);
        assert_eq!(forwarded_client(&HeaderMap::new(), &trusted), None);
    }

    #[tokio::test]
    async fn metrics_behind_trusted_local_proxy_are_closed() {
        let st = AppState { trusted_proxies: Arc::new(vec!["127.0.0.1/32".parse().unwrap()]), ..state() };
        let mut req = Request::get("/metrics").body(Body::empty()).unwrap();
        req.extensions_mut().insert(ConnectInfo("127.0.0.1:50000".parse::<SocketAddr>().unwrap()));
        req.headers_mut().insert("x-forwarded-for", "192.0.2.10".parse().unwrap());
        assert_eq!(router(st, None).oneshot(req).await.unwrap().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn metrics_only_local_or_with_token() {
        let (code, _, _) = get_from(router(state(), None), "/metrics", "192.0.2.10:50000", None).await;
        assert_eq!(code, StatusCode::NOT_FOUND, "с чужого адреса без токена");
        let st = AppState { metrics_token: Some(pult_core::Secret::new("metrics-token-1234")), ..state() };
        let (code, _, _) = get_from(router(st.clone(), None), "/metrics", "192.0.2.10:50000", Some("wrong")).await;
        assert_eq!(code, StatusCode::NOT_FOUND, "неверный токен");
        let (code, _, _) = get_from(router(st, None), "/metrics", "192.0.2.10:50000", Some("metrics-token-1234")).await;
        assert_eq!(code, StatusCode::OK, "верный токен");
        let (code, _, _) = get_from(router(state(), None), "/metrics", "[::1]:50000", None).await;
        assert_eq!(code, StatusCode::OK, "с самого сервера");
    }

    #[tokio::test]
    async fn unknown_route_is_404_and_labelled_unmatched() {
        let st = state();
        let (code, _, _) = get(router(st.clone(), None), "/api/nope/123").await;
        assert_eq!(code, StatusCode::NOT_FOUND);
        let (_, _, body) = get(router(st, None), "/metrics").await;
        assert!(!body.contains("nope/123"), "{body}");
    }
}
