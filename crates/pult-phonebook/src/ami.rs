//! Звонок по клику через Asterisk Manager Interface (FreePBX 16 и любая АТС на Asterisk).
//!
//! Портал подключается к AMI, входит по MD5-вызову (пароль по сети не передаётся) и командой
//! Originate сначала звонит на внутренний номер звонящего; когда тот снимает трубку, АТС набирает
//! абонента. Пользователь AMI во FreePBX («Asterisk Manager Users»): write — `originate`,
//! read — `call`, permit — только адрес сервера портала.

use crate::telephony::{CallError, CallState};
use md5::{Digest, Md5};
use pult_core::Secret;
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpStream,
    time::timeout,
};
use uuid::Uuid;

/// Куда и как звонить.
pub struct AmiConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub secret: Secret,
    /// Канал звонящего, `{ext}` — его внутренний номер (`Local/{ext}@from-internal`, `PJSIP/{ext}`).
    pub channel: String,
    /// Контекст набора абонента (во FreePBX — `from-internal`).
    pub context: String,
}

/// Сколько ждать, пока звонящий снимет трубку (АТС), и сколько — ответа АТС (портал).
const RING_MS: u64 = 30_000;
const WAIT: Duration = Duration::from_secs(45);
const CONNECT: Duration = Duration::from_secs(8);
/// Строка ответа AMI длиннее этого — не AMI.
const MAX_LINE: u64 = 8 * 1024;

type Msg = Vec<(String, String)>;

fn get<'a>(m: &'a Msg, k: &str) -> Option<&'a str> {
    m.iter().find(|(a, _)| a.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
}

/// Значение поля без переводов строк: иначе в AMI можно дописать свою команду.
fn safe(v: &str) -> Result<&str, CallError> {
    if v.contains(['\r', '\n']) {
        Err(CallError::Other("недопустимый символ в команде АТС".into()))
    } else {
        Ok(v)
    }
}

