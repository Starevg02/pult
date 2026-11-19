//! Домен AD в БД (`ad_domains`): подключение, служебная учётка (пароль зашифрован
//! `PULT_SECRET_KEY`), пул контроллеров с кешем — пересоздаётся, когда домен меняют в админке.

use pult_ad::{AdConfig, DcPool, TlsMode};
use pult_core::{ApiError, Core, Secret};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, RwLock};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DomainRow {
    pub id: Uuid,
    pub dns_name: String,
    pub netbios_name: Option<String>,
    pub base_dn: String,
    pub controllers: Vec<String>,
    pub tls_mode: String,
    pub bind_dn: String,
    pub bind_secret: Vec<u8>,
    pub ca_cert_pem: Option<String>,
    pub user_filter: String,
    pub timeout_secs: i32,
    pub sync_state: serde_json::Value,
    pub enabled: bool,
    pub version: i32,
}

const SELECT: &str =
    "SELECT id, dns_name, netbios_name, base_dn, controllers, tls_mode, bind_dn, bind_secret, ca_cert_pem,
        user_filter, timeout_secs, sync_state, enabled, version FROM ad_domains";

pub async fn current(db: &sqlx::PgPool) -> Result<Option<DomainRow>, sqlx::Error> {
    sqlx::query_as(&format!("{SELECT} WHERE enabled ORDER BY created_at LIMIT 1")).fetch_optional(db).await // SAFE: константа SELECT из кода
}

pub fn tls_mode(s: &str) -> TlsMode {
    if s == "starttls" { TlsMode::Starttls } else { TlsMode::Ldaps }
}

impl DomainRow {
    pub fn to_config(&self, core: &Core) -> Result<AdConfig, ApiError> {
        let password = core.secrets.decrypt(&self.bind_secret).map_err(ApiError::internal)?;
        Ok(AdConfig {
            controllers: self.controllers.clone(),
            tls: tls_mode(&self.tls_mode),
            base_dn: self.base_dn.clone(),
            bind_dn: self.bind_dn.clone(),
            bind_password: password,
            ca_cert: None,
            ca_cert_pem: self.ca_cert_pem.clone(),
            user_filter: self.user_filter.clone(),
            timeout_secs: u64::try_from(self.timeout_secs).unwrap_or(5),
        })
    }
}

/// DNS-имя домена из базового DN: `DC=corp,DC=example,DC=local` → `corp.example.local`.
pub fn dns_from_base_dn(base_dn: &str) -> String {
    base_dn
        .split(',')
        .filter_map(|p| {
            let (k, v) = p.trim().split_once('=')?;
            k.trim().eq_ignore_ascii_case("dc").then(|| v.trim().to_lowercase())
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Первый запуск: домена в БД нет, а в конфиге есть секция `[ad]` — заводим из неё.
/// Дальше домен ведётся в админке, секция `[ad]` больше не читается.
pub async fn seed_from_config(core: &Core, cfg: &AdConfig, netbios: Option<&str>) -> Result<bool, ApiError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM ad_domains)").fetch_one(&core.db).await?;
    if exists {
        return Ok(false);
    }
    store(core, cfg, netbios, false).await?;
    tracing::info!("домен AD заведён из секции [ad] конфига; дальше он настраивается в админке");
    Ok(true)
}

/// Записать домен (команда `pult setup ad`): новый или заменить настройки существующего с тем же DNS-именем.
pub async fn store(core: &Core, cfg: &AdConfig, netbios: Option<&str>, replace: bool) -> Result<(), ApiError> {
    let ca = match &cfg.ca_cert {
        Some(p) => Some(std::fs::read_to_string(p).map_err(|e| ApiError::internal(format!("{}: {e}", p.display())))?),
        None => cfg.ca_cert_pem.clone(),
    };
    let sealed = core.secrets.encrypt(&cfg.bind_password).map_err(ApiError::internal)?;
    let tls = match cfg.tls {
        TlsMode::Ldaps => "ldaps",
        TlsMode::Starttls => "starttls",
    };
    sqlx::query(
        "INSERT INTO ad_domains (dns_name, netbios_name, base_dn, controllers, tls_mode, bind_dn, bind_secret, ca_cert_pem,
                                 user_filter, timeout_secs)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT (dns_name) DO UPDATE SET
             netbios_name = EXCLUDED.netbios_name, base_dn = EXCLUDED.base_dn, controllers = EXCLUDED.controllers,
             tls_mode = EXCLUDED.tls_mode, bind_dn = EXCLUDED.bind_dn, bind_secret = EXCLUDED.bind_secret,
             ca_cert_pem = coalesce(EXCLUDED.ca_cert_pem, ad_domains.ca_cert_pem), user_filter = EXCLUDED.user_filter,
             timeout_secs = EXCLUDED.timeout_secs, enabled = true, version = ad_domains.version + 1
           WHERE $11",
    )
    .bind(dns_from_base_dn(&cfg.base_dn))
    .bind(netbios)
    .bind(&cfg.base_dn)
    .bind(&cfg.controllers)
    .bind(tls)
    .bind(&cfg.bind_dn)
    .bind(sealed)
    .bind(ca)
    .bind(&cfg.user_filter)
    .bind(i32::try_from(cfg.timeout_secs).unwrap_or(5))
    .bind(replace)
    .execute(&core.db)
    .await?;
    Ok(())
}

