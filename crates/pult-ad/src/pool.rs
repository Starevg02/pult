//! Пул контроллеров домена: подключение с TLS, вход пользователя, поиск.
//!
//! Вход: поиск учётки служебной записью → bind найденным DN и введённым паролем
//! на первом доступном DC из списка. Рабочий DC запоминается.

use crate::{
    config::{AdConfig, ConfigError, DcEndpoint, TlsMode},
    errors::{AdError, BindFailure, UnavailableReport},
    failover::{Attempt, try_in_order},
};
use ldap3::{Ldap, LdapConnAsync, LdapConnSettings, LdapError, Scope, SearchEntry, ldap_escape};
use pult_core::Secret;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// Код LDAP `invalidCredentials`.
const RC_INVALID_CREDENTIALS: u32 = 49;

/// Пользователь, прошедший bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedUser {
    pub dn: String,
    /// `objectGUID` учётки — ключ связи с `ad_accounts` (sAMAccountName может смениться).
    pub object_guid: Option<String>,
    pub sam_account_name: String,
    pub display_name: Option<String>,
    /// Контроллер, который подтвердил пароль.
    pub controller: String,
}

/// Результат проверки одного контроллера (админка → «Проверить подключение»).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DcCheck {
    pub controller: String,
    pub ok: bool,
    /// Причина отказа без паролей.
    pub error: Option<String>,
    /// Что с этим делать — для человека.
    pub hint: Option<String>,
}

/// Подсказка по тексту ошибки подключения к контроллеру.
pub fn connection_hint(error: &str) -> Option<&'static str> {
    let e = error.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| e.contains(n));
    Some(if has(&["unknownissuer", "unknown issuer"]) {
        "Сертификат контроллера выдан неизвестным центром. Вставьте корневой сертификат вашего CA (PEM) в настройках домена."
    } else if has(&["notvalidforname", "not valid for name"]) {
        "Имя контроллера не совпадает с его сертификатом. Укажите полное DNS-имя контроллера, а не IP или короткое имя."
    } else if has(&["expired", "notvalidyet"]) {
        "Сертификат контроллера просрочен или ещё не действует, либо расходится время на серверах."
    } else if has(&[
        "connection reset",
        "os error 104",
        "os error 10054",
        "handshake eof",
        "unexpected eof",
        "tls handshake",
    ]) {
        "Контроллер оборвал TLS-соединение. Чаще всего у него нет сертификата LDAPS: он появляется сам, если в домене есть AD CS; без AD CS сертификат нужно выпустить и установить (см. документацию)."
    } else if has(&["connection refused", "os error 111", "os error 10061"]) {
        "Порт закрыт. Проверьте порт LDAPS (636) на контроллере и файрвол."
    } else if has(&["timed out", "timeout", "elapsed"]) {
        "Контроллер не ответил вовремя. Проверьте сеть и файрвол между порталом и контроллером."
    } else if has(&["failed to lookup", "name or service not known", "no such host", "temporary failure in name"]) {
        "Имя контроллера не находится в DNS. Проверьте имя и DNS сервера портала."
    } else if has(&["код ldap 49", "invalid credentials", "data 52e"]) {
        "Неверный логин или пароль служебной учётки."
    } else {
        return None;
    })
}

/// Пул DC одного домена.
pub struct DcPool {
    pub(crate) cfg: AdConfig,
    endpoints: Vec<DcEndpoint>,
    tls: Arc<rustls::ClientConfig>,
    /// Индекс DC, который последним ответил.
    current: AtomicUsize,
}

impl DcPool {
    /// Проверяет конфиг и готовит TLS. Сетевых операций нет.
    pub fn new(cfg: AdConfig) -> Result<Self, AdError> {
        let endpoints = cfg.endpoints()?;
        let tls = Arc::new(tls_config(&cfg)?);
        Ok(Self { cfg, endpoints, tls, current: AtomicUsize::new(0) })
    }

    pub fn endpoints(&self) -> &[DcEndpoint] {
        &self.endpoints
    }

    /// Текущий рабочий контроллер.
    pub fn current(&self) -> &DcEndpoint {
        &self.endpoints[self.current.load(Ordering::Relaxed) % self.endpoints.len()]
    }

