//! Helper на ПК инженера.
//!
//! 1. Портал открывает ссылку `pult://connect?t=<одноразовый токен>` (схема `pult:` — в HKCU).
//! 2. Helper обменивает токен на параметры у портала по HTTPS. Адрес портала — из настройки helper
//!    (`register --portal`), не из ссылки: чужая страница не может направить helper на свой сервер.
//! 3. Запускает `mstsc /v:<ПК> /shadow:<id> [/control] [/noConsentPrompt]` или быстрое действие
//!    от учётки инженера и сообщает порталу итог.
//!
//! Всё, что приходит в ссылке и от портала, проверяется до запуска процесса:
//! имя ПК — только символы имени хоста, токен — base64url фиксированной длины.
//! Аргументы передаются списком (без командной оболочки), поэтому склеить лишнюю команду нельзя.

use pult_api_types::{ConnectParams, RemoteAction, ShadowMode};
use url::Url;

#[cfg(windows)]
pub mod win;

/// Порты проверки доступности: RDP, SMB (теневое подключение идёт через него и RPC), WinRM.
pub const PROBE_PORTS: [u16; 3] = [3389, 445, 5985];

/// Режим подключения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Только просмотр — всегда с запросом согласия (бывшая роль Viewer).
    View,
    /// Управление.
    Control,
}

/// Параметры, которые портал выдаёт по токену.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectInfo {
    pub computer: String,
    pub session_id: u32,
    pub mode: Mode,
    /// Политика ПК или режим требуют согласия пользователя.
    pub consent_required: bool,
    /// Спросить учётные данные (`/prompt`): тень под отдельной админской учёткой инженера,
    /// а не под той, под которой он работает (настройка helper, не ссылки).
    pub prompt_credentials: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HelperError {
    #[error("ссылка не pult://connect: {0}")]
    BadLink(String),
    #[error("в ссылке нет токена или он неверного вида")]
    BadToken,
    #[error("недопустимое имя компьютера: {0}")]
    BadComputer(String),
    #[error("адрес портала должен быть https:// (http — только localhost): {0}")]
    BadPortal(String),
    #[error("портал не прислал номер сессии")]
    NoSession,
    #[error("учётная запись должна быть вида ДОМЕН\\имя или имя@домен: {0}")]
    BadAccount(String),
    #[error("путь или аргумент с пробелом или кавычкой не передать через runas: {0}")]
    BadProgramPath(String),
}

/// Учётка для `runas /netonly`: `ДОМЕН\имя` или `имя@домен`, только символы имён AD.
pub fn validate_account(account: &str) -> Result<(), HelperError> {
    let bad = || HelperError::BadAccount(account.chars().take(80).collect());
    let (left, right) = account.split_once('\\').or_else(|| account.split_once('@')).ok_or_else(bad)?;
    let part = |s: &str| {
        !s.is_empty() && !s.starts_with(['-', '.']) && s.chars().all(|c| c.is_ascii_alphanumeric() || "-._".contains(c))
    };
    if account.len() <= 120 && part(left) && part(right) { Ok(()) } else { Err(bad()) }
}

/// Действия, которые можно запустить под другой учёткой через `runas /netonly` (аналог NetOnly):
/// учётные данные используются только для обращений к удалённому ПК. Проводник (диск C$) так не
/// запускается — Windows передаёт окно уже открытому Проводнику текущего пользователя.
pub fn netonly_supported(action: RemoteAction) -> bool {
    matches!(action, RemoteAction::Manage | RemoteAction::Powershell)
}

/// Аргументы `runas.exe /netonly /user:<учётка> "<программа> <аргументы>"`. `runas` принимает
/// командную строку одной строкой, поэтому пробелы и кавычки в пути и кавычки в аргументах запрещены;
/// имя ПК в аргументах уже проверено. Пароль спрашивает сам `runas`, helper его не видит.
pub fn netonly_args(account: &str, program: &str, args: &[String]) -> Result<Vec<String>, HelperError> {
    validate_account(account)?;
    if program.contains([' ', '"']) {
        return Err(HelperError::BadProgramPath(program.chars().take(120).collect()));
    }
    let mut line = program.to_string();
    for a in args {
        if a.contains('"') {
            return Err(HelperError::BadProgramPath(a.chars().take(120).collect()));
        }
        line.push(' ');
        line.push_str(a);
    }
    Ok(vec!["/netonly".into(), format!("/user:{account}"), line])
}

/// Адрес портала из настройки helper: только HTTPS (кроме localhost для разработки).
pub fn portal_base(portal: &str) -> Result<Url, HelperError> {
    let bad = || HelperError::BadPortal(portal.chars().take(80).collect());
    let mut u = Url::parse(portal.trim()).map_err(|_| bad())?;
    let local = matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(u.scheme() == "https" || (u.scheme() == "http" && local)) || u.host_str().is_none() {
        return Err(bad());
    }
    if !u.path().ends_with('/') {
        u.set_path(&format!("{}/", u.path()));
    }
    u.set_query(None);
    u.set_fragment(None);
    Ok(u)
}

