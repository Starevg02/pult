//! Вход через провайдера OpenID Connect (Keycloak, ADFS, Entra ID): код авторизации с PKCE.
//!
//! Провайдер только подтверждает личность. Учётка берётся из зеркала AD по логину, UPN или почте из
//! ID-токена; права, отключение и второй фактор — как у входа паролем. Состояние входа (state, nonce,
//! верификатор PKCE) лежит в зашифрованной cookie на 10 минут, а не в базе: брошенные входы ничего не копят.

use crate::auth;
use axum::{
    Json,
    extract::{Query, Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Secret,
    audit::{self, Actor, Entry, Source},
    crypto::{constant_time_eq, random_token, sha256},
    session::{self, AuthMethod},
    settings::{self, OidcClaim, OidcSecret, OidcSettings, SecuritySettings},
};
use ring::signature;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Путь возврата от провайдера: его регистрируют у провайдера как redirect URI.
pub const CALLBACK_PATH: &str = "/api/auth/oidc/callback";
/// Сколько секунд живёт начатый вход.
const PENDING_SECS: i64 = 600;
/// Настройки провайдера и его ключи перечитываются раз в час (и сразу — при незнакомом ключе).
const CACHE_TTL: Duration = Duration::from_secs(3600);
const MAX_BODY: usize = 1 << 20;
/// Допуск расхождения часов с провайдером, секунд.
const SKEW: i64 = 60;
/// Метка содержимого зашифрованной cookie: другой шифротекст портала за неё не сойдёт.
const COOKIE_TAG: &str = "oidc1:";

// ------------------------------------------------------------------ Провайдер

#[derive(Clone, Debug, Deserialize)]
struct Metadata {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

/// Открытый ключ провайдера (JWK): RSA или EC P-256.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Jwk {
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default, rename = "use")]
    usage: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
    #[serde(default)]
    crv: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
}

#[derive(Deserialize)]
struct JwkSet {
    keys: Vec<Value>,
}

#[derive(Clone)]
struct Provider {
    /// Издатель из настроек портала (без `/` в конце).
    configured: String,
    meta: Metadata,
    keys: Vec<Jwk>,
    at: Instant,
}

static CACHE: Mutex<Option<Provider>> = Mutex::new(None);
/// Последняя неудача: издатель, когда, ошибка. 10 секунд новые запросы её и получают, не дёргая провайдера.
static FAILED: Mutex<Option<(String, Instant, String)>> = Mutex::new(None);
/// Чтение настроек провайдера — по одному: пачка входов при истёкшем кеше не превращается в пачку запросов.
static DISCOVERY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const FAIL_TTL: Duration = Duration::from_secs(10);

fn forget_provider() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *FAILED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

fn loopback(u: &reqwest::Url) -> bool {
    matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
}

/// Адрес провайдера: только HTTPS. HTTP — лишь для провайдера на этом же сервере, если так задан
/// и сам издатель (разработка, тесты).
fn safe_url(raw: &str, allow_http: bool) -> Result<reqwest::Url, &'static str> {
    let u = reqwest::Url::parse(raw.trim()).map_err(|_| "Адрес провайдера записан неверно.")?;
    let scheme_ok = match u.scheme() {
        "https" => true,
        "http" => allow_http && loopback(&u),
        _ => false,
    };
    if !scheme_ok
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
    {
        return Err("Нужен адрес https:// без логина и фрагмента.");
    }
    Ok(u)
}

fn issuer_key(issuer: &str) -> &str {
    issuer.trim().trim_end_matches('/')
}

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| e.to_string())
}

fn net(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "Провайдер не ответил вовремя.".into()
    } else {
        "Нет связи с провайдером.".into()
    }
}

/// Ответ провайдера в JSON, не больше 1 МБ.
async fn read_json<T: DeserializeOwned>(mut r: reqwest::Response) -> Result<T, String> {
    let status = r.status();
    let mut buf = Vec::new();
    while let Some(c) = r.chunk().await.map_err(net)? {
        if buf.len() + c.len() > MAX_BODY {
            return Err("Ответ провайдера слишком большой.".into());
        }
        buf.extend_from_slice(&c);
    }
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    serde_json::from_slice(&buf).map_err(|_| "Провайдер ответил не в формате JSON.".to_string())
}

async fn discover(issuer: &str) -> Result<Provider, String> {
    let configured = issuer_key(issuer).to_string();
    let base = safe_url(&configured, true).map_err(str::to_string)?;
    let allow_http = base.scheme() == "http";
    let client = http()?;
    let url = format!("{configured}/.well-known/openid-configuration");
    let meta: Metadata = read_json(client.get(&url).send().await.map_err(net)?).await?;
    if issuer_key(&meta.issuer) != configured {
        return Err("Провайдер называет себя другим издателем: проверьте адрес.".into());
    }
    // Код, верификатор и секрет клиента уходят только на сервер издателя, а не туда, куда укажет документ.
    for u in [&meta.authorization_endpoint, &meta.token_endpoint, &meta.jwks_uri] {
        let e = safe_url(u, allow_http).map_err(str::to_string)?;
        if e.origin() != base.origin() {
            return Err("Адреса провайдера должны быть на том же сервере, что и издатель.".into());
        }
    }
    let set: JwkSet = read_json(client.get(&meta.jwks_uri).send().await.map_err(net)?).await?;
    let keys = set.keys.into_iter().filter_map(|k| serde_json::from_value::<Jwk>(k).ok()).collect();
    Ok(Provider { configured, meta, keys, at: Instant::now() })
}

