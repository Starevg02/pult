//! Windows: сбор сведений о ПК и установка обновления.

use crate::{AgentReport, client, config, inventory, run, sys, wts};
use pult_api_types::HardwareInventory;
use std::{
    net::{IpAddr, UdpSocket},
    path::{Path, PathBuf},
};
use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;

pub struct WindowsHost {
    /// Хост и порт портала — чтобы узнать адрес ПК, с которого агент к нему ходит.
    portal: Option<(String, u16)>,
}

/// Адрес этого ПК в сторону портала (UDP-«подключение» ничего не отправляет).
fn local_ip(host: &str, port: u16) -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect((host, port)).ok()?;
    s.local_addr().ok().map(|a| a.ip()).filter(|ip| !ip.is_unspecified())
}

impl run::Host for WindowsHost {
    fn collect(&self) -> anyhow::Result<AgentReport> {
        let mut sessions = wts::sessions()?;
        sessions.retain(|s| s.is_user_session());
        let hostname = sys::hostname().or_else(|| std::env::var("COMPUTERNAME").ok()).unwrap_or_default();
        Ok(AgentReport {
            hostname,
            domain: sys::dns_domain().or_else(|| std::env::var("USERDOMAIN").ok()),
            os: format!("windows-{}", std::env::consts::ARCH),
            agent_version: env!("CARGO_PKG_VERSION").to_string(),
            sessions,
            fqdn: sys::fqdn(),
            os_version: sys::os_version(),
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
        const BIOS: &str = r"HARDWARE\DESCRIPTION\System\BIOS";
        const CPU: &str = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
        let smb = sys::smbios().map(|t| inventory::parse_smbios(&t)).unwrap_or_default();
        // Тот же SMBIOS, разобранный Windows, — если таблицу прочитать не вышло.
        let bios = |name: &str| sys::reg_string(HKEY_LOCAL_MACHINE, BIOS, name).filter(|s| !inventory::placeholder(s));
        Some(HardwareInventory {
            manufacturer: smb.manufacturer.or_else(|| bios("SystemManufacturer")),
            model: smb.product.or_else(|| bios("SystemProductName")),
            serial_number: smb.serial,
            bios_version: smb.bios_version.or_else(|| bios("BIOSVersion")),
            cpu: sys::reg_string(HKEY_LOCAL_MACHINE, CPU, "ProcessorNameString")
                .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" ")),
            memory_mb: sys::installed_memory_mb(),
            disks: sys::fixed_disks(),
            software: inventory::software(sys::uninstall_entries()),
        })
    }

    fn install_update(&self, data: &[u8]) -> anyhow::Result<()> {
        let exe = std::env::current_exe()?;
        replace_exe(&exe, data)?;
        tracing::info!("новая версия агента установлена — перезапуск службы");
        // Выход без «служба остановлена»: SCM по настройке восстановления (MSI) запустит новую версию.
        std::process::exit(1);
    }
}

fn sibling(exe: &Path, ext: &str) -> PathBuf {
    exe.with_extension(ext)
}

/// Работающий exe переименовать можно, перезаписать — нет: новая версия ставится рядом и меняется местами.
pub fn replace_exe(exe: &Path, data: &[u8]) -> std::io::Result<()> {
    let (new, old) = (sibling(exe, "exe.new"), sibling(exe, "exe.old"));
    std::fs::write(&new, data)?;
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old)?;
    if let Err(e) = std::fs::rename(&new, exe) {
        // Вернуть прежнюю версию: служба должна подняться в любом случае.
        let _ = std::fs::rename(&old, exe);
        return Err(e);
    }
    Ok(())
}

impl WindowsHost {
    /// Сбор сведений для портала из настроек (адрес портала — чтобы узнать свой адрес в его сторону).
    pub fn new(settings: &crate::config::Settings) -> Self {
        let portal = settings
            .portal
            .as_deref()
            .and_then(|p| client::base_url(p).ok())
            .and_then(|u| Some((u.host_str()?.to_string(), u.port_or_known_default()?)));
        Self { portal }
    }
}

/// Папка агента только для SYSTEM и администраторов. Без этого ключ агента прочитал бы любой
/// пользователь ПК, поэтому ошибка — отказ запуска. Файлы прежних версий получают те же права
/// (SetFileSecurity на папке не меняет уже созданные файлы).
pub fn secure_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    sys::protect(dir).map_err(|e| anyhow::anyhow!("не удалось ограничить права на папку агента: {e}"))?;
    for entry in std::fs::read_dir(dir)?.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_file())
            && let Err(e) = sys::protect(&entry.path())
        {
            tracing::warn!(file = %entry.path().display(), error = %e, "не удалось ограничить права на файл агента");
        }
    }
    Ok(())
}

/// Агент службы: настройки из реестра, состояние в защищённой папке.
pub fn windows_agent() -> anyhow::Result<run::Agent<WindowsHost>> {
    let settings = sys::settings();
    let dir = config::data_dir();
    secure_dir(&dir)?;
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(sibling(&exe, "exe.old"));
    }
    let host = WindowsHost::new(&settings);
    Ok(run::Agent::new(host, settings, config::state_path(&dir))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_swap() {
        let dir = std::env::temp_dir().join(format!("pult-agent-swap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("pult-agent.exe");
        std::fs::write(&exe, b"old").unwrap();
        replace_exe(&exe, b"new").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(dir.join("pult-agent.exe.old")).unwrap(), b"old");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
