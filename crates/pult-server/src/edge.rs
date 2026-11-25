//! Пограничный прокси для публикации портала в Интернет (`pult edge nginx`): готовая конфигурация
//! nginx на отдельном узле в DMZ. Портал сам проверяет вход, сеансы и права; прокси добавляет то, что
//! удобнее делать на границе: TLS снаружи, ограничение частоты запросов и попыток входа, закрытые
//! снаружи пути (агенты, helper, метрики, Kerberos), лимит размера запроса и журнал.

use std::fmt::Write as _;

/// Параметры конфигурации.
pub struct EdgeOptions {
    /// Внешнее имя портала (`pult.example.com`).
    pub host: String,
    /// Внутренний адрес портала (`https://10.0.0.10`).
    pub upstream: String,
    /// Сертификат (с цепочкой) и ключ для внешнего имени на узле прокси.
    pub cert: String,
    pub key: String,
    /// Сертификат CA, которым подписан внутренний сертификат портала; без него прокси не проверяет портал.
    pub upstream_ca: Option<String>,
    /// Наибольший размер запроса, МБ (вложения заявок).
    pub body_mb: u32,
    /// Попыток входа в минуту с одного адреса.
    pub login_per_minute: u32,
}

/// Пути, которые снаружи не нужны: агенты и helper работают в сети организации, метрики — для мониторинга,
/// Kerberos работает только в домене.
pub const CLOSED: &[&str] = &["/api/agent", "/api/remote/v1", "/metrics", "/api/auth/negotiate", "/api/auth/kerberos"];

fn host_ok(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.contains('.')
        && !h.starts_with(['-', '.'])
        && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

/// Путь к файлу на узле прокси: абсолютный, без пробелов и символов синтаксиса nginx.
fn path_ok(p: &str) -> bool {
    p.starts_with('/') && !p.chars().any(|c| c.is_whitespace() || matches!(c, ';' | '{' | '}' | '"' | '\'' | '$' | '#'))
}

/// Внутренний адрес: `https://имя-или-IP[:порт]` без пути.
fn upstream_ok(u: &str) -> bool {
    let Some(rest) = u.strip_prefix("https://") else { return false };
    let rest = rest.trim_end_matches('/');
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '[' | ']'))
}

