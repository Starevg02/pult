//! Подключение инженера: портал решает всё (права, политика, сессия), helper только исполняет.
//!
//! 1. `POST /api/remote/connections` — проверка и запись подключения, одноразовый токен на 60 с,
//!    ссылка `pult://connect?t=…`. Отказ по политике тоже пишется в аудит.
//! 2. `POST /api/remote/v1/exchange` — helper меняет токен на параметры (один раз).
//! 3. `POST /api/remote/v1/result` — helper сообщает итог (тем же токеном, в течение 10 минут).
//!
//! Helper не входит в портал: токен — единственное, что он предъявляет. Украденный токен даёт
//! лишь имя ПК и номер сессии — само подключение идёт под учёткой Windows инженера и по правам
//! теневого подключения на ПК.

use crate::{
    RemoteSettings,
    agent::{Peer, valid_fqdn, valid_hostname},
    perm,
    policy::{self, Ask, Refusal, Rights, Rule},
    tokens,
};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};
use pult_api_types::{ConnectParams, ConnectResult, ExchangeRequest, RemoteAction, ShadowMode};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser,
    audit::{self, Actor, Entry, Source},
    settings,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

/// Срок одноразового токена подключения.
pub const TOKEN_SECS: i64 = 60;
/// Сколько после обмена токена helper может сообщить итог.
pub const RESULT_MINUTES: i64 = 10;

#[derive(Debug, Deserialize, ToSchema)]
pub struct NewConnection {
    /// ПК; без него — ПК из заявки.
    pub device_id: Option<Uuid>,
    /// Заявка: ПК и сессия заявителя берутся из неё и данных агента.
    pub ticket_number: Option<i64>,
    pub action: RemoteAction,
    /// Сессия для `shadow`; без неё — сессия заявителя.
    pub session_id: Option<u32>,
    pub mode: Option<ShadowMode>,
    /// Без запроса согласия (только управление и только если политика ПК позволяет).
    #[serde(default)]
    pub skip_consent: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct IssuedConnection {
    pub id: Uuid,
    /// Открыть в браузере — запустится helper.
    pub link: String,
    pub expires_in_secs: i64,
    pub action: RemoteAction,
    pub hostname: String,
    pub computer: String,
    pub session_id: Option<u32>,
    pub target_user: Option<String>,
    pub mode: Option<ShadowMode>,
    /// `prompted` | `skipped`.
    pub consent: Option<String>,
}

type DeviceTicket = (Uuid, Option<Uuid>, Option<Uuid>);
type DeviceRow = (String, Option<String>, bool, bool);

fn mode_str(m: ShadowMode) -> &'static str {
    match m {
        ShadowMode::View => "view",
        ShadowMode::Control => "control",
    }
}

fn parse_mode(s: Option<&str>) -> Option<ShadowMode> {
    match s {
        Some("view") => Some(ShadowMode::View),
        Some("control") => Some(ShadowMode::Control),
        _ => None,
    }
}

fn parse_action(s: &str) -> RemoteAction {
    match s {
        "files" => RemoteAction::Files,
        "manage" => RemoteAction::Manage,
        "powershell" => RemoteAction::Powershell,
        "probe" => RemoteAction::Probe,
        _ => RemoteAction::Shadow,
    }
}

/// Правила политики для ПК.
pub async fn rules(core: &Core, hostname: &str, fqdn: Option<&str>) -> ApiResult<Vec<Rule>> {
    let all: Vec<(String, String)> =
        sqlx::query_as("SELECT device_pattern, rule FROM remote_policies").fetch_all(&core.db).await?;
    let parsed: Vec<(&str, Rule)> = all.iter().filter_map(|(p, r)| Rule::parse(r).map(|r| (p.as_str(), r))).collect();
    Ok(policy::rules_for(parsed, hostname, fqdn))
}

/// Подключиться к ПК: проверка прав и политики, одноразовая ссылка для helper.
#[utoipa::path(post, path = "/api/remote/connections", tag = "remote", request_body = NewConnection,
    responses((status = 201, body = IssuedConnection), (status = 400, description = "ПК не на связи, нет сессии…"),
              (status = 403, description = "Нет права или ПК запрещён политикой (`device_denied`)")))]
