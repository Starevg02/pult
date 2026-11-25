//! `pult-agent` — агент рабочей станции. Службу ставит MSI (`pult-agent.exe service`);
//! остальные команды — для проверки на ПК и для подписи обновлений на ПК администратора.

use anyhow::Context;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "pult-agent", version, about = "Агент Pult: сессии пользователей → портал")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Точка входа службы: Windows (запускает SCM) или Linux (запускает systemd).
    Service,
    /// Работать в консоли (проверка; нужны права администратора). Настройки — из реестра.
    Run,
    /// Показать сведения и сессии этого ПК (JSON) — то, что уйдёт на портал.
    Sessions {
        /// Windows: все сессии, включая служебные; Linux: ещё и оборудование с пакетами.
        #[arg(long)]
        all: bool,
    },
    /// Создать ключ подписи обновлений (на ПК администратора; хранить офлайн).
    Keygen {
        /// Файл закрытого ключа (PKCS#8); не перезаписывается.
        #[arg(long)]
        out: PathBuf,
    },
    /// Подписать новую версию агента для загрузки в портал.
    Sign {
        #[arg(long)]
        key: PathBuf,
        #[arg(long)]
        version: String,
        file: PathBuf,
    },
}

#[allow(clippy::print_stdout)]
fn say(s: &str) {
    println!("{s}");
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Service => service(),
        Command::Run => console(),
        Command::Sessions { all } => sessions(all),
        Command::Keygen { out } => {
            let (pkcs8, public) = pult_agent::update::keygen()?;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&out)
                .and_then(|mut f| std::io::Write::write_all(&mut f, &pkcs8))
                .with_context(|| format!("запись ключа в {}", out.display()))?;
            say(&format!(
                "Закрытый ключ: {} — храните офлайн.\nОткрытый ключ (UpdatePublicKey для агентов):\n{public}",
                out.display()
            ));
            Ok(())
        }
        Command::Sign { key, version, file } => {
            let pkcs8 = std::fs::read(&key).with_context(|| format!("чтение ключа {}", key.display()))?;
            let data = std::fs::read(&file).with_context(|| format!("чтение {}", file.display()))?;
            let (signature, sha256) = pult_agent::update::sign(&pkcs8, &version, &data)?;
            say(&serde_json::to_string_pretty(&serde_json::json!({
                "version": version, "sha256": sha256, "signature": signature,
            }))?);
            Ok(())
        }
    }
}

#[cfg(windows)]
fn log_to_file() -> anyhow::Result<()> {
    let dir = pult_agent::config::data_dir();
    // Права на папку — до первого файла: журнал создаётся уже защищённым.
    pult_agent::host::secure_dir(&dir)?;
    let path = dir.join("agent.log");
    // Журнал не растёт бесконечно: больше 5 МБ — в agent.log.old.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 5 * 1024 * 1024) {
        let _ = std::fs::rename(&path, dir.join("agent.log.old"));
    }
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        tracing_subscriber::fmt().with_ansi(false).with_writer(std::sync::Mutex::new(f)).init();
    }
    Ok(())
}

#[cfg(windows)]
fn service() -> anyhow::Result<()> {
    log_to_file()?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "служба агента запускается");
    pult_agent::service::start().context("запуск службы (команда service только для SCM)")
}

#[cfg(windows)]
fn console() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    let agent = pult_agent::windows_agent()?;
    let (_stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::runtime::Runtime::new()?.block_on(pult_agent::run::run(agent, Default::default(), stop_rx));
    Ok(())
}

#[cfg(windows)]
fn sessions(all: bool) -> anyhow::Result<()> {
    use pult_agent::run::Host;
    // Только чтение: папку агента и его состояние не трогаем.
    let host = pult_agent::host::WindowsHost::new(&pult_agent::sys::settings());
    let mut r = host.collect()?;
    if all {
        r.sessions = pult_agent::wts::sessions()?;
    }
    say(&serde_json::to_string_pretty(&r)?);
    Ok(())
}

/// Linux: служба systemd запускает `pult-agent service`, журнал — в journald (stderr).
#[cfg(target_os = "linux")]
fn service() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_ansi(false).with_writer(std::io::stderr).init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "агент запускается");
    let agent = pult_agent::linux::linux_agent()?;
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::runtime::Runtime::new()?.block_on(async move {
        // systemd останавливает службу сигналом SIGTERM.
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            let _ = stop_tx.send(true);
        });
        pult_agent::run::run(agent, Default::default(), stop_rx).await;
        anyhow::Ok(())
    })
}

#[cfg(target_os = "linux")]
fn console() -> anyhow::Result<()> {
    service()
}

#[cfg(target_os = "linux")]
fn sessions(all: bool) -> anyhow::Result<()> {
    use pult_agent::run::Host;
    let host = pult_agent::linux::LinuxHost::new(&pult_agent::linux::settings());
    let mut r = host.collect()?;
    if all {
        r.inventory = host.inventory();
    }
    say(&serde_json::to_string_pretty(&r)?);
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn service() -> anyhow::Result<()> {
    anyhow::bail!("агент работает в Windows и Linux")
}

#[cfg(not(any(windows, target_os = "linux")))]
fn console() -> anyhow::Result<()> {
    service()
}

#[cfg(not(any(windows, target_os = "linux")))]
fn sessions(_: bool) -> anyhow::Result<()> {
    service()
}
