//! Вход и синхронизация по сети, без настоящего AD: два поддельных контроллера домена с LDAPS.
//!
//! Поддельный DC говорит по протоколу LDAP (`ldap3_proto`) поверх настоящего TLS с собственным
//! CA и отвечает на неудачный bind так же, как AD: код 49 и `data 52e/533/775…` в тексте.
//! Проверяется клиент Pult целиком: доверие к CA домена, поиск DN служебной учёткой, bind
//! пользователем, перебор DC, разбор кодов, постраничное и инкрементальное чтение.
//!
//! Что подделка **не** проверяет: схему и поведение настоящего AD (атрибуты, `objectCategory`,
//! ограничения размера, реплики). Это — Samba AD из `deploy/compose.dev.yml` или тестовая OU.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use futures_util::{SinkExt, StreamExt};
use ldap3_proto::{
    LdapCodec,
    control::LdapControl,
    proto::{
        LdapBindCred, LdapBindResponse, LdapFilter, LdapMsg, LdapOp, LdapPartialAttribute, LdapResult, LdapResultCode,
        LdapSearchResultEntry, LdapSearchScope,
    },
};
use pult_ad::{AdConfig, AdError, BindFailure, DcPool, TlsMode};
use pult_core::Secret;
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_rustls::{TlsAcceptor, rustls};
use tokio_util::codec::{FramedRead, FramedWrite};

const SVC_DN: &str = "svc-pult@pult.test";
const SVC_PASS: &str = "svc-Pass-123";
const USER_PASS: &str = "User-Pass-456";
const BASE_DN: &str = "DC=pult,DC=test";
const HIGHEST_USN: u64 = 120;

// ---------- каталог ----------

struct User {
    sam: &'static str,
    display: &'static str,
    uac: u32,
    usn: u64,
    /// Код AD, которым DC отвечает на **верный** пароль (отключена, заблокирована…).
    status: Option<&'static str>,
}

impl User {
    fn dn(&self) -> String {
        format!("CN={},OU=Users,{BASE_DN}", self.display)
    }

    /// У `ivanov` GUID случайно оказывается корректным UTF-8 (так бывает и в настоящем AD),
    /// у остальных — нет. Оба случая должны доходить до синхронизации.
    fn guid(&self) -> Vec<u8> {
        let mut g = vec![if self.sam == "ivanov" { 0 } else { 0xff }; 16];
        g[0] = u8::try_from(self.usn).unwrap();
        g
    }

    fn attrs(&self) -> Vec<(String, Vec<Vec<u8>>)> {
        let s = |v: &str| vec![v.as_bytes().to_vec()];
        vec![
            (
                "objectClass".into(),
                ["top", "person", "organizationalPerson", "user"].map(|v| v.as_bytes().to_vec()).to_vec(),
            ),
            ("objectCategory".into(), s("person")),
            ("objectGUID".into(), vec![self.guid()]),
            ("distinguishedName".into(), s(&self.dn())),
            ("sAMAccountName".into(), s(self.sam)),
            ("userPrincipalName".into(), s(&format!("{}@pult.test", self.sam))),
            ("displayName".into(), s(self.display)),
            ("mail".into(), s(&format!("{}@pult.test", self.sam))),
            ("userAccountControl".into(), s(&self.uac.to_string())),
            ("uSNChanged".into(), s(&self.usn.to_string())),
        ]
    }
}

fn directory() -> Vec<User> {
    let u = |sam, display, uac, usn, status| User { sam, display, uac, usn, status };
    vec![
        u("ivanov", "Иванов Иван", 512, 101, None),
        u("petrova", "Петрова Анна", 512, 102, None),
        u("disabled", "Отключённый", 514, 103, Some("533")),
        u("locked", "Заблокированный", 512, 104, Some("775")),
        u("expired", "Истёкший пароль", 512, 105, Some("532")),
        u("mustchange", "Сменить пароль", 512, 106, Some("773")),
        u("sidorov", "Сидоров Пётр", 512, 110, None),
    ]
}

