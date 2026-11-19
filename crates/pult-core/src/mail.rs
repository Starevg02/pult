//! Почта через очередь `outbox_emails`: письмо сначала записывается в БД — в той же
//! транзакции, что и событие, — а задача `mail.send` отправляет его по SMTP с повторами.
//! Только STARTTLS или TLS; без шифрования отправка невозможна (как и для AD).

use crate::{Secret, crypto::SecretBox, settings::SettingsGroup};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{
        Mailbox, MultiPart,
        header::{ContentType, Header, HeaderName, HeaderValue},
    },
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
    },
};
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};
use std::time::Duration;
use utoipa::ToSchema;
use uuid::Uuid;

/// После стольких неудач письмо помечается `failed`.
pub const MAX_ATTEMPTS: i32 = 8;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SmtpSecurity {
    /// Порт 587: соединение без шифрования, затем обязательный STARTTLS.
    #[default]
    Starttls,
    /// Порт 465: TLS с первого байта.
    Tls,
    /// Без шифрования — только при `PULT_INSECURE_MAIL=1` (разработка, CI).
    None,
}

/// Настройки SMTP (админка → «Почта»). Пароль — отдельно и зашифрованным.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct MailSettings {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub security: SmtpSecurity,
    /// Пусто — без входа (релей по адресу).
    pub username: String,
    /// Адрес отправителя, например `helpdesk@example.local`.
    pub from_address: String,
    pub from_name: String,
}

impl Default for MailSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 587,
            security: SmtpSecurity::Starttls,
            username: String::new(),
            from_address: String::new(),
            from_name: "Pult".into(),
        }
    }
}

impl SettingsGroup for MailSettings {
    const KEY: &'static str = "mail";
}

impl MailSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        if self.host.trim().is_empty() || self.host.contains(['/', ' ', ':']) {
            return Err("Укажите имя SMTP-сервера без схемы и порта.".into());
        }
        if self.port == 0 {
            return Err("Неверный порт.".into());
        }
        self.from_address.parse::<lettre::Address>().map_err(|_| "Адрес отправителя не похож на email.".to_string())?;
        Ok(())
    }

    /// Изменилось, куда и под кем уходит пароль: тогда сохранённый пароль не используется.
    pub fn endpoint_changed(&self, new: &Self) -> bool {
        !self.host.trim().eq_ignore_ascii_case(new.host.trim())
            || self.port != new.port
            || self.security != new.security
            || self.username.trim() != new.username.trim()
    }

    fn from(&self) -> Result<Mailbox, String> {
        let addr = self.from_address.parse::<lettre::Address>().map_err(|e| e.to_string())?;
        Ok(Mailbox::new((!self.from_name.trim().is_empty()).then(|| self.from_name.clone()), addr))
    }
}

/// Пароль SMTP: base64(шифротекст).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MailSecret {
    pub sealed: Option<String>,
}

impl SettingsGroup for MailSecret {
    const KEY: &'static str = "mail_secret";
}

/// Заголовки цепочки: почтовые клиенты собирают переписку, ответ возвращается в заявку.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadHeaders {
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
}

/// Письмо с HTML-версией и заголовками цепочки.
#[derive(Debug, Clone, Default)]
pub struct Outgoing {
    pub to: Vec<String>,
    pub subject: String,
    pub text: String,
    pub html: Option<String>,
    pub headers: ThreadHeaders,
}

pub async fn enqueue_message(db: impl PgExecutor<'_>, m: &Outgoing) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO outbox_emails (to_addrs, subject, body_text, body_html, headers) VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(&m.to)
    .bind(m.subject.chars().take(250).collect::<String>())
    .bind(&m.text)
    .bind(&m.html)
    .bind(serde_json::to_value(&m.headers).unwrap_or_default())
    .fetch_one(db)
    .await
}

