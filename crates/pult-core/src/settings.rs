//! Прикладные настройки в таблице `settings` (меняются из админки).
//!
//! Каждая группа — свой ключ и своя структура с значениями по умолчанию: пустая таблица —
//! рабочая конфигурация. Неизвестные поля в JSON игнорируются, недостающие — по умолчанию.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sqlx::{PgExecutor, types::ipnetwork::IpNetwork};
use std::net::IpAddr;
use uuid::Uuid;

/// Группа настроек со своим ключом в таблице `settings`.
pub trait SettingsGroup: Serialize + DeserializeOwned + Default {
    const KEY: &'static str;
}

pub async fn load<T: SettingsGroup>(db: impl PgExecutor<'_>) -> Result<T, sqlx::Error> {
    let v: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = $1").bind(T::KEY).fetch_optional(db).await?;
    Ok(v.and_then(|v| match serde_json::from_value(v) {
        Ok(t) => Some(t),
        Err(e) => {
            tracing::error!(key = T::KEY, error = %e, "настройки повреждены — используются значения по умолчанию");
            None
        }
    })
    .unwrap_or_default())
}

pub async fn save<T: SettingsGroup>(db: impl PgExecutor<'_>, value: &T, by: Option<Uuid>) -> Result<(), sqlx::Error> {
    let v = serde_json::to_value(value).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    sqlx::query(
        "INSERT INTO settings (key, value, updated_by) VALUES ($1, $2, $3)
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_by = EXCLUDED.updated_by, updated_at = now()",
    )
    .bind(T::KEY)
    .bind(v)
    .bind(by)
    .execute(db)
    .await?;
    Ok(())
}

/// Что делать с открытыми сеансами, если ни один контроллер домена не отвечает.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdOutagePolicy {
    /// Приостановить все сеансы до восстановления связи.
    #[default]
    Block,
    /// Продолжать работу ограниченное время.
    Grace,
}

/// Сеансы и вход (админка → «Безопасность»).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct SecuritySettings {
    /// Сеанс закрывается после стольких часов бездействия.
    pub session_idle_hours: u32,
    /// Абсолютный срок сеанса, дней.
    pub session_absolute_days: u32,
    /// Неудачных попыток входа на один логин за окно.
    pub login_max_failures_per_login: u32,
    /// Неудачных попыток с одного IP за окно.
    pub login_max_failures_per_ip: u32,
    pub login_window_minutes: u32,
    pub ad_outage_policy: AdOutagePolicy,
    /// Для `grace`: сколько минут работать без связи с доменом.
    pub ad_outage_grace_minutes: u32,
    /// Сети организации (CIDR). С других адресов код из приложения спрашивается у тех, кто его подключил,
    /// и у ролей из `totp_required_roles`.
    pub internal_networks: Vec<String>,
    /// Коды ролей, которым вход из интернета — только с кодом из приложения. Пусто — код по желанию.
    pub totp_required_roles: Vec<String>,
    /// Вход через домен без пароля (Kerberos) — только из сетей организации. Нужны keytab
    /// (`KRB5_KTNAME`) и SPN `HTTP/<имя портала>` у служебной учётки.
    pub kerberos_login: bool,
    /// Смена своего пароля AD в профиле. Домен проверяет старый пароль и свою политику паролей; нужен LDAPS.
    pub ad_password_change: bool,
}

/// Частные сети. Сам сервер (127.0.0.1) — не «внутри»: так ходит прокси на этом же сервере, и без его
/// заголовков все клиенты из интернета выглядели бы своими.
pub const DEFAULT_INTERNAL_NETWORKS: [&str; 4] = ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "fc00::/7"];

impl Default for SecuritySettings {
    fn default() -> Self {
        Self {
            session_idle_hours: 8,
            session_absolute_days: 7,
            login_max_failures_per_login: 5,
            login_max_failures_per_ip: 20,
            login_window_minutes: 5,
            ad_outage_policy: AdOutagePolicy::Block,
            ad_outage_grace_minutes: 30,
            internal_networks: DEFAULT_INTERNAL_NETWORKS.iter().map(|s| (*s).to_string()).collect(),
            totp_required_roles: Vec::new(),
            kerberos_login: false,
            ad_password_change: false,
        }
    }
}

impl SettingsGroup for SecuritySettings {
    const KEY: &'static str = "security";
}