pub async fn create_connection(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<NewConnection>,
) -> ApiResult<(StatusCode, Json<IssuedConnection>)> {
    let rights =
        Rights { view: user.grants.has(&perm::CONNECT_VIEW), control: user.grants.has(&perm::CONNECT_CONTROL) };
    if !rights.view && !rights.control {
        return Err(ApiError::Forbidden);
    }
    let ticket: Option<(i64, DeviceTicket)> = match b.ticket_number {
        Some(n) => {
            user.require(&pult_helpdesk::perm::WORK)?;
            let row: DeviceTicket = sqlx::query_as(
                // Учётка заявителя: из справочника, а если карточки нет — его пользователя Pult.
                "SELECT t.id, t.device_id, coalesce(p.ad_account_id, ru.ad_account_id)
                   FROM tickets t LEFT JOIN persons p ON p.id = t.requester_id
                   LEFT JOIN users ru ON ru.id = t.requester_user_id
                  WHERE t.number = $1",
            )
            .bind(n)
            .fetch_optional(&core.db)
            .await?
            .ok_or(ApiError::NotFound)?;
            Some((n, row))
        }
        None => None,
    };
    let device_id = match (b.device_id, &ticket) {
        (Some(d), _) => Some(d),
        // Только устройство, с которым заявку связали при создании или инженер, — не поиск по
        // имени из поля «Компьютер»: его мог вписать сам заявитель.
        (None, Some((_, (_, d, _)))) => *d,
        _ => None,
    }
    .ok_or_else(|| ApiError::bad_request("no_device", "У заявки не указан компьютер с агентом Pult."))?;
    let cfg: RemoteSettings = settings::load(&core.db).await?;
    let (hostname, domain, is_active, online): DeviceRow = sqlx::query_as(
        "SELECT d.hostname, dom.dns_name, d.is_active,
                coalesce(d.last_seen_at > now() - make_interval(mins => $2), false)
           FROM devices d LEFT JOIN ad_computers c ON c.id = d.ad_computer_id
           LEFT JOIN ad_domains dom ON dom.id = c.domain_id
          WHERE d.id = $1",
    )
    .bind(device_id)
    .bind(cfg.online_minutes.clamp(1, 60) as i32)
    .fetch_optional(&core.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    if !is_active {
        return Err(ApiError::bad_request("device_inactive", "Агент на этом ПК отключён администратором."));
    }
    let ticket_n = ticket.as_ref().map(|(n, _)| *n);
    // Политики и цель подключения — по имени из AD и домену, а не по данным агента.
    let computer = policy::connect_target(&hostname, domain.as_deref());
    let rules = rules(&core, &hostname, Some(&computer)).await?;
    let ask = Ask { action: b.action, mode: b.mode, skip_consent: b.skip_consent };
    let allowed = match policy::decide(&rules, rights, ask) {
        Ok(a) => a,
        Err(why) => {
            let reason = match why {
                Refusal::NoPermission => "no_permission",
                Refusal::DeviceDenied => "device_denied",
                Refusal::ConsentOnly => "consent_only",
            };
            audit::record(
                &core.db,
                &user.actor,
                Entry::new("remote.connect.denied", Source::Portal).object("device", device_id).details(json!({
                    "hostname": hostname, "action": b.action.as_str(), "mode": b.mode.map(mode_str),
                    "skip_consent": b.skip_consent, "reason": reason, "ticket": ticket_n,
                })),
            )
            .await?;
            return Err(match why {
                Refusal::NoPermission => ApiError::Forbidden,
                Refusal::DeviceDenied => ApiError::Refused {
                    code: "device_denied",
                    message: "Подключение к этому ПК запрещено политикой удалённой поддержки.".into(),
                },
                Refusal::ConsentOnly => ApiError::Refused {
                    code: "consent_only",
                    message: "На этом ПК разрешено только теневое подключение с согласием пользователя.".into(),
                },
            });
        }
    };
    // Сессия: явно выбранная или сессия заявителя по данным агента. Нужны свежие данные.
    let (session, target_user, target_account): (Option<i32>, Option<String>, Option<Uuid>) =
        if b.action == RemoteAction::Shadow {
            if !online {
                return Err(ApiError::bad_request(
                    "device_offline",
                    "ПК не на связи: агент давно не присылал данные. Проверьте, включён ли компьютер.",
                ));
            }
            let row: Option<(i32, String, Option<Uuid>)> = match (b.session_id, &ticket) {
                (Some(s), _) => {
                    sqlx::query_as(
                        "SELECT session_id, username, account_id FROM device_sessions
                          WHERE device_id = $1 AND session_id = $2",
                    )
                    .bind(device_id)
                    .bind(i32::try_from(s).unwrap_or(-1))
                    .fetch_optional(&core.db)
                    .await?
                }
                (None, Some((_, (_, _, Some(account))))) => {
                    let r = sqlx::query_as(
                        "SELECT session_id, username, account_id FROM device_sessions
                          WHERE device_id = $1 AND account_id = $2
                          ORDER BY (state = 'active') DESC, updated_at DESC LIMIT 1",
                    )
                    .bind(device_id)
                    .bind(account)
                    .fetch_optional(&core.db)
                    .await?;
                    if r.is_none() {
                        return Err(ApiError::bad_request(
                            "requester_not_logged_on",
                            "Заявитель сейчас не работает на этом ПК (по данным агента).",
                        ));
                    }
                    r
                }
                _ => return Err(ApiError::bad_request("session_required", "Выберите сессию пользователя.")),
            };
            let (s, u, a) = row.ok_or_else(|| {
                ApiError::bad_request("session_not_found", "Этой сессии на ПК уже нет — обновите список сессий.")
            })?;
            (Some(s), Some(u), a)
        } else {
            (None, None, None)
        };
    // Сессия выбрана инженером из списка — в аудите видно, заявителя ли она.
    let requester_session = match &ticket {
        Some((_, (_, _, Some(requester)))) if session.is_some() => Some(target_account == Some(*requester)),
        _ => None,
    };
    if !(valid_hostname(&computer) || valid_fqdn(&computer)) {
        tracing::error!(%hostname, "недопустимое имя ПК в базе");
        return Err(ApiError::internal("invalid computer name"));
    }
    let (token, token_hash) = tokens::issue(tokens::CONNECT_PREFIX)?;
    let mode = allowed.mode.map(mode_str);
    let consent = allowed.consent.map(|c| c.as_str());
    let mut tx = core.db.begin().await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO remote_connections (operator_id, device_id, action, target_session_id, target_user,
                                         target_account_id, mode, consent, ticket_id, computer, token_hash)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
    )
    .bind(user.user_id)
    .bind(device_id)
    .bind(b.action.as_str())
    .bind(session)
    .bind(&target_user)
    .bind(target_account)
    .bind(mode)
    .bind(consent)
    .bind(ticket.as_ref().map(|(_, t)| t.0))
    .bind(&computer)
    .bind(&token_hash)
    .fetch_one(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("remote.connect.requested", Source::Portal).object("remote_connection", id).details(json!({
            "hostname": hostname, "action": b.action.as_str(), "session": session, "user": target_user,
            "mode": mode, "consent": consent, "ticket": ticket_n,
            "requester_session": requester_session,
        })),
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(IssuedConnection {
            id,
            link: format!("pult://connect?t={token}"),
            expires_in_secs: TOKEN_SECS,
            action: b.action,
            hostname,
            computer,
            session_id: session.map(|s| s as u32),
            target_user,
            mode: allowed.mode,
            consent: consent.map(str::to_string),
        }),
    ))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ConnectionView {
    pub id: Uuid,
    /// `issued` (ждём helper), `expired` (helper не забрал ссылку), `opened`, `started`, `failed`.
    pub status: String,
    pub action: RemoteAction,
    pub hostname: String,
    pub target_user: Option<String>,
    pub mode: Option<ShadowMode>,
    pub consent: Option<String>,
    pub error: Option<String>,
    #[schema(value_type = Option<Object>)]
    pub result: Option<Value>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

type ConnRow = (
    Uuid,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Value>,
    OffsetDateTime,
    Option<OffsetDateTime>,
    Uuid,
);

/// Состояние подключения — для того, кто его запросил (и администратора).
#[utoipa::path(get, path = "/api/remote/connections/{id}", tag = "remote", params(("id" = Uuid, Path)),
    responses((status = 200, body = ConnectionView), (status = 404, description = "Нет такого")))]