/// Программа (путь относительно `%SystemRoot%`) и аргументы для действия
/// (проверка доступности выполняется самим helper — `None`).
pub fn launch(p: &ConnectParams, prompt_credentials: bool) -> Result<Option<(&'static str, Vec<String>)>, HelperError> {
    validate_computer(&p.computer)?;
    let c = &p.computer;
    Ok(match p.action {
        RemoteAction::Shadow => {
            let info = ConnectInfo {
                computer: c.clone(),
                session_id: p.session_id.ok_or(HelperError::NoSession)?,
                mode: match p.mode {
                    Some(ShadowMode::Control) => Mode::Control,
                    _ => Mode::View,
                },
                consent_required: p.consent_required,
                prompt_credentials,
            };
            Some((r"System32\mstsc.exe", mstsc_args(&info)?))
        }
        RemoteAction::Files => Some(("explorer.exe", vec![format!(r"\\{c}\C$")])),
        RemoteAction::Manage => Some((r"System32\mmc.exe", vec!["compmgmt.msc".into(), format!(r"/computer=\\{c}")])),
        RemoteAction::Powershell => Some((
            r"System32\WindowsPowerShell\v1.0\powershell.exe",
            vec!["-NoExit".into(), "-Command".into(), format!("Enter-PSSession -ComputerName {c}")],
        )),
        RemoteAction::Probe => None,
    })
}

/// Разбирает `pult://connect?t=…` и возвращает токен.
pub fn parse_link(link: &str) -> Result<String, HelperError> {
    let url = Url::parse(link).map_err(|_| HelperError::BadLink(link.chars().take(80).collect()))?;
    if url.scheme() != "pult" || url.host_str() != Some("connect") {
        return Err(HelperError::BadLink(link.chars().take(80).collect()));
    }
    let token = url.query_pairs().find(|(k, _)| k == "t").map(|(_, v)| v.into_owned()).ok_or(HelperError::BadToken)?;
    let ok =
        (16..=128).contains(&token.len()) && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok { Ok(token) } else { Err(HelperError::BadToken) }
}

/// Имя хоста или FQDN: буквы, цифры, `-`, `.`; не начинается с `-` и `/`; не IP-адрес
/// (портал выдаёт только имена ПК из домена).
pub fn validate_computer(name: &str) -> Result<(), HelperError> {
    let ok = !name.is_empty()
        && name.parse::<std::net::IpAddr>().is_err()
        && name.len() <= 253
        && !name.starts_with(['-', '.'])
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    if ok { Ok(()) } else { Err(HelperError::BadComputer(name.chars().take(80).collect())) }
}