    fn settings(&self) -> LdapConnSettings {
        LdapConnSettings::new()
            .set_conn_timeout(self.cfg.timeout())
            .set_config(self.tls.clone())
            .set_starttls(self.cfg.tls == TlsMode::Starttls)
    }

    /// Открывает соединение с конкретным DC и входит служебной учёткой.
    async fn service_conn(&self, dc: &DcEndpoint) -> Attempt<Ldap, AdError> {
        let mut ldap = match self.connect(dc).await {
            Ok(l) => l,
            Err(why) => return Attempt::Unreachable(why),
        };
        match ldap.simple_bind(&self.cfg.bind_dn, self.cfg.bind_password.expose()).await {
            Ok(r) if r.rc == 0 => Attempt::Answered(Ok(ldap)),
            Ok(r) => Attempt::Answered(Err(AdError::ServiceBind { rc: r.rc, text: clean(&r.text) })),
            Err(e) if is_connectivity(&e) => Attempt::Unreachable(e.to_string()),
            Err(e) => Attempt::Answered(Err(AdError::Ldap { rc: 0, text: e.to_string() })),
        }
    }

    async fn connect(&self, dc: &DcEndpoint) -> Result<Ldap, String> {
        let url = dc.url(self.cfg.tls);
        let (conn, ldap) = LdapConnAsync::with_settings(self.settings(), &url).await.map_err(|e| e.to_string())?;
        ldap3::drive!(conn);
        Ok(ldap)
    }

    /// Выполняет `op` на первом доступном DC, начиная с рабочего, и запоминает ответивший.
    pub(crate) async fn with_service_conn<T, F, Fut>(&self, mut op: F) -> Result<(DcEndpoint, T), AdError>
    where
        F: FnMut(Ldap) -> Fut,
        Fut: std::future::Future<Output = Result<T, AdError>>,
    {
        let start = self.current.load(Ordering::Relaxed);
        let res = try_in_order(&self.endpoints, start, |i, _| {
            let dc = self.endpoints[i].clone();
            async move { self.service_conn(&dc).await }
        })
        .await;
        match res {
            Ok((i, Ok(ldap))) => {
                if i != start {
                    tracing::warn!(from = %self.endpoints[start % self.endpoints.len()], to = %self.endpoints[i], "переключение на другой контроллер домена");
                }
                self.current.store(i, Ordering::Relaxed);
                Ok((self.endpoints[i].clone(), op(ldap).await?))
            }
            Ok((_, Err(e))) => Err(e),
            Err(failures) => Err(self.unavailable(failures)),
        }
    }

    fn unavailable(&self, failures: Vec<(usize, String)>) -> AdError {
        AdError::Unavailable(UnavailableReport(
            failures.into_iter().map(|(i, why)| (self.endpoints[i].to_string(), why)).collect(),
        ))
    }

    /// Вход пользователя: `ivanov`, `DOMAIN\ivanov` или `ivanov@domain`.
    /// Пароль используется только для одного bind и не попадает в ошибки и журнал.
    pub async fn authenticate(&self, login: &str, password: &Secret) -> Result<AuthenticatedUser, AdError> {
        if password.is_empty() {
            // Пустой пароль в AD — анонимный bind, который «успешен». Отказываем сами.
            return Err(AdError::Bind(BindFailure::InvalidCredentials));
        }
        let filter = login_filter(login);
        let base = self.cfg.base_dn.clone();
        // 1. Ищем DN служебной учёткой.
        let (dc, found) = self
            .with_service_conn(|mut ldap| {
                let filter = filter.clone();
                let base = base.clone();
                async move {
                    let (rs, _) = ldap
                        .search(
                            &base,
                            Scope::Subtree,
                            &filter,
                            vec!["distinguishedName", "sAMAccountName", "displayName", "objectGUID"],
                        )
                        .await
                        .and_then(|r| r.success())
                        .map_err(ldap_err)?;
                    let _ = ldap.unbind().await;
                    Ok(rs.into_iter().next().map(SearchEntry::construct))
                }
            })
            .await?;
        // Нет такого пользователя — тот же ответ, что и на неверный пароль.
        let Some(entry) = found else {
            tracing::info!(dc = %dc, "вход отклонён: учётка не найдена");
            return Err(AdError::Bind(BindFailure::InvalidCredentials));
        };
        let first = |a: &str| entry.attrs.get(a).and_then(|v| v.first().cloned());
        let sam = first("sAMAccountName").unwrap_or_default();
        // 2. Bind пользователем на том же DC. Неверный пароль на другом DC не повторяем.
        let mut ldap = self.connect(&dc).await.map_err(|why| self.unavailable(vec![(self.index_of(&dc), why)]))?;
        let result = ldap.simple_bind(&entry.dn, password.expose()).await;
        let _ = ldap.unbind().await;
        match result {
            Ok(r) if r.rc == 0 => Ok(AuthenticatedUser {
                dn: entry.dn.clone(),
                object_guid: crate::sync::entry_guid(&entry),
                sam_account_name: sam,
                display_name: first("displayName"),
                controller: dc.to_string(),
            }),
            Ok(r) if r.rc == RC_INVALID_CREDENTIALS => {
                let reason = BindFailure::from_ad_message(&r.text);
                tracing::info!(dc = %dc, user = %sam, ?reason, "вход отклонён");
                Err(AdError::Bind(reason))
            }
            Ok(r) => Err(AdError::Ldap { rc: r.rc, text: clean(&r.text) }),
            Err(e) => Err(ldap_err(e)),
        }
    }

