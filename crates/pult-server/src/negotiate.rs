//! Вход через домен без пароля (SPNEGO/Kerberos).
//!
//! Браузер на доменном ПК получает у KDC билет на `HTTP/<имя портала>` и присылает его в
//! `Authorization: Negotiate <base64>`. Сервер проверяет билет ключом из keytab (`KRB5_KTNAME`)
//! средствами MIT GSSAPI (`libgssapi`) — своего разбора Kerberos нет. Каждый запрос — отдельный
//! контекст: Kerberos укладывается в один обмен, состояние между запросами не нужно.
//!
//! Только Kerberos: NTLM (его шлёт браузер без билета — сайт не в зоне «Местная интрасеть» или
//! открыт по IP) не принимается и не просится повторно. Сессию по результату создаёт вход; этот
//! маршрут отвечает, кого распознал.
//!
//! GSSAPI есть только в сборке для Linux; на Windows маршрут отвечает 501.

use axum::{
    Json,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;

/// Кто вошёл.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct NegotiateResult {
    /// Принципал Kerberos, например `ivanov@CORP.EXAMPLE.LOCAL`.
    pub principal: String,
}

/// Причина отказа — для экрана; подробности — только в журнале сервера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// Заголовка нет — сервер просит билет (`WWW-Authenticate: Negotiate`).
    NegotiateRequired,
    /// Браузер прислал NTLM — билета Kerberos у него нет.
    NtlmNotSupported,
    BadToken,
    /// Билет не прошёл проверку (чужой ключ, истёк, повтор).
    #[cfg_attr(not(unix), allow(dead_code))] // создаётся только в сборке с GSSAPI
    Rejected,
    /// Сборка без GSSAPI (Windows).
    #[cfg_attr(unix, allow(dead_code))] // создаётся только в сборке без GSSAPI
    NotAvailable,
    /// Слишком много проверок сразу — повторить позже.
    Busy,
}

/// Одновременных проверок билета: GSSAPI блокирует поток (keytab, кеш повторов), а вход — без пароля,
/// поэтому поток запросов не должен занять все блокирующие потоки сервера.
const CHECKS_AT_ONCE: usize = 4;
static CHECKS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(CHECKS_AT_ONCE);

/// Ответ при отказе — без подробностей наружу, причина — в журнале.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct NegotiateError {
    pub error: DenyReason,
}

/// Что прислал браузер.
#[derive(Debug, PartialEq, Eq)]
pub enum Incoming {
    /// Заголовка нет — нужно предложить Negotiate.
    None,
    Token(Vec<u8>),
    /// Токен NTLM — у браузера нет билета Kerberos.
    Ntlm,
    Bad,
}

const NTLMSSP: &[u8] = b"NTLMSSP\0";
/// Не больше 64 КБ: билет с группами AD — единицы КБ.
const MAX_TOKEN: usize = 64 * 1024;

pub fn parse_authorization(h: Option<&HeaderValue>) -> Incoming {
    let Some(v) = h.and_then(|v| v.to_str().ok()) else { return Incoming::None };
    let Some((scheme, rest)) = v.trim().split_once(' ') else { return Incoming::Bad };
    if !scheme.eq_ignore_ascii_case("negotiate") {
        return if scheme.eq_ignore_ascii_case("ntlm") { Incoming::Ntlm } else { Incoming::None };
    }
    let rest = rest.trim();
    if rest.len() > MAX_TOKEN * 4 / 3 + 4 {
        return Incoming::Bad;
    }
    match STANDARD.decode(rest) {
        Ok(tok) if tok.is_empty() => Incoming::Bad,
        // NTLM в обёртке SPNEGO или без неё — внутри всегда есть сигнатура NTLMSSP.
        Ok(tok) if tok.windows(NTLMSSP.len()).any(|w| w == NTLMSSP) => Incoming::Ntlm,
        Ok(tok) => Incoming::Token(tok),
        Err(_) => Incoming::Bad,
    }
}

fn deny(status: StatusCode, error: DenyReason, challenge: bool) -> Response {
    let mut resp = (status, Json(NegotiateError { error })).into_response();
    if challenge {
        resp.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Negotiate"));
    }
    resp
}

/// Проверка входа через домен.
#[utoipa::path(get, path = "/api/auth/negotiate", tag = "auth-prototype",
    responses(
        (status = 200, description = "Билет Kerberos принят", body = NegotiateResult),
        (status = 401, description = "Нужен билет Kerberos (WWW-Authenticate: Negotiate) или он отклонён", body = NegotiateError),
        (status = 429, description = "Слишком много проверок сразу", body = NegotiateError),
        (status = 501, description = "Сборка без GSSAPI (Windows)", body = NegotiateError)))]