/// Конфигурация nginx (`/etc/nginx/conf.d/pult-edge.conf`).
pub fn nginx(o: &EdgeOptions) -> Result<String, String> {
    if !host_ok(&o.host) {
        return Err(format!("внешнее имя должно быть DNS-именем: {}", o.host));
    }
    if !upstream_ok(&o.upstream) {
        return Err(format!("внутренний адрес портала — https://имя[:порт]: {}", o.upstream));
    }
    for p in [Some(&o.cert), Some(&o.key), o.upstream_ca.as_ref()].into_iter().flatten() {
        if !path_ok(p) {
            return Err(format!("путь к файлу — абсолютный, без пробелов и ; {{ }} \" ' $ #: {p}"));
        }
    }
    if !(1..=1024).contains(&o.body_mb) || !(1..=600).contains(&o.login_per_minute) {
        return Err("размер запроса — 1…1024 МБ, попыток входа — 1…600 в минуту".into());
    }
    let upstream = o.upstream.trim_end_matches('/');
    let host = &o.host;
    let mut s = String::new();
    let _ = writeln!(s, "# Пограничный прокси Pult для {host} (создано: pult {}).", env!("CARGO_PKG_VERSION"));
    s.push_str(
        "# На портале задайте PULT_TRUSTED_PROXIES=<адрес этого узла> и перезапустите его: иначе все запросы\n\
         # будут выглядеть пришедшими с адреса прокси. Проверка: nginx -t && systemctl reload nginx.\n\
         # Здесь же — серверы по умолчанию: уберите другие default_server (в Debian — sites-enabled/default).\n\n",
    );
    s.push_str("limit_req_zone $binary_remote_addr zone=pult_login:10m rate=");
    let _ = writeln!(s, "{}r/m;", o.login_per_minute);
    s.push_str(
        "limit_req_zone $binary_remote_addr zone=pult_api:10m rate=20r/s;\n\
         limit_conn_zone $binary_remote_addr zone=pult_conn:10m;\n\
         log_format pult_edge '$remote_addr [$time_iso8601] \"$request_method $uri\" $status $body_bytes_sent '\n\
         \x20                    'rt=$request_time limited=$limit_req_status';\n\n",
    );
    s.push_str(
        "# Запросы с чужим именем (сканеры по адресу, подставной Host) — без ответа и без сертификата портала.\n\
         server {\n    listen 80 default_server;\n    listen [::]:80 default_server;\n    return 444;\n}\n\
         server {\n    listen 443 ssl default_server;\n    listen [::]:443 ssl default_server;\n    \
         ssl_reject_handshake on;\n}\n\n",
    );
    let _ = writeln!(
        s,
        "server {{\n    listen 80;\n    listen [::]:80;\n    server_name {host};\n    \
         # Проверка ACME (Let's Encrypt, HTTP-01): certbot certonly --webroot -w /var/www/acme -d {host}\n    \
         location ^~ /.well-known/acme-challenge/ {{ root /var/www/acme; }}\n    \
         location / {{ return 301 https://{host}$request_uri; }}\n}}\n"
    );
    let _ = writeln!(
        s,
        "server {{\n    listen 443 ssl;\n    listen [::]:443 ssl;\n    http2 on;\n    server_name {host};\n\n    \
         ssl_certificate {};\n    ssl_certificate_key {};\n    ssl_protocols TLSv1.2 TLSv1.3;\n    \
         ssl_prefer_server_ciphers off;\n    ssl_session_cache shared:pult_tls:10m;\n    ssl_session_tickets off;\n\n    \
         add_header Strict-Transport-Security \"max-age=31536000\" always;\n    server_tokens off;\n    \
         access_log /var/log/nginx/pult-edge.log pult_edge;\n\n    \
         client_max_body_size {}m;\n    client_body_timeout 30s;\n    client_header_timeout 10s;\n    \
         large_client_header_buffers 4 16k;\n    limit_conn pult_conn 50;\n    limit_req_status 429;\n    \
         limit_conn_status 429;\n",
        o.cert, o.key, o.body_mb
    );
    s.push_str(
        "    # Только методы, которые использует портал.\n    \
         if ($request_method !~ ^(GET|HEAD|POST|PUT|PATCH|DELETE)$) { return 405; }\n    \
         # Скрытые файлы и обход путей.\n    location ~ /\\. { return 404; }\n\n",
    );
    s.push_str("    # Снаружи закрыто: агенты, helper, метрики, Kerberos.\n");
    for p in CLOSED {
        let _ = writeln!(s, "    location ^~ {p} {{ return 404; }}");
    }
    s.push_str("\n    # Общие заголовки к порталу: адрес клиента заменяется, а не дописывается (подделать нельзя).\n");
    let proxy = format!(
        "        proxy_pass {upstream};\n        proxy_http_version 1.1;\n        proxy_set_header Host {host};\n        \
         proxy_set_header X-Forwarded-For $remote_addr;\n        proxy_set_header X-Forwarded-Proto https;\n        \
         proxy_set_header Connection \"\";\n"
    );
    let _ = writeln!(
        s,
        "    location = /api/auth/login {{\n        limit_req zone=pult_login burst=5 nodelay;\n{proxy}    }}"
    );
    let _ = writeln!(
        s,
        "    location = /api/me/password {{\n        limit_req zone=pult_login burst=5 nodelay;\n{proxy}    }}"
    );
    let _ = writeln!(
        s,
        "    location ^~ /api/auth/oidc/ {{\n        limit_req zone=pult_login burst=10 nodelay;\n{proxy}    }}"
    );
    let _ = writeln!(
        s,
        "    location ^~ /api/public/ {{\n        limit_req zone=pult_login burst=5 nodelay;\n{proxy}    }}"
    );
    let _ = writeln!(
        s,
        "    # События (SSE): без буферизации, соединение живёт долго.\n    location = /api/events {{\n        \
         proxy_buffering off;\n        proxy_read_timeout 1h;\n{proxy}    }}"
    );
    let _ = writeln!(s, "    location /api/ {{\n        limit_req zone=pult_api burst=100 nodelay;\n{proxy}    }}");
    let _ = writeln!(s, "    location / {{\n{proxy}    }}");
    s.push('\n');
    match &o.upstream_ca {
        Some(ca) => {
            let _ = writeln!(
                s,
                "    # Проверка сертификата портала: прокси не отправит пароли подменённому серверу.\n    \
                 proxy_ssl_verify on;\n    proxy_ssl_trusted_certificate {ca};\n    proxy_ssl_server_name on;\n    \
                 proxy_ssl_protocols TLSv1.2 TLSv1.3;"
            );
        }
        None => s.push_str(
            "    # ВНИМАНИЕ: сертификат портала не проверяется (не задан --upstream-ca). Пароли идут к порталу\n    \
             # по TLS, но подмену сервера внутри сети прокси не заметит. Укажите CA внутреннего сертификата.\n    \
             proxy_ssl_verify off;\n    proxy_ssl_server_name on;\n",
        ),
    }
    s.push_str("}\n");
    Ok(s)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn opts() -> EdgeOptions {
        EdgeOptions {
            host: "pult.example.com".into(),
            upstream: "https://10.0.0.10".into(),
            cert: "/etc/ssl/pult/fullchain.pem".into(),
            key: "/etc/ssl/pult/key.pem".into(),
            upstream_ca: Some("/etc/ssl/pult/corp-ca.pem".into()),
            body_mb: 26,
            login_per_minute: 10,
        }
    }

    #[test]
    fn config_closes_internal_paths_and_limits_login() {
        let c = nginx(&opts()).unwrap();
        for p in CLOSED {
            assert!(c.contains(&format!("location ^~ {p} {{ return 404; }}")), "{p}");
        }
        assert!(c.contains("rate=10r/m;"));
        assert!(c.contains("location = /api/auth/login {\n        limit_req zone=pult_login"));
        assert!(c.contains("proxy_set_header X-Forwarded-For $remote_addr;"), "адрес клиента заменяется");
        assert!(!c.contains("$proxy_add_x_forwarded_for"), "дописывание позволило бы подделать адрес");
        assert!(c.contains("client_max_body_size 26m;"));
        assert!(c.contains("proxy_ssl_verify on;\n    proxy_ssl_trusted_certificate /etc/ssl/pult/corp-ca.pem;"));
        assert!(c.contains("proxy_buffering off;"));
        assert!(c.contains("location ^~ /api/agent { return 404; }"), "и сам путь, и всё под ним");
        assert!(
            c.contains("proxy_set_header Host pult.example.com;") && !c.contains("$host"),
            "Host — свой, не клиента"
        );
        assert!(c.contains("listen 443 ssl default_server;") && c.contains("ssl_reject_handshake on;"));
        assert!(c.contains("location = /api/me/password {\n        limit_req zone=pult_login"));
        assert_eq!(c.matches('{').count(), c.matches('}').count(), "скобки сбалансированы");
    }

    #[test]
    fn without_ca_the_config_warns() {
        let c = nginx(&EdgeOptions { upstream_ca: None, ..opts() }).unwrap();
        assert!(c.contains("ВНИМАНИЕ") && c.contains("proxy_ssl_verify off;"));
    }

    #[test]
    fn injection_into_config_is_rejected() {
        let bad = [
            EdgeOptions { host: "pult.example.com; include /etc/passwd".into(), ..opts() },
            EdgeOptions { host: "localhost".into(), ..opts() },
            EdgeOptions { upstream: "http://10.0.0.10".into(), ..opts() },
            EdgeOptions { upstream: "https://10.0.0.10/x;".into(), ..opts() },
            EdgeOptions { cert: "relative.pem".into(), ..opts() },
            EdgeOptions { key: "/etc/ssl/a key.pem".into(), ..opts() },
            EdgeOptions { upstream_ca: Some("/ca.pem;}".into()), ..opts() },
            EdgeOptions { body_mb: 0, ..opts() },
            EdgeOptions { login_per_minute: 0, ..opts() },
        ];
        for o in bad {
            assert!(nginx(&o).is_err(), "{} {} {}", o.host, o.upstream, o.cert);
        }
        assert!(nginx(&EdgeOptions { upstream: "https://pult-int.corp.local:8443/".into(), ..opts() }).is_ok());
    }
}