    fn index_of(&self, dc: &DcEndpoint) -> usize {
        self.endpoints.iter().position(|e| e == dc).unwrap_or(0)
    }

    /// Вход служебной учёткой на любой доступный DC: жив ли домен (проверка доступности).
    pub async fn ping(&self) -> Result<String, AdError> {
        let (dc, ()) = self
            .with_service_conn(|mut ldap| async move {
                let _ = ldap.unbind().await;
                Ok(())
            })
            .await?;
        Ok(dc.to_string())
    }

    /// Проверяет каждый контроллер по отдельности: TLS, вход служебной учёткой.
    pub async fn check_controllers(&self) -> Vec<DcCheck> {
        let mut out = Vec::with_capacity(self.endpoints.len());
        for dc in &self.endpoints {
            let (ok, error) = match self.service_conn(dc).await {
                Attempt::Answered(Ok(mut ldap)) => {
                    let _ = ldap.unbind().await;
                    (true, None)
                }
                Attempt::Answered(Err(e)) => (false, Some(e.to_string())),
                Attempt::Unreachable(why) => (false, Some(why)),
            };
            let hint = error.as_deref().and_then(connection_hint).map(str::to_string);
            out.push(DcCheck { controller: dc.to_string(), ok, error, hint });
        }
        out
    }
}

/// Фильтр поиска учётки по логину. Значение экранируется (защита от LDAP-инъекции).
pub(crate) fn login_filter(login: &str) -> String {
    let login = login.trim();
    let (attr, value) = if let Some((_, user)) = login.split_once('\\') {
        ("sAMAccountName", user)
    } else if login.contains('@') {
        ("userPrincipalName", login)
    } else {
        ("sAMAccountName", login)
    };
    format!("(&(objectCategory=person)(objectClass=user)({attr}={}))", ldap_escape(value))
}

/// Сетевые ошибки и ошибки TLS — повод перейти к другому DC. Ответ сервера — нет.
fn is_connectivity(e: &LdapError) -> bool {
    !matches!(e, LdapError::LdapResult { .. } | LdapError::FilterParsing)
}

fn ldap_err(e: LdapError) -> AdError {
    match e {
        LdapError::LdapResult { result } => AdError::Ldap { rc: result.rc, text: clean(&result.text) },
        other => AdError::Ldap { rc: 0, text: other.to_string() },
    }
}

/// AD добавляет к тексту ошибки завершающий `\0`.
fn clean(text: &str) -> String {
    text.trim_end_matches('\0').trim().to_string()
}