impl SecuritySettings {
    /// Пределы, за которые администратор выйти не может.
    pub fn validate(&self) -> Result<(), String> {
        let check = |ok: bool, what: &str| if ok { Ok(()) } else { Err(what.to_string()) };
        check((1..=72).contains(&self.session_idle_hours), "бездействие — от 1 до 72 часов")?;
        check((1..=30).contains(&self.session_absolute_days), "срок сеанса — от 1 до 30 дней")?;
        check((1..=20).contains(&self.login_max_failures_per_login), "попыток на логин — от 1 до 20")?;
        check((1..=200).contains(&self.login_max_failures_per_ip), "попыток с IP — от 1 до 200")?;
        check((1..=60).contains(&self.login_window_minutes), "окно — от 1 до 60 минут")?;
        check((1..=480).contains(&self.ad_outage_grace_minutes), "работа без домена — от 1 до 480 минут")?;
        check(self.internal_networks.len() <= 64, "внутренних сетей — не больше 64")?;
        check(
            self.totp_required_roles.len() <= 64
                && self.totp_required_roles.iter().all(|r| !r.is_empty() && r.len() <= 64),
            "роли с обязательным кодом записаны неверно",
        )?;
        for n in &self.internal_networks {
            match n.trim().parse::<IpNetwork>() {
                // Сеть шире /8 — почти весь интернет «внутри», и второй фактор не спрашивается ни у кого.
                // fc00::/7 — все частные адреса IPv6, уже её не бывает.
                Ok(net) if net.prefix() < if net.is_ipv4() { 8 } else { 7 } => {
                    return Err("внутренняя сеть слишком широкая: нужна /8 или уже".into());
                }
                Ok(_) => {}
                Err(_) => return Err("внутренняя сеть записана неверно (пример: 10.0.0.0/8)".into()),
            }
        }
        Ok(())
    }

    /// Адрес из сети организации. Неразборчивые строки пропускаются (`validate` их не пускает).
    pub fn is_internal(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        self.internal_networks.iter().filter_map(|n| n.trim().parse::<IpNetwork>().ok()).any(|n| n.contains(ip))
    }
}

/// Какое поле ID-токена OIDC — логин сотрудника.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OidcClaim {
    /// `preferred_username`: логин AD (`ivanov`) или UPN (`ivanov@corp.example`).
    #[default]
    PreferredUsername,
    /// `upn` (ADFS, Entra ID).
    Upn,
    /// `email` — только подтверждённый (`email_verified` не `false`).
    Email,
}

/// Вход через OpenID Connect (админка → «Безопасность»). Секрет клиента — отдельно и зашифрованным.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct OidcSettings {
    pub enabled: bool,
    /// Издатель, например `https://sso.example.local/realms/corp`: настройки берутся из
    /// `<издатель>/.well-known/openid-configuration`.
    pub issuer: String,
    pub client_id: String,
    /// Адрес возврата, зарегистрированный у провайдера: `https://<портал>/api/auth/oidc/callback`.
    pub redirect_uri: String,
    pub username_claim: OidcClaim,
    /// Надпись на кнопке входа; пусто — «Войти через SSO».
    pub button_label: String,
    /// Вход через провайдера и из интернета. Тем, кому снаружи нужен код из приложения, он всё равно
    /// закрыт: они входят паролем и кодом.
    pub allow_external: bool,
}

impl Default for OidcSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer: String::new(),
            client_id: String::new(),
            redirect_uri: String::new(),
            username_claim: OidcClaim::PreferredUsername,
            button_label: String::new(),
            allow_external: false,
        }
    }
}

impl SettingsGroup for OidcSettings {
    const KEY: &'static str = "oidc";
}

impl OidcSettings {
    /// Изменилось, кому уходит секрет клиента: тогда сохранённый секрет не используется.
    pub fn endpoint_changed(&self, new: &Self) -> bool {
        let iss = |s: &str| s.trim().trim_end_matches('/').to_lowercase();
        iss(&self.issuer) != iss(&new.issuer) || self.client_id.trim() != new.client_id.trim()
    }
}

/// Секрет клиента OIDC: base64(шифротекст). Нет — публичный клиент (только PKCE).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OidcSecret {
    pub sealed: Option<String>,
}

impl SettingsGroup for OidcSecret {
    const KEY: &'static str = "oidc_secret";
}