pub async fn get_connection(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ConnectionView>> {
    let r: ConnRow = sqlx::query_as(
        "SELECT c.id, CASE WHEN c.status = 'issued' AND c.created_at < now() - make_interval(secs => $2)
                           THEN 'expired' ELSE c.status END,
                c.action, d.hostname, c.target_user, c.mode, c.consent, c.error, c.result, c.created_at,
                c.finished_at, c.operator_id
           FROM remote_connections c JOIN devices d ON d.id = c.device_id WHERE c.id = $1",
    )
    .bind(id)
    .bind(TOKEN_SECS as f64)
    .fetch_optional(&core.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    if r.11 != user.user_id && !user.grants.has(&perm::MANAGE) {
        return Err(ApiError::NotFound);
    }
    Ok(Json(ConnectionView {
        id: r.0,
        status: r.1,
        action: parse_action(&r.2),
        hostname: r.3,
        target_user: r.4,
        mode: parse_mode(r.5.as_deref()),
        consent: r.6,
        error: r.7,
        result: r.8,
        created_at: r.9,
        finished_at: r.10,
    }))
}

fn invalid_token() -> ApiError {
    ApiError::bad_request(
        "token_invalid",
        "Ссылка подключения недействительна или устарела — нажмите «Подключиться» в портале ещё раз.",
    )
}

type OpenedRow =
    (Uuid, Uuid, String, String, Option<i32>, Option<String>, Option<String>, Option<String>, String, Option<i64>);

/// Helper меняет одноразовый токен на параметры подключения.
#[utoipa::path(post, path = "/api/remote/v1/exchange", tag = "remote-helper", request_body = ExchangeRequest,
    responses((status = 200, body = ConnectParams), (status = 400, description = "Токен неверен, использован или истёк")))]
