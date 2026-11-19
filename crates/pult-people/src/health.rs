//! Связь с доменом: раз в минуту — вход служебной учёткой на любой DC.
//! После 3 неудач подряд при политике `block` (или по истечении `grace`) сеансы приостанавливаются:
//! проверка сеанса в ядре отвечает «Нет связи с доменом», пока связь не вернётся.

use crate::domain::AdHandle;
use pult_ad::AdError;
use pult_core::{
    ApiError, Core,
    settings::{self, AdHealth, AdOutagePolicy, SecuritySettings},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

/// Сколько неудач подряд при `block` — до приостановки.
pub const BLOCK_AFTER_FAILURES: u32 = 3;

pub fn next(prev: &AdHealth, ok: bool, s: &SecuritySettings, now: OffsetDateTime) -> AdHealth {
    if ok {
        return AdHealth::default();
    }
    let failures = prev.failures.saturating_add(1);
    let down_since = prev.down_since.clone().or_else(|| now.format(&Rfc3339).ok());
    let minutes_down = down_since
        .as_deref()
        .and_then(|d| OffsetDateTime::parse(d, &Rfc3339).ok())
        .map(|d| (now - d).whole_minutes())
        .unwrap_or(0);
    let suspended = match s.ad_outage_policy {
        AdOutagePolicy::Block => failures >= BLOCK_AFTER_FAILURES,
        AdOutagePolicy::Grace => minutes_down >= i64::from(s.ad_outage_grace_minutes),
    };
    AdHealth { failures, down_since, suspended }
}

pub async fn check(core: &Core, ad: &AdHandle) -> Result<AdHealth, ApiError> {
    let Some((_, pool)) = ad.pool(core).await? else {
        // Домен не настроен — приостанавливать нечего.
        return Ok(AdHealth::default());
    };
    // Одиночный сбой сети — не повод считать домен недоступным: вторая попытка через 2 с.
    let mut res = pool.ping().await;
    if matches!(res, Err(AdError::Unavailable(_))) {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        res = pool.ping().await;
    }
    let ok = match res {
        Ok(_) => true,
        Err(AdError::Unavailable(r)) => {
            tracing::warn!(report = %r, "проверка связи с доменом: контроллеры не отвечают (две попытки)");
            false
        }
        // Ответ есть (например, сменили пароль служебной учётки) — домен жив, сеансы не трогаем.
        Err(e) => {
            tracing::error!(error = %e, "проверка связи с доменом: ошибка служебной учётки");
            true
        }
    };
    let prev: AdHealth = settings::load(&core.db).await?;
    let s: SecuritySettings = settings::load(&core.db).await?;
    let h = next(&prev, ok, &s, OffsetDateTime::now_utc());
    if h != prev {
        if h.suspended && !prev.suspended {
            tracing::error!("нет связи с доменом — сеансы приостановлены");
        } else if !h.suspended && prev.suspended {
            tracing::warn!("связь с доменом восстановлена — сеансы снова работают");
        }
        settings::save(&core.db, &h, None).await?;
    }
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Duration;

    #[test]
    fn block_after_three_failures() {
        let s = SecuritySettings::default();
        let t = OffsetDateTime::now_utc();
        let h1 = next(&AdHealth::default(), false, &s, t);
        let h2 = next(&h1, false, &s, t + Duration::minutes(1));
        assert!(!h2.suspended);
        let h3 = next(&h2, false, &s, t + Duration::minutes(2));
        assert!(h3.suspended && h3.failures == 3);
        assert_eq!(h3.down_since, h1.down_since);
        assert_eq!(next(&h3, true, &s, t), AdHealth::default());
    }

    #[test]
    fn grace_waits_for_minutes() {
        let s = SecuritySettings {
            ad_outage_policy: AdOutagePolicy::Grace,
            ad_outage_grace_minutes: 30,
            ..Default::default()
        };
        let t = OffsetDateTime::now_utc();
        let mut h = next(&AdHealth::default(), false, &s, t);
        for m in 1..30 {
            h = next(&h, false, &s, t + Duration::minutes(m));
            assert!(!h.suspended, "минута {m}");
        }
        h = next(&h, false, &s, t + Duration::minutes(30));
        assert!(h.suspended);
    }
}
