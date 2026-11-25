//! Агент → портал (`/api/agent/v1/…`): регистрация, отчёты, обновление. Без cookie и CSRF —
//! агент предъявляет токен регистрации (один раз) и затем свой ключ в `Authorization: Bearer`.
//!
//! Почему регистрация проверяет ПК: токен многоразовый и лежит на каждом ПК, где стоит агент, —
//! его может прочитать любой, у кого есть доступ к этим ПК. Если бы по нему можно было
//! зарегистрироваться под любым именем, нарушитель подставил бы инженеру ложную сессию
//! «пользователь X на ПК Y». Поэтому имя должно быть включённым компьютером AD, а запрос
//! (по настройке, по умолчанию) — прийти с адреса, который DNS даёт этому ПК. Отчёт с ключом
//! принимается только под тем именем, под которым агент регистрировался.

use crate::{RemoteSettings, tokens};
use axum::{
    Json,
    body::Bytes,
    extract::{FromRequestParts, Path, State},
    http::{HeaderMap, StatusCode, header, request::Parts},
    response::IntoResponse,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use pult_api_types::{AgentReport, AgentUpdate, EnrollRequest, EnrollResponse, ReportResponse, SessionState};
use pult_core::{
    ApiError, ApiResult, Core,
    audit::{self, Actor, Entry, Source},
    settings,
};
use serde_json::json;
use sqlx::{Postgres, Transaction};
use std::net::IpAddr;
use time::OffsetDateTime;
use uuid::Uuid;

/// Сессий в одном отчёте не больше (терминальный сервер — сотни, не тысячи).
const MAX_SESSIONS: usize = 1000;
const MAX_IPS: usize = 16;

/// Кто обратился: адрес и User-Agent (агент, helper) — для аудита и проверки адреса.
pub struct Peer(pub Actor);

impl<S: Send + Sync> FromRequestParts<S> for Peer {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(pult_core::session::actor_from(parts, None)))
    }
}