pub async fn exchange(
    State(core): State<Core>,
    Peer(actor): Peer,
    Json(b): Json<ExchangeRequest>,
) -> ApiResult<Json<ConnectParams>> {
    let token = b.token.trim();
    if !tokens::well_formed(token, tokens::CONNECT_PREFIX) {
        return Err(invalid_token());
    }
    let mut tx = core.db.begin().await?;
    let r: OpenedRow = sqlx::query_as(
        "UPDATE remote_connections c SET status = 'opened', opened_at = now()
           FROM devices d
          WHERE d.id = c.device_id AND c.token_hash = $1 AND c.status = 'issued'
            AND c.created_at > now() - make_interval(secs => $2)
            AND EXISTS (SELECT 1 FROM users u WHERE u.id = c.operator_id AND u.is_active)
         RETURNING c.id, c.operator_id, c.action, c.computer, c.target_session_id, c.mode, c.consent, c.target_user,
                   d.hostname, (SELECT t.number FROM tickets t WHERE t.id = c.ticket_id)",
    )
    .bind(tokens::hash(token))
    .bind(TOKEN_SECS as f64)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(invalid_token)?;
    let (id, operator, action, computer, session, mode, consent, target_user, hostname, ticket) = r;
    let helper = Actor { user_id: Some(operator), ..actor };
    audit::record(
        &mut *tx,
        &helper,
        Entry::new("remote.connect.opened", Source::Api).object("remote_connection", id).details(json!({
            "hostname": hostname, "action": action, "session": session, "user": target_user,
            "mode": mode, "consent": consent, "ticket": ticket,
        })),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(ConnectParams {
        connection_id: id.to_string(),
        action: parse_action(&action),
        computer,
        session_id: session.map(|s| s as u32),
        mode: parse_mode(mode.as_deref()),
        // Без явного «skipped» согласие запрашивается: по умолчанию — безопасный вариант.
        consent_required: consent.as_deref() != Some("skipped"),
        target_user,
    }))
}