fn matches(f: &LdapFilter, attrs: &[(String, Vec<Vec<u8>>)]) -> bool {
    let values =
        |a: &str| attrs.iter().find(|(n, _)| n.eq_ignore_ascii_case(a)).map(|(_, v)| v.clone()).unwrap_or_default();
    match f {
        LdapFilter::And(fs) => fs.iter().all(|f| matches(f, attrs)),
        LdapFilter::Or(fs) => fs.iter().any(|f| matches(f, attrs)),
        LdapFilter::Not(f) => !matches(f, attrs),
        LdapFilter::Present(a) => !values(a).is_empty(),
        LdapFilter::Equality(a, v) => values(a).iter().any(|x| String::from_utf8_lossy(x).eq_ignore_ascii_case(v)),
        LdapFilter::GreaterOrEqual(a, v) => {
            let v: u64 = v.parse().unwrap_or(u64::MAX);
            values(a).iter().any(|x| String::from_utf8_lossy(x).parse::<u64>().is_ok_and(|n| n >= v))
        }
        _ => false,
    }
}

// ---------- поддельный DC ----------

#[derive(Default)]
struct Stats {
    tls_ok: AtomicUsize,
    service_binds: AtomicUsize,
    user_binds: AtomicUsize,
    pages: AtomicUsize,
}

struct FakeDc {
    addr: SocketAddr,
    stats: Arc<Stats>,
    listener: JoinHandle<()>,
}

impl FakeDc {
    async fn start(ca: &TestCa) -> Self {
        let acceptor = ca.acceptor();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stats = Arc::new(Stats::default());
        let st = stats.clone();
        let listener = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (acceptor, st) = (acceptor.clone(), st.clone());
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else { return };
                    st.tls_ok.fetch_add(1, Ordering::Relaxed);
                    serve(tls, st).await;
                });
            }
        });
        Self { addr, stats, listener }
    }

    /// «Выключить» DC: порт закрывается, новые подключения отклоняются.
    async fn stop(self) {
        self.listener.abort();
        let _ = self.listener.await;
    }

    fn endpoint(&self) -> String {
        self.addr.to_string()
    }
}

async fn serve(tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>, st: Arc<Stats>) {
    let (r, w) = tokio::io::split(tls);
    let mut reader = FramedRead::new(r, LdapCodec::default());
    let mut writer = FramedWrite::new(w, LdapCodec::default());
    let users = directory();
    let mut service = false;
    while let Some(Ok(msg)) = reader.next().await {
        let id = msg.msgid;
        let replies = match msg.op {
            LdapOp::BindRequest(b) => {
                let LdapBindCred::Simple(pass) = b.cred else { break };
                vec![bind_reply(id, bind(&users, &b.dn, &pass, &st, &mut service))]
            }
            LdapOp::SearchRequest(s) if s.base.is_empty() && s.scope == LdapSearchScope::Base => vec![
                entry(
                    id,
                    String::new(),
                    vec![("highestCommittedUSN".into(), vec![HIGHEST_USN.to_string().into_bytes()])],
                ),
                done(id, LdapResultCode::Success, "", vec![]),
            ],
            LdapOp::SearchRequest(_) if !service => {
                vec![done(
                    id,
                    LdapResultCode::OperationsError,
                    "000004DC: LdapErr: DSID-0C090A5C, comment: In order to perform this operation a successful bind must be completed on the connection., data 0, v4563\0",
                    vec![],
                )]
            }
            LdapOp::SearchRequest(s) => {
                let found: Vec<&User> = users.iter().filter(|u| matches(&s.filter, &u.attrs())).collect();
                let paged = msg.ctrl.iter().find_map(|c| match c {
                    LdapControl::SimplePagedResults { size, cookie } => Some((*size, cookie.clone())),
                    _ => None,
                });
                let (from, to, ctrl) = match paged {
                    Some((size, cookie)) => {
                        st.pages.fetch_add(1, Ordering::Relaxed);
                        let from =
                            if cookie.is_empty() { 0 } else { String::from_utf8(cookie).unwrap().parse().unwrap() };
                        let to = (from + usize::try_from(size).unwrap()).min(found.len());
                        let next = if to < found.len() { to.to_string().into_bytes() } else { Vec::new() };
                        (from, to, vec![LdapControl::SimplePagedResults { size: 0, cookie: next }])
                    }
                    None => (0, found.len(), vec![]),
                };
                let mut out: Vec<LdapMsg> = found[from..to].iter().map(|u| entry(id, u.dn(), u.attrs())).collect();
                out.push(done(id, LdapResultCode::Success, "", ctrl));
                out
            }
            LdapOp::UnbindRequest => break,
            _ => vec![done(id, LdapResultCode::UnwillingToPerform, "не поддерживается подделкой", vec![])],
        };
        for r in replies {
            if writer.send(r).await.is_err() {
                return;
            }
        }
    }
}

