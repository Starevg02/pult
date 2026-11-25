//! Linux: сеансы пользователей (`loginctl`), сведения о системе и оборудование (`/etc/os-release`,
//! `/sys/class/dmi/id`, `/proc`, `df`), установленные пакеты (`dpkg-query` или `rpm`).
//!
//! Настройки — `/etc/pult-agent.conf` (строки `PortalUrl=`, `EnrollToken=`), состояние — в
//! `/var/lib/pult-agent` (только root). Служба — systemd (`deploy/linux-agent/pult-agent.service`).
//! Обновление ставится пакетом дистрибутива: команду обновления от портала агент не выполняет.
//! Подключение к ПК с Linux портал не предлагает: агент сообщает сеансы и оборудование.

use crate::{
    AgentReport, Session, SessionState, client, config,
    config::Settings,
    inventory::{self, UninstallEntry},
    run,
};
use pult_api_types::{DeviceDisk, HardwareInventory};
use pult_core::Secret;
use std::{
    net::{IpAddr, UdpSocket},
    path::{Path, PathBuf},
    process::Command,
};

/// Файл настроек агента.
pub const CONF: &str = "/etc/pult-agent.conf";
/// Папка состояния (ключ агента).
pub const DATA_DIR: &str = "/var/lib/pult-agent";

/// `PortalUrl=…`, `EnrollToken=…`, `UpdatePublicKey=…`; `#` — комментарий. Кавычки вокруг значения снимаются.
pub fn parse_settings(text: &str) -> Settings {
    let mut s = Settings::default();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
        if v.is_empty() {
            continue;
        }
        match k.trim() {
            "PortalUrl" => s.portal = Some(v),
            "EnrollToken" => s.enroll_token = Some(Secret::new(v)),
            "UpdatePublicKey" => s.update_key = Some(v),
            _ => {}
        }
    }
    s
}

pub fn settings() -> Settings {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::metadata(CONF) else { return Settings::default() };
    if let Err(why) = conf_secure(meta.uid(), meta.mode()) {
        tracing::error!(file = CONF, "настройки агента не читаются: {why}");
        return Settings::default();
    }
    if meta.mode() & 0o004 != 0 {
        tracing::warn!(file = CONF, "файл настроек читают все пользователи: в нём токен регистрации (chmod 600)");
    }
    std::fs::read_to_string(CONF).map(|t| parse_settings(&t)).unwrap_or_default()
}

/// Агент работает от root и доверяет файлу адрес портала и ключ обновлений: менять файл может только root.
pub fn conf_secure(uid: u32, mode: u32) -> Result<(), &'static str> {
    if uid != 0 {
        Err("владелец файла — не root (chown root:root)")
    } else if mode & 0o022 != 0 {
        Err("файл могут менять не только root (chmod 600)")
    } else {
        Ok(())
    }
}

fn run_cmd(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd).args(args).env("LC_ALL", "C").output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// `PRETTY_NAME` из `/etc/os-release`.
pub fn pretty_name(os_release: &str) -> Option<String> {
    os_release
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
}

/// Сеанс из вывода `loginctl show-session <id> -p …` (строки `Ключ=значение`). Только сеансы
/// пользователей (`Class=user`) с числовым номером.
pub fn parse_session(text: &str) -> Option<Session> {
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('=')))
            .map(str::trim)
            .filter(|v| !v.is_empty())
    };
    if get("Class") != Some("user") {
        return None;
    }
    let id: u32 = get("Id")?.parse().ok()?;
    let name = get("Name")?;
    // Учётки домена через SSSD: `ivanov@corp.example` или `CORP\ivanov`.
    let (user, domain) = match (name.split_once('@'), name.split_once('\\')) {
        (Some((u, d)), _) => (u.to_string(), Some(d.to_string())),
        (None, Some((d, u))) => (u.to_string(), Some(d.to_string())),
        _ => (name.to_string(), None),
    };
    let remote = get("Remote") == Some("yes");
    let state = match get("State") {
        Some("active") => SessionState::Active,
        Some("online") => SessionState::Idle,
        Some("closing") => SessionState::Disconnected,
        _ => SessionState::Connected,
    };
    let station = if remote {
        get("Service").unwrap_or("remote").to_string()
    } else {
        get("TTY").or(get("Seat")).unwrap_or("console").to_string()
    };
    Some(Session {
        id,
        station,
        state,
        user: Some(user),
        domain,
        client_name: if remote { get("RemoteHost").map(str::to_string) } else { None },
        logon_at: None,
    })
}

