//! `pult-helper` — helper на ПК инженера: обрабатывает ссылки `pult://connect?t=…` из портала.
//!
//! Установка без прав администратора: `pult-helper.exe register --portal https://pult.example.local/`.

use anyhow::Context;
use clap::{Parser, Subcommand};
use pult_api_types::{ConnectParams, ConnectResult, ExchangeRequest, RemoteAction};
use pult_helper::{PROBE_PORTS, launch, parse_link, portal_base, validate_computer};
use std::{
    net::{TcpStream, ToSocketAddrs},
    path::PathBuf,
    time::Duration,
};

#[derive(Parser)]
#[command(name = "pult-helper", version, about = "Helper Pult: ссылки pult:// → mstsc /shadow и быстрые действия")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Зарегистрировать схему ссылок pult: для текущего пользователя (HKCU) и запомнить адрес портала.
    Register {
        /// Адрес портала, например https://pult.example.local/
        #[arg(long)]
        portal: String,
        /// Для теневого подключения спрашивать учётные данные (отдельная админская учётка инженера).
        #[arg(long)]
        prompt_credentials: bool,
    },
    /// Убрать схему ссылок и настройки helper.
    Unregister,
    /// Открыть ссылку pult://connect?t=… (так helper вызывает браузер).
    Open { link: String },
}

#[allow(clippy::print_stdout)]
fn say(s: &str) {
    println!("{s}");
}

/// Окно консоли не должно закрыться раньше, чем инженер прочтёт ошибку.
fn pause() {
    say("\nНажмите Enter, чтобы закрыть окно.");
    let _ = std::io::stdin().read_line(&mut String::new());
}

fn main() {
    let cli = Cli::parse();
    let interactive = matches!(cli.command, Command::Open { .. });
    if let Err(e) = run(cli.command) {
        say(&format!("Ошибка: {e:#}"));
        if interactive {
            pause();
        }
        std::process::exit(1);
    }
}

fn run(c: Command) -> anyhow::Result<()> {
    match c {
        Command::Register { portal, prompt_credentials } => {
            let base = portal_base(&portal)?;
            let exe = install_self()?.to_string_lossy().into_owned();
            register(&exe, base.as_str(), prompt_credentials)?;
            let creds = if prompt_credentials {
                "\nТеневое подключение: с запросом учётных данных"
            } else {
                ""
            };
            say(&format!("Готово: ссылки pult:// открывает {exe}\nПортал: {base}{creds}"));
            Ok(())
        }
        Command::Unregister => {
            unregister();
            say("Схема pult: и настройки helper удалены.");
            Ok(())
        }
        Command::Open { link } => open(&link),
    }
}

#[cfg(windows)]
fn register(exe: &str, portal: &str, prompt_credentials: bool) -> anyhow::Result<()> {
    pult_helper::win::register(exe, portal, prompt_credentials).context("запись в реестр (HKCU)")
}

#[cfg(windows)]
fn unregister() {
    pult_helper::win::unregister();
}

#[cfg(windows)]
fn configured_portal() -> Option<String> {
    pult_helper::win::get(pult_helper::win::SETTINGS_KEY, "Portal")
}

#[cfg(windows)]
fn prompt_credentials() -> bool {
    pult_helper::win::get(pult_helper::win::SETTINGS_KEY, "PromptCredentials").as_deref() == Some("1")
}

#[cfg(not(windows))]
fn register(_: &str, _: &str, _: bool) -> anyhow::Result<()> {
    anyhow::bail!("helper работает только в Windows")
}

#[cfg(not(windows))]
fn unregister() {}

#[cfg(not(windows))]
fn configured_portal() -> Option<String> {
    std::env::var("PULT_PORTAL").ok()
}

#[cfg(not(windows))]
fn prompt_credentials() -> bool {
    false
}