/// Ответ на bind, как у AD: 49 + `data XXX` в тексте.
fn bind(users: &[User], dn: &str, pass: &str, st: &Stats, service: &mut bool) -> (LdapResultCode, String) {
    let ad = |code: &str| {
        format!("80090308: LdapErr: DSID-0C09044E, comment: AcceptSecurityContext error, data {code}, v4563\0")
    };
    if dn.eq_ignore_ascii_case(SVC_DN) {
        st.service_binds.fetch_add(1, Ordering::Relaxed);
        *service = pass == SVC_PASS;
        return if *service {
            (LdapResultCode::Success, String::new())
        } else {
            (LdapResultCode::InvalidCredentials, ad("52e"))
        };
    }
    st.user_binds.fetch_add(1, Ordering::Relaxed);
    match users.iter().find(|u| u.dn().eq_ignore_ascii_case(dn)) {
        Some(u) if pass == USER_PASS => match u.status {
            None => (LdapResultCode::Success, String::new()),
            Some(code) => (LdapResultCode::InvalidCredentials, ad(code)),
        },
        Some(_) => (LdapResultCode::InvalidCredentials, ad("52e")),
        None => (LdapResultCode::InvalidCredentials, ad("525")),
    }
}

fn result(code: LdapResultCode, message: &str) -> LdapResult {
    LdapResult { code, matcheddn: String::new(), message: message.into(), referral: vec![] }
}

fn bind_reply(msgid: i32, (code, message): (LdapResultCode, String)) -> LdapMsg {
    let op = LdapOp::BindResponse(LdapBindResponse { res: result(code, &message), saslcreds: None });
    LdapMsg { msgid, op, ctrl: vec![] }
}

fn entry(msgid: i32, dn: String, attrs: Vec<(String, Vec<Vec<u8>>)>) -> LdapMsg {
    let attributes = attrs.into_iter().map(|(atype, vals)| LdapPartialAttribute { atype, vals }).collect();
    LdapMsg { msgid, op: LdapOp::SearchResultEntry(LdapSearchResultEntry { dn, attributes }), ctrl: vec![] }
}

fn done(msgid: i32, code: LdapResultCode, message: &str, ctrl: Vec<LdapControl>) -> LdapMsg {
    LdapMsg { msgid, op: LdapOp::SearchResultDone(result(code, message)), ctrl }
}

// ---------- TLS: собственный CA домена ----------

struct TestCa {
    issuer: rcgen::CertifiedIssuer<'static, rcgen::KeyPair>,
    pem_path: PathBuf,
}

impl TestCa {
    fn new(name: &str) -> Self {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.distinguished_name.push(rcgen::DnType::CommonName, name);
        let issuer = rcgen::CertifiedIssuer::self_signed(params, rcgen::KeyPair::generate().unwrap()).unwrap();
        // Тесты идут параллельно, а шаг часов Windows — 100 нс: имя файла по времени совпадало,
        // и тесты перезаписывали CA друг друга. Счётчик делает имя уникальным.
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let pem_path = std::env::temp_dir().join(format!("pult-fake-ca-{}-{seq}.pem", std::process::id()));
        std::fs::write(&pem_path, issuer.pem()).unwrap();
        Self { issuer, pem_path }
    }

    /// Сертификат DC на адрес 127.0.0.1, подписанный этим CA.
    fn acceptor(&self) -> TlsAcceptor {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["127.0.0.1".to_string()])
            .unwrap()
            .signed_by(&key, &self.issuer)
            .unwrap();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            )
            .unwrap();
        TlsAcceptor::from(Arc::new(cfg))
    }
}

impl Drop for TestCa {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.pem_path);
    }
}

fn pool(dcs: &[&FakeDc], trusted: &TestCa) -> DcPool {
    pool_with_service_password(dcs, trusted, SVC_PASS)
}