/// Короткое имя ПК: буквы, цифры, дефис; до 63 символов.
pub fn valid_hostname(s: &str) -> bool {
    !s.is_empty() && s.len() <= 63 && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// FQDN: метки как у имени ПК через точку, до 253 символов.
pub fn valid_fqdn(s: &str) -> bool {
    s.len() <= 253 && s.split('.').all(valid_hostname)
}

fn clip(s: Option<&str>, n: usize) -> Option<String> {
    s.map(|v| v.trim().chars().filter(|c| !c.is_control()).take(n).collect::<String>()).filter(|v| !v.is_empty())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Проверенный отчёт.
struct Clean {
    hostname: String,
    fqdn: Option<String>,
    domain: Option<String>,
    os: Option<String>,
    os_version: Option<String>,
    agent_version: Option<String>,
    ips: Vec<String>,
    sessions: Vec<CleanSession>,
}

struct CleanSession {
    id: i32,
    user: String,
    domain: Option<String>,
    state: &'static str,
    station: Option<String>,
    client: Option<String>,
    logon_at: Option<OffsetDateTime>,
}

fn clean(r: &AgentReport) -> Result<Clean, ApiError> {
    let bad = |m: &str| ApiError::bad_request("invalid_report", m.to_string());
    let hostname = r.hostname.trim().to_ascii_uppercase();
    if !valid_hostname(&hostname) {
        return Err(bad("Неверное имя компьютера."));
    }
    let fqdn = clip(r.fqdn.as_deref(), 253).map(|f| f.to_ascii_lowercase());
    if fqdn.as_deref().is_some_and(|f| !valid_fqdn(f)) {
        return Err(bad("Неверное DNS-имя компьютера."));
    }
    if r.sessions.len() > MAX_SESSIONS {
        return Err(bad("Слишком много сессий в отчёте."));
    }
    let ips = r
        .ip_addrs
        .iter()
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .map(|i| i.to_canonical())
        .filter(|i| !i.is_loopback() && !i.is_unspecified())
        .map(|i| i.to_string())
        .take(MAX_IPS)
        .collect();
    let sessions = r
        .sessions
        .iter()
        .filter(|s| s.is_user_session() && s.id <= i32::MAX as u32)
        .filter_map(|s| {
            let user = clip(s.user.as_deref(), 256)?;
            let state = match s.state {
                SessionState::Active => "active",
                SessionState::Disconnected => "disconnected",
                _ => "idle",
            };
            Some(CleanSession {
                id: s.id as i32,
                user,
                domain: clip(s.domain.as_deref(), 256),
                state,
                station: clip(Some(&s.station), 100),
                client: clip(s.client_name.as_deref(), 256),
                logon_at: s.logon_at.and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok()),
            })
        })
        .collect();
    Ok(Clean {
        hostname,
        fqdn,
        domain: clip(r.domain.as_deref(), 256),
        os: clip(Some(&r.os), 100),
        os_version: clip(r.os_version.as_deref(), 200),
        agent_version: clip(Some(&r.agent_version), 50),
        ips,
        sessions,
    })
}

/// Текущее состояние ПК: сведения и сессии целиком заменяются данными отчёта.
async fn store(tx: &mut Transaction<'_, Postgres>, device: Uuid, c: &Clean, ip: Option<IpAddr>) -> ApiResult<()> {
    sqlx::query(
        "UPDATE devices SET fqdn = coalesce($2, fqdn), domain_name = $3, os = $4, os_version = $5, agent_version = $6,
                ip_addrs = $7::text[]::inet[], last_ip = $8::text::inet, last_seen_at = now()
          WHERE id = $1",
    )
    .bind(device)
    .bind(&c.fqdn)
    .bind(&c.domain)
    .bind(&c.os)
    .bind(&c.os_version)
    .bind(&c.agent_version)
    .bind(&c.ips)
    .bind(ip.map(|i| i.to_canonical().to_string()))
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM device_sessions WHERE device_id = $1").bind(device).execute(&mut **tx).await?;
    if c.sessions.is_empty() {
        return Ok(());
    }
    // Учётка AD — по логину и домену сессии (NetBIOS-имя; если оно не задано — первая часть
    // DNS-имени домена). Без домена или при неоднозначности не связываем.
    sqlx::query(
        "INSERT INTO device_sessions (device_id, session_id, username, user_domain, state, station, client_name, logon_at, account_id)
         SELECT $1, s.id, s.u, s.d, s.st, s.sta, s.c, s.l,
                (SELECT CASE WHEN count(*) = 1 THEN min(a.id::text)::uuid END
                   FROM ad_accounts a JOIN ad_domains dm ON dm.id = a.domain_id
                  WHERE lower(a.sam_account_name) = lower(s.u) AND NOT a.is_missing
                    AND (upper(dm.netbios_name) = upper(s.d) OR lower(dm.dns_name) = lower(s.d)
                         OR (dm.netbios_name IS NULL AND upper(split_part(dm.dns_name, '.', 1)) = upper(s.d))))
           FROM unnest($2::int[], $3::text[], $4::text[], $5::text[], $6::text[], $7::text[], $8::timestamptz[])
                AS s(id, u, d, st, sta, c, l)
         ON CONFLICT (device_id, session_id) DO NOTHING",
    )
    .bind(device)
    .bind(c.sessions.iter().map(|s| s.id).collect::<Vec<_>>())
    .bind(c.sessions.iter().map(|s| s.user.clone()).collect::<Vec<_>>())
    .bind(c.sessions.iter().map(|s| s.domain.clone()).collect::<Vec<_>>())
    .bind(c.sessions.iter().map(|s| s.state).collect::<Vec<_>>())
    .bind(c.sessions.iter().map(|s| s.station.clone()).collect::<Vec<_>>())
    .bind(c.sessions.iter().map(|s| s.client.clone()).collect::<Vec<_>>())
    .bind(c.sessions.iter().map(|s| s.logon_at).collect::<Vec<_>>())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

type EnrollmentTokenRow = (Uuid, String);
type ComputerRow = (Uuid, Option<String>, Vec<String>);

/// Регистрация агента: токен регистрации → ключ агента (показывается один раз).
#[utoipa::path(post, path = "/api/agent/v1/enroll", tag = "agent", request_body = EnrollRequest,
    responses((status = 201, body = EnrollResponse), (status = 401, description = "Токен неверен, отозван или истёк"),
              (status = 400, description = "ПК не найден в AD или адрес не совпадает с DNS")))]
