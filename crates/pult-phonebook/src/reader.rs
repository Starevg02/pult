//! Чтение PhonebookDB с SQL Server через `tiberius` (работает и с Linux).
//!
//! Только чтение. Подключение — SQL-логин только на чтение (вопрос 12) или, позже, Kerberos.
//! TLS: проверка сертификата сервера; для внутреннего CA — `ca_cert`. Отключить проверку нельзя
//! (Phonebook 1.0 подключается с `TrustServerCertificate=True` — в Pult так не делаем).

use crate::{LegacyAudit, LegacyEmployee, Snapshot};
use pult_core::Secret;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel, Row};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("строка подключения: {0}")]
    BadConnectionString(String),
    #[error("строка подключения просит не проверять сертификат (TrustServerCertificate=True); укажите CA сервера")]
    TrustServerCertificate,
    #[error("нет связи с SQL Server: {0}")]
    Network(String),
    #[error("ошибка SQL Server: {0}")]
    Sql(String),
}

impl From<tiberius::error::Error> for ReadError {
    fn from(e: tiberius::error::Error) -> Self {
        Self::Sql(e.to_string())
    }
}

/// Как входить в SQL Server.
#[derive(Debug, Clone)]
pub enum Login {
    /// SQL-логин только на чтение.
    Sql { user: String, password: Secret },
    /// Windows-вход без пароля. На Windows — учётка текущего пользователя (SSPI). На Linux —
    /// Kerberos через GSSAPI: билет из кеша (`kinit`) или из keytab служебной учётки
    /// (`KRB5_CLIENT_KTNAME=/etc/pult/pult-sql.keytab`); у SQL Server должен быть SPN
    /// `MSSQLSvc/<сервер>:<порт>`, иначе Kerberos не сработает (NTLM с Linux не используется).
    Windows,
}

/// Параметры подключения.
#[derive(Debug, Clone)]
pub struct Source {
    /// Строка ADO.NET без пароля, например `Server=tcp:192.0.2.10,1433;Database=PhonebookDB;Encrypt=true`.
    pub ado: String,
    pub login: Login,
    /// PEM-сертификат CA SQL Server, если он не в системном хранилище.
    pub ca_cert: Option<std::path::PathBuf>,
    /// Не проверять сертификат SQL Server. Только для разового чтения (`--dry-run`) с сервера
    /// с самоподписанным сертификатом; задаётся явно флагом, не строкой подключения.
    pub skip_tls_verify: bool,
}

/// Ключи строки подключения, которых быть не должно.
const FORBIDDEN_KEYS: [&str; 2] = ["password=", "pwd="];

/// Настройки tiberius. Отдельно от подключения — проверяется тестами.
pub fn config(src: &Source) -> Result<Config, ReadError> {
    let lower = src.ado.to_ascii_lowercase().replace(' ', "");
    if lower.contains("trustservercertificate=true") || lower.contains("trustservercertificate=yes") {
        return Err(ReadError::TrustServerCertificate);
    }
    // Пароль в строке подключения не принимаем: он попал бы в историю оболочки и список процессов.
    if FORBIDDEN_KEYS.iter().any(|k| lower.contains(k)) {
        return Err(ReadError::BadConnectionString(
            "пароль не передаётся в строке подключения — используйте переменную PULT_PHONEBOOK_PASSWORD".into(),
        ));
    }
    let mut cfg = Config::from_ado_string(&src.ado).map_err(|e| ReadError::BadConnectionString(e.to_string()))?;
    cfg.encryption(EncryptionLevel::Required);
    match &src.login {
        Login::Sql { user, password } => cfg.authentication(AuthMethod::sql_server(user, password.expose())),
        Login::Windows => cfg.authentication(AuthMethod::Integrated),
    }
    match (&src.ca_cert, src.skip_tls_verify) {
        (Some(_), true) => {
            return Err(ReadError::BadConnectionString(
                "--ca-cert и отключение проверки сертификата взаимоисключающие".into(),
            ));
        }
        (Some(ca), false) => cfg.trust_cert_ca(ca.display()),
        (None, true) => cfg.trust_cert(),
        (None, false) => {}
    }
    cfg.application_name("Pult import (read-only)");
    Ok(cfg)
}

async fn connect(src: &Source) -> Result<Client<Compat<TcpStream>>, ReadError> {
    let cfg = config(src)?;
    let tcp = TcpStream::connect(cfg.get_addr()).await.map_err(|e| ReadError::Network(e.to_string()))?;
    tcp.set_nodelay(true).map_err(|e| ReadError::Network(e.to_string()))?;
    Ok(Client::connect(cfg, tcp.compat_write()).await?)
}