/// Аргументы `mstsc`. Без согласия — только управление и только если политика не требует согласия.
pub fn mstsc_args(info: &ConnectInfo) -> Result<Vec<String>, HelperError> {
    validate_computer(&info.computer)?;
    let mut args = vec![format!("/v:{}", info.computer), format!("/shadow:{}", info.session_id)];
    if info.mode == Mode::Control {
        args.push("/control".into());
        if !info.consent_required {
            args.push("/noConsentPrompt".into());
        }
    }
    if info.prompt_credentials {
        args.push("/prompt".into());
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(mode: Mode, consent: bool) -> ConnectInfo {
        ConnectInfo {
            computer: "WS-001.corp.local".into(),
            session_id: 2,
            mode,
            consent_required: consent,
            prompt_credentials: false,
        }
    }

    #[test]
    fn view_always_asks_consent() {
        assert_eq!(mstsc_args(&info(Mode::View, false)).unwrap(), ["/v:WS-001.corp.local", "/shadow:2"]);
    }

    #[test]
    fn control_without_consent_only_when_policy_allows() {
        assert_eq!(
            mstsc_args(&info(Mode::Control, false)).unwrap(),
            ["/v:WS-001.corp.local", "/shadow:2", "/control", "/noConsentPrompt"]
        );
        assert_eq!(mstsc_args(&info(Mode::Control, true)).unwrap(), ["/v:WS-001.corp.local", "/shadow:2", "/control"]);
    }

    #[test]
    fn other_credentials_are_prompted_when_configured() {
        // Инженер работает под обычной учёткой, тень — под отдельной админской: mstsc спрашивает её.
        let mut i = info(Mode::View, true);
        i.prompt_credentials = true;
        assert_eq!(mstsc_args(&i).unwrap(), ["/v:WS-001.corp.local", "/shadow:2", "/prompt"]);
        let p = params(RemoteAction::Shadow, "ws-1");
        assert_eq!(
            launch(&p, true).unwrap(),
            Some((
                r"System32\mstsc.exe",
                vec!["/v:ws-1".into(), "/shadow:3".into(), "/control".into(), "/prompt".into()]
            ))
        );
        assert_eq!(
            launch(&params(RemoteAction::Files, "ws-1"), true).unwrap(),
            Some(("explorer.exe", vec![r"\\ws-1\C$".into()])),
            "остальные действия — как раньше"
        );
    }

    #[test]
    fn netonly_runs_with_other_account() {
        let p = params(RemoteAction::Manage, "ws-1");
        let (prog, args) = launch(&p, false).unwrap().unwrap();
        let full = format!(r"C:\Windows\{prog}");
        assert_eq!(
            netonly_args(r"CORP\adm-ivanov", &full, &args).unwrap(),
            ["/netonly", r"/user:CORP\adm-ivanov", r"C:\Windows\System32\mmc.exe compmgmt.msc /computer=\\ws-1"]
        );
        assert!(netonly_args("adm@corp.example.test", &full, &args).is_ok());
        for bad in ["adm", r"CORP\adm ivanov", r"CORP\", "adm\"&calc@x", "@corp", r"CORP\adm /savecred", "-x@corp"] {
            assert!(netonly_args(bad, &full, &args).is_err(), "{bad}");
        }
        assert!(netonly_args(r"CORP\adm", r"C:\Program Files\x.exe", &args).is_err());
        assert!(netonly_supported(RemoteAction::Powershell) && !netonly_supported(RemoteAction::Files));
    }

    #[test]
    fn computer_injection_is_rejected() {
        for bad in ["WS-001 /noConsentPrompt", "WS-001&calc", "/v:evil", "-x", "", "ws\"01", "ws;01"] {
            let mut i = info(Mode::View, true);
            i.computer = bad.into();
            assert!(mstsc_args(&i).is_err(), "{bad}");
        }
    }

    fn params(action: RemoteAction, computer: &str) -> ConnectParams {
        ConnectParams {
            connection_id: "c".into(),
            action,
            computer: computer.into(),
            session_id: Some(3),
            mode: Some(ShadowMode::Control),
            consent_required: true,
            target_user: None,
        }
    }

    #[test]
    fn actions() {
        let l = |a| launch(&params(a, "ws-1.example.local"), false).unwrap();
        assert_eq!(
            l(RemoteAction::Shadow),
            Some((r"System32\mstsc.exe", vec!["/v:ws-1.example.local".into(), "/shadow:3".into(), "/control".into()]))
        );
        // Программы — относительно %SystemRoot%, не по имени: иначе Windows сначала ищет их рядом с helper.
        assert_eq!(l(RemoteAction::Files), Some(("explorer.exe", vec![r"\\ws-1.example.local\C$".into()])));
        assert_eq!(
            l(RemoteAction::Manage),
            Some((r"System32\mmc.exe", vec!["compmgmt.msc".into(), r"/computer=\\ws-1.example.local".into()]))
        );
        let (prog, args) = l(RemoteAction::Powershell).unwrap();
        assert_eq!(
            (prog, args.last().unwrap().as_str()),
            (r"System32\WindowsPowerShell\v1.0\powershell.exe", "Enter-PSSession -ComputerName ws-1.example.local")
        );
        for ip in ["10.9.9.9", "::1", "127.0.0.1"] {
            assert!(validate_computer(ip).is_err(), "IP вместо имени ПК: {ip}");
        }
        assert_eq!(l(RemoteAction::Probe), None);
        for a in [RemoteAction::Files, RemoteAction::Manage, RemoteAction::Powershell, RemoteAction::Probe] {
            assert!(
                launch(&params(a, "ws; Remove-Item C:\\"), false).is_err(),
                "имя ПК проверяется для любого действия: {a:?}"
            );
        }
        let mut p = params(RemoteAction::Shadow, "ws-1");
        p.session_id = None;
        assert_eq!(launch(&p, false), Err(HelperError::NoSession));
    }

    #[test]
    fn portal_must_be_https() {
        assert_eq!(portal_base("https://pult.example.local").unwrap().as_str(), "https://pult.example.local/");
        assert!(portal_base("http://localhost:8080").is_ok());
        assert!(portal_base("http://pult.example.local").is_err());
        assert!(portal_base("javascript:alert(1)").is_err());
        assert!(portal_base("").is_err());
    }

    #[test]
    fn link_parsing() {
        let t = "AbCdEfGhIjKlMnOpQrStUv_-0123456789";
        assert_eq!(parse_link(&format!("pult://connect?t={t}")).unwrap(), t);
        assert_eq!(
            parse_link("https://connect?t=AbCdEfGhIjKlMnOp"),
            Err(HelperError::BadLink("https://connect?t=AbCdEfGhIjKlMnOp".into()))
        );
        assert_eq!(parse_link("pult://connect?t=short"), Err(HelperError::BadToken));
        assert_eq!(parse_link("pult://connect?t=AbCdEfGhIjKlMnOp%20%2Fcontrol"), Err(HelperError::BadToken));
        assert!(matches!(parse_link("pult://other?t=AbCdEfGhIjKlMnOpQr"), Err(HelperError::BadLink(_))));
    }
}