fn client() -> anyhow::Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent(concat!("pult-helper/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

fn open(link: &str) -> anyhow::Result<()> {
    let token = parse_link(link)?;
    let portal = configured_portal()
        .context("адрес портала не настроен: выполните «pult-helper.exe register --portal https://…»")?;
    let base = portal_base(&portal)?;
    let http = client()?;
    let resp = http
        .post(base.join("api/remote/v1/exchange")?)
        .json(&ExchangeRequest { token: token.clone() })
        .send()
        .map_err(|e| anyhow::anyhow!("нет связи с порталом: {}", e.without_url()))?;
    if !resp.status().is_success() {
        let body: serde_json::Value = resp.json().unwrap_or_default();
        anyhow::bail!("{}", body["message"].as_str().unwrap_or("портал отклонил ссылку"));
    }
    let p: ConnectParams = resp.json().context("ответ портала")?;
    let target = p.target_user.as_deref().map(|u| format!(", пользователь {u}")).unwrap_or_default();
    say(&format!("Pult: {} → {}{target}", action_name(p.action), p.computer));
    let (ok, error, details) = match launch(&p, prompt_credentials()) {
        Ok(Some((program, args))) => match std::process::Command::new(system_path(program)).args(&args).spawn() {
            // Аргументы — списком, без командной оболочки.
            Ok(_) => (true, None, None),
            Err(e) => (false, Some(format!("{program}: {e}")), None),
        },
        Ok(None) => {
            let d = probe(&p.computer);
            say(&serde_json::to_string_pretty(&d)?);
            (true, None, Some(d))
        }
        Err(e) => (false, Some(e.to_string()), None),
    };
    let report = ConnectResult { token, ok, error: error.clone(), details };
    if let Err(e) = http.post(base.join("api/remote/v1/result")?).json(&report).send() {
        say(&format!("Итог не отправлен порталу: {}", e.without_url()));
    }
    match error {
        Some(e) => anyhow::bail!(e),
        None => {
            if p.action == RemoteAction::Probe {
                pause();
            }
            Ok(())
        }
    }
}

/// Полный путь к программе Windows: поиск по имени начинается с папки helper (часто «Загрузки»),
/// куда можно подложить свой `mstsc.exe`.
fn system_path(relative: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    root.join(relative)
}

/// Постоянное место helper: `%LOCALAPPDATA%\Pult\pult-helper.exe` (а не папка, откуда его запустили).
fn install_self() -> anyhow::Result<PathBuf> {
    let current = std::env::current_exe()?;
    let Some(base) = std::env::var_os("LOCALAPPDATA") else { return Ok(current) };
    let dir = PathBuf::from(base).join("Pult");
    let target = dir.join("pult-helper.exe");
    let same = std::fs::canonicalize(&current).ok() == std::fs::canonicalize(&target).ok();
    if !same {
        std::fs::create_dir_all(&dir).with_context(|| format!("создание {}", dir.display()))?;
        std::fs::copy(&current, &target).with_context(|| format!("копирование в {}", target.display()))?;
    }
    Ok(target)
}

fn action_name(a: RemoteAction) -> &'static str {
    match a {
        RemoteAction::Shadow => "теневое подключение",
        RemoteAction::Files => "диск C$",
        RemoteAction::Manage => "управление компьютером",
        RemoteAction::Powershell => "PowerShell",
        RemoteAction::Probe => "проверка доступности",
    }
}

/// Проверка доступности с ПК инженера: ping и порты 3389/445/5985.
fn probe(computer: &str) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    if validate_computer(computer).is_err() {
        return out.into();
    }
    let ping = std::process::Command::new(system_path(r"System32\PING.EXE"))
        .args(["-n", "1", "-w", "1500", computer])
        .stdout(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    out.insert("ping".into(), ping.into());
    let addrs: Vec<_> = (computer, 0).to_socket_addrs().map(|a| a.collect()).unwrap_or_default();
    out.insert("dns".into(), (!addrs.is_empty()).into());
    for port in PROBE_PORTS {
        let open = addrs.iter().any(|a| {
            let mut a = *a;
            a.set_port(port);
            TcpStream::connect_timeout(&a, Duration::from_secs(2)).is_ok()
        });
        out.insert(port.to_string(), open.into());
    }
    out.into()
}