fn sessions() -> Vec<Session> {
    let Some(list) = run_cmd("loginctl", &["list-sessions", "--no-legend", "--no-pager"]) else {
        return Vec::new();
    };
    list.lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter_map(|id| {
            let props = "-pId -pName -pClass -pState -pRemote -pRemoteHost -pService -pTTY -pSeat";
            let mut args = vec!["show-session", id];
            args.extend(props.split(' '));
            run_cmd("loginctl", &args)
        })
        .filter_map(|t| parse_session(&t))
        .collect()
}

/// Диски из `df -B1 --output=target,size,avail`: только настоящие файловые системы.
pub fn parse_df(text: &str) -> Vec<DeviceDisk> {
    const GB: u64 = 1024 * 1024 * 1024;
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (target, size, avail) = (f.next()?, f.next()?.parse::<u64>().ok()?, f.next()?.parse::<u64>().ok()?);
            let ok = target == "/" || ["/home", "/srv", "/var", "/data", "/opt"].iter().any(|p| target.starts_with(p));
            (ok && size >= GB).then(|| DeviceDisk { name: target.to_string(), size_gb: size / GB, free_gb: avail / GB })
        })
        .take(16)
        .collect()
}

/// Пакеты: строки `имя<TAB>версия<TAB>поставщик`.
pub fn parse_packages(text: &str) -> Vec<pult_api_types::InstalledSoftware> {
    inventory::software(text.lines().filter_map(|l| {
        let mut f = l.split('\t');
        let name = f.next()?.trim();
        (!name.is_empty()).then(|| UninstallEntry {
            name: Some(name.to_string()),
            version: f.next().map(str::to_string),
            publisher: f.next().map(|p| p.split('<').next().unwrap_or(p).trim().to_string()),
            ..Default::default()
        })
    }))
}

fn packages() -> Vec<pult_api_types::InstalledSoftware> {
    run_cmd("dpkg-query", &["-W", "-f", "${Package}\t${Version}\t${Maintainer}\n"])
        .or_else(|| run_cmd("rpm", &["-qa", "--qf", "%{NAME}\t%{VERSION}-%{RELEASE}\t%{VENDOR}\n"]))
        .map(|t| parse_packages(&t))
        .unwrap_or_default()
}

/// `MemTotal` из `/proc/meminfo`, МБ.
pub fn mem_total_mb(meminfo: &str) -> Option<u64> {
    let kb: u64 = meminfo.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.split_whitespace().next()?.parse().ok()?;
    Some(kb / 1024)
}

pub struct LinuxHost {
    portal: Option<(String, u16)>,
}

impl LinuxHost {
    pub fn new(settings: &Settings) -> Self {
        let portal = settings
            .portal
            .as_deref()
            .and_then(|p| client::base_url(p).ok())
            .and_then(|u| Some((u.host_str()?.to_string(), u.port_or_known_default()?)));
        Self { portal }
    }
}

/// Адрес этого ПК в сторону портала (UDP-«подключение» ничего не отправляет).
fn local_ip(host: &str, port: u16) -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect((host, port)).ok()?;
    s.local_addr().ok().map(|a| a.ip()).filter(|ip| !ip.is_unspecified())
}

impl run::Host for LinuxHost {
    fn collect(&self) -> anyhow::Result<AgentReport> {
        let hostname = read("/proc/sys/kernel/hostname").unwrap_or_default();
        let hostname = hostname.split('.').next().unwrap_or_default().to_string();
        let fqdn = run_cmd("hostname", &["--fqdn"]).map(|s| s.trim().to_lowercase()).filter(|s| s.contains('.'));
        Ok(AgentReport {
            domain: fqdn.as_deref().and_then(|f| f.split_once('.')).map(|(_, d)| d.to_string()),
            hostname,
            os: format!("linux-{}", std::env::consts::ARCH),
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            sessions: sessions(),
            fqdn,
            os_version: std::fs::read_to_string("/etc/os-release").ok().and_then(|t| pretty_name(&t)),
            ip_addrs: self
                .portal
                .as_ref()
                .and_then(|(h, p)| local_ip(h, *p))
                .map(|i| i.to_string())
                .into_iter()
                .collect(),
            inventory: None,
        })
    }