/// Ключ входа: md5(вызов + пароль) в hex.
fn login_key(challenge: &str, secret: &str) -> String {
    let d = Md5::digest(format!("{challenge}{secret}").as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

struct Conn {
    s: BufReader<TcpStream>,
}

impl Conn {
    async fn send(&mut self, fields: &[(&str, &str)]) -> Result<(), CallError> {
        let mut out = String::new();
        for (k, v) in fields {
            out.push_str(k);
            out.push_str(": ");
            out.push_str(safe(v)?);
            out.push_str("\r\n");
        }
        out.push_str("\r\n");
        self.s.get_mut().write_all(out.as_bytes()).await.map_err(|_| CallError::Unavailable)
    }

    async fn line(&mut self) -> Result<String, CallError> {
        let mut buf = String::new();
        let n = (&mut self.s).take(MAX_LINE).read_line(&mut buf).await.map_err(|_| CallError::Unavailable)?;
        if n == 0 {
            return Err(CallError::Unavailable);
        }
        Ok(buf.trim_end_matches(['\r', '\n']).to_string())
    }

    /// Одно сообщение AMI: строки «Ключ: значение» до пустой строки.
    async fn read(&mut self) -> Result<Msg, CallError> {
        let mut m = Msg::new();
        loop {
            let l = self.line().await?;
            if l.is_empty() {
                if m.is_empty() {
                    continue;
                }
                return Ok(m);
            }
            if let Some((k, v)) = l.split_once(':') {
                m.push((k.trim().to_string(), v.trim().to_string()));
            }
            if m.len() > 500 {
                return Err(CallError::Other("слишком длинный ответ АТС".into()));
            }
        }
    }

    /// Ответ на действие `id`; события по пути пропускаются.
    async fn response(&mut self, id: &str) -> Result<Msg, CallError> {
        loop {
            let m = self.read().await?;
            if get(&m, "Response").is_some() && get(&m, "ActionID") == Some(id) {
                return Ok(m);
            }
        }
    }
}

async fn connect(cfg: &AmiConfig) -> Result<Conn, CallError> {
    let tcp = TcpStream::connect((cfg.host.as_str(), cfg.port)).await.map_err(|_| CallError::Unavailable)?;
    let mut c = Conn { s: BufReader::new(tcp) };
    if !c.line().await?.starts_with("Asterisk Call Manager") {
        return Err(CallError::Other("по адресу отвечает не AMI".into()));
    }
    c.send(&[("Action", "Challenge"), ("ActionID", "pult-challenge"), ("AuthType", "MD5")]).await?;
    let m = c.response("pult-challenge").await?;
    let challenge = get(&m, "Challenge").ok_or(CallError::Denied)?.to_string();
    let key = login_key(&challenge, cfg.secret.expose());
    c.send(&[
        ("Action", "Login"),
        ("ActionID", "pult-login"),
        ("AuthType", "MD5"),
        ("Username", &cfg.user),
        ("Key", &key),
        ("Events", "call"),
    ])
    .await?;
    let m = c.response("pult-login").await?;
    if get(&m, "Response") != Some("Success") {
        return Err(CallError::Denied);
    }
    Ok(c)
}

struct Entry {
    owner: Uuid,
    state: CallState,
    at: Instant,
}

/// Звонки последних минут (портал — один процесс; окно звонка опрашивает раз в секунду).
static CALLS: LazyLock<Mutex<HashMap<String, Entry>>> = LazyLock::new(Mutex::default);

fn put(id: &str, owner: Uuid, state: &str, error: Option<String>) {
    let mut calls = CALLS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    calls.retain(|_, e| e.at.elapsed() < Duration::from_secs(600));
    let done = state != "calling";
    calls.insert(
        id.to_string(),
        Entry { owner, state: CallState { id: id.to_string(), state: state.into(), error, done }, at: Instant::now() },
    );
}

/// Занять «линию» пользователя: проверка и запись под одной блокировкой, чтобы параллельные запросы
/// не подняли несколько звонков сразу.
fn reserve(id: &str, owner: Uuid) -> bool {
    let mut calls = CALLS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if calls.values().any(|e| e.owner == owner && !e.state.done && e.at.elapsed() < WAIT) {
        return false;
    }
    let state = CallState { id: id.to_string(), state: "calling".into(), error: None, done: false };
    calls.insert(id.to_string(), Entry { owner, state, at: Instant::now() });
    true
}

fn release(id: &str) {
    CALLS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(id);
}

/// Состояние звонка; чужие звонки не видны.
pub fn state(owner: Uuid, id: &str) -> Result<CallState, CallError> {
    let calls = CALLS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match calls.get(id) {
        Some(e) if e.owner == owner => Ok(e.state.clone()),
        _ => Err(CallError::NotFound),
    }
}

/// Причина неудачи из OriginateResponse (коды Asterisk `AST_CONTROL_*`).
fn reason(code: Option<&str>) -> &'static str {
    match code {
        Some("0") => "Ваш телефон не зарегистрирован на АТС или номер не найден.",
        Some("1") => "Звонок сброшен.",
        Some("3") => "Вы не сняли трубку.",
        Some("5") => "Ваша линия занята.",
        Some("8") => "АТС перегружена, попробуйте позже.",
        _ => "Звонок не состоялся.",
    }
}

/// Позвонить: телефон `caller_ext` звонит первым, после ответа АТС набирает `target_ext`.
pub async fn start_call(cfg: AmiConfig, owner: Uuid, caller_ext: &str, target_ext: &str) -> Result<String, CallError> {
    let id = Uuid::now_v7().to_string();
    if !reserve(&id, owner) {
        return Err(CallError::Busy);
    }
    match originate(&cfg, owner, &id, caller_ext, target_ext).await {
        Ok(()) => Ok(id),
        Err(e) => {
            release(&id);
            Err(e)
        }
    }
}

async fn originate(
    cfg: &AmiConfig,
    owner: Uuid,
    id: &str,
    caller_ext: &str,
    target_ext: &str,
) -> Result<(), CallError> {
    let channel = cfg.channel.replace("{ext}", caller_ext);
    let mut c = timeout(CONNECT, connect(cfg)).await.map_err(|_| CallError::Unavailable)??;
    c.send(&[
        ("Action", "Originate"),
        ("ActionID", id),
        ("Channel", &channel),
        ("Context", &cfg.context),
        ("Exten", target_ext),
        ("Priority", "1"),
        ("CallerID", caller_ext),
        ("Timeout", &RING_MS.to_string()),
        ("Async", "true"),
    ])
    .await?;
    let m = timeout(CONNECT, c.response(id)).await.map_err(|_| CallError::Unavailable)??;
    if get(&m, "Response") != Some("Success") {
        let msg = get(&m, "Message").unwrap_or_default();
        return Err(if msg.to_lowercase().contains("permission") {
            CallError::Denied
        } else {
            CallError::Other(msg.chars().take(200).collect())
        });
    }
    let call = id.to_string();
    tokio::spawn(async move {
        let outcome = timeout(WAIT, async {
            loop {
                let m = c.read().await?;
                if get(&m, "Event") == Some("OriginateResponse") && get(&m, "ActionID") == Some(call.as_str()) {
                    return Ok::<_, CallError>(if get(&m, "Response") == Some("Success") {
                        ("connected", None)
                    } else {
                        ("failed", Some(reason(get(&m, "Reason")).to_string()))
                    });
                }
            }
        })
        .await;
        let (st, err) = match outcome {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => ("failed", Some(e.to_string())),
            Err(_) => ("failed", Some("АТС не ответила.".into())),
        };
        put(&call, owner, st, err);
        let _ = c.send(&[("Action", "Logoff"), ("ActionID", "pult-logoff")]).await;
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn md5_key() {
        // md5(вызов + пароль): md5("abc") из RFC 1321.
        assert_eq!(login_key("a", "bc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(login_key("", ""), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn no_line_breaks() {
        assert!(safe("4512").is_ok());
        assert!(safe("4512\r\nAction: Command").is_err());
    }

    /// Поддельная АТС: проверяет вход и Originate, отвечает успехом и событием.
    async fn fake_pbx(answer: &'static str) -> (u16, tokio::task::JoinHandle<Vec<Msg>>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        let h = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut c = Conn { s: BufReader::new(s) };
            c.s.get_mut().write_all(b"Asterisk Call Manager/7.0.3\r\n").await.unwrap();
            let mut seen = Vec::new();
            loop {
                let Ok(m) = c.read().await else { break };
                let id = get(&m, "ActionID").unwrap_or_default().to_string();
                let action = get(&m, "Action").unwrap_or_default().to_string();
                seen.push(m.clone());
                match action.as_str() {
                    "Challenge" => {
                        c.send(&[("Response", "Success"), ("ActionID", &id), ("Challenge", "840415273")]).await.unwrap()
                    }
                    "Login" => {
                        let ok = get(&m, "Key") == Some(login_key("840415273", "s3cret-ami").as_str());
                        c.send(&[("Response", if ok { "Success" } else { "Error" }), ("ActionID", &id)]).await.unwrap();
                    }
                    "Originate" => {
                        c.send(&[
                            ("Response", "Success"),
                            ("ActionID", &id),
                            ("Message", "Originate successfully queued"),
                        ])
                        .await
                        .unwrap();
                        c.send(&[("Event", "Newchannel"), ("Channel", "Local/4500@from-internal-0001")]).await.unwrap();
                        c.send(&[
                            ("Event", "OriginateResponse"),
                            ("ActionID", &id),
                            ("Response", answer),
                            ("Reason", if answer == "Success" { "4" } else { "3" }),
                        ])
                        .await
                        .unwrap();
                    }
                    "Logoff" => break,
                    _ => {}
                }
            }
            seen
        });
        (port, h)
    }

    fn cfg(port: u16, secret: &str) -> AmiConfig {
        AmiConfig {
            host: "127.0.0.1".into(),
            port,
            user: "pult".into(),
            secret: Secret::new(secret.to_string()),
            channel: "Local/{ext}@from-internal".into(),
            context: "from-internal".into(),
        }
    }

    async fn settle(owner: Uuid, id: &str) -> CallState {
        for _ in 0..50 {
            let s = state(owner, id).unwrap();
            if s.done {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("звонок не завершился");
    }

    #[tokio::test]
    async fn originate_rings_caller_then_target() {
        let (port, pbx) = fake_pbx("Success").await;
        let owner = Uuid::now_v7();
        let id = start_call(cfg(port, "s3cret-ami"), owner, "4500", "4512").await.unwrap();
        let s = settle(owner, &id).await;
        assert_eq!((s.state.as_str(), s.error), ("connected", None));
        assert!(state(Uuid::now_v7(), &id).is_err(), "чужой звонок не виден");
        let seen = pbx.await.unwrap();
        let login = seen.iter().find(|m| get(m, "Action") == Some("Login")).unwrap();
        assert!(get(login, "Secret").is_none(), "пароль по сети не идёт");
        let o = seen.iter().find(|m| get(m, "Action") == Some("Originate")).unwrap();
        assert_eq!(get(o, "Channel"), Some("Local/4500@from-internal"));
        assert_eq!(
            (get(o, "Exten"), get(o, "Context"), get(o, "CallerID")),
            (Some("4512"), Some("from-internal"), Some("4500"))
        );
    }

    #[tokio::test]
    async fn not_answered_and_wrong_password() {
        let (port, _pbx) = fake_pbx("Failure").await;
        let owner = Uuid::now_v7();
        let id = start_call(cfg(port, "s3cret-ami"), owner, "4500", "4512").await.unwrap();
        let s = settle(owner, &id).await;
        assert_eq!((s.state.as_str(), s.error.as_deref()), ("failed", Some("Вы не сняли трубку.")));

        let (port, _pbx) = fake_pbx("Success").await;
        assert_eq!(start_call(cfg(port, "wrong"), Uuid::now_v7(), "4500", "4512").await, Err(CallError::Denied));
        assert_eq!(start_call(cfg(1, "x"), Uuid::now_v7(), "4500", "4512").await, Err(CallError::Unavailable));
    }

    #[tokio::test]
    async fn one_call_at_a_time_and_failed_start_frees_the_line() {
        let me = Uuid::now_v7();
        assert!(reserve("a", me));
        assert!(!reserve("b", me), "второй звонок, пока идёт первый, не поднимаем");
        assert!(reserve("c", Uuid::now_v7()), "у другого пользователя своя линия");
        release("a");
        assert!(reserve("d", me));
        release("d");
        assert_eq!(start_call(cfg(1, "x"), me, "4500", "4512").await, Err(CallError::Unavailable));
        assert!(reserve("e", me), "неудачный звонок линию не держит");
        assert_eq!(state(me, "e").map(|s| s.state), Ok("calling".to_string()));
    }
}
