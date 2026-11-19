//! Звонок по клику: настройки, адаптер CallGateway; Asterisk AMI — в `ami.rs`.
//!
//! Вызов идёт с сервера портала, браузер к шлюзу не обращается. Как CallGateway узнаёт
//! звонящего (делегация Kerberos или доверенный вызов) — открытый вопрос плана; адаптер
//! передаёт логин звонящего в теле запроса и, если задан, токен доступа. По умолчанию
//! телефония выключена и кнопка «Позвонить» скрыта.

use pult_core::Secret;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use utoipa::ToSchema;

/// Настройки телефонии (админка → «Телефония»). Токен хранится зашифрованным отдельно.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct TelephonySettings {
    pub enabled: bool,
    /// `ami` — АТС на Asterisk (FreePBX) напрямую; `callgateway` — через CallGateway.
    pub provider: String,
    /// Адрес CallGateway, например `https://callgateway.example.local:8443`.
    pub base_url: String,
    /// Адрес АТС для AMI (имя или IP) и порт (обычно 5038).
    pub ami_host: String,
    pub ami_port: u16,
    /// Пользователь AMI; пароль хранится зашифрованным отдельно, как токен CallGateway.
    pub ami_user: String,
    /// Канал звонящего, `{ext}` — его внутренний номер.
    pub ami_channel: String,
    /// Контекст набора абонента.
    pub ami_context: String,
    /// Шаблон ссылки «Написать письмо»: `mailto:{email}` или адрес веб-почты с `{email}`.
    pub mail_template: String,
}

impl Default for TelephonySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "ami".into(),
            base_url: String::new(),
            ami_host: String::new(),
            ami_port: 5038,
            ami_user: String::new(),
            ami_channel: "Local/{ext}@from-internal".into(),
            ami_context: "from-internal".into(),
            mail_template: "mailto:{email}".into(),
        }
    }
}

impl pult_core::settings::SettingsGroup for TelephonySettings {
    const KEY: &'static str = "telephony";
}

impl TelephonySettings {
    pub fn validate(&self) -> Result<(), String> {
        let plain =
            |v: &str, extra: &str| !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c));
        match self.provider.as_str() {
            "callgateway" if self.enabled && !self.base_url.starts_with("https://") => {
                return Err("Адрес CallGateway — только https://".into());
            }
            "callgateway" => {}
            "ami" if self.enabled => {
                if !plain(&self.ami_host, ".-") {
                    return Err("Адрес АТС — имя или IP без порта.".into());
                }
                if self.ami_port == 0 {
                    return Err("Укажите порт AMI (обычно 5038).".into());
                }
                if !plain(&self.ami_user, "._-@") {
                    return Err("Пользователь AMI — латиница, цифры, . _ - @".into());
                }
                if !self.ami_channel.contains("{ext}") || !plain(&self.ami_channel, "/@._-{}") {
                    return Err("Канал звонящего — например Local/{ext}@from-internal или PJSIP/{ext}.".into());
                }
                if !plain(&self.ami_context, "_-") {
                    return Err("Контекст набора — например from-internal.".into());
                }
            }
            "ami" => {}
            _ => return Err("Неизвестный способ звонка.".into()),
        }
        let t = self.mail_template.trim();
        if !(t.starts_with("mailto:") || t.starts_with("https://")) || !t.contains("{email}") {
            return Err("Шаблон письма — mailto: или https:// с подстановкой {email}".into());
        }
        Ok(())
    }
}

/// Звонить можно только на внутренний номер (3–4 цифры).
pub fn callable(internal_ext: Option<&str>) -> bool {
    internal_ext.is_some_and(|e| (3..=4).contains(&e.len()) && e.chars().all(|c| c.is_ascii_digit()))
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CallError {
    #[error("Телефония не настроена.")]
    Disabled,
    #[error("Звонить можно только на внутренний номер.")]
    NotInternal,
    #[error("Нет права звонить или к учётке не привязана трубка.")]
    Denied,
    #[error("В вашей карточке справочника нет внутреннего номера — АТС не знает, на какой телефон звонить вам.")]
    NoCallerExt,
    #[error("Уже идёт звонок.")]
    Busy,
    #[error("АТС недоступна. Попробуйте позже.")]
    Unavailable,
    #[error("Звонок не найден.")]
    NotFound,
    #[error("Ошибка телефонии: {0}")]
    Other(String),
}

/// Состояние звонка (как в окне звонка Phonebook).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct CallState {
    pub id: String,
    /// `calling`, `connected`, `finished`, `failed` или иное значение шлюза.
    pub state: String,
    pub error: Option<String>,
    /// Звонок завершён или дальше портал его не отслеживает — опрос можно прекратить.
    #[serde(default)]
    pub done: bool,
}