    fn inventory(&self) -> Option<HardwareInventory> {
        let dmi = |f: &str| read(&format!("/sys/class/dmi/id/{f}")).filter(|s| !inventory::placeholder(s));
        let cpu = std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("model name"))
                .and_then(|r| r.split_once(':'))
                .map(|(_, v)| v.split_whitespace().collect::<Vec<_>>().join(" "))
        });
        Some(HardwareInventory {
            manufacturer: dmi("sys_vendor"),
            model: dmi("product_name"),
            // Серийный номер читает только root — агент работает от root.
            serial_number: dmi("product_serial"),
            bios_version: dmi("bios_version"),
            cpu,
            memory_mb: std::fs::read_to_string("/proc/meminfo").ok().and_then(|t| mem_total_mb(&t)),
            disks: run_cmd("df", &["-B1", "--output=target,size,avail"]).map(|t| parse_df(&t)).unwrap_or_default(),
            software: packages(),
        })
    }

    fn install_update(&self, _data: &[u8]) -> anyhow::Result<()> {
        anyhow::bail!("на Linux агент обновляется пакетом дистрибутива")
    }
}

/// Папка состояния только для root.
pub fn secure_dir(dir: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    for entry in std::fs::read_dir(dir)?.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            std::fs::set_permissions(entry.path(), std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

pub fn data_dir() -> PathBuf {
    PathBuf::from(DATA_DIR)
}

/// Агент службы: настройки из `/etc/pult-agent.conf`, состояние в `/var/lib/pult-agent`.
pub fn linux_agent() -> anyhow::Result<run::Agent<LinuxHost>> {
    let settings = settings();
    let dir = data_dir();
    secure_dir(&dir)?;
    let host = LinuxHost::new(&settings);
    Ok(run::Agent::new(host, settings, config::state_path(&dir))?)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn settings_file_must_be_roots() {
        assert!(conf_secure(0, 0o100600).is_ok());
        assert!(conf_secure(0, 0o100644).is_ok(), "чтение всем — только предупреждение");
        assert!(conf_secure(1000, 0o100600).is_err());
        assert!(conf_secure(0, 0o100664).is_err());
        assert!(conf_secure(0, 0o100602).is_err());
    }

    #[test]
    fn settings_file() {
        let s = parse_settings(
            "# Pult\nPortalUrl = \"https://pult.example.test\"\nEnrollToken=pae_x\nUpdatePublicKey=\nOther=1\n",
        );
        assert_eq!(s.portal.as_deref(), Some("https://pult.example.test"));
        assert_eq!(s.enroll_token.as_ref().map(|t| t.expose().to_string()).as_deref(), Some("pae_x"));
        assert!(s.update_key.is_none());
    }

    #[test]
    fn sessions_from_loginctl() {
        let local = "Id=2\nName=ivanov@corp.example.test\nClass=user\nState=active\nRemote=no\nTTY=tty2\nSeat=seat0\nService=gdm-password\n";
        let s = parse_session(local).unwrap();
        assert_eq!(
            (s.id, s.user.as_deref(), s.domain.as_deref(), s.state),
            (2, Some("ivanov"), Some("corp.example.test"), SessionState::Active)
        );
        assert_eq!((s.station.as_str(), s.client_name.as_deref()), ("tty2", None));
        assert!(s.is_user_session());
        let ssh = "Id=15\nName=petrova\nClass=user\nState=online\nRemote=yes\nRemoteHost=10.1.2.3\nService=sshd\n";
        let s = parse_session(ssh).unwrap();
        assert_eq!(
            (s.station.as_str(), s.client_name.as_deref(), s.state),
            ("sshd", Some("10.1.2.3"), SessionState::Idle)
        );
        assert!(
            parse_session("Id=c1\nName=lightdm\nClass=greeter\nState=online\n").is_none(),
            "экран входа — не сеанс"
        );
        assert!(parse_session("Id=c3\nName=x\nClass=user\n").is_none(), "нечисловой номер");
    }

    #[test]
    fn system_facts() {
        assert_eq!(
            pretty_name("NAME=\"Debian\"\nPRETTY_NAME=\"Debian GNU/Linux 13 (trixie)\"\n").as_deref(),
            Some("Debian GNU/Linux 13 (trixie)")
        );
        assert_eq!(mem_total_mb("MemTotal:       16314564 kB\nMemFree: 1 kB\n"), Some(15932));
        let df = "Mounted on        1B-blocks        Avail\n/      105088212992  52544106496\n/boot  1023303680  800000000\n/home  214748364800 107374182400\n/run   1000000000 1000000000\n";
        let d = parse_df(df);
        assert_eq!(d.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["/", "/home"]);
        assert_eq!((d[0].size_gb, d[0].free_gb), (97, 48));
        let p = parse_packages("bash\t5.2.37-2\tMatthias Klose <doko@debian.org>\ncurl\t8.14.1-2\t\n\n");
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].name.as_str(), p[0].publisher.as_deref()), ("bash", Some("Matthias Klose")));
    }
}
