//! Компьютеры домена: список из AD, адреса по прямым записям DNS, имя ПК по IP и поиск.
//!
//! В объекте компьютера в AD нет ни IP-адреса, ни текущего пользователя, а обратной зоны DNS
//! для рабочих станций часто нет. Прямые записи есть: ПК регистрирует их сам. Поэтому имя ПК
//! определяется сопоставлением адреса клиента с адресами компьютеров из AD.

use crate::domain::AdHandle;
use pult_ad::AdError;
use pult_core::{ApiError, Core};
use serde::Serialize;
use sqlx::PgPool;
use std::{future::Future, net::IpAddr, time::Duration};
use tokio::task::JoinSet;
use utoipa::ToSchema;
use uuid::Uuid;

/// Сколько имён разрешаем одновременно.
const RESOLVE_BATCH: usize = 16;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(3);
const PAGE_SIZE: i32 = 500;
const MAX_QUERY_CHARS: usize = 100;

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Summary {
    pub seen: usize,
    pub resolved: usize,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow, ToSchema)]
pub struct ComputerRow {
    pub id: Uuid,
    pub name: String,
    pub dns_host_name: Option<String>,
    pub description: Option<String>,
    pub operating_system: Option<String>,
    pub addresses: Vec<String>,
}

/// Адрес внутренней сети. Внешним клиентам имя ПК не определяем.
fn is_internal(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v) => v.is_private(),
        IpAddr::V6(v) => (v.segments()[0] & 0xfe00) == 0xfc00,
    }
}

/// Адреса по прямым записям DNS. Не нашлось или не ответили — пусто: лучше не подставить
/// имя ПК, чем подставить его по устаревшему адресу.
pub async fn resolve(host: String) -> Vec<IpAddr> {
    match tokio::time::timeout(RESOLVE_TIMEOUT, tokio::net::lookup_host((host.as_str(), 0))).await {
        Ok(Ok(found)) => found.map(|a| a.ip()).collect(),
        _ => Vec::new(),
    }
}

/// Обновить список компьютеров из AD и их адреса.
pub async fn run(core: &Core, ad: &AdHandle) -> Result<Summary, ApiError> {
    sync(core, ad, resolve).await
}

/// То же с заданным разрешением имён (в тестах DNS нет).
pub async fn sync<R, F>(core: &Core, ad: &AdHandle, resolver: R) -> Result<Summary, ApiError>
where
    R: Fn(String) -> F,
    F: Future<Output = Vec<IpAddr>> + Send + 'static,
{
    let Some((domain, pool)) = ad.pool(core).await? else {
        return Err(ApiError::bad_request("ad_not_configured", "Домен AD не настроен."));
    };
    let computers = pool.computers(PAGE_SIZE).await.map_err(|e| match e {
        AdError::Unavailable(_) => {
            ApiError::Unavailable { code: "ad_unavailable", message: "Нет связи с доменом.".into() }
        }
        other => ApiError::bad_request("ad_sync_failed", other.to_string()),
    })?;

    let mut addresses: Vec<Vec<String>> = vec![Vec::new(); computers.len()];
    let hosts: Vec<(usize, String)> = computers
        .iter()
        .enumerate()
        .map(|(i, c)| (i, c.dns_host_name.clone().unwrap_or_else(|| format!("{}.{}", c.name, domain.dns_name))))
        .collect();
    for batch in hosts.chunks(RESOLVE_BATCH) {
        let mut set = JoinSet::new();
        for (i, host) in batch {
            let (i, lookup) = (*i, resolver(host.clone()));
            set.spawn(async move { (i, lookup.await) });
        }
        while let Some(done) = set.join_next().await {
            let (i, found) = done.map_err(ApiError::internal)?;
            let mut list: Vec<String> = found
                .into_iter()
                .map(|a| a.to_canonical())
                .filter(|a| is_internal(*a))
                .map(|a| a.to_string())
                .collect();
            list.sort();
            list.dedup();
            addresses[i] = list;
        }
    }

    let mut tx = core.db.begin().await?;
    for (c, ips) in computers.iter().zip(&addresses) {
        let guid = Uuid::parse_str(&c.object_guid).map_err(ApiError::internal)?;
        sqlx::query(
            "INSERT INTO ad_computers (domain_id, object_guid, name, dns_host_name, description, operating_system,
                                       is_enabled, addresses, synced_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8::text[]::inet[], now())
             ON CONFLICT (domain_id, object_guid) DO UPDATE SET
                 name = EXCLUDED.name, dns_host_name = EXCLUDED.dns_host_name, description = EXCLUDED.description,
                 operating_system = EXCLUDED.operating_system, is_enabled = EXCLUDED.is_enabled,
                 addresses = EXCLUDED.addresses, synced_at = now()",
        )
        .bind(domain.id)
        .bind(guid)
        .bind(&c.name)
        .bind(&c.dns_host_name)
        .bind(&c.description)
        .bind(&c.operating_system)
        .bind(c.is_enabled)
        .bind(ips)
        .execute(&mut *tx)
        .await?;
    }
    // Пустой ответ — скорее нет прав на чтение компьютеров, чем пустой домен: список не стираем.
    if computers.is_empty() {
        tracing::warn!("AD не вернул ни одного компьютера: проверьте права служебной учётки");
    } else {
        sqlx::query("DELETE FROM ad_computers WHERE domain_id = $1 AND synced_at < now()")
            .bind(domain.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    let summary = Summary { seen: computers.len(), resolved: addresses.iter().filter(|a| !a.is_empty()).count() };
    tracing::info!(seen = summary.seen, resolved = summary.resolved, "компьютеры домена обновлены");
    Ok(summary)
}

/// Имя ПК по обратному DNS, не дольше секунды.
async fn by_ptr(ip: IpAddr) -> Option<String> {
    let lookup = tokio::task::spawn_blocking(move || dns_lookup::lookup_addr(&ip));
    let name = tokio::time::timeout(Duration::from_secs(1), lookup).await.ok()?.ok()?.ok()?;
    let short = name.split('.').next().unwrap_or(&name).to_string();
    // Без PTR getnameinfo возвращает сам адрес.
    (!short.is_empty() && short.parse::<u32>().is_err() && !name.contains(':')).then_some(short)
}

/// Компьютер, с которого пришёл запрос. Несколько ПК на одном адресе — значит, чья-то запись
/// устарела: имя не подставляем, чтобы не показать чужой компьютер.
pub async fn of_client(db: &PgPool, ip: Option<IpAddr>) -> Result<Option<String>, sqlx::Error> {
    let Some(ip) = ip.map(|i| i.to_canonical()).filter(|i| is_internal(*i)) else {
        return Ok(None);
    };
    let names: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM ad_computers WHERE is_enabled AND addresses @> ARRAY[$1::text::inet] ORDER BY name LIMIT 2",
    )
    .bind(ip.to_string())
    .fetch_all(db)
    .await?;
    Ok(match names.as_slice() {
        [] => by_ptr(ip).await,
        [one] => Some(one.clone()),
        _ => None,
    })
}