/// Письмо в очередь. Вызывается в транзакции события — письмо уйдёт, только если событие сохранилось.
pub async fn enqueue(
    db: impl PgExecutor<'_>,
    to: &[String],
    subject: &str,
    body_text: &str,
) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar("INSERT INTO outbox_emails (to_addrs, subject, body_text) VALUES ($1, $2, $3) RETURNING id")
        .bind(to)
        .bind(subject.chars().take(250).collect::<String>())
        .bind(body_text)
        .fetch_one(db)
        .await
}

/// Письмо всем активным суперадминистраторам с адресом (сбой синхронизации AD и т. п.).
pub async fn enqueue_superadmins(db: &PgPool, subject: &str, body_text: &str) -> Result<Option<Uuid>, sqlx::Error> {
    let to: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT coalesce(p.email, a.mail) FROM users u
           JOIN user_roles ur ON ur.user_id = u.id JOIN roles r ON r.id = ur.role_id AND r.code = 'superadmin'
           JOIN ad_accounts a ON a.id = u.ad_account_id LEFT JOIN persons p ON p.id = u.person_id
          WHERE u.is_active AND coalesce(p.email, a.mail) LIKE '%@%'",
    )
    .fetch_all(db)
    .await?;
    if to.is_empty() {
        return Ok(None);
    }
    enqueue(db, &to, subject, body_text).await.map(Some)
}

fn transport(s: &MailSettings, password: Option<Secret>) -> Result<AsyncSmtpTransport<Tokio1Executor>, String> {
    let tls = TlsParameters::builder(s.host.clone()).build_rustls().map_err(|e| e.to_string())?;
    let mut b = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&s.host)
        .port(s.port)
        .timeout(Some(Duration::from_secs(20)))
        .tls(match s.security {
            SmtpSecurity::Starttls => Tls::Required(tls),
            SmtpSecurity::Tls => Tls::Wrapper(tls),
            SmtpSecurity::None => Tls::None,
        });
    if !s.username.is_empty() {
        let p = password.map(|p| p.expose().to_string()).unwrap_or_default();
        b = b.credentials(Credentials::new(s.username.clone(), p));
    }
    Ok(b.build())
}

/// Автоматическое письмо (RFC 3834): почтовые роботы не отвечают на него, петель не будет.
#[derive(Clone)]
struct AutoSubmitted;

impl Header for AutoSubmitted {
    fn name() -> HeaderName {
        HeaderName::new_from_ascii_str("Auto-Submitted")
    }
    fn parse(_: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self)
    }
    fn display(&self) -> HeaderValue {
        HeaderValue::new(Self::name(), "auto-generated".to_string())
    }
}

fn message(
    s: &MailSettings,
    to: &[String],
    subject: &str,
    text: &str,
    html: Option<&str>,
    h: &ThreadHeaders,
) -> Result<Message, String> {
    let mut m = Message::builder().from(s.from()?).subject(subject).header(AutoSubmitted);
    for t in to {
        m = m.to(t.parse::<Mailbox>().map_err(|e| format!("{t}: {e}"))?);
    }
    if let Some(id) = &h.message_id {
        m = m.message_id(Some(id.clone()));
    }
    if let Some(v) = &h.in_reply_to {
        m = m.in_reply_to(v.clone());
    }
    if let Some(v) = &h.references {
        m = m.references(v.clone());
    }
    match html {
        Some(h) => {
            m.multipart(MultiPart::alternative_plain_html(text.to_string(), h.to_string())).map_err(|e| e.to_string())
        }
        None => m.header(ContentType::TEXT_PLAIN).body(text.to_string()).map_err(|e| e.to_string()),
    }
}

pub async fn password(db: &PgPool, secrets: &SecretBox) -> Result<Option<Secret>, String> {
    use base64::Engine;
    let s: MailSecret = crate::settings::load(db).await.map_err(|e| e.to_string())?;
    match s.sealed {
        Some(b) => {
            let raw = base64::engine::general_purpose::STANDARD.decode(b).map_err(|e| e.to_string())?;
            secrets.decrypt(&raw).map(Some).map_err(|e| e.to_string())
        }
        None => Ok(None),
    }
}