fn pool_with_service_password(dcs: &[&FakeDc], trusted: &TestCa, svc_pass: &str) -> DcPool {
    DcPool::new(AdConfig {
        controllers: dcs.iter().map(|d| d.endpoint()).collect(),
        tls: TlsMode::Ldaps,
        base_dn: BASE_DN.into(),
        bind_dn: SVC_DN.into(),
        bind_password: Secret::new(svc_pass),
        ca_cert: Some(trusted.pem_path.clone()),
        ca_cert_pem: None,
        user_filter: "(&(objectCategory=person)(objectClass=user))".into(),
        timeout_secs: 2,
    })
    .unwrap()
}

// ---------- Вход и перебор DC ----------

#[tokio::test]
async fn login_works_and_survives_first_dc_going_down() {
    let ca = TestCa::new("PULT-TEST-CA");
    let (dc1, dc2) = (FakeDc::start(&ca).await, FakeDc::start(&ca).await);
    let p = pool(&[&dc1, &dc2], &ca);

    let u = p.authenticate("ivanov", &Secret::new(USER_PASS)).await.unwrap();
    assert_eq!((u.sam_account_name.as_str(), u.display_name.as_deref()), ("ivanov", Some("Иванов Иван")));
    assert_eq!(u.controller, dc1.endpoint());
    // Те же учётки в других формах логина.
    assert!(p.authenticate("PULT\\petrova", &Secret::new(USER_PASS)).await.is_ok());
    assert!(p.authenticate("sidorov@pult.test", &Secret::new(USER_PASS)).await.is_ok());

    let dc2_addr = dc2.endpoint();
    let dc1_stats = dc1.stats.clone();
    dc1.stop().await;
    let u = p.authenticate("ivanov", &Secret::new(USER_PASS)).await.unwrap();
    assert_eq!(u.controller, dc2_addr, "после остановки первого DC вход идёт через второй");
    assert_eq!(p.current().to_string(), dc2_addr, "рабочий DC запомнен");
    assert_eq!(dc1_stats.user_binds.load(Ordering::Relaxed), 3);
}