/// Настройки и ключи провайдера: из кеша, если свежие и для того же издателя. `refresh` — перечитать
/// (незнакомый ключ, проверка из админки); перечитанное за последние 5 секунд считается свежим.
async fn provider(issuer: &str, refresh: bool) -> Result<Provider, String> {
    let key = issuer_key(issuer);
    let fresh = |max: Duration| {
        CACHE.lock().unwrap_or_else(|e| e.into_inner()).clone().filter(|p| p.configured == key && p.at.elapsed() < max)
    };
    let max = if refresh { Duration::from_secs(5) } else { CACHE_TTL };
    if let Some(p) = fresh(max) {
        return Ok(p);
    }
    let _one = DISCOVERY.lock().await;
    // Пока ждали очереди, провайдера мог перечитать соседний запрос.
    if let Some(p) = fresh(max) {
        return Ok(p);
    }
    if !refresh {
        let failed = FAILED.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some((_, _, e)) = failed.filter(|(k, at, _)| k == key && at.elapsed() < FAIL_TTL) {
            return Err(e);
        }
    }
    match discover(key).await {
        Ok(p) => {
            *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.clone());
            *FAILED.lock().unwrap_or_else(|e| e.into_inner()) = None;
            Ok(p)
        }
        Err(e) => {
            *FAILED.lock().unwrap_or_else(|e| e.into_inner()) = Some((key.to_string(), Instant::now(), e.clone()));
            Err(e)
        }
    }
}

// ------------------------------------------------------------------ ID-токен

/// Что должно быть в ID-токене.
pub struct Expect<'a> {
    pub issuer: &'a str,
    pub client_id: &'a str,
    pub nonce: &'a str,
    /// Сейчас, секунды Unix.
    pub now: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    Malformed,
    /// Алгоритм не RS256 и не ES256 (в том числе `none`).
    Alg,
    /// Нет подходящего ключа: провайдер мог сменить ключи.
    UnknownKey,
    Signature,
    /// Не сошлось поле: `iss`, `aud`, `azp`, `exp`, `iat`, `nbf`, `nonce`, `sub`.
    Claim(&'static str),
}

fn b64json(part: &str) -> Result<Value, TokenError> {
    let raw = URL_SAFE_NO_PAD.decode(part).map_err(|_| TokenError::Malformed)?;
    serde_json::from_slice(&raw).map_err(|_| TokenError::Malformed)
}

impl Jwk {
    fn fits(&self, alg: &str) -> bool {
        if self.usage.as_deref().is_some_and(|u| u != "sig") || self.alg.as_deref().is_some_and(|a| a != alg) {
            return false;
        }
        match alg {
            "RS256" => self.kty == "RSA",
            "ES256" => self.kty == "EC" && self.crv.as_deref() == Some("P-256"),
            _ => false,
        }
    }

    fn verify(&self, alg: &str, msg: &[u8], sig: &[u8]) -> bool {
        let d = |v: &Option<String>| v.as_deref().and_then(|s| URL_SAFE_NO_PAD.decode(s).ok());
        match alg {
            "RS256" => {
                let (Some(n), Some(e)) = (d(&self.n), d(&self.e)) else { return false };
                let start = n.iter().position(|b| *b != 0).unwrap_or(n.len());
                signature::RsaPublicKeyComponents { n: &n[start..], e: &e[..] }
                    .verify(&signature::RSA_PKCS1_2048_8192_SHA256, msg, sig)
                    .is_ok()
            }
            "ES256" => {
                let (Some(x), Some(y)) = (d(&self.x), d(&self.y)) else { return false };
                if x.len() != 32 || y.len() != 32 {
                    return false;
                }
                let mut point = Vec::with_capacity(65);
                point.push(4);
                point.extend_from_slice(&x);
                point.extend_from_slice(&y);
                signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point).verify(msg, sig).is_ok()
            }
            _ => false,
        }
    }
}

/// Проверяет подпись и поля ID-токена; возвращает его поля.
pub fn verify_id_token(token: &str, keys: &[Jwk], ex: &Expect<'_>) -> Result<Value, TokenError> {
    let mut it = token.split('.');
    let (Some(h), Some(p), Some(s), None) = (it.next(), it.next(), it.next(), it.next()) else {
        return Err(TokenError::Malformed);
    };
    let head = b64json(h)?;
    let sig = URL_SAFE_NO_PAD.decode(s).map_err(|_| TokenError::Malformed)?;
    let alg = head["alg"].as_str().ok_or(TokenError::Alg)?;
    if !matches!(alg, "RS256" | "ES256") {
        return Err(TokenError::Alg);
    }
    let kid = head["kid"].as_str();
    let signed = &token.as_bytes()[..h.len() + 1 + p.len()];
    let candidates: Vec<&Jwk> =
        keys.iter().filter(|k| k.fits(alg) && (kid.is_none() || k.kid.as_deref() == kid)).collect();
    if candidates.is_empty() {
        return Err(TokenError::UnknownKey);
    }
    if !candidates.iter().any(|k| k.verify(alg, signed, &sig)) {
        return Err(TokenError::Signature);
    }
    let claims = b64json(p)?;
    check_claims(&claims, ex)?;
    Ok(claims)
}

