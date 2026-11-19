//! Фронтенд, встроенный в бинарник (`rust-embed`, ADR-0003).
//!
//! Источник — `web/dist` (сборка Vite). В release-сборке файлы вшиваются в бинарник,
//! в debug — читаются с диска, чтобы не пересобирать сервер при правке фронтенда.
//! Если фронтенд не собран, на `/` отдаётся служебная страница.
//!
//! Маршрутизация SPA: неизвестный путь без расширения → `index.html`; `/api/*` сюда не попадает.
//! Файлы с хешем в имени (`assets/…`) кешируются надолго, `index.html` — нет.

use axum::{
    http::{HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Assets;

const PLACEHOLDER: &str = "<!doctype html><html lang=\"ru\"><meta charset=\"utf-8\"><title>Pult</title>\
<body style=\"font-family:system-ui;margin:3rem\"><h1>Pult</h1><p>Сервер работает, фронтенд ещё не собран \
(<code>web/dist</code>). Состояние — <a href=\"/health\">/health</a>.</p></body></html>";

/// Обработчик для всего, что не совпало с маршрутами API.
pub async fn serve(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if path.starts_with("api/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = if path.is_empty() { "index.html" } else { path };
    if let Some(resp) = file(path) {
        return resp;
    }
    // Клиентский маршрут SPA (`/tickets/1300`) → index.html. Отсутствующий файл с расширением — 404.
    let looks_like_file = path.rsplit('/').next().is_some_and(|last| last.contains('.'));
    if !looks_like_file {
        if let Some(resp) = file("index.html") {
            return resp;
        }
    }
    if path == "index.html" || !looks_like_file {
        return ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], PLACEHOLDER).into_response();
    }
    StatusCode::NOT_FOUND.into_response()
}

fn file(path: &str) -> Option<Response> {
    let f = Assets::get(path)?;
    let mime = f.metadata.mimetype().to_string();
    let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    let mut resp = f.data.into_owned().into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&mime).unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    Some(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn body(r: Response) -> (StatusCode, String) {
        use http_body_util::BodyExt;
        let code = r.status();
        let b = r.into_body().collect().await.unwrap().to_bytes();
        (code, String::from_utf8_lossy(&b).into_owned())
    }

    #[tokio::test]
    async fn api_paths_are_not_swallowed_by_spa() {
        let (code, _) = body(serve("/api/unknown".parse().unwrap()).await).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn root_and_spa_routes_return_html() {
        for uri in ["/", "/tickets/1300"] {
            let (code, text) = body(serve(uri.parse().unwrap()).await).await;
            assert_eq!(code, StatusCode::OK, "{uri}");
            assert!(text.contains("<html") || text.contains("<!doctype"), "{uri}");
        }
    }

    #[tokio::test]
    async fn missing_asset_is_404() {
        let (code, _) = body(serve("/assets/missing-abc123.js".parse().unwrap()).await).await;
        assert_eq!(code, StatusCode::NOT_FOUND);
    }
}