/// TLS: системные корневые сертификаты + CA домена из файла (если указан).
/// Отключить проверку сертификата нельзя — такой настройки нет.
fn tls_config(cfg: &AdConfig) -> Result<rustls::ClientConfig, ConfigError> {
    use rustls_pki_types::{CertificateDer, pem::PemObject};
    let mut roots = rustls::RootCertStore::empty();
    // Битый сертификат в системном хранилище не должен выбрасывать остальные.
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    if let Some(path) = &cfg.ca_cert {
        let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(path)
            .and_then(|it| it.collect::<Result<_, _>>())
            .map_err(|e| ConfigError::CaCert(format!("{}: {e}", path.display())))?;
        if certs.is_empty() {
            return Err(ConfigError::CaCert(format!("{}: нет сертификатов", path.display())));
        }
        for c in certs {
            roots.add(c).map_err(|e| ConfigError::CaCert(e.to_string()))?;
        }
    }
    if let Some(pem) = cfg.ca_cert_pem.as_deref().filter(|p| !p.trim().is_empty()) {
        let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(pem.as_bytes())
            .collect::<Result<_, _>>()
            .map_err(|e| ConfigError::CaCert(e.to_string()))?;
        if certs.is_empty() {
            return Err(ConfigError::CaCert("в тексте нет сертификатов PEM".into()));
        }
        for c in certs {
            roots.add(c).map_err(|e| ConfigError::CaCert(e.to_string()))?;
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| ConfigError::CaCert(e.to_string()))
        .map(|b| {
            let mut c = b.with_root_certificates(roots).with_no_client_auth();
            // Без возобновления сессий TLS 1.3: AD (Schannel) изредка не отвечал на возобновление,
            // и подключение висело до тайм-аута (стенд: ~8% проверок связи). Подключений к AD мало —
            // полное рукопожатие ничего не стоит.
            c.resumption = rustls::client::Resumption::disabled();
            c
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_for_typical_failures() {
        let hint = |e: &str| connection_hint(e).unwrap_or("");
        assert!(hint("I/O error: Connection reset by peer (os error 104)").contains("LDAPS"));
        assert!(hint("invalid peer certificate: UnknownIssuer").contains("CA"));
        assert!(hint("invalid peer certificate: NotValidForName").contains("DNS-имя"));
        assert!(hint("служебная учётка не вошла в домен (код LDAP 49): 80090308: data 52e").contains("пароль"));
        assert!(hint("deadline has elapsed").contains("не ответил"));
        assert_eq!(connection_hint("что-то совсем другое"), None);
    }

    #[test]
    fn login_forms() {
        assert_eq!(login_filter("ivanov"), "(&(objectCategory=person)(objectClass=user)(sAMAccountName=ivanov))");
        assert_eq!(login_filter("CORP\\ivanov"), "(&(objectCategory=person)(objectClass=user)(sAMAccountName=ivanov))");
        assert_eq!(
            login_filter("ivanov@corp.local"),
            "(&(objectCategory=person)(objectClass=user)(userPrincipalName=ivanov@corp.local))"
        );
    }

    #[test]
    fn login_is_escaped() {
        let f = login_filter("*)(objectClass=*");
        assert!(f.contains("sAMAccountName=\\2a\\29\\28objectClass=\\2a"), "{f}");
    }

    #[tokio::test]
    async fn empty_password_is_rejected_without_network() {
        let pool = DcPool::new(AdConfig {
            controllers: vec!["dc1.invalid".into()],
            tls: TlsMode::Ldaps,
            base_dn: "DC=example,DC=local".into(),
            bind_dn: "svc".into(),
            bind_password: Secret::new("x"),
            ca_cert: None,
            ca_cert_pem: None,
            user_filter: String::new(),
            timeout_secs: 1,
        })
        .unwrap();
        let r = pool.authenticate("ivanov", &Secret::new("")).await;
        assert!(matches!(r, Err(AdError::Bind(BindFailure::InvalidCredentials))));
    }

    #[tokio::test]
    async fn unreachable_dcs_are_reported_without_password() {
        let pool = DcPool::new(AdConfig {
            controllers: vec!["127.0.0.1:1".into(), "127.0.0.1:2".into()],
            tls: TlsMode::Ldaps,
            base_dn: "DC=example,DC=local".into(),
            bind_dn: "svc".into(),
            bind_password: Secret::new("svc-pass-xyz"),
            ca_cert: None,
            ca_cert_pem: None,
            user_filter: String::new(),
            timeout_secs: 1,
        })
        .unwrap();
        let err = pool.authenticate("ivanov", &Secret::new("user-pass-xyz")).await.unwrap_err();
        let text = format!("{err} {err:?}");
        assert!(matches!(err, AdError::Unavailable(ref r) if r.0.len() == 2), "{text}");
        assert!(!text.contains("pass-xyz"), "{text}");
    }
}