/// Пул контроллеров текущего домена, пересоздаётся при изменении домена (по `version`).
/// Кеш: домен, его версия и пул.
type Cached = Option<(Uuid, i32, Arc<DcPool>)>;

#[derive(Default, Clone)]
pub struct AdHandle {
    cache: Arc<RwLock<Cached>>,
}

impl AdHandle {
    /// `None` — домен не настроен.
    pub async fn pool(&self, core: &Core) -> Result<Option<(DomainRow, Arc<DcPool>)>, ApiError> {
        let Some(row) = current(&core.db).await? else { return Ok(None) };
        if let Ok(g) = self.cache.read()
            && let Some((id, ver, pool)) = g.as_ref()
            && *id == row.id
            && *ver == row.version
        {
            return Ok(Some((row, pool.clone())));
        }
        let pool =
            Arc::new(DcPool::new(row.to_config(core)?).map_err(|e| ApiError::bad_request("ad_config", e.to_string()))?);
        if let Ok(mut g) = self.cache.write() {
            *g = Some((row.id, row.version, pool.clone()));
        }
        Ok(Some((row, pool)))
    }
}

/// Домен для админки: всё, кроме пароля.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DomainView {
    pub id: Uuid,
    pub dns_name: String,
    pub netbios_name: Option<String>,
    pub base_dn: String,
    pub controllers: Vec<String>,
    pub tls_mode: String,
    pub bind_dn: String,
    pub has_ca_cert: bool,
    pub user_filter: String,
    pub timeout_secs: i32,
    pub enabled: bool,
    pub version: i32,
}

impl From<&DomainRow> for DomainView {
    fn from(r: &DomainRow) -> Self {
        Self {
            id: r.id,
            dns_name: r.dns_name.clone(),
            netbios_name: r.netbios_name.clone(),
            base_dn: r.base_dn.clone(),
            controllers: r.controllers.clone(),
            tls_mode: r.tls_mode.clone(),
            bind_dn: r.bind_dn.clone(),
            has_ca_cert: r.ca_cert_pem.as_deref().is_some_and(|c| !c.trim().is_empty()),
            user_filter: r.user_filter.clone(),
            timeout_secs: r.timeout_secs,
            enabled: r.enabled,
            version: r.version,
        }
    }
}

/// Изменение домена в админке. Пароль и сертификат: не переданы — прежние.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DomainInput {
    pub netbios_name: Option<String>,
    pub base_dn: String,
    pub controllers: Vec<String>,
    pub tls_mode: String,
    pub bind_dn: String,
    pub bind_password: Option<String>,
    /// PEM; пустая строка — удалить.
    pub ca_cert_pem: Option<String>,
    pub user_filter: String,
    pub timeout_secs: i32,
    /// Для оптимистичной блокировки; при создании — 0.
    pub version: i32,
}

impl DomainInput {
    /// Проверка до сохранения: контроллеры без схемы, фильтр в скобках, разумный тайм-аут.
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.tls_mode.as_str(), "ldaps" | "starttls") {
            return Err("Режим TLS — ldaps или starttls.".into());
        }
        let probe = AdConfig {
            controllers: self.controllers.clone(),
            tls: tls_mode(&self.tls_mode),
            base_dn: self.base_dn.clone(),
            bind_dn: self.bind_dn.clone(),
            bind_password: Secret::new("x"),
            ca_cert: None,
            ca_cert_pem: self.ca_cert_pem.clone(),
            user_filter: self.user_filter.clone(),
            timeout_secs: 5,
        };
        probe.endpoints().map_err(|e| e.to_string())?;
        if dns_from_base_dn(&self.base_dn).is_empty() {
            return Err("Базовый DN — вида DC=corp,DC=example,DC=local.".into());
        }
        let f = self.user_filter.trim();
        if !(f.starts_with('(') && f.ends_with(')')) {
            return Err("Фильтр пользователей — выражение LDAP в скобках.".into());
        }
        if !(1..=60).contains(&self.timeout_secs) {
            return Err("Тайм-аут — от 1 до 60 секунд.".into());
        }
        if self.bind_dn.trim().is_empty() {
            return Err("Укажите служебную учётку.".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns_name_from_dn() {
        assert_eq!(dns_from_base_dn("DC=Corp, dc=Example,DC=local"), "corp.example.local");
        assert_eq!(dns_from_base_dn("OU=Users,DC=x,DC=test"), "x.test");
        assert_eq!(dns_from_base_dn(""), "");
    }

    #[test]
    fn input_validation() {
        let mut d = DomainInput {
            netbios_name: None,
            base_dn: "DC=corp,DC=example,DC=local".into(),
            controllers: vec!["dc1.corp.example.local".into()],
            tls_mode: "ldaps".into(),
            bind_dn: "svc@corp.example.local".into(),
            bind_password: None,
            ca_cert_pem: None,
            user_filter: "(&(objectCategory=person)(objectClass=user))".into(),
            timeout_secs: 5,
            version: 0,
        };
        assert!(d.validate().is_ok());
        d.controllers = vec!["ldap://dc1".into()]; // SAFE: проверка отказа
        assert!(d.validate().is_err());
        d.controllers = vec!["dc1".into()];
        d.user_filter = "objectClass=user".into();
        assert!(d.validate().is_err());
    }
}