/// Читает всё, что нужно для переноса. Поддерживает старые установки без `Email`, архива и аудита.
pub async fn read(src: &Source) -> Result<Snapshot, ReadError> {
    let mut c = connect(src).await?;
    let flags = one_row(
        &mut c,
        "SELECT CAST(CASE WHEN COL_LENGTH('dbo.Employees','Email') IS NULL THEN 0 ELSE 1 END AS bit),
                CAST(CASE WHEN COL_LENGTH('dbo.Employees','IsArchived') IS NULL THEN 0 ELSE 1 END AS bit),
                CAST(CASE WHEN OBJECT_ID('dbo.PhonebookAudit','U') IS NULL THEN 0 ELSE 1 END AS bit)",
    )
    .await?;
    let has_email = flags.get::<bool, _>(0).unwrap_or(false);
    let has_archive = flags.get::<bool, _>(1).unwrap_or(false);
    let has_audit = flags.get::<bool, _>(2).unwrap_or(false);

    let departments =
        pairs(&mut c, "SELECT DepartmentID, DepartmentName FROM dbo.Departments ORDER BY DepartmentID").await?;
    let positions = pairs(&mut c, "SELECT PositionID, PositionName FROM dbo.Positions ORDER BY PositionID").await?;

    // Имена столбцов фиксированы, необязательные подставляются константами — ввода пользователя в SQL нет.
    let email = if has_email { "e.Email" } else { "CAST(NULL AS nvarchar(254))" };
    let archived = if has_archive { "e.IsArchived" } else { "CAST(0 AS bit)" };
    let sql = format!(
        "SELECT e.EmployeeID, e.LastName, e.FirstName, e.MiddleName, e.DepartmentID, e.PositionID, \
                e.WorkPhone, e.InternalPhone, e.MobilePhone, {email}, {archived} \
         FROM dbo.Employees AS e ORDER BY e.EmployeeID" // SAFE: подставляются только константы выше
    );
    let rows = c.simple_query(sql).await?.into_first_result().await?;
    let text = |r: &Row, i: usize| r.get::<&str, _>(i).unwrap_or_default().to_string();
    let employees = rows
        .iter()
        .map(|r| LegacyEmployee {
            id: r.get::<i32, _>(0).unwrap_or_default(),
            last_name: text(r, 1),
            first_name: text(r, 2),
            middle_name: text(r, 3),
            department_id: r.get::<i32, _>(4),
            position_id: r.get::<i32, _>(5),
            work_phone: text(r, 6),
            internal_phone: text(r, 7),
            mobile_phone: text(r, 8),
            email: text(r, 9),
            is_archived: r.get::<bool, _>(10).unwrap_or(false),
        })
        .collect();

    // Журнал Phonebook: время — строкой ISO 8601, XML — текстом (типы столбцов в установках разные).
    let audit: Vec<LegacyAudit> = if has_audit {
        let rows = c
            .simple_query(
                "SELECT CAST(AuditId AS bigint), CONVERT(nvarchar(33), OccurredAt, 126), ActionName, EntityName, \
                        SubjectName, LoginName, ApplicationName, \
                        CAST(BeforeXml AS nvarchar(max)), CAST(AfterXml AS nvarchar(max)) \
                 FROM dbo.PhonebookAudit ORDER BY AuditId",
            )
            .await?
            .into_first_result()
            .await?;
        let text = |r: &Row, i: usize| r.get::<&str, _>(i).unwrap_or_default().to_string();
        rows.iter()
            .map(|r| LegacyAudit {
                id: r.get::<i64, _>(0).unwrap_or_default(),
                occurred_at: text(r, 1),
                action: text(r, 2),
                entity: text(r, 3),
                subject: text(r, 4),
                login: text(r, 5),
                application: text(r, 6),
                before_xml: text(r, 7),
                after_xml: text(r, 8),
            })
            .collect()
    } else {
        Vec::new()
    };
    Ok(Snapshot {
        departments,
        positions,
        employees,
        audit_records: audit.len(),
        audit,
        has_email_column: has_email,
        has_archive_columns: has_archive,
        has_audit_table: has_audit,
    })
}

async fn one_row(c: &mut Client<Compat<TcpStream>>, sql: &str) -> Result<Row, ReadError> {
    c.simple_query(sql).await?.into_row().await?.ok_or_else(|| ReadError::Sql(format!("пустой ответ: {sql}")))
}

async fn pairs(c: &mut Client<Compat<TcpStream>>, sql: &str) -> Result<Vec<(i32, String)>, ReadError> {
    let rows = c.simple_query(sql).await?.into_first_result().await?;
    Ok(rows
        .iter()
        .map(|r| (r.get::<i32, _>(0).unwrap_or_default(), r.get::<&str, _>(1).unwrap_or_default().trim().to_string()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(ado: &str) -> Source {
        Source {
            ado: ado.into(),
            login: Login::Sql { user: "pult_ro".into(), password: Secret::new("ro-secret") },
            ca_cert: None,
            skip_tls_verify: false,
        }
    }

    #[test]
    fn trust_server_certificate_is_refused() {
        let e =
            config(&src("Server=tcp:192.0.2.10,1433;Database=PhonebookDB;TrustServerCertificate=True")).unwrap_err();
        assert!(matches!(e, ReadError::TrustServerCertificate));
    }

    #[test]
    fn password_in_connection_string_is_refused() {
        let e = config(&src("Server=tcp:192.0.2.10,1433;Database=PhonebookDB;User Id=x;Password=y")).unwrap_err();
        assert!(matches!(e, ReadError::BadConnectionString(_)));
    }

    #[test]
    fn valid_config() {
        let cfg = config(&src("Server=tcp:192.0.2.10,1433;Database=PhonebookDB;Encrypt=true")).unwrap();
        assert_eq!(cfg.get_addr(), "192.0.2.10:1433");
        assert!(!format!("{:?}", src("x")).contains("ro-secret"));
    }

    #[test]
    fn skip_tls_verify_is_explicit_and_exclusive_with_ca() {
        let mut s = src("Server=tcp:192.0.2.10,1433;Database=PhonebookDB;Encrypt=true");
        s.skip_tls_verify = true;
        assert!(config(&s).is_ok());
        s.ca_cert = Some("ca.pem".into());
        assert!(matches!(config(&s), Err(ReadError::BadConnectionString(_))));
        // Строкой подключения проверку по-прежнему не отключить.
        let s = src("Server=tcp:192.0.2.10,1433;Database=PhonebookDB;TrustServerCertificate=yes");
        assert!(matches!(config(&s), Err(ReadError::TrustServerCertificate)));
    }
}