type FinishedRow =
    (Uuid, Uuid, String, Option<Uuid>, String, Option<i32>, Option<String>, Option<String>, Option<String>);

/// Итог действия helper: подключение запущено или ошибка.
#[utoipa::path(post, path = "/api/remote/v1/result", tag = "remote-helper", request_body = ConnectResult,
    responses((status = 204, description = "Принято"), (status = 400, description = "Токен неверен или итог уже сообщён")))]
pub async fn result(
    State(core): State<Core>,
    Peer(actor): Peer,
    Json(b): Json<ConnectResult>,
) -> ApiResult<StatusCode> {
    let token = b.token.trim();
    if !tokens::well_formed(token, tokens::CONNECT_PREFIX) {
        return Err(invalid_token());
    }
    let error: Option<String> = b.error.as_deref().map(|e| e.chars().filter(|c| !c.is_control()).take(1000).collect());
    let details = b.details.filter(|d| d.is_object() && d.to_string().len() <= 4096);
    let mut tx = core.db.begin().await?;
    let r: FinishedRow = sqlx::query_as(
        "UPDATE remote_connections c
            SET status = CASE WHEN $2 THEN 'started' ELSE 'failed' END, error = $3, result = $4, finished_at = now()
           FROM devices d
          WHERE d.id = c.device_id AND c.token_hash = $1 AND c.status = 'opened'
            AND c.opened_at > now() - make_interval(mins => $5)
         RETURNING c.id, c.operator_id, c.action, c.ticket_id, d.hostname, c.target_session_id, c.target_user,
                   c.mode, c.consent",
    )
    .bind(tokens::hash(token))
    .bind(b.ok)
    .bind(&error)
    .bind(&details)
    .bind(RESULT_MINUTES as i32)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(invalid_token)?;
    let (id, operator, action, ticket, hostname, session, target_user, mode, consent) = r;
    let ticket_n: Option<i64> = match ticket {
        Some(t) => {
            sqlx::query_scalar("SELECT number FROM tickets WHERE id = $1").bind(t).fetch_optional(&mut *tx).await?
        }
        None => None,
    };
    let helper = Actor { user_id: Some(operator), ..actor };
    audit::record(
        &mut *tx,
        &helper,
        Entry::new(if b.ok { "remote.connect.started" } else { "remote.connect.failed" }, Source::Api)
            .object("remote_connection", id)
            .details(json!({
                "hostname": hostname, "action": action, "session": session, "user": target_user, "mode": mode,
                "consent": consent, "ticket": ticket_n, "error": error, "result": details, "reported_by": "helper",
            })),
    )
    .await?;
    // В ленту заявки — внутренняя заметка (видят инженеры). Итог сообщает helper инженера:
    // он знает, что запустил подключение, но не знает, принял ли пользователь запрос.
    if let (Some(t), true, "shadow") = (ticket, b.ok, action.as_str()) {
        let how = match (mode.as_deref(), consent.as_deref()) {
            (Some("control"), Some("skipped")) => "управление без запроса согласия",
            (Some("control"), _) => "управление, пользователю показан запрос согласия",
            _ => "просмотр, пользователю показан запрос согласия",
        };
        let body = format!(
            "Helper инженера запустил подключение к ПК {}, сессия {}: {}.",
            hostname,
            target_user.as_deref().unwrap_or("—"),
            how
        );
        sqlx::query(
            "INSERT INTO ticket_comments (ticket_id, author_id, visibility, body, source)
             VALUES ($1, $2, 'internal', $3, 'system')",
        )
        .bind(t)
        .bind(operator)
        .bind(body)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