fn check_claims(c: &Value, ex: &Expect<'_>) -> Result<(), TokenError> {
    use TokenError::Claim;
    if c["iss"].as_str() != Some(ex.issuer) {
        return Err(Claim("iss"));
    }
    let aud_ok = match &c["aud"] {
        Value::String(a) => a == ex.client_id,
        Value::Array(a) => a.iter().any(|v| v.as_str() == Some(ex.client_id)),
        _ => false,
    };
    if !aud_ok {
        return Err(Claim("aud"));
    }
    // Токен для нескольких получателей — только с `azp`, равным нашему клиенту.
    let several = c["aud"].as_array().is_some_and(|a| a.len() > 1);
    match c["azp"].as_str() {
        Some(azp) if azp != ex.client_id => return Err(Claim("azp")),
        None if several => return Err(Claim("azp")),
        _ => {}
    }
    let exp = c["exp"].as_i64().ok_or(Claim("exp"))?;
    if exp + SKEW < ex.now {
        return Err(Claim("exp"));
    }
    let iat = c["iat"].as_i64().ok_or(Claim("iat"))?;
    if iat > ex.now + SKEW {
        return Err(Claim("iat"));
    }
    if c["nbf"].as_i64().is_some_and(|nbf| nbf > ex.now + SKEW) {
        return Err(Claim("nbf"));
    }
    let nonce = c["nonce"].as_str().ok_or(Claim("nonce"))?;
    if !constant_time_eq(nonce, ex.nonce) {
        return Err(Claim("nonce"));
    }
    if c["sub"].as_str().is_none_or(|s| s.is_empty() || s.len() > 255) {
        return Err(Claim("sub"));
    }
    Ok(())
}

/// Логин из ID-токена: поле зеркала AD (`sam`, `upn`, `mail`) и значение в нижнем регистре.
pub fn username(c: &Value, claim: OidcClaim) -> Option<(&'static str, String)> {
    let get =
        |k: &str| c[k].as_str().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty() && s.chars().count() <= 256);
    match claim {
        OidcClaim::PreferredUsername => {
            let v = get("preferred_username")?;
            if let Some((_, user)) = v.rsplit_once('\\') {
                (!user.is_empty()).then(|| ("sam", user.to_string()))
            } else if v.contains('@') {
                Some(("upn", v))
            } else {
                Some(("sam", v))
            }
        }
        OidcClaim::Upn => get("upn").filter(|v| v.contains('@')).map(|v| ("upn", v)),
        // Почту, которую провайдер не подтвердил, пользователь мог вписать себе сам.
        OidcClaim::Email => {
            let verified =
                matches!(&c["email_verified"], Value::Bool(true)) || c["email_verified"].as_str() == Some("true");
            if !verified {
                return None;
            }
            get("email").filter(|v| v.contains('@')).map(|v| ("mail", v))
        }
    }
}

// ------------------------------------------------------------------ Вход

/// Начатый вход: state, nonce, верификатор PKCE, издатель, срок.
#[derive(Serialize, Deserialize)]
struct Pending {
    s: String,
    n: String,
    v: String,
    i: String,
    exp: i64,
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn cookie_name(core: &Core) -> &'static str {
    if core.cookies.secure { "__Host-pult_oidc" } else { "pult_oidc" }
}

fn cookie(core: &Core, value: &str, max_age: i64) -> String {
    let secure = if core.cookies.secure { "; Secure" } else { "" };
    format!("{}={value}; HttpOnly; Max-Age={max_age}; Path=/; SameSite=Lax{secure}", cookie_name(core))
}

fn seal(core: &Core, p: &Pending) -> Result<String, ApiError> {
    let body = serde_json::to_string(p).map_err(ApiError::internal)?;
    let raw = core.secrets.encrypt(&Secret::new(format!("{COOKIE_TAG}{body}"))).map_err(ApiError::internal)?;
    Ok(URL_SAFE_NO_PAD.encode(raw))
}

fn unseal(core: &Core, v: &str) -> Option<Pending> {
    let raw = URL_SAFE_NO_PAD.decode(v).ok()?;
    let s = core.secrets.decrypt(&raw).ok()?;
    serde_json::from_str(s.expose().strip_prefix(COOKIE_TAG)?).ok()
}