pub async fn enroll(
    State(core): State<Core>,
    Peer(actor): Peer,
    Json(b): Json<EnrollRequest>,
) -> ApiResult<(StatusCode, Json<EnrollResponse>)> {
    if !tokens::well_formed(b.token.trim(), tokens::ENROLL_PREFIX) {
        return Err(ApiError::Unauthorized);
    }
    let c = clean(&b.report)?;
    let cfg: RemoteSettings = settings::load(&core.db).await?;
    let mut tx = core.db.begin().await?;
    let token: Option<EnrollmentTokenRow> = sqlx::query_as(
        "UPDATE agent_enrollment_tokens SET uses = uses + 1, last_used_at = now()
          WHERE token_hash = $1 AND revoked_at IS NULL AND expires_at > now() RETURNING id, name",
    )
    .bind(tokens::hash(b.token.trim()))
    .fetch_optional(&mut *tx)
    .await?;
    let Some((token_id, token_name)) = token else {
        tracing::warn!(host = %c.hostname, ip = ?actor.ip, "регистрация агента: неверный токен");
        return Err(ApiError::Unauthorized);
    };
    // Компьютер AD с этим именем; при нескольких доменах — тот, чьё DNS-имя прислал агент.
    let computers: Vec<ComputerRow> = sqlx::query_as(
        "SELECT id, dns_host_name, ARRAY(SELECT host(a) FROM unnest(addresses) a) FROM ad_computers
          WHERE is_enabled AND lower(name) = lower($1)
          ORDER BY (lower(dns_host_name) = lower($2)) DESC NULLS LAST LIMIT 2",
    )
    .bind(&c.hostname)
    .bind(&c.fqdn)
    .fetch_all(&mut *tx)
    .await?;
    let refuse = |code: &'static str, message: &str| {
        tracing::warn!(host = %c.hostname, ip = ?actor.ip, code, "регистрация агента отклонена");
        ApiError::bad_request(code, message.to_string())
    };
    let Some((computer_id, dns_name, addresses)) = computers.into_iter().next() else {
        return Err(refuse(
            "unknown_computer",
            "Компьютера с таким именем нет среди включённых компьютеров AD (синхронизация каталога).",
        ));
    };
    if cfg.require_dns_match {
        let from = actor.ip.map(|i| i.to_canonical().to_string());
        if !from.as_ref().is_some_and(|f| addresses.contains(f)) {
            return Err(refuse(
                "address_mismatch",
                "Адрес, с которого пришёл запрос, не совпадает с адресом этого ПК в DNS.",
            ));
        }
    }
    let (key, key_hash) = tokens::issue(tokens::AGENT_KEY_PREFIX)?;
    // Отключённый администратором ПК сам не возвращается: сначала «Включить» в настройках.
    let enrolled: Option<(Uuid, bool)> = sqlx::query_as(
        "INSERT INTO devices (hostname, fqdn, ad_computer_id, agent_key_hash, enrollment_token_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT ((lower(hostname))) DO UPDATE
            SET hostname = EXCLUDED.hostname, fqdn = coalesce(EXCLUDED.fqdn, devices.fqdn),
                ad_computer_id = EXCLUDED.ad_computer_id, agent_key_hash = EXCLUDED.agent_key_hash,
                enrollment_token_id = EXCLUDED.enrollment_token_id, enrolled_at = now()
          WHERE devices.is_active
         RETURNING id, (xmax::text <> '0')",
    )
    .bind(&c.hostname)
    .bind(c.fqdn.clone().or(dns_name.map(|d| d.to_ascii_lowercase())))
    .bind(computer_id)
    .bind(&key_hash)
    .bind(token_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((device, re_enrolled)) = enrolled else {
        tracing::warn!(host = %c.hostname, ip = ?actor.ip, "регистрация отключённого ПК отклонена");
        return Err(ApiError::Refused {
            code: "device_deactivated",
            message: "ПК отключён администратором Pult: регистрация снова возможна после «Включить».".into(),
        });
    };
    store(&mut tx, device, &c, actor.ip).await?;
    audit::record(
        &mut *tx,
        &actor,
        Entry::new("remote.device.enrolled", Source::Agent).object("device", device).details(json!({
            "hostname": c.hostname, "token": token_name, "re_enrolled": re_enrolled,
            "agent_version": c.agent_version, "dns_checked": cfg.require_dns_match,
        })),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(host = %c.hostname, re_enrolled, "агент зарегистрирован");
    Ok((StatusCode::CREATED, Json(EnrollResponse { device_id: device.to_string(), agent_key: key })))
}

/// Устройство по ключу агента. Отключённое устройство ключ не принимает — агенту нужна новая регистрация.
async fn device_by_key(core: &Core, headers: &HeaderMap) -> ApiResult<(Uuid, String, Option<String>)> {
    let key = tokens::bearer(headers).ok_or(ApiError::Unauthorized)?;
    sqlx::query_as("SELECT id, hostname, agent_version FROM devices WHERE agent_key_hash = $1 AND is_active")
        .bind(tokens::hash(key))
        .fetch_optional(&core.db)
        .await?
        .ok_or(ApiError::Unauthorized)
}

type AgentPackageRow = (Uuid, String, Vec<u8>, Option<Vec<u8>>);

/// Назначенное обновление, если версия агента другая. Пакеты обновления собираются только для Windows:
/// агентам на других ОС их не предлагаем.
async fn pending_update(core: &Core, cfg: &RemoteSettings, c: &Clean) -> ApiResult<Option<AgentUpdate>> {
    let Some(id) = cfg.update_package_id else { return Ok(None) };
    if !c.os.as_deref().is_some_and(|o| o.starts_with("windows")) {
        return Ok(None);
    }
    let version = c.agent_version.as_deref();
    let p: Option<AgentPackageRow> = sqlx::query_as(
        "SELECT id, version, sha256, signature FROM agent_packages WHERE id = $1 AND kind = 'agent_update'",
    )
    .bind(id)
    .fetch_optional(&core.db)
    .await?;
    Ok(p.filter(|(_, v, _, _)| Some(v.as_str()) != version).map(|(id, version, sha, sig)| AgentUpdate {
        package_id: id.to_string(),
        version,
        sha256: hex(&sha),
        signature: STANDARD.encode(sig.unwrap_or_default()),
    }))
}

/// Отчёт агента: текущее состояние ПК и сессий.
#[utoipa::path(post, path = "/api/agent/v1/report", tag = "agent", request_body = AgentReport,
    security(("agent_key" = [])),
    responses((status = 200, body = ReportResponse), (status = 401, description = "Ключ неверен или устройство отключено"),
              (status = 409, description = "Имя ПК изменилось — нужна новая регистрация")))]
