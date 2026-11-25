//! Помощники для тестов Pult: поддельный контроллер домена по LDAPS с изменяемым каталогом.
//!
//! Говорит по протоколу LDAP (`ldap3_proto`) поверх настоящего TLS с собственным CA и отвечает
//! на bind как AD: код 49 и `data 52e/533/775…`. Поддерживает то, что использует Pult:
//! bind, поиск по фильтру (в том числе постранично), поиск одной записи по DN, корневой DSE
//! с `highestCommittedUSN`, запись (modify) учёткой записи: участник группы, `lockoutTime`,
//! `userAccountControl`, `unicodePwd`, `pwdLastSet`. Служебная учётка чтения писать не может —
//! как с делегированием в настоящем AD. Каталог можно менять во время теста (отключить учётку и т. п.).
//!
//! Схему и поведение настоящего AD подделка не воспроизводит — для этого Samba AD в CI.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use futures_util::{SinkExt, StreamExt};
use ldap3_proto::{
    LdapCodec,
    control::LdapControl,
    proto::{
        LdapBindCred, LdapBindResponse, LdapFilter, LdapModifyRequest, LdapModifyType, LdapMsg, LdapOp,
        LdapPartialAttribute, LdapResult, LdapResultCode, LdapSearchResultEntry, LdapSearchScope,
    },
};
use std::{
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_rustls::{TlsAcceptor, rustls};
use tokio_util::codec::{FramedRead, FramedWrite};

pub const SVC_DN: &str = "svc-pult@pult.test";
pub const SVC_PASS: &str = "svc-Pass-123";
/// Учётка записи (делегированные права): ей разрешён modify.
pub const WRITER_DN: &str = "svc-pult-writer@pult.test";
pub const WRITER_PASS: &str = "svc-Writer-789";
/// `maxPwdAge` домена: 42 дня, как по умолчанию в AD.
pub const MAX_PWD_AGE_DAYS: u64 = 42;
/// FILETIME «сейчас» подделки: 2026-01-01T00:00:00Z.
pub const NOW_FILETIME: u64 = 134_116_992_000_000_000;
/// Пароль всех пользователей подделки.
pub const USER_PASS: &str = "User-Pass-456";
pub const BASE_DN: &str = "DC=pult,DC=test";
pub const USER_FILTER: &str = "(&(objectCategory=person)(objectClass=user))";

#[derive(Debug, Clone)]
pub struct FakeUser {
    pub sam: String,
    pub given: Option<String>,
    pub sn: Option<String>,
    pub display: String,
    pub mail: Option<String>,
    pub uac: u32,
    pub usn: u64,
    /// Код AD на **верный** пароль (533 — отключена, 775 — заблокирована…).
    pub status: Option<&'static str>,
    /// Последний байт GUID — у каждой учётки свой.
    pub guid_seed: u8,
    /// `lockoutTime` (FILETIME), 0 — не заблокирована.
    pub lockout: u64,
    pub pwd_last_set: u64,
    /// Пароль, заданный через `unicodePwd`; `None` — [`USER_PASS`].
    pub password: Option<String>,
    /// `adminCount = 1`: учётка под защитой AdminSDHolder (член привилегированной группы).
    pub protected: bool,
    /// `userPrincipalName`; `None` — `sam@pult.test`.
    pub upn: Option<String>,
}

impl FakeUser {
    pub fn person(sam: &str, sn: &str, given: &str, guid_seed: u8) -> Self {
        Self {
            sam: sam.into(),
            given: Some(given.into()),
            sn: Some(sn.into()),
            display: format!("{sn} {given}"),
            mail: Some(format!("{sam}@pult.test")),
            uac: 512,
            usn: 100 + u64::from(guid_seed),
            status: None,
            guid_seed,
            lockout: 0,
            pwd_last_set: NOW_FILETIME,
            password: None,
            protected: false,
            upn: None,
        }
    }

    pub fn dn(&self) -> String {
        format!("CN={},OU=Users,{BASE_DN}", self.display)
    }

    /// Отключённая учётка (`ACCOUNTDISABLE`): вход — `data 533`.
    pub fn disabled(mut self) -> Self {
        self.uac |= 2;
        self.status = Some("533");
        self
    }

    /// Привилегированная учётка (`adminCount = 1`).
    pub fn protected(mut self) -> Self {
        self.protected = true;
        self
    }

    /// GUID в виде строки, как его покажет Pult.
    pub fn guid_string(&self) -> String {
        let g = self.guid();
        format!(
            "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            g[3], g[2], g[1], g[0], g[5], g[4], g[7], g[6], g[8], g[9], g[10], g[11], g[12], g[13], g[14], g[15]
        )
    }

    fn guid(&self) -> Vec<u8> {
        let mut g = vec![0xa5; 16];
        g[15] = self.guid_seed;
        g
    }

    fn attrs(&self) -> Vec<(String, Vec<Vec<u8>>)> {
        let s = |v: &str| vec![v.as_bytes().to_vec()];
        let mut a = vec![
            (
                "objectClass".into(),
                ["top", "person", "organizationalPerson", "user"].map(|v| v.as_bytes().to_vec()).to_vec(),
            ),
            ("objectCategory".into(), s("person")),
            ("objectGUID".into(), vec![self.guid()]),
            ("distinguishedName".into(), s(&self.dn())),
            ("sAMAccountName".into(), s(&self.sam)),
            ("userPrincipalName".into(), s(&self.upn.clone().unwrap_or_else(|| format!("{}@pult.test", self.sam)))),
            ("displayName".into(), s(&self.display)),
            ("userAccountControl".into(), s(&self.uac.to_string())),
            ("uSNChanged".into(), s(&self.usn.to_string())),
            ("lockoutTime".into(), s(&self.lockout.to_string())),
            ("pwdLastSet".into(), s(&self.pwd_last_set.to_string())),
            ("lastLogonTimestamp".into(), s(&NOW_FILETIME.to_string())),
        ];
        if self.protected {
            a.push(("adminCount".into(), s("1")));
        }
        for (k, v) in [("givenName", &self.given), ("sn", &self.sn), ("mail", &self.mail)] {
            if let Some(v) = v {
                a.push((k.into(), s(v)));
            }
        }
        a
    }
}

/// Компьютер домена.
#[derive(Debug, Clone)]
pub struct FakeComputer {
    pub name: String,
    pub dns: Option<String>,
    pub description: Option<String>,
    pub uac: u32,
    pub guid_seed: u8,
}

impl FakeComputer {
    pub fn new(name: &str, description: Option<&str>, guid_seed: u8) -> Self {
        Self {
            name: name.into(),
            dns: Some(format!("{}.pult.test", name.to_lowercase())),
            description: description.map(Into::into),
            uac: 4096,
            guid_seed,
        }
    }

    pub fn disabled(mut self) -> Self {
        self.uac |= 2;
        self
    }

    fn entry(&self) -> Entry {
        let s = |v: &str| vec![v.as_bytes().to_vec()];
        let mut guid = vec![0xc7; 16];
        guid[15] = self.guid_seed;
        let mut a: Vec<(String, Vec<Vec<u8>>)> = vec![
            (
                "objectClass".into(),
                ["top", "person", "organizationalPerson", "user", "computer"].map(|v| v.as_bytes().to_vec()).to_vec(),
            ),
            ("objectCategory".into(), s("computer")),
            ("objectGUID".into(), vec![guid]),
            ("name".into(), s(&self.name)),
            ("operatingSystem".into(), s("Windows 10 Pro")),
            ("userAccountControl".into(), s(&self.uac.to_string())),
        ];
        for (k, v) in [("dNSHostName", &self.dns), ("description", &self.description)] {
            if let Some(v) = v {
                a.push((k.into(), s(v)));
            }
        }
        (format!("CN={},CN=Computers,{BASE_DN}", self.name), a)
    }
}

/// Каталог, общий для всех подделанных DC теста.
/// Группа: имя и DN участников.
type Group = (String, Vec<String>);
/// Запись каталога: DN и атрибуты.
type Entry = (String, Vec<(String, Vec<Vec<u8>>)>);

#[derive(Clone, Default)]
pub struct Directory(Arc<Mutex<Vec<FakeUser>>>, Arc<Mutex<Vec<Group>>>, Arc<Mutex<Vec<FakeComputer>>>);

impl Directory {
    pub fn new(users: Vec<FakeUser>) -> Self {
        Self(Arc::new(Mutex::new(users)), Arc::default(), Arc::default())
    }

    /// Группа безопасности с участниками (по `sAMAccountName`).
    pub fn add_group(&self, cn: &str, members: &[&str]) {
        let users = self.0.lock().unwrap();
        let dns = members.iter().filter_map(|m| users.iter().find(|u| u.sam == *m).map(FakeUser::dn)).collect();
        self.1.lock().unwrap().push((cn.to_string(), dns));
    }

    pub fn add_computer(&self, c: FakeComputer) {
        self.2.lock().unwrap().push(c);
    }

    pub fn remove_computer(&self, name: &str) {
        self.2.lock().unwrap().retain(|c| c.name != name);
    }

    /// Заблокировать учётку (как после неверных паролей): `lockoutTime`, вход — `data 775`.
    pub fn lock(&self, sam: &str) {
        let usn = self.next_usn();
        let mut g = self.0.lock().unwrap();
        let u = g.iter_mut().find(|u| u.sam == sam).expect("нет такой учётки");
        u.lockout = NOW_FILETIME;
        u.status = Some("775");
        u.usn = usn;
    }

    /// Участники группы (DN).
    pub fn members(&self, cn: &str) -> Vec<String> {
        self.1.lock().unwrap().iter().find(|(n, _)| n == cn).map(|(_, m)| m.clone()).unwrap_or_default()
    }

    /// DN группы подделки.
    pub fn group_dn(cn: &str) -> String {
        format!("CN={cn},OU=Groups,{BASE_DN}")
    }

    /// Запись учётки с `memberOf` (обратная ссылка, как в AD).
    fn user_entry(&self, u: &FakeUser) -> Entry {
        let dn = u.dn();
        let mut a = u.attrs();
        let member_of: Vec<Vec<u8>> = self
            .1
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, m)| m.iter().any(|x| x.eq_ignore_ascii_case(&dn)))
            .map(|(cn, _)| Self::group_dn(cn).into_bytes())
            .collect();
        if !member_of.is_empty() {
            a.push(("memberOf".into(), member_of));
        }
        (dn, a)
    }

    /// Изменение записи учёткой записи. Ответ — код LDAP и текст.
    fn modify(&self, m: &LdapModifyRequest) -> (LdapResultCode, &'static str) {
        let value = |c: &ldap3_proto::proto::LdapModify| c.modification.vals.first().cloned().unwrap_or_default();
        if let Some(cn) = self
            .group_entries()
            .iter()
            .find(|(dn, _)| dn.eq_ignore_ascii_case(&m.dn))
            .map(|(_, a)| String::from_utf8_lossy(&a.iter().find(|(n, _)| n == "cn").unwrap().1[0]).to_string())
        {
            let mut groups = self.1.lock().unwrap();
            let (_, members) = groups.iter_mut().find(|(n, _)| *n == cn).unwrap();
            for c in &m.changes {
                if c.operation != LdapModifyType::Add || !c.modification.atype.eq_ignore_ascii_case("member") {
                    return (LdapResultCode::UnwillingToPerform, "не поддерживается подделкой\0");
                }
                for v in &c.modification.vals {
                    let dn = String::from_utf8_lossy(v).to_string();
                    if members.iter().any(|x| x.eq_ignore_ascii_case(&dn)) {
                        return (
                            LdapResultCode::EntryAlreadyExists,
                            "00000562: UpdErr: DSID-031A11F8, problem 6005 (ENTRY_EXISTS), data 0\0",
                        );
                    }
                    members.push(dn);
                }
            }
            return (LdapResultCode::Success, "");
        }
        let usn = self.next_usn();
        let mut users = self.0.lock().unwrap();
        let Some(u) = users.iter_mut().find(|u| u.dn().eq_ignore_ascii_case(&m.dn)) else {
            return (LdapResultCode::NoSuchObject, "0000208D: NameErr: DSID-0310028D, problem 2001 (NO_OBJECT)\0");
        };
        for c in &m.changes {
            let v = value(c);
            let text = String::from_utf8_lossy(&v).to_string();
            match c.modification.atype.as_str() {
                "lockoutTime" => {
                    u.lockout = text.parse().unwrap_or(0);
                    if u.lockout == 0 && u.status == Some("775") {
                        u.status = None;
                    }
                }
                "userAccountControl" => {
                    u.uac = text.parse().unwrap_or(u.uac);
                    if u.uac & 2 != 0 {
                        u.status = Some("533");
                    } else if u.status == Some("533") {
                        u.status = None;
                    }
                }
                "unicodePwd" => {
                    let units: Vec<u16> = v.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
                    let p = String::from_utf16_lossy(&units);
                    u.password = Some(p.trim_matches('"').to_string());
                    u.pwd_last_set = NOW_FILETIME + 1;
                }
                "pwdLastSet" => u.pwd_last_set = text.parse().unwrap_or(0),
                _ => return (LdapResultCode::UnwillingToPerform, "не поддерживается подделкой\0"),
            }
        }
        u.usn = usn;
        (LdapResultCode::Success, "")
    }

    pub fn clear_computers(&self) {
        self.2.lock().unwrap().clear();
    }

    fn computer_entries(&self) -> Vec<Entry> {
        self.2.lock().unwrap().iter().map(FakeComputer::entry).collect()
    }

    fn group_entries(&self) -> Vec<Entry> {
        let s = |v: &str| vec![v.as_bytes().to_vec()];
        self.1
            .lock()
            .unwrap()
            .iter()
            .map(|(cn, members)| {
                let dn = Self::group_dn(cn);
                let attrs = vec![
                    ("objectClass".into(), vec![b"top".to_vec(), b"group".to_vec()]),
                    ("objectCategory".into(), s("group")),
                    ("cn".into(), s(cn)),
                    ("sAMAccountName".into(), s(cn)),
                    ("member".into(), members.iter().map(|m| m.as_bytes().to_vec()).collect()),
                ];
                (dn, attrs)
            })
            .collect()
    }

    fn highest_usn(&self) -> u64 {
        self.0.lock().unwrap().iter().map(|u| u.usn).max().unwrap_or(1)
    }

    /// Пользователь меняет свой пароль, как в AD: Delete старого + Add нового `unicodePwd`.
    /// `00000056` — неверный старый пароль; `0000052D` — политика (не короче 8 символов и не прежний).
    fn change_own_password(&self, m: &LdapModifyRequest) -> (LdapResultCode, &'static str) {
        let pwd = |op: LdapModifyType| {
            m.changes
                .iter()
                .find(|c| c.operation == op && c.modification.atype.eq_ignore_ascii_case("unicodePwd"))
                .and_then(|c| c.modification.vals.first())
                .map(|v| {
                    let units: Vec<u16> = v.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
                    String::from_utf16_lossy(&units).trim_matches('"').to_string()
                })
        };
        let (Some(old), Some(new)) = (pwd(LdapModifyType::Delete), pwd(LdapModifyType::Add)) else {
            return (LdapResultCode::InsufficentAccessRights, "00002098: SecErr: DSID-03150F94, problem 4003\0");
        };
        let usn = self.next_usn();
        let mut users = self.0.lock().unwrap();
        let Some(u) = users.iter_mut().find(|u| u.dn().eq_ignore_ascii_case(&m.dn)) else {
            return (LdapResultCode::NoSuchObject, "0000208D: NameErr: DSID-0310028D, problem 2001 (NO_OBJECT)\0");
        };
        if old != u.password.as_deref().unwrap_or(USER_PASS) {
            return (
                LdapResultCode::ConstraintViolation,
                "00000056: AtrErr: DSID-03190F80, #1: 0: 00000056: problem 1005 (CONSTRAINT_ATT_TYPE), data 0, Att 9005a (unicodePwd)\0",
            );
        }
        if new.chars().count() < 8 || new == old {
            return (
                LdapResultCode::ConstraintViolation,
                "0000052D: AtrErr: DSID-03191083, #1: 0: 0000052D: problem 1005 (CONSTRAINT_ATT_TYPE), data 0, Att 9005a (unicodePwd)\0",
            );
        }
        u.password = Some(new);
        u.pwd_last_set = NOW_FILETIME + 1;
        u.usn = usn;
        (LdapResultCode::Success, "")
    }

    fn next_usn(&self) -> u64 {
        self.highest_usn() + 1
    }

    /// Заменить все учётки каталога (например, «AD вернул пустую выборку»); возвращает прежние.
    pub fn replace_users(&self, users: Vec<FakeUser>) -> Vec<FakeUser> {
        std::mem::replace(&mut *self.0.lock().unwrap(), users)
    }

    /// Удалить учётку из каталога.
    pub fn remove(&self, sam: &str) {
        self.0.lock().unwrap().retain(|u| u.sam != sam);
    }

    /// Отключить учётку (как в «Пользователи и компьютеры AD»): UAC 514, USN растёт.
    pub fn disable(&self, sam: &str) {
        let usn = self.next_usn();
        let mut g = self.0.lock().unwrap();
        let u = g.iter_mut().find(|u| u.sam == sam).expect("нет такой учётки");
        u.uac |= 2;
        u.status = Some("533");
        u.usn = usn;
    }

    pub fn enable(&self, sam: &str) {
        let usn = self.next_usn();
        let mut g = self.0.lock().unwrap();
        let u = g.iter_mut().find(|u| u.sam == sam).expect("нет такой учётки");
        u.uac &= !2;
        u.status = None;
        u.usn = usn;
    }

    pub fn add(&self, mut u: FakeUser) {
        u.usn = self.next_usn();
        self.0.lock().unwrap().push(u);
    }

    pub fn snapshot(&self) -> Vec<FakeUser> {
        self.0.lock().unwrap().clone()
    }
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