enum Fail {
    /// Вход через провайдера выключен: 404.
    Off,
    /// На страницу входа с кодом ошибки.
    Code(&'static str),
    Api(ApiError),
}

impl From<ApiError> for Fail {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

impl From<sqlx::Error> for Fail {
    fn from(e: sqlx::Error) -> Self {
        Self::Api(e.into())
    }
}

fn redirect(to: &str, mut h: HeaderMap) -> Response {
    if let Ok(v) = HeaderValue::from_str(to) {
        h.insert(header::LOCATION, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // В адресе возврата — код авторизации: он не должен уйти дальше в Referer.
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    (StatusCode::SEE_OTHER, h).into_response()
}

fn with_cookie(mut h: HeaderMap, c: &str) -> HeaderMap {
    if let Ok(v) = HeaderValue::from_str(c) {
        h.append(header::SET_COOKIE, v);
    }
    h
}

fn fail(core: &Core, f: Fail) -> Response {
    let code = match f {
        Fail::Off => return ApiError::NotFound.into_response(),
        Fail::Code(c) => c,
        Fail::Api(e) => {
            tracing::error!(error = %e, "OIDC: вход не удался");
            "error"
        }
    };
    // Не тот state или нет cookie: начатый вход не трогаем — чужая ссылка на возврат не сбрасывает его.
    let h = if code == "state" { HeaderMap::new() } else { with_cookie(HeaderMap::new(), &cookie(core, "", 0)) };
    redirect(&format!("/?oidc_error={code}"), h)
}

fn unavailable(e: String) -> Fail {
    tracing::warn!(error = %e, "OIDC: провайдер недоступен");
    Fail::Code("unavailable")
}

/// Перейти к провайдеру. Кнопка на странице входа — обычная ссылка сюда.
#[utoipa::path(get, path = "/api/auth/oidc/start", tag = "auth",
    responses((status = 303, description = "К провайдеру или на страницу входа с `oidc_error`"),
        (status = 404, description = "Вход через провайдера выключен")))]
pub async fn oidc_start(State(core): State<Core>, req: Request) -> Response {
    let (parts, _) = req.into_parts();
    match start(&core, &parts).await {
        Ok(r) => r,
        Err(f) => fail(&core, f),
    }
}

async fn start(core: &Core, parts: &Parts) -> Result<Response, Fail> {
    let s: OidcSettings = settings::load(&core.db).await?;
    if !s.enabled {
        return Err(Fail::Off);
    }
    let sec: SecuritySettings = settings::load(&core.db).await?;
    if !s.allow_external && session::request_is_external(parts, &sec) {
        return Err(Fail::Code("external"));
    }
    let p = provider(&s.issuer, false).await.map_err(unavailable)?;
    let rnd = || random_token(32).map_err(|e| Fail::Api(ApiError::internal(e)));
    let pending = Pending { s: rnd()?, n: rnd()?, v: rnd()?, i: p.configured.clone(), exp: now() + PENDING_SECS };
    let challenge = URL_SAFE_NO_PAD.encode(sha256(pending.v.as_bytes()));
    let mut url = reqwest::Url::parse(&p.meta.authorization_endpoint).map_err(|_| Fail::Code("unavailable"))?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", s.client_id.trim())
        .append_pair("redirect_uri", s.redirect_uri.trim())
        .append_pair("scope", "openid profile email")
        .append_pair("state", &pending.s)
        .append_pair("nonce", &pending.n)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    let c = cookie(core, &seal(core, &pending)?, PENDING_SECS);
    Ok(redirect(url.as_str(), with_cookie(HeaderMap::new(), &c)))
}

#[derive(Deserialize, IntoParams)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    /// Провайдер отказал (`access_denied` и т. п.).
    pub error: Option<String>,
}

/// Возврат от провайдера: обмен кода на ID-токен, проверка и новый сеанс.
#[utoipa::path(get, path = "/api/auth/oidc/callback", tag = "auth", params(CallbackQuery),
    responses((status = 303, description = "Вошли — на главную; нет — на страницу входа с `oidc_error`"),
        (status = 404, description = "Вход через провайдера выключен")))]
pub async fn oidc_callback(State(core): State<Core>, req: Request) -> Response {
    let (parts, _) = req.into_parts();
    match callback(&core, &parts).await {
        Ok(r) => r,
        Err(f) => fail(&core, f),
    }
}

async fn callback(core: &Core, parts: &Parts) -> Result<Response, Fail> {
    let s: OidcSettings = settings::load(&core.db).await?;
    if !s.enabled {
        return Err(Fail::Off);
    }
    let Query(q) = Query::<CallbackQuery>::try_from_uri(&parts.uri).map_err(|_| Fail::Code("state"))?;
    // Вход начат в этом же браузере, недавно и у того же провайдера, и state совпадает.
    let pending = session::cookie(&parts.headers, cookie_name(core))
        .and_then(|v| unseal(core, v))
        .filter(|p| p.exp >= now() && p.i == issuer_key(&s.issuer))
        .filter(|p| q.state.as_deref().is_some_and(|st| constant_time_eq(st, &p.s)))
        .ok_or(Fail::Code("state"))?;
    if q.error.is_some() {
        return Err(Fail::Code("denied"));
    }
    let code = q.code.filter(|c| !c.is_empty() && c.len() <= 4096).ok_or(Fail::Code("state"))?;
    let actor = session::actor_from(parts, None);
    let sec: SecuritySettings = settings::load(&core.db).await?;
    let external = session::request_is_external(parts, &sec);
    if external && !s.allow_external {
        return Err(Fail::Code("external"));
    }
    let p = provider(&s.issuer, false).await.map_err(unavailable)?;
    let secret = client_secret(core).await?;
    let id_token = exchange(&p, &s, &code, &pending.v, secret.as_ref()).await.map_err(|e| {
        tracing::warn!(error = %e, "OIDC: обмен кода на токен не удался");
        Fail::Code("token")
    })?;
    let client_id = s.client_id.trim();
    let t = now();
    let first =
        verify_id_token(&id_token, &p.keys, &Expect { issuer: &p.meta.issuer, client_id, nonce: &pending.n, now: t });
    let claims = match first {
        // Провайдер сменил ключи: перечитать один раз (не чаще раза в 10 секунд).
        Err(TokenError::UnknownKey) if p.at.elapsed() > Duration::from_secs(10) => {
            let p = provider(&s.issuer, true).await.map_err(unavailable)?;
            verify_id_token(
                &id_token,
                &p.keys,
                &Expect { issuer: &p.meta.issuer, client_id, nonce: &pending.n, now: t },
            )
        }
        r => r,
    }
    .map_err(|e| {
        tracing::warn!(error = ?e, "OIDC: ID-токен не прошёл проверку");
        Fail::Code("token")
    })?;
    finish(core, &s, &sec, &p.meta.issuer, actor, external, &claims).await
}