/// ПК, с которого пришёл запрос и где у учётки сейчас активная сессия — по данным агента
///. Нужны оба условия: сессию сообщает сам агент, и без совпадения адреса
/// подделанный отчёт с чужого ПК привязал бы к заявкам сотрудника этот ПК.
pub async fn of_session(
    db: &PgPool,
    account: Option<Uuid>,
    ip: Option<IpAddr>,
) -> Result<Option<(Uuid, String)>, sqlx::Error> {
    let (Some(account), Some(ip)) = (account, ip) else { return Ok(None) };
    sqlx::query_as(
        "SELECT d.id, d.hostname
           FROM device_sessions s JOIN devices d ON d.id = s.device_id AND d.is_active
          WHERE s.account_id = $1 AND s.state = 'active' AND d.last_seen_at > now() - interval '10 minutes'
            AND (d.last_ip = $2::text::inet OR $2::text::inet = ANY(d.ip_addrs))
          ORDER BY s.updated_at DESC
          LIMIT 1",
    )
    .bind(account)
    .bind(ip.to_canonical().to_string())
    .fetch_optional(db)
    .await
}

/// ПК с агентом по имени из заявки (короткому или FQDN).
pub async fn device_by_name(db: &PgPool, name: Option<&str>) -> Result<Option<Uuid>, sqlx::Error> {
    let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else { return Ok(None) };
    sqlx::query_scalar("SELECT id FROM devices WHERE lower(hostname) = lower(split_part($1, '.', 1)) AND is_active")
        .bind(name)
        .fetch_optional(db)
        .await
}

/// Поиск по части имени, DNS-имени, описанию или точному IP. Сверху точные совпадения,
/// затем совпадения с начала имени.
pub async fn search(db: &PgPool, query: &str, limit: i64) -> Result<Vec<ComputerRow>, sqlx::Error> {
    let needle: String = query.trim().chars().take(MAX_QUERY_CHARS).collect();
    if needle.is_empty() {
        return Ok(Vec::new());
    }
    let ip = needle.parse::<IpAddr>().ok().map(|i| i.to_canonical().to_string());
    sqlx::query_as(
        "SELECT id, name, dns_host_name, description, operating_system,
                ARRAY(SELECT host(a) FROM unnest(addresses) a ORDER BY 1) AS addresses
           FROM ad_computers
          WHERE is_enabled
            AND (strpos(lower(name), lower($1)) > 0
                 OR strpos(lower(coalesce(dns_host_name, '')), lower($1)) > 0
                 OR strpos(lower(coalesce(description, '')), lower($1)) > 0
                 OR $2::text::inet = ANY(addresses))
          ORDER BY CASE WHEN lower(name) = lower($1) OR lower(dns_host_name) = lower($1) THEN 0
                        WHEN starts_with(lower(name), lower($1)) THEN 1
                        WHEN starts_with(lower(dns_host_name), lower($1)) THEN 2
                        ELSE 3 END, name
          LIMIT $3",
    )
    .bind(&needle)
    .bind(ip)
    .bind(limit.clamp(1, 20))
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_addresses() {
        for a in ["10.1.2.3", "192.168.1.1", "172.16.0.1", "::ffff:10.0.0.1", "fd00::1"] {
            assert!(is_internal(a.parse().unwrap()), "{a}");
        }
        for a in ["8.8.8.8", "127.0.0.1", "2001:db8::1", "::ffff:8.8.8.8"] {
            assert!(!is_internal(a.parse().unwrap()), "{a}");
        }
    }
}