/// Отправить одно письмо сразу (кнопка «Проверить»). Ошибка — текст без пароля.
pub async fn send_now(
    s: &MailSettings,
    password: Option<Secret>,
    to: &[String],
    subject: &str,
    text: &str,
) -> Result<(), String> {
    let t = transport(s, password)?;
    let m = message(s, to, subject, text, None, &ThreadHeaders::default())?;
    t.send(m).await.map(|_| ()).map_err(|e| e.to_string())
}

#[derive(sqlx::FromRow)]
struct Queued {
    id: Uuid,
    to_addrs: Vec<String>,
    subject: String,
    body_text: String,
    body_html: Option<String>,
    headers: serde_json::Value,
    attempts: i32,
}

/// Сколько времени одна пачка может отправлять письма (меньше времени задачи `mail.send`).
const SEND_BUDGET: Duration = Duration::from_secs(150);

/// Задача `mail.send`: отправить накопившиеся письма. Несколько копий сервера не берут одно письмо.
pub async fn send_due(db: &PgPool, secrets: &SecretBox) -> Result<usize, String> {
    let s: MailSettings = crate::settings::load(db).await.map_err(|e| e.to_string())?;
    if !s.enabled {
        return Ok(0);
    }
    // Зависшие в «отправляется» (сервер упал посреди отправки) — вернуть в очередь.
    sqlx::query("UPDATE outbox_emails SET status = 'queued' WHERE status = 'sending' AND next_attempt_at < now() - interval '10 minutes'")
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    let batch: Vec<Queued> = sqlx::query_as(
        "UPDATE outbox_emails SET status = 'sending', next_attempt_at = now()
          WHERE id IN (SELECT id FROM outbox_emails WHERE status = 'queued' AND next_attempt_at <= now()
                        ORDER BY created_at LIMIT 50 FOR UPDATE SKIP LOCKED)
      RETURNING id, to_addrs, subject, body_text, body_html, headers, attempts",
    )
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    if batch.is_empty() {
        return Ok(0);
    }
    let pw = password(db, secrets).await?;
    let t = transport(&s, pw);
    let mut sent = 0;
    // Почтовый сервер «висит» (20 с на письмо): пачка не должна пережить время задачи (240 с), иначе
    // её прервут посреди отправки. Неотправленные к сроку письма сразу возвращаются в очередь.
    let deadline = std::time::Instant::now() + SEND_BUDGET;
    let mut rest = batch.into_iter();
    while let Some(q) = rest.next() {
        if std::time::Instant::now() >= deadline {
            let ids: Vec<Uuid> = std::iter::once(q.id).chain(rest.map(|q| q.id)).collect();
            tracing::warn!(left = ids.len(), "почтовый сервер отвечает медленно — остальные письма в следующий раз");
            sqlx::query("UPDATE outbox_emails SET status = 'queued' WHERE id = ANY($1) AND status = 'sending'")
                .bind(&ids)
                .execute(db)
                .await
                .map_err(|e| e.to_string())?;
            break;
        }
        let h: ThreadHeaders = serde_json::from_value(q.headers.clone()).unwrap_or_default();
        let r = match (&t, message(&s, &q.to_addrs, &q.subject, &q.body_text, q.body_html.as_deref(), &h)) {
            (Ok(t), Ok(m)) => t.send(m).await.map(|_| ()).map_err(|e| e.to_string()),
            (Err(e), _) => Err(e.clone()),
            (_, Err(e)) => Err(e),
        };
        match r {
            Ok(()) => {
                sent += 1;
                sqlx::query("UPDATE outbox_emails SET status = 'sent', sent_at = now(), attempts = attempts + 1, last_error = NULL WHERE id = $1")
                    .bind(q.id)
                    .execute(db)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Err(e) => {
                let attempts = q.attempts + 1;
                tracing::warn!(id = %q.id, attempts, error = %e, "письмо не отправлено");
                // Повтор через 1, 2, 4… минут, не реже раза в час.
                let delay = 2_i64.saturating_pow(u32::try_from(attempts.min(6)).unwrap_or(6)).min(60);
                sqlx::query(
                    "UPDATE outbox_emails SET status = CASE WHEN $2 >= $4 THEN 'failed' ELSE 'queued' END, attempts = $2,
                            last_error = $3, next_attempt_at = now() + make_interval(mins => $5)
                      WHERE id = $1",
                )
                .bind(q.id)
                .bind(attempts)
                .bind(e.chars().take(500).collect::<String>())
                .bind(MAX_ATTEMPTS)
                .bind(i32::try_from(delay).unwrap_or(60))
                .execute(db)
                .await
                .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_headers_and_html_are_sent() {
        let s = MailSettings { from_address: "helpdesk@example.test".into(), ..Default::default() };
        let h = ThreadHeaders {
            message_id: Some("<ticket-1.a@example.test>".into()),
            in_reply_to: Some("<ticket-1@example.test>".into()),
            references: Some("<ticket-1@example.test>".into()),
        };
        let m = message(&s, &["a@example.test".into()], "[#1] x", "текст", Some("<p>html</p>"), &h).unwrap();
        let out = String::from_utf8_lossy(&m.formatted()).to_string();
        assert!(out.contains("Message-ID: <ticket-1.a@example.test>"), "{out}");
        assert!(
            out.contains("In-Reply-To: <ticket-1@example.test>") && out.contains("References: <ticket-1@example.test>"),
            "{out}"
        );
        assert!(out.contains("Auto-Submitted: auto-generated"), "{out}");
        assert!(out.contains("multipart/alternative"), "{out}");
    }

    #[test]
    fn endpoint_change_detected() {
        let a = MailSettings { host: "smtp.example.test".into(), username: "pult".into(), ..Default::default() };
        let same =
            MailSettings { host: " SMTP.example.test ".into(), from_name: "Другое имя".into(), ..a.clone() };
        assert!(!a.endpoint_changed(&same), "имя отправителя и регистр хоста — не смена сервера");
        assert!(a.endpoint_changed(&MailSettings { host: "evil.example.test".into(), ..a.clone() }));
        assert!(a.endpoint_changed(&MailSettings { port: 25, ..a.clone() }));
        assert!(a.endpoint_changed(&MailSettings { security: SmtpSecurity::Tls, ..a.clone() }));
        assert!(a.endpoint_changed(&MailSettings { username: "other".into(), ..a.clone() }));
    }

    #[test]
    fn validation() {
        let mut s = MailSettings {
            enabled: true,
            host: "smtp.example.test".into(),
            from_address: "helpdesk@example.test".into(),
            ..Default::default()
        };
        assert!(s.validate().is_ok());
        s.host = "smtp://x".into();
        assert!(s.validate().is_err());
        s.host = "smtp.example.test".into();
        s.from_address = "nope".into();
        assert!(s.validate().is_err());
        assert!(MailSettings::default().validate().is_ok(), "выключенная почта не проверяется");
    }

    #[test]
    fn message_builds_with_cyrillic() {
        let s = MailSettings {
            from_address: "helpdesk@example.test".into(),
            from_name: "IT-поддержка".into(),
            ..Default::default()
        };
        let m =
            message(&s, &["ivanov@example.test".into()], "Синхронизация AD", "Текст", None, &ThreadHeaders::default())
                .unwrap();
        let raw = String::from_utf8(m.formatted()).unwrap();
        assert!(raw.contains("To: ivanov@example.test"));
        assert!(message(&s, &["bad address".into()], "x", "y", None, &ThreadHeaders::default()).is_err());
    }
}