/// Провайдер «звонок по клику».
pub struct CallGateway {
    base: String,
    token: Option<Secret>,
    http: reqwest::Client,
}

impl CallGateway {
    pub fn new(base_url: &str, token: Option<Secret>) -> Result<Self, CallError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .https_only(true)
            .build()
            .map_err(|e| CallError::Other(e.to_string()))?;
        Ok(Self { base: base_url.trim_end_matches('/').to_string(), token, http })
    }

    fn req(&self, r: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.token {
            Some(t) => r.bearer_auth(t.expose()),
            None => r,
        }
    }

    /// Начать звонок от `caller_login` сотруднику с номером Phonebook `employee_id`.
    pub async fn start_call(&self, caller_login: &str, employee_id: i32) -> Result<String, CallError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Body<'a> {
            employee_id: i32,
            caller_sam: &'a str,
        }
        #[derive(Deserialize)]
        struct Started {
            id: serde_json::Value,
        }
        let resp = self
            .req(self.http.post(format!("{}/api/calls", self.base)))
            .json(&Body { employee_id, caller_sam: caller_login })
            .send()
            .await
            .map_err(|_| CallError::Unavailable)?;
        let resp = check(resp)?;
        let s: Started = resp.json().await.map_err(|e| CallError::Other(e.to_string()))?;
        Ok(match s.id {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        })
    }

    pub async fn call_state(&self, id: &str) -> Result<CallState, CallError> {
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(CallError::NotFound);
        }
        let resp = self
            .req(self.http.get(format!("{}/api/calls/{id}", self.base)))
            .send()
            .await
            .map_err(|_| CallError::Unavailable)?;
        let resp = check(resp)?;
        let v: serde_json::Value = resp.json().await.map_err(|e| CallError::Other(e.to_string()))?;
        let state = v.get("state").and_then(|s| s.as_str()).unwrap_or("unknown").to_lowercase();
        Ok(CallState {
            id: id.to_string(),
            done: matches!(state.as_str(), "finished" | "failed" | "ended" | "completed"),
            state,
            error: v.get("error").and_then(|s| s.as_str()).map(str::to_string),
        })
    }
}

/// Коды ответа шлюза → понятная ошибка (как `TelephonyClient.cs`).
fn check(resp: reqwest::Response) -> Result<reqwest::Response, CallError> {
    map_status(resp.status().as_u16()).map(|()| resp)
}

fn map_status(code: u16) -> Result<(), CallError> {
    match code {
        200..=299 => Ok(()),
        401 | 403 => Err(CallError::Denied),
        404 => Err(CallError::NotFound),
        409 => Err(CallError::Busy),
        503 => Err(CallError::Unavailable),
        other => Err(CallError::Other(format!("HTTP {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_internal_numbers() {
        assert!(callable(Some("4512")) && callable(Some("123")));
        assert!(!callable(Some("12")) && !callable(Some("84951234567")) && !callable(None));
    }

    #[test]
    fn status_mapping() {
        assert_eq!(map_status(201), Ok(()));
        assert_eq!(map_status(403), Err(CallError::Denied));
        assert_eq!(map_status(409), Err(CallError::Busy));
        assert_eq!(map_status(503), Err(CallError::Unavailable));
    }

    #[test]
    fn settings_validation() {
        let mut s = TelephonySettings::default();
        assert!(s.validate().is_ok());
        s.enabled = true;
        assert!(s.validate().is_err(), "AMI без адреса");
        s.ami_host = "pbx.example.test".into();
        s.ami_user = "pult".into();
        assert!(s.validate().is_ok());
        s.ami_channel = "PJSIP/{ext}
Action: Command"
            .into();
        assert!(s.validate().is_err(), "перевод строки в канале");
        s.ami_channel = "PJSIP/{ext}".into();
        s.ami_host = "pbx:5038".into();
        assert!(s.validate().is_err(), "порт отдельно");
        s.provider = "callgateway".into();
        s.base_url = "http://gw".into();
        assert!(s.validate().is_err());
        s.base_url = "https://gw".into();
        s.mail_template = "https://mail.example.test/?view=compose&to={email}".into();
        assert!(s.validate().is_ok());
        s.mail_template = "javascript:alert(1)".into();
        assert!(s.validate().is_err());
    }
}