/// Помощник (админка → «Помощник»): своя модель на отдельном сервере за OpenAI-совместимым API
/// (vLLM, Ollama, LM Studio и другие). Ключ API — отдельно и зашифрованным.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct AssistSettings {
    pub enabled: bool,
    /// Адрес API без `/chat/completions`, например `http://llm.example.local:8000/v1`.
    pub base_url: String,
    /// Имя модели на сервере, например `qwen2.5-14b-instruct`.
    pub model: String,
    /// Сколько ждать ответа модели, секунд.
    pub timeout_secs: u32,
    /// Предел длины ответа, токенов.
    pub max_tokens: u32,
}

impl Default for AssistSettings {
    fn default() -> Self {
        Self { enabled: false, base_url: String::new(), model: String::new(), timeout_secs: 60, max_tokens: 1024 }
    }
}

impl SettingsGroup for AssistSettings {
    const KEY: &'static str = "assist";
}

impl AssistSettings {
    /// Изменилось, куда уходит ключ API: тогда сохранённый ключ не используется.
    pub fn endpoint_changed(&self, new: &Self) -> bool {
        let url = |s: &str| s.trim().trim_end_matches('/').to_lowercase();
        url(&self.base_url) != url(&new.base_url)
    }
}

/// Ключ API помощника: base64(шифротекст). Нет — сервер модели без ключа.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AssistSecret {
    pub sealed: Option<String>,
}

impl SettingsGroup for AssistSecret {
    const KEY: &'static str = "assist_secret";
}

/// Язык установки (админка → «Система»): для новых пользователей и писем на адреса без учётки.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct LocaleSettings {
    /// `ru`, `en` или `zh`.
    pub default_locale: String,
}

impl Default for LocaleSettings {
    fn default() -> Self {
        Self { default_locale: "ru".into() }
    }
}

impl SettingsGroup for LocaleSettings {
    const KEY: &'static str = "locale";
}

/// Состояние связи с доменом — пишет проверка доступности AD, читает проверка сеанса.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct AdHealth {
    /// Неудачных проверок подряд.
    pub failures: u32,
    /// С какого момента нет связи (RFC 3339), если нет.
    pub down_since: Option<String>,
    /// Сеансы приостановлены (3 неудачи подряд при политике `block` или истёк `grace`).
    pub suspended: bool,
}

impl SettingsGroup for AdHealth {
    const KEY: &'static str = "ad_health";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_defaults() {
        let s: SecuritySettings = serde_json::from_str(r#"{"session_idle_hours": 4}"#).unwrap_or_default();
        assert_eq!(s.session_idle_hours, 4);
        assert_eq!(s.session_absolute_days, 7);
        assert_eq!(s.ad_outage_policy, AdOutagePolicy::Block);
        assert!(s.validate().is_ok());
        assert!(SecuritySettings { session_idle_hours: 0, ..s.clone() }.validate().is_err());
        assert_eq!(s.internal_networks.len(), DEFAULT_INTERNAL_NETWORKS.len(), "старые настройки — частные сети");
        assert!(s.totp_required_roles.is_empty(), "код по умолчанию никому не обязателен");
        let roles = |r: Vec<String>| SecuritySettings { totp_required_roles: r, ..s.clone() };
        assert!(roles(vec!["engineer".into(), "superadmin".into()]).validate().is_ok());
        assert!(roles(vec![String::new()]).validate().is_err());
        assert!(roles(vec!["x".repeat(65)]).validate().is_err());
    }

    #[test]
    fn internal_networks() {
        let s = SecuritySettings::default();
        for ip in ["10.20.0.32", "192.168.1.5", "172.20.0.1", "fd00::5", "::ffff:10.1.2.3"] {
            assert!(s.is_internal(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "203.0.113.7", "172.32.0.1", "2a00:1450::1", "127.0.0.1", "::1"] {
            assert!(!s.is_internal(ip.parse().unwrap()), "{ip}");
        }
        let only = SecuritySettings { internal_networks: vec!["10.20.0.0/16".into()], ..s.clone() };
        assert!(only.is_internal("10.20.1.1".parse().unwrap()));
        assert!(!only.is_internal("10.21.1.1".parse().unwrap()));
        for bad in ["0.0.0.0/0", "::/0", "10.0.0.0/7", "сеть", "10.0.0.300/8"] {
            let s = SecuritySettings { internal_networks: vec![bad.into()], ..s.clone() };
            assert!(s.validate().is_err(), "{bad}");
        }
    }
}