#[tokio::test]
async fn wrong_password_is_not_retried_on_another_dc() {
    let ca = TestCa::new("PULT-TEST-CA");
    let (dc1, dc2) = (FakeDc::start(&ca).await, FakeDc::start(&ca).await);
    let p = pool(&[&dc1, &dc2], &ca);
    let e = p.authenticate("ivanov", &Secret::new("wrong")).await.unwrap_err();
    assert!(matches!(e, AdError::Bind(BindFailure::InvalidCredentials)), "{e:?}");
    assert_eq!(dc1.stats.user_binds.load(Ordering::Relaxed), 1);
    assert_eq!(dc2.stats.user_binds.load(Ordering::Relaxed), 0, "лишняя попытка увеличила бы счётчик блокировки");
    assert_eq!(dc2.stats.tls_ok.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn ad_status_codes_become_clear_reasons() {
    let ca = TestCa::new("PULT-TEST-CA");
    let dc = FakeDc::start(&ca).await;
    let p = pool(&[&dc], &ca);
    for (login, want) in [
        ("disabled", BindFailure::AccountDisabled),
        ("locked", BindFailure::AccountLocked),
        ("expired", BindFailure::PasswordExpired),
        ("mustchange", BindFailure::MustChangePassword),
    ] {
        let e = p.authenticate(login, &Secret::new(USER_PASS)).await.unwrap_err();
        assert!(matches!(e, AdError::Bind(r) if r == want), "{login}: {e:?}");
    }
    // Отключённая учётка с неверным паролем — AD не раскрывает статус.
    let e = p.authenticate("disabled", &Secret::new("wrong")).await.unwrap_err();
    assert!(matches!(e, AdError::Bind(BindFailure::InvalidCredentials)), "{e:?}");
    // Нет такого логина — тот же ответ, bind пользователем даже не выполняется.
    let before = dc.stats.user_binds.load(Ordering::Relaxed);
    let e = p.authenticate("nobody", &Secret::new(USER_PASS)).await.unwrap_err();
    assert!(matches!(e, AdError::Bind(BindFailure::InvalidCredentials)), "{e:?}");
    assert_eq!(dc.stats.user_binds.load(Ordering::Relaxed), before);
    // Попытка LDAP-инъекции в логине не находит чужую учётку.
    let e = p.authenticate("*", &Secret::new(USER_PASS)).await.unwrap_err();
    assert!(matches!(e, AdError::Bind(BindFailure::InvalidCredentials)), "{e:?}");
}

#[tokio::test]
async fn untrusted_certificate_is_refused_before_any_password_is_sent() {
    let (ours, foreign) = (TestCa::new("PULT-TEST-CA"), TestCa::new("ROGUE-CA"));
    let rogue = FakeDc::start(&foreign).await;
    let p = pool(&[&rogue], &ours);
    let e = p.authenticate("ivanov", &Secret::new(USER_PASS)).await.unwrap_err();
    let text = format!("{e} {e:?}");
    assert!(matches!(e, AdError::Unavailable(_)), "{text}");
    assert_eq!(rogue.stats.tls_ok.load(Ordering::Relaxed), 0, "TLS не должен установиться");
    assert_eq!(rogue.stats.service_binds.load(Ordering::Relaxed) + rogue.stats.user_binds.load(Ordering::Relaxed), 0);
    assert!(!text.contains(SVC_PASS) && !text.contains(USER_PASS), "{text}");
}

#[tokio::test]
async fn rogue_first_dc_is_skipped_and_trusted_second_is_used() {
    let (ours, foreign) = (TestCa::new("PULT-TEST-CA"), TestCa::new("ROGUE-CA"));
    let (rogue, good) = (FakeDc::start(&foreign).await, FakeDc::start(&ours).await);
    let p = pool(&[&rogue, &good], &ours);
    let u = p.authenticate("ivanov", &Secret::new(USER_PASS)).await.unwrap();
    assert_eq!(u.controller, good.endpoint());
    assert_eq!(rogue.stats.service_binds.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn wrong_service_password_is_a_configuration_error() {
    let ca = TestCa::new("PULT-TEST-CA");
    let dc = FakeDc::start(&ca).await;
    let p = pool_with_service_password(&[&dc], &ca, "stale-svc-password");
    let e = p.authenticate("ivanov", &Secret::new(USER_PASS)).await.unwrap_err();
    let text = format!("{e} {e:?}");
    assert!(matches!(e, AdError::ServiceBind { rc: 49, .. }), "{text}");
    assert!(!text.contains("stale-svc-password") && !text.contains(USER_PASS), "{text}");
    assert_eq!(dc.stats.user_binds.load(Ordering::Relaxed), 0);
}

// ---------- Синхронизация ----------

#[tokio::test]
async fn full_sync_reads_all_pages() {
    let ca = TestCa::new("PULT-TEST-CA");
    let dc = FakeDc::start(&ca).await;
    let p = pool(&[&dc], &ca);
    let batch = p.full_sync(3).await.unwrap();
    assert_eq!(batch.accounts.len(), 7);
    assert_eq!(dc.stats.pages.load(Ordering::Relaxed), 3, "7 учёток страницами по 3");
    assert_eq!((batch.highest_usn, batch.controller.as_str()), (HIGHEST_USN, dc.endpoint().as_str()));
    let disabled: Vec<_> =
        batch.accounts.iter().filter(|a| !a.is_enabled).map(|a| a.sam_account_name.as_str()).collect();
    assert_eq!(disabled, vec!["disabled"]);
    let ivanov = batch.accounts.iter().find(|a| a.sam_account_name == "ivanov").unwrap();
    assert_eq!(ivanov.object_guid, "00000065-0000-0000-0000-000000000000");
    assert_eq!((ivanov.mail.as_deref(), ivanov.usn_changed), (Some("ivanov@pult.test"), 101));
}

#[tokio::test]
async fn incremental_sync_returns_only_changes_and_follows_failover() {
    let ca = TestCa::new("PULT-TEST-CA");
    let (dc1, dc2) = (FakeDc::start(&ca).await, FakeDc::start(&ca).await);
    let p = pool(&[&dc1, &dc2], &ca);
    let batch = p.incremental_sync(105, 500).await.unwrap();
    let mut sams: Vec<_> = batch.accounts.iter().map(|a| a.sam_account_name.clone()).collect();
    sams.sort();
    assert_eq!(sams, vec!["mustchange", "sidorov"]);
    assert_eq!(batch.controller, dc1.endpoint());

    // DC сменился — вызывающий код видит это по `controller` и делает полную синхронизацию.
    let dc2_addr = dc2.endpoint();
    dc1.stop().await;
    let batch = p.incremental_sync(105, 500).await.unwrap();
    assert_eq!(batch.controller, dc2_addr);
}
