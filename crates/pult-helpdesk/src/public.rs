//! Без входа: «Не могу войти» и ссылки «Да / Нет» из письма.

use crate::{
    HelpdeskState,
    settings::HelpdeskSettings,
    tickets::{self, NewTicket, Who},
};
use axum::{
    Extension, Json,
    extract::{ConnectInfo, FromRef, Path, State},
    http::StatusCode,
};
use pult_core::l10n::tr;
use pult_core::{ApiError, ApiResult, Core, audit::Actor, audit::Source, session::UnverifiedForwarding, settings};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::types::ipnetwork::IpNetwork;
use std::net::{IpAddr, SocketAddr};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
    HelpdeskState: FromRef<S>,
{
    OpenApiRouter::new().routes(routes!(login_help)).routes(routes!(token_info, token_respond))
}

pub const LOGIN_HELP_PER_HOUR: i64 = 3;

fn allowed(ip: IpAddr, subnets: &[String]) -> bool {
    subnets.iter().filter_map(|s| s.parse::<IpNetwork>().ok()).any(|n| n.contains(ip))
}

#[derive(Deserialize, ToSchema)]
pub struct LoginHelpInput {
    pub name: String,
    pub phone: String,
    #[serde(default)]
    pub room: String,
    /// `forgot`, `locked`, `expired` или `other`.
    pub issue: String,
    #[serde(default)]
    pub comment: String,
}

#[derive(Serialize, ToSchema)]
pub struct Accepted {
    pub accepted: bool,
}

fn issue_title(issue: &str) -> &'static str {
    match issue {
        "forgot" => "Забыл пароль",
        "locked" => "Учётная запись заблокирована",
        "expired" => "Истёк срок пароля",
        _ => "Не могу войти",
    }
}

/// «Не могу войти». Ответ одинаковый для любых ФИО: форма ничего не говорит о справочнике.
#[utoipa::path(post, path = "/api/public/login-help", tag = "public", request_body = LoginHelpInput,
    responses((status = 202, body = Accepted), (status = 403, body = pult_core::error::ErrorBody), (status = 429, body = pult_core::error::ErrorBody)))]
async fn login_help(
    State(core): State<Core>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    unverified: Option<Extension<UnverifiedForwarding>>,
    Json(b): Json<LoginHelpInput>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    let ip = addr.ip();
    let cfg: HelpdeskSettings = settings::load(&core.db).await?;
    // Через прокси без доверия (PULT_TRUSTED_PROXIES) виден адрес прокси, а не клиента: такой запрос
    // «из сети» не считается — иначе форму из интернета открывал бы прокси на этом же сервере.
    if unverified.is_some() || !allowed(ip, &cfg.login_help_subnets) {
        return Err(ApiError::Forbidden);
    }
    let (name, phone) = (b.name.trim(), b.phone.trim());
    if name.is_empty() || phone.is_empty() {
        return Err(ApiError::bad_request("invalid_form", "Укажите ФИО и телефон."));
    }
    let recent: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM tickets WHERE channel = 'login_help' AND created_at > now() - interval '1 hour' AND unverified_contact->>'ip' = $1",
    )
    .bind(ip.to_string())
    .fetch_one(&core.db)
    .await?;
    if recent >= LOGIN_HELP_PER_HOUR {
        return Err(ApiError::TooManyRequests {
            message: "Слишком много обращений. Позвоните в IT-службу.".into(),
            retry_after_secs: 3600,
        });
    }
    let category: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT id FROM ticket_categories WHERE code = 'account' AND is_active")
            .fetch_optional(&core.db)
            .await?;
    let clip = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    let who = Who { actor: Actor { ip: Some(ip), ..Actor::system() }, source: Source::Portal, ..Who::system() };
    tickets::create(
        &core,
        &who,
        NewTicket {
            title: format!("{}: {}", tr(issue_title(&b.issue)), clip(name, 100)),
            description: clip(b.comment.trim(), 2000),
            category_id: category,
            unverified_contact: Some(json!({
                "name": clip(name, 200), "phone": clip(phone, 50), "room": clip(b.room.trim(), 50),
                "issue": clip(&b.issue, 20), "ip": ip.to_string(),
            })),
            channel: "login_help".into(),
            contact_phone: Some(clip(phone, 50)),
            contact_room: Some(clip(b.room.trim(), 50)),
            form_answers: json!({}),
            ..Default::default()
        },
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(Accepted { accepted: true })))
}

#[derive(Serialize, ToSchema)]
pub struct TokenInfo {
    pub valid: bool,
    pub number: Option<i64>,
    pub title: Option<String>,
}

/// Что за ссылка. Ничего не меняет: почтовые сканеры открывают ссылки сами.
#[utoipa::path(get, path = "/api/public/ticket-response/{token}", tag = "public", params(("token" = String, Path)),
    responses((status = 200, body = TokenInfo)))]
async fn token_info(State(core): State<Core>, Path(token): Path<String>) -> ApiResult<Json<TokenInfo>> {
    let mut tx = core.db.begin().await?;
    let found = tickets::lock_token(&mut tx, &token).await?;
    Ok(Json(match found {
        Some((t, _)) => TokenInfo { valid: true, number: Some(t.number), title: Some(t.title) },
        None => TokenInfo { valid: false, number: None, title: None },
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct TokenResponse {
    pub accepted: bool,
    #[serde(default)]
    pub comment: String,
}

/// «Да, решена» / «Нет, не решена» по ссылке из письма — без входа, один раз.
#[utoipa::path(post, path = "/api/public/ticket-response/{token}", tag = "public", params(("token" = String, Path)),
    request_body = TokenResponse, responses((status = 204), (status = 400, body = pult_core::error::ErrorBody)))]
async fn token_respond(
    State(core): State<Core>,
    Path(token): Path<String>,
    Json(b): Json<TokenResponse>,
) -> ApiResult<StatusCode> {
    tickets::respond_by_token(&core, &token, b.accepted, &b.comment).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnets() {
        let s = vec!["10.0.0.0/8".to_string(), "::1/128".to_string()];
        assert!(allowed("10.1.2.3".parse().unwrap(), &s));
        assert!(allowed("::1".parse().unwrap(), &s));
        assert!(!allowed("192.0.2.1".parse().unwrap(), &s));
        assert!(!allowed("10.1.2.3".parse().unwrap(), &[]), "пустой список — форма закрыта");
    }
}