async fn client_secret(core: &Core) -> Result<Option<Secret>, ApiError> {
    let st: OidcSecret = settings::load(&core.db).await?;
    match st.sealed {
        Some(b) => {
            let raw = STANDARD.decode(b).map_err(ApiError::internal)?;
            core.secrets.decrypt(&raw).map(Some).map_err(ApiError::internal)
        }
        None => Ok(None),
    }
}

/// Код авторизации → ID-токен (`client_secret_post`; без секрета — публичный клиент с PKCE).
async fn exchange(
    p: &Provider,
    s: &OidcSettings,
    code: &str,
    verifier: &str,
    secret: Option<&Secret>,
) -> Result<String, String> {
    #[derive(Deserialize)]
    struct TokenResponse {
        id_token: Option<String>,
    }
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", s.redirect_uri.trim()),
        ("code_verifier", verifier),
        ("client_id", s.client_id.trim()),
    ];
    if let Some(sec) = secret {
        form.push(("client_secret", sec.expose()));
    }
    let r = http()?
        .post(&p.meta.token_endpoint)
        .header(header::ACCEPT, "application/json")
        .form(&form)
        .send()
        .await
        .map_err(net)?;
    let t: TokenResponse = read_json(r).await?;
    t.id_token.filter(|t| t.len() <= 16 * 1024).ok_or_else(|| "Провайдер не выдал ID-токен.".to_string())
}

/// Отказ во входе — в журнал аудита. В попытки входа (лимиты подбора пароля) он не пишется: иначе
/// отказы провайдера закрывали бы сотруднику вход паролем.
async fn refuse(core: &Core, actor: &Actor, login: &str, reason: &'static str, external: bool) -> Fail {
    let r = audit::record(
        &core.db,
        actor,
        Entry::new("auth.login_failed", Source::Portal)
            .details(json!({"login": login, "reason": reason, "method": "oidc", "external": external})),
    )
    .await;
    if let Err(e) = r {
        return Fail::Api(e.into());
    }
    Fail::Code(reason)
}

#[allow(clippy::too_many_arguments)]
async fn finish(
    core: &Core,
    s: &OidcSettings,
    sec: &SecuritySettings,
    issuer: &str,
    actor: Actor,
    external: bool,
    claims: &Value,
) -> Result<Response, Fail> {
    let Some((field, login)) = username(claims, s.username_claim) else {
        return Err(refuse(core, &actor, "-", "no_account", external).await);
    };
    let rows: Vec<(Uuid, bool, String)> = sqlx::query_as(
        "SELECT a.id, a.is_enabled, lower(a.sam_account_name)
           FROM ad_accounts a JOIN ad_domains d ON d.id = a.domain_id
          WHERE d.enabled AND NOT a.is_missing
            AND CASE $2 WHEN 'sam' THEN lower(a.sam_account_name) WHEN 'upn' THEN lower(a.upn) ELSE lower(a.mail) END
                = lower($1)
          LIMIT 2",
    )
    .bind(&login)
    .bind(field)
    .fetch_all(&core.db)
    .await?;
    let (account_id, enabled, key) = match rows.as_slice() {
        [one] => one.clone(),
        [] => return Err(refuse(core, &actor, &login, "no_account", external).await),
        // Один логин в двух доменах или общая почта: кого впускать — неизвестно.
        _ => return Err(refuse(core, &actor, &login, "ambiguous", external).await),
    };
    if !enabled {
        return Err(refuse(core, &actor, &key, "account_disabled", external).await);
    }
    let subject = claims["sub"].as_str().unwrap_or_default();
    let mut tx = core.db.begin().await?;
    let user_id = auth::upsert_user(&mut tx, account_id).await?;
    let actor = Actor { user_id: Some(user_id), ..actor };
    // Логин в токене провайдер может дать сменить, `sub` — постоянный. Первый вход привязывает `sub`
    // к пользователю; дальше под этим пользователем входит только он, а этот `sub` — только под ним.
    sqlx::query("INSERT INTO oidc_identities (issuer, subject, user_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
        .bind(issuer)
        .bind(subject)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;
    let bound: Option<Uuid> = sqlx::query_scalar(
        "UPDATE oidc_identities SET last_login_at = now() WHERE issuer = $1 AND subject = $2 RETURNING user_id",
    )
    .bind(issuer)
    .bind(subject)
    .fetch_optional(&mut *tx)
    .await?;
    if bound != Some(user_id) {
        drop(tx);
        tracing::warn!(user = %user_id, "OIDC: учётка привязана к другому пользователю провайдера");
        return Err(refuse(core, &actor, &key, "identity_mismatch", external).await);
    }
    // Кому снаружи нужен код из приложения, тот входит паролем и кодом: провайдер код портала не проверяет.
    if external && auth::user_needs_code(&mut tx, user_id, &sec.totp_required_roles).await? {
        drop(tx);
        return Err(refuse(core, &actor, &key, "otp_required", true).await);
    }
    audit::record(
        &mut *tx,
        &actor,
        Entry::new("auth.login", Source::Portal)
            .object("user", user_id)
            .details(json!({"method": "oidc", "claim": field, "external": external})),
    )
    .await?;
    tx.commit().await?;
    auth::attempt(core, &key, &actor, "ok", external).await;
    let h = auth::session_headers(core, user_id, &actor, AuthMethod::Oidc).await?;
    Ok(redirect("/", with_cookie(h, &cookie(core, "", 0))))
}

// ------------------------------------------------------------------ Админка

fn validate(s: &OidcSettings) -> Result<(), &'static str> {
    if s.button_label.chars().count() > 60 {
        return Err("Надпись на кнопке — не длиннее 60 символов.");
    }
    if !s.enabled {
        return Ok(());
    }
    let issuer = safe_url(issuer_key(&s.issuer), true)?;
    if issuer.query().is_some() {
        return Err("Адрес издателя — без параметров.");
    }
    let id = s.client_id.trim();
    if id.is_empty() || id.len() > 200 || id.chars().any(char::is_control) {
        return Err("Укажите идентификатор клиента.");
    }
    let back = safe_url(&s.redirect_uri, true)?;
    if back.path() != CALLBACK_PATH || back.query().is_some() {
        return Err("Адрес возврата должен заканчиваться на /api/auth/oidc/callback.");
    }
    Ok(())
}