pub async fn report(
    State(core): State<Core>,
    Peer(actor): Peer,
    headers: HeaderMap,
    Json(r): Json<AgentReport>,
) -> ApiResult<Json<ReportResponse>> {
    let (device, hostname, _) = device_by_key(&core, &headers).await?;
    let c = clean(&r)?;
    if !c.hostname.eq_ignore_ascii_case(&hostname) {
        // Ключ одного ПК не годится для отчётов под другим именем (переименование — новая регистрация).
        return Err(ApiError::Conflict { current: json!({"hostname": hostname}) });
    }
    let hw = r.inventory.as_ref().map(crate::inventory::clean);
    let mut tx = core.db.begin().await?;
    store(&mut tx, device, &c, actor.ip).await?;
    if let Some(hw) = &hw {
        crate::inventory::store(&mut tx, device, hw).await?;
    }
    tx.commit().await?;
    let cfg: RemoteSettings = settings::load(&core.db).await?;
    Ok(Json(ReportResponse {
        next_report_secs: cfg.report_interval_secs.clamp(30, 600),
        update: pending_update(&core, &cfg, &c).await?,
        want_inventory: crate::inventory::wanted(&core, device).await?,
    }))
}

/// Назначенный пакет в памяти одной копией: сотни ПК, скачивающих его разом, делят один буфер
/// (содержимое пакета по id не меняется), а база читается один раз.
static PACKAGE: std::sync::Mutex<Option<(Uuid, Bytes)>> = std::sync::Mutex::new(None);

fn cached(id: Uuid) -> Option<Bytes> {
    PACKAGE.lock().ok()?.as_ref().filter(|(p, _)| *p == id).map(|(_, b)| b.clone())
}