#[derive(Default)]
pub struct Stats {
    pub service_binds: AtomicUsize,
    pub user_binds: AtomicUsize,
    /// Выполненные (успешные) modify — для проверки, что повтор задачи не пишет второй раз.
    pub modifies: AtomicUsize,
}

/// Поддельный DC на 127.0.0.1 со случайным портом.
pub struct FakeDc {
    addr: SocketAddr,
    pub stats: Arc<Stats>,
    listener: JoinHandle<()>,
}

impl FakeDc {
    pub async fn start(ca: &TestCa, dir: Directory) -> Self {
        let acceptor = ca.acceptor();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stats = Arc::new(Stats::default());
        let st = stats.clone();
        let listener = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let (acceptor, st, dir) = (acceptor.clone(), st.clone(), dir.clone());
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else { return };
                    serve(tls, st, dir).await;
                });
            }
        });
        Self { addr, stats, listener }
    }

    /// «Выключить» DC: порт закрывается.
    pub async fn stop(self) {
        self.listener.abort();
        let _ = self.listener.await;
    }

    pub fn endpoint(&self) -> String {
        self.addr.to_string()
    }
}

async fn serve(tls: tokio_rustls::server::TlsStream<tokio::net::TcpStream>, st: Arc<Stats>, dir: Directory) {
    let (r, w) = tokio::io::split(tls);
    let mut reader = FramedRead::new(r, LdapCodec::default());
    let mut writer = FramedWrite::new(w, LdapCodec::default());
    let mut service = false;
    let mut writer_bound = false;
    // DN пользователя, вошедшего на этом соединении: ему можно сменить свой пароль.
    let mut user_bound: Option<String> = None;
    while let Some(Ok(msg)) = reader.next().await {
        let id = msg.msgid;
        let users = dir.snapshot();
        let replies = match msg.op {
            LdapOp::BindRequest(b) => {
                let LdapBindCred::Simple(pass) = b.cred else { break };
                writer_bound = b.dn.eq_ignore_ascii_case(WRITER_DN) && pass == WRITER_PASS;
                if writer_bound {
                    service = true;
                    vec![bind_reply(id, (LdapResultCode::Success, String::new()))]
                } else {
                    let r = bind(&users, &b.dn, &pass, &st, &mut service);
                    user_bound = (r.0 == LdapResultCode::Success && !service).then(|| b.dn.clone());
                    vec![bind_reply(id, r)]
                }
            }
            LdapOp::ModifyRequest(m) => {
                let (code, text) = if writer_bound {
                    dir.modify(&m)
                } else if user_bound.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(&m.dn)) {
                    dir.change_own_password(&m)
                } else {
                    (
                        LdapResultCode::InsufficentAccessRights,
                        "00002098: SecErr: DSID-03150F94, problem 4003 (INSUFF_ACCESS_RIGHTS), data 0\0",
                    )
                };
                if code == LdapResultCode::Success {
                    st.modifies.fetch_add(1, Ordering::Relaxed);
                }
                vec![LdapMsg { msgid: id, op: LdapOp::ModifyResponse(result(code, text)), ctrl: vec![] }]
            }
            LdapOp::SearchRequest(s) if s.base.is_empty() && s.scope == LdapSearchScope::Base => vec![
                entry(
                    id,
                    String::new(),
                    vec![("highestCommittedUSN".into(), vec![dir.highest_usn().to_string().into_bytes()])],
                ),
                done(id, LdapResultCode::Success, "", vec![]),
            ],
            LdapOp::SearchRequest(_) if !service => vec![done(
                id,
                LdapResultCode::OperationsError,
                "000004DC: LdapErr: DSID-0C090A5C, comment: In order to perform this operation a successful bind must be completed on the connection., data 0, v4563\0",
                vec![],
            )],
            LdapOp::SearchRequest(s) if s.scope == LdapSearchScope::Base => {
                let domain: Entry = (
                    BASE_DN.into(),
                    vec![
                        ("objectClass".into(), vec![b"top".to_vec(), b"domain".to_vec()]),
                        ("maxPwdAge".into(), vec![format!("-{}", MAX_PWD_AGE_DAYS * 86_400 * 10_000_000).into_bytes()]),
                    ],
                );
                let found = users
                    .iter()
                    .find(|u| u.dn().eq_ignore_ascii_case(&s.base))
                    .map(|u| dir.user_entry(u))
                    .or_else(|| dir.group_entries().into_iter().find(|(dn, _)| dn.eq_ignore_ascii_case(&s.base)))
                    .or_else(|| s.base.eq_ignore_ascii_case(BASE_DN).then_some(domain));
                match found {
                    Some((dn, a)) if matches(&s.filter, &a) => {
                        vec![entry(id, dn, a), done(id, LdapResultCode::Success, "", vec![])]
                    }
                    Some(_) => vec![done(id, LdapResultCode::Success, "", vec![])],
                    None => vec![done(
                        id,
                        LdapResultCode::NoSuchObject,
                        "0000208D: NameErr: DSID-0310028D, problem 2001 (NO_OBJECT)\0",
                        vec![],
                    )],
                }
            }
            LdapOp::SearchRequest(s) if dir.group_entries().iter().any(|(_, a)| matches(&s.filter, a)) => {
                let mut out: Vec<LdapMsg> = dir
                    .group_entries()
                    .into_iter()
                    .filter(|(_, a)| matches(&s.filter, a))
                    .map(|(dn, a)| entry(id, dn, a))
                    .collect();
                out.push(done(id, LdapResultCode::Success, "", vec![]));
                out
            }
            LdapOp::SearchRequest(s) => {
                let found: Vec<Entry> = users
                    .iter()
                    .map(|u| dir.user_entry(u))
                    .chain(dir.computer_entries())
                    .filter(|(_, a)| matches(&s.filter, a))
                    .collect();
                let paged = msg.ctrl.iter().find_map(|c| match c {
                    LdapControl::SimplePagedResults { size, cookie } => Some((*size, cookie.clone())),
                    _ => None,
                });
                let (from, to, ctrl) = match paged {
                    Some((size, cookie)) => {
                        let from =
                            if cookie.is_empty() { 0 } else { String::from_utf8(cookie).unwrap().parse().unwrap() };
                        let to = (from + usize::try_from(size).unwrap()).min(found.len());
                        let next = if to < found.len() { to.to_string().into_bytes() } else { Vec::new() };
                        (from, to, vec![LdapControl::SimplePagedResults { size: 0, cookie: next }])
                    }
                    None => (0, found.len(), vec![]),
                };
                let mut out: Vec<LdapMsg> =
                    found[from..to].iter().map(|(dn, a)| entry(id, dn.clone(), a.clone())).collect();
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

fn bind(users: &[FakeUser], dn: &str, pass: &str, st: &Stats, service: &mut bool) -> (LdapResultCode, String) {
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
        Some(u) if pass == u.password.as_deref().unwrap_or(USER_PASS) => match u.status {
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
    LdapMsg {
        msgid,
        op: LdapOp::BindResponse(LdapBindResponse { res: result(code, &message), saslcreds: None }),
        ctrl: vec![],
    }
}

fn entry(msgid: i32, dn: String, attrs: Vec<(String, Vec<Vec<u8>>)>) -> LdapMsg {
    let attributes = attrs.into_iter().map(|(atype, vals)| LdapPartialAttribute { atype, vals }).collect();
    LdapMsg { msgid, op: LdapOp::SearchResultEntry(LdapSearchResultEntry { dn, attributes }), ctrl: vec![] }
}

fn done(msgid: i32, code: LdapResultCode, message: &str, ctrl: Vec<LdapControl>) -> LdapMsg {
    LdapMsg { msgid, op: LdapOp::SearchResultDone(result(code, message)), ctrl }
}

/// Собственный CA домена для тестов.
pub struct TestCa {
    issuer: rcgen::CertifiedIssuer<'static, rcgen::KeyPair>,
}

impl TestCa {
    pub fn new(name: &str) -> Self {
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.distinguished_name.push(rcgen::DnType::CommonName, name);
        Self { issuer: rcgen::CertifiedIssuer::self_signed(params, rcgen::KeyPair::generate().unwrap()).unwrap() }
    }

    /// Сертификат CA в PEM — то, что администратор вставит в админке.
    pub fn pem(&self) -> String {
        self.issuer.pem()
    }

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

/// Номер для уникальных имён в одном прогоне.
pub fn seq() -> usize {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    SEQ.fetch_add(1, Ordering::Relaxed)
}