#[derive(Serialize, ToSchema)]
pub struct OidcView {
    pub settings: OidcSettings,
    /// Секрет клиента задан (значение не показывается).
    pub has_secret: bool,
    /// Сколько пользователей привязано к провайдеру (`sub` → пользователь).
    pub identities: i64,
}

#[derive(Deserialize, ToSchema)]
pub struct OidcUpdate {
    pub settings: OidcSettings,
    /// Новый секрет клиента; не передан — прежний; пустая строка — удалить (публичный клиент).
    pub client_secret: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct OidcCheck {
    pub ok: bool,
    pub error: Option<String>,
    /// Сколько ключей подписи опубликовал провайдер.
    pub keys: usize,
}

async fn oidc_view(core: &Core) -> ApiResult<OidcView> {
    let s: OidcSettings = settings::load(&core.db).await?;
    let st: OidcSecret = settings::load(&core.db).await?;
    let identities: i64 = sqlx::query_scalar("SELECT count(*) FROM oidc_identities").fetch_one(&core.db).await?;
    Ok(OidcView { settings: s, has_secret: st.sealed.is_some(), identities })
}

/// Вход через провайдера OpenID Connect. Только суперадминистратор: провайдер решает, под чьей учёткой
/// входят, и свой провайдер дал бы войти под любой.
#[utoipa::path(get, path = "/api/admin/oidc", tag = "admin", responses((status = 200, body = OidcView)))]
pub async fn get_oidc(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<OidcView>> {
    user.require_superadmin()?;
    Ok(Json(oidc_view(&core).await?))
}

/// Сбросить привязки пользователей к провайдеру (например, после переноса учёток в другой провайдер).
#[utoipa::path(delete, path = "/api/admin/oidc/identities", tag = "admin", responses((status = 200, body = OidcView)))]
pub async fn reset_oidc_identities(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<OidcView>> {
    user.require_superadmin()?;
    let mut tx = core.db.begin().await?;
    let n = sqlx::query("DELETE FROM oidc_identities").execute(&mut *tx).await?.rows_affected();
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("settings.update", Source::Portal)
            .object("settings", "oidc")
            .details(json!({"identities_reset": n})),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(oidc_view(&core).await?))
}

#[utoipa::path(put, path = "/api/admin/oidc", tag = "admin", request_body = OidcUpdate,
    responses((status = 200, body = OidcView), (status = 400, body = pult_core::error::ErrorBody)))]
pub async fn put_oidc(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<OidcUpdate>,
) -> ApiResult<Json<OidcView>> {
    user.require_superadmin()?;
    validate(&b.settings).map_err(|m| ApiError::bad_request("invalid_settings", m))?;
    let mut tx = core.db.begin().await?;
    let before: OidcSettings = settings::load(&mut *tx).await?;
    // Сохранённый секрет не уходит другому провайдеру или под другим клиентом.
    let stored: OidcSecret = settings::load(&mut *tx).await?;
    if stored.sealed.is_some() && b.client_secret.is_none() && before.endpoint_changed(&b.settings) {
        return Err(ApiError::bad_request(
            "secret_required",
            "Издатель или клиент изменились — введите секрет клиента заново.",
        ));
    }
    settings::save(&mut *tx, &b.settings, Some(user.user_id)).await?;
    let mut details = audit::diff(&json!(before), &json!(b.settings));
    if let Some(sec) = &b.client_secret {
        let sealed = if sec.is_empty() {
            None
        } else {
            let raw = core.secrets.encrypt(&Secret::new(sec.clone())).map_err(ApiError::internal)?;
            Some(STANDARD.encode(raw))
        };
        settings::save(&mut *tx, &OidcSecret { sealed }, Some(user.user_id)).await?;
        if let Some(o) = details.as_object_mut() {
            o.insert("client_secret".into(), json!(if sec.is_empty() { "удалён" } else { "изменён" }));
        }
    }
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("settings.update", Source::Portal).object("settings", "oidc").details(details),
    )
    .await?;
    tx.commit().await?;
    forget_provider();
    Ok(Json(oidc_view(&core).await?))
}