pub async fn negotiate(headers: HeaderMap) -> Response {
    match parse_authorization(headers.get(header::AUTHORIZATION)) {
        Incoming::None => deny(StatusCode::UNAUTHORIZED, DenyReason::NegotiateRequired, true),
        Incoming::Ntlm => {
            tracing::warn!(
                "вход через домен: браузер прислал NTLM вместо Kerberos — адрес не в зоне «Местная интрасеть» или открыт по IP"
            );
            // Без повторного вызова: иначе браузер будет крутить NTLM по кругу.
            deny(StatusCode::UNAUTHORIZED, DenyReason::NtlmNotSupported, false)
        }
        Incoming::Bad => deny(StatusCode::BAD_REQUEST, DenyReason::BadToken, false),
        Incoming::Token(tok) => match CHECKS.try_acquire() {
            Ok(_slot) => accept(tok).await,
            Err(_) => deny(StatusCode::TOO_MANY_REQUESTS, DenyReason::Busy, false),
        },
    }
}

#[cfg(unix)]
async fn accept(tok: Vec<u8>) -> Response {
    use libgssapi::context::{SecurityContext, ServerCtx};
    // GSSAPI блокирует (чтение keytab, кеш повторов) — не на рабочем потоке tokio.
    let res = tokio::task::spawn_blocking(move || {
        let mut ctx = ServerCtx::new(None);
        let out = ctx.step(&tok, None)?;
        let principal = if ctx.is_complete() { Some(ctx.source_name()?.to_string()) } else { None };
        Ok::<_, libgssapi::error::Error>((out.map(|b| STANDARD.encode(&*b)), principal))
    })
    .await;
    match res {
        Ok(Ok((out, Some(principal)))) => {
            tracing::info!(%principal, "вход через домен: билет Kerberos принят");
            let mut resp = Json(NegotiateResult { principal }).into_response();
            // Взаимная аутентификация: браузер проверяет, что ответил настоящий сервер.
            if let Some(v) = out.and_then(|t| HeaderValue::from_str(&format!("Negotiate {t}")).ok()) {
                resp.headers_mut().insert(header::WWW_AUTHENTICATE, v);
            }
            resp
        }
        Ok(Ok((_, None))) => {
            tracing::warn!("вход через домен: обмен Kerberos не завершился за один шаг");
            deny(StatusCode::UNAUTHORIZED, DenyReason::Rejected, false)
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "вход через домен: билет отклонён");
            deny(StatusCode::UNAUTHORIZED, DenyReason::Rejected, false)
        }
        Err(e) => {
            tracing::error!(error = %e, "вход через домен: сбой проверки");
            deny(StatusCode::INTERNAL_SERVER_ERROR, DenyReason::Rejected, false)
        }
    }
}

#[cfg(not(unix))]
async fn accept(_tok: Vec<u8>) -> Response {
    deny(StatusCode::NOT_IMPLEMENTED, DenyReason::NotAvailable, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(v: &str) -> HeaderValue {
        HeaderValue::from_str(v).unwrap()
    }

    #[test]
    fn parses_negotiate_and_rejects_the_rest() {
        assert_eq!(parse_authorization(None), Incoming::None);
        assert_eq!(parse_authorization(Some(&h("Basic dXNlcjpwYXNz"))), Incoming::None);
        assert_eq!(parse_authorization(Some(&h("Negotiate YWJj"))), Incoming::Token(b"abc".to_vec()));
        assert_eq!(parse_authorization(Some(&h("negotiate  YWJj "))), Incoming::Token(b"abc".to_vec()));
        assert_eq!(parse_authorization(Some(&h("Negotiate !!!"))), Incoming::Bad);
        assert_eq!(parse_authorization(Some(&h("Negotiate"))), Incoming::Bad);
        assert_eq!(parse_authorization(Some(&h("Negotiate "))), Incoming::Bad);
    }

    #[test]
    fn detects_ntlm_raw_and_wrapped() {
        let raw = STANDARD.encode(b"NTLMSSP\0\x01\0\0\0");
        assert_eq!(parse_authorization(Some(&h(&format!("Negotiate {raw}")))), Incoming::Ntlm);
        let wrapped = STANDARD.encode([b"\x60\x28\x06\x06+\x06\x01\x05\x05\x02".as_slice(), b"NTLMSSP\0\x01"].concat());
        assert_eq!(parse_authorization(Some(&h(&format!("Negotiate {wrapped}")))), Incoming::Ntlm);
        assert_eq!(parse_authorization(Some(&h("NTLM TlRMTVNTUAAB"))), Incoming::Ntlm);
    }

    #[test]
    fn oversized_token_is_rejected() {
        let big = STANDARD.encode(vec![0u8; MAX_TOKEN + 1024]);
        assert_eq!(parse_authorization(Some(&h(&format!("Negotiate {big}")))), Incoming::Bad);
    }

    #[tokio::test]
    async fn no_header_gets_negotiate_challenge() {
        let r = negotiate(HeaderMap::new()).await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(r.headers().get(header::WWW_AUTHENTICATE).unwrap(), "Negotiate");
    }

    #[tokio::test]
    async fn ntlm_is_refused_without_new_challenge() {
        let mut hm = HeaderMap::new();
        hm.insert(header::AUTHORIZATION, h("NTLM TlRMTVNTUAAB"));
        let r = negotiate(hm).await;
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
        assert!(r.headers().get(header::WWW_AUTHENTICATE).is_none(), "иначе браузер зациклит NTLM");
    }
}
