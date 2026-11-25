//! Ошибки AD и разбор кодов `data XXX` из ответа на bind.
//!
//! AD отвечает на неудачный bind кодом 49 (invalidCredentials) и подробностью в тексте:
//! `80090308: LdapErr: DSID-0C09044E, comment: AcceptSecurityContext error, data 52e, v4563`.

use crate::config::ConfigError;
use std::fmt;

/// Причина отказа во входе — то, что увидит пользователь.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindFailure {
    /// 52e — неверный пароль; 525 — нет такого пользователя (наружу не различаем).
    InvalidCredentials,
    /// 530 — вход запрещён в это время.
    LogonHours,
    /// 531 — вход запрещён с этого компьютера.
    Workstation,
    /// 532 — истёк срок пароля.
    PasswordExpired,
    /// 533 — учётная запись отключена.
    AccountDisabled,
    /// 701 — истёк срок действия учётной записи.
    AccountExpired,
    /// 773 — нужно сменить пароль.
    MustChangePassword,
    /// 775 — учётная запись заблокирована.
    AccountLocked,
    /// Другой код — показываем общий текст, в журнал пишем код.
    Other,
}

impl BindFailure {
    /// Разбирает текст ошибки AD. Неизвестный или отсутствующий код → [`BindFailure::Other`].
    pub fn from_ad_message(text: &str) -> Self {
        let code = text
            .split(',')
            .map(str::trim)
            .find_map(|part| part.strip_prefix("data "))
            .map(|c| c.trim().to_ascii_lowercase());
        match code.as_deref() {
            Some("52e") | Some("525") => Self::InvalidCredentials,
            Some("530") => Self::LogonHours,
            Some("531") => Self::Workstation,
            Some("532") => Self::PasswordExpired,
            Some("533") => Self::AccountDisabled,
            Some("701") => Self::AccountExpired,
            Some("773") => Self::MustChangePassword,
            Some("775") => Self::AccountLocked,
            _ => Self::Other,
        }
    }

    /// Текст для экрана входа. Не раскрывает, существует ли логин.
    pub fn user_message(self) -> &'static str {
        match self {
            Self::InvalidCredentials | Self::Other => "Неверный логин или пароль.",
            Self::LogonHours => "Вход в это время запрещён. Обратитесь в IT-службу.",
            Self::Workstation => "Вход с этого компьютера запрещён. Обратитесь в IT-службу.",
            Self::PasswordExpired | Self::MustChangePassword => "Истёк срок пароля. Смените пароль на компьютере.",
            Self::AccountDisabled => "Учётная запись отключена. Обратитесь в IT-службу.",
            Self::AccountExpired => "Срок действия учётной записи истёк. Обратитесь в IT-службу.",
            Self::AccountLocked => "Учётная запись заблокирована. Обратитесь в IT-службу.",
        }
    }
}

/// Ошибки клиента AD. Ни один вариант не содержит пароль.
#[derive(Debug, thiserror::Error)]
pub enum AdError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Новый пароль отклонён политикой домена: длина, сложность, история или минимальный срок.
    #[error("пароль не подходит под политику домена")]
    PasswordPolicy,
    /// Не ответил ни один контроллер.
    #[error("нет связи ни с одним контроллером домена: {0}")]
    Unavailable(UnavailableReport),
    /// Отказ во входе с понятной причиной.
    #[error("вход отклонён: {0:?}")]
    Bind(BindFailure),
    /// Служебная учётка не смогла войти — ошибка настройки портала, не пользователя.
    #[error("служебная учётка не вошла в домен (код LDAP {rc}): {text}")]
    ServiceBind { rc: u32, text: String },
    #[error("ошибка LDAP (код {rc}): {text}")]
    Ldap { rc: u32, text: String },
}

/// Что случилось с каждым контроллером при попытке подключения.
#[derive(Debug, Default)]
pub struct UnavailableReport(pub Vec<(String, String)>);

impl fmt::Display for UnavailableReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.0.iter().map(|(dc, e)| format!("{dc} — {e}")).collect();
        f.write_str(&parts.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(code: &str) -> String {
        format!("80090308: LdapErr: DSID-0C09044E, comment: AcceptSecurityContext error, data {code}, v4563\0")
    }

    #[test]
    fn maps_ad_codes() {
        let cases = [
            ("52e", BindFailure::InvalidCredentials),
            ("525", BindFailure::InvalidCredentials),
            ("530", BindFailure::LogonHours),
            ("531", BindFailure::Workstation),
            ("532", BindFailure::PasswordExpired),
            ("533", BindFailure::AccountDisabled),
            ("701", BindFailure::AccountExpired),
            ("773", BindFailure::MustChangePassword),
            ("775", BindFailure::AccountLocked),
            ("999", BindFailure::Other),
        ];
        for (code, expected) in cases {
            assert_eq!(BindFailure::from_ad_message(&msg(code)), expected, "code {code}");
        }
    }

    #[test]
    fn samba_and_empty_messages_fall_back() {
        // Samba отвечает без `data XXX`.
        assert_eq!(BindFailure::from_ad_message("Invalid credentials"), BindFailure::Other);
        assert_eq!(BindFailure::from_ad_message(""), BindFailure::Other);
    }

    #[test]
    fn unknown_user_looks_like_bad_password() {
        assert_eq!(
            BindFailure::from_ad_message(&msg("525")).user_message(),
            BindFailure::from_ad_message(&msg("52e")).user_message()
        );
    }
}