/// Проверить связь с провайдером: его настройки и ключи подписи.
#[utoipa::path(post, path = "/api/admin/oidc/test", tag = "admin", responses((status = 200, body = OidcCheck)))]
pub async fn test_oidc(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<OidcCheck>> {
    user.require_superadmin()?;
    let s: OidcSettings = settings::load(&core.db).await?;
    if s.issuer.trim().is_empty() {
        let error = pult_core::l10n::tr("Укажите издателя.").into_owned();
        return Ok(Json(OidcCheck { ok: false, error: Some(error), keys: 0 }));
    }
    Ok(Json(match provider(&s.issuer, true).await {
        Ok(p) => OidcCheck { ok: !p.keys.is_empty(), error: None, keys: p.keys.len() },
        Err(e) => OidcCheck { ok: false, error: Some(pult_core::l10n::tr(&e).into_owned()), keys: 0 },
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use ring::{
        rand::SystemRandom,
        signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair},
    };

    fn key() -> (EcdsaKeyPair, Jwk) {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let kp = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng).unwrap();
        let pk = kp.public_key().as_ref();
        let jwk = Jwk {
            kty: "EC".into(),
            kid: Some("k1".into()),
            crv: Some("P-256".into()),
            x: Some(URL_SAFE_NO_PAD.encode(&pk[1..33])),
            y: Some(URL_SAFE_NO_PAD.encode(&pk[33..65])),
            ..Default::default()
        };
        (kp, jwk)
    }

    fn sign(kp: &EcdsaKeyPair, head: &Value, claims: &Value) -> String {
        let msg =
            format!("{}.{}", URL_SAFE_NO_PAD.encode(head.to_string()), URL_SAFE_NO_PAD.encode(claims.to_string()));
        let sig = kp.sign(&SystemRandom::new(), msg.as_bytes()).unwrap();
        format!("{msg}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()))
    }

    const NOW: i64 = 1_800_000_000;

    fn claims() -> Value {
        json!({"iss": "https://sso.test/realms/corp", "aud": "pult", "sub": "u1", "exp": NOW + 300,
               "iat": NOW, "nonce": "n0nce", "preferred_username": "Ivanov"})
    }

    fn ex() -> Expect<'static> {
        Expect { issuer: "https://sso.test/realms/corp", client_id: "pult", nonce: "n0nce", now: NOW }
    }

    #[test]
    fn es256_token() {
        let (kp, jwk) = key();
        let head = json!({"alg": "ES256", "kid": "k1"});
        let keys = [jwk];
        let tok = sign(&kp, &head, &claims());
        assert_eq!(verify_id_token(&tok, &keys, &ex()).unwrap()["sub"], "u1");
        // Чужая подпись, подмена полей, незнакомый ключ, `none`.
        let (other, _) = key();
        assert_eq!(verify_id_token(&sign(&other, &head, &claims()), &keys, &ex()), Err(TokenError::Signature));
        let mut parts: Vec<String> = tok.split('.').map(str::to_string).collect();
        let mut forged = claims();
        forged["preferred_username"] = json!("admin");
        parts[1] = URL_SAFE_NO_PAD.encode(forged.to_string());
        assert_eq!(verify_id_token(&parts.join("."), &keys, &ex()), Err(TokenError::Signature));
        let tok2 = sign(&kp, &json!({"alg": "ES256", "kid": "k2"}), &claims());
        assert_eq!(verify_id_token(&tok2, &keys, &ex()), Err(TokenError::UnknownKey));
        let none = format!("{}.{}.", URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#), parts[1]);
        assert_eq!(verify_id_token(&none, &keys, &ex()), Err(TokenError::Alg));
        let rs = sign(&kp, &json!({"alg": "RS256", "kid": "k1"}), &claims());
        assert_eq!(verify_id_token(&rs, &keys, &ex()), Err(TokenError::UnknownKey), "EC-ключ не для RS256");
        assert_eq!(verify_id_token("a.b", &keys, &ex()), Err(TokenError::Malformed));
    }

    /// Подпись RS256 от openssl (ключ 2048 бит выброшен, остались открытые данные).
    #[test]
    fn rs256_signature() {
        let jwk = Jwk {
            kty: "RSA".into(),
            n: Some("qgTXvoPDk5reasyyfh6t9FVNgwdGFHvYToD-19CUDmfjnVOAQGliqlfT220tg_UCVJ2ydpP_S1StXVztnICMJ1USCV2ZXvcPx6oxIJUUAkUFo_Q0YyNKTrpDDP_QoeEmrEQYh_Y-KRiTFwjsrzrS-WodfJO5eTBGrz5EOxtXDrJpVfDVC0W7PFPta8wTGdwK96L9HBlEfI82HzdD36f2oBNYFz8wZXyz3H8Ws3ksPLSXNucSFj0cfPwvOBDOtEBXFhoW67isel6Xk8mYOMKjUwvcXlMpCDFl28krr-omXF0Rqxdk0zlV5UCgm9zaalKBCKJXag53_kcPGWJSldatKw".into()),
            e: Some("AQAB".into()),
            ..Default::default()
        };
        let msg = b"eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ1MSJ9";
        let sig = URL_SAFE_NO_PAD.decode("FnZe4Or3vpZPQDY7wiiZFMCcNmdFa3bslgRauztj3VLplWnL683tMpBj5GHVhDP7Qos4NXBQUpLh1B3zDyI5kUlAuzHxa1_0CcLuFi25Qpne6Kq6NFANg13M9Bc7Ns4izf7GHDnAHdnZ0IlIyV0GUJRf1RZ3korBvtVd7YDJPzFiqUFOYFxp3F3KRx-KhsdLIovLPRa-9WsNszbRp1CMGZE9jUFNApbBBS6NpSfgxyJfYR9CQYXf1m_uUVPqArV3flfu4mOy6mNbb80k313SN2XuvVGzoO3_t9hoMjjmS_N4JodmLrjQgse_pMwQuEbd0w6rawOMMf6iW9nNbniyuA").unwrap();
        assert!(jwk.fits("RS256") && !jwk.fits("ES256"));
        assert!(jwk.verify("RS256", msg, &sig));
        assert!(!jwk.verify("RS256", b"eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJ1MiJ9", &sig));
        assert!(!Jwk { usage: Some("enc".into()), ..jwk }.fits("RS256"), "ключ шифрования не для подписи");
    }

    #[test]
    fn token_claims() {
        let (kp, jwk) = key();
        let keys = [jwk];
        let head = json!({"alg": "ES256"});
        let with = |k: &str, v: Value| {
            let mut c = claims();
            if v.is_null() {
                c.as_object_mut().unwrap().remove(k);
            } else {
                c[k] = v;
            }
            verify_id_token(&sign(&kp, &head, &c), &keys, &ex())
        };
        assert!(with("aud", json!(["pult"])).is_ok());
        assert!(with("exp", json!(NOW - 30)).is_ok(), "допуск часов");
        for (k, v, why) in [
            ("iss", json!("https://evil.test"), "iss"),
            ("aud", json!("other"), "aud"),
            ("aud", json!(["pult", "other"]), "azp"),
            ("azp", json!("other"), "azp"),
            ("exp", json!(NOW - 120), "exp"),
            ("exp", Value::Null, "exp"),
            ("iat", json!(NOW + 600), "iat"),
            ("nbf", json!(NOW + 600), "nbf"),
            ("nonce", json!("replay"), "nonce"),
            ("nonce", Value::Null, "nonce"),
            ("sub", json!(""), "sub"),
        ] {
            assert_eq!(with(k, v), Err(TokenError::Claim(why)), "{k}");
        }
    }

    #[test]
    fn usernames() {
        let u = |c: Value, k| username(&c, k);
        let p = OidcClaim::PreferredUsername;
        assert_eq!(u(json!({"preferred_username": " Ivanov "}), p), Some(("sam", "ivanov".into())));
        assert_eq!(u(json!({"preferred_username": "CORP\\Ivanov"}), p), Some(("sam", "ivanov".into())));
        assert_eq!(u(json!({"preferred_username": "Ivan@Corp.Test"}), p), Some(("upn", "ivan@corp.test".into())));
        assert_eq!(u(json!({"preferred_username": "CORP\\"}), p), None);
        assert_eq!(u(json!({}), p), None);
        assert_eq!(u(json!({"upn": "ivan@corp.test"}), OidcClaim::Upn), Some(("upn", "ivan@corp.test".into())));
        assert_eq!(u(json!({"upn": "ivan"}), OidcClaim::Upn), None);
        let e = OidcClaim::Email;
        assert_eq!(u(json!({"email": "a@b.c"}), e), None, "неподтверждённая почта");
        assert_eq!(u(json!({"email": "a@b.c", "email_verified": false}), e), None);
        assert_eq!(u(json!({"email": "A@b.c", "email_verified": true}), e), Some(("mail", "a@b.c".into())));
        assert_eq!(u(json!({"email": "a@b.c", "email_verified": "true"}), e), Some(("mail", "a@b.c".into())));
    }

    #[test]
    fn urls() {
        assert!(safe_url("https://sso.test/realms/corp", false).is_ok());
        assert!(safe_url("http://sso.test", true).is_err(), "HTTP — только на этот же сервер");
        assert!(safe_url("http://127.0.0.1:8080", true).is_ok());
        assert!(safe_url("http://127.0.0.1:8080", false).is_err());
        assert!(safe_url("https://u:p@sso.test", false).is_err());
        assert!(safe_url("ftp://sso.test", true).is_err());
        let ok = OidcSettings {
            enabled: true,
            issuer: "https://sso.test/realms/corp/".into(),
            client_id: "pult".into(),
            redirect_uri: "https://pult.test/api/auth/oidc/callback".into(),
            ..Default::default()
        };
        assert_eq!(validate(&ok), Ok(()));
        assert!(validate(&OidcSettings { redirect_uri: "https://pult.test/".into(), ..ok.clone() }).is_err());
        assert!(validate(&OidcSettings { client_id: " ".into(), ..ok.clone() }).is_err());
        assert!(validate(&OidcSettings { issuer: "https://sso.test/?a=1".into(), ..ok.clone() }).is_err());
        assert_eq!(validate(&OidcSettings { enabled: false, issuer: "x".into(), ..ok }), Ok(()), "выключено");
    }
}