/// Пакет обновления — только назначенный и только агенту с действующим ключом.
#[utoipa::path(get, path = "/api/agent/v1/update/{id}", tag = "agent", security(("agent_key" = [])),
    params(("id" = Uuid, Path)),
    responses((status = 200, description = "Пакет", content_type = "application/octet-stream"),
              (status = 401, description = "Ключ неверен"), (status = 404, description = "Обновление не назначено")))]
pub async fn download_update(
    State(core): State<Core>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    device_by_key(&core, &headers).await?;
    let cfg: RemoteSettings = settings::load(&core.db).await?;
    if cfg.update_package_id != Some(id) {
        return Err(ApiError::NotFound);
    }
    let data = match cached(id) {
        Some(d) => d,
        None => {
            let d: Vec<u8> =
                sqlx::query_scalar("SELECT data FROM agent_packages WHERE id = $1 AND kind = 'agent_update'")
                    .bind(id)
                    .fetch_optional(&core.db)
                    .await?
                    .ok_or(ApiError::NotFound)?;
            let d = Bytes::from(d);
            if let Ok(mut slot) = PACKAGE.lock() {
                *slot = Some((id, d.clone()));
            }
            d
        }
    };
    Ok(([(header::CONTENT_TYPE, "application/octet-stream"), (header::CACHE_CONTROL, "no-store")], data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pult_api_types::Session;

    fn report(sessions: Vec<Session>) -> AgentReport {
        AgentReport {
            hostname: "ws-001".into(),
            domain: Some("CORP".into()),
            os: "windows-x86_64".into(),
            agent_version: "0.5.0".into(),
            sessions,
            fqdn: Some("WS-001.Corp.Local".into()),
            os_version: None,
            ip_addrs: vec!["10.0.0.5".into(), "127.0.0.1".into(), "bogus".into(), "::ffff:10.0.0.6".into()],
            inventory: None,
        }
    }

    fn s(id: u32, state: SessionState, user: Option<&str>) -> Session {
        Session {
            id,
            station: "Console".into(),
            state,
            user: user.map(Into::into),
            domain: Some("CORP".into()),
            client_name: None,
            logon_at: Some(1_700_000_000),
        }
    }

    #[test]
    fn names() {
        assert!(valid_hostname("WS-001"));
        assert!(!valid_hostname("-x") && !valid_hostname("") && !valid_hostname("a b") && !valid_hostname("a.b"));
        assert!(!valid_hostname(&"a".repeat(64)));
        assert!(valid_fqdn("ws-1.example.local"));
        assert!(!valid_fqdn("ws-1..example") && !valid_fqdn("ws/1.example"));
    }

    #[test]
    fn report_is_cleaned() {
        let c = clean(&report(vec![
            s(1, SessionState::Active, Some("ivanov")),
            s(0, SessionState::Disconnected, Some("SYSTEM")),
            s(2, SessionState::Disconnected, Some("petrov")),
            s(3, SessionState::Listen, None),
            s(4, SessionState::Idle, Some("sidorov")),
        ]))
        .unwrap();
        assert_eq!(c.hostname, "WS-001");
        assert_eq!(c.fqdn.as_deref(), Some("ws-001.corp.local"));
        assert_eq!(
            c.ips,
            vec!["10.0.0.5".to_string(), "10.0.0.6".to_string()],
            "без петли, мусора; IPv4 в IPv6 — как IPv4"
        );
        let got: Vec<(i32, &str)> = c.sessions.iter().map(|s| (s.id, s.state)).collect();
        assert_eq!(got, vec![(1, "active"), (2, "disconnected"), (4, "idle")], "только сессии пользователей");
        assert!(c.sessions[0].logon_at.is_some());
    }

    #[test]
    fn bad_reports() {
        let mut r = report(vec![]);
        r.hostname = "ws 1; drop".into();
        assert!(clean(&r).is_err());
        let mut r = report(vec![]);
        r.fqdn = Some("evil/..".into());
        assert!(clean(&r).is_err());
        let r = report((1..=1001).map(|i| s(i, SessionState::Active, Some("u"))).collect());
        assert!(clean(&r).is_err());
    }
}
