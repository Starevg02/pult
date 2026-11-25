//! Windows: реестр, имена ПК, версия ОС, оборудование, права на папку агента.
#![allow(unsafe_code)]

use crate::{config::Settings, inventory::UninstallEntry};
use pult_api_types::DeviceDisk;
use pult_core::Secret;
use std::{os::windows::ffi::OsStrExt, path::Path};
use windows::{
    Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::{
            Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
            DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SetFileSecurityW,
        },
        Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives},
        System::{
            Registry::{
                HKEY, HKEY_LOCAL_MACHINE, HKEY_USERS, KEY_READ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegCloseKey,
                RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
            },
            SystemInformation::{
                COMPUTER_NAME_FORMAT, ComputerNameDnsDomain, ComputerNameDnsFullyQualified, ComputerNameNetBIOS,
                GetComputerNameExW, GetPhysicallyInstalledSystemMemory, GetSystemFirmwareTable, RSMB,
            },
        },
    },
    core::{HSTRING, PCWSTR, PWSTR},
};

/// Строковое значение реестра; нет ключа или значения — `None`.
pub fn reg_string(root: HKEY, path: &str, name: &str) -> Option<String> {
    let (path, name) = (HSTRING::from(path), HSTRING::from(name));
    let mut bytes: u32 = 0;
    // SAFETY: первый вызов узнаёт размер (буфер не передаём), строки живут до конца функции.
    let rc = unsafe { RegGetValueW(root, &path, &name, RRF_RT_REG_SZ, None, None, Some(&mut bytes)) };
    if rc.is_err() || bytes < 2 {
        return None;
    }
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
    // SAFETY: буфер размером `bytes` байт; RegGetValueW пишет не больше и завершает строку нулём.
    let rc = unsafe {
        RegGetValueW(root, &path, &name, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr().cast()), Some(&mut bytes))
    };
    if rc.is_err() {
        return None;
    }
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end])).filter(|s| !s.trim().is_empty())
}

/// Значение DWORD реестра.
pub fn reg_dword(root: HKEY, path: &str, name: &str) -> Option<u32> {
    let (path, name) = (HSTRING::from(path), HSTRING::from(name));
    let mut v: u32 = 0;
    let mut bytes = std::mem::size_of::<u32>() as u32;
    // SAFETY: буфер — одно u32 размером `bytes`; RRF_RT_REG_DWORD не даст записать больше.
    let rc = unsafe {
        RegGetValueW(root, &path, &name, RRF_RT_REG_DWORD, None, Some((&raw mut v).cast()), Some(&mut bytes))
    };
    rc.is_ok().then_some(v)
}

/// Имена подразделов ключа реестра; нет ключа — пусто.
pub fn reg_subkeys(root: HKEY, path: &str) -> Vec<String> {
    let mut key = HKEY::default();
    // SAFETY: строка пути живёт до конца вызова; открытый ключ закрываем ниже.
    let rc = unsafe { RegOpenKeyExW(root, &HSTRING::from(path), None, KEY_READ, &mut key) };
    if rc.is_err() {
        return Vec::new();
    }
    let mut out = Vec::new();
    // Имя раздела реестра — не длиннее 255 символов.
    let mut buf = [0u16; 256];
    for i in 0..20_000u32 {
        let mut len = buf.len() as u32;
        // SAFETY: буфер на `len` символов; функция пишет не больше и сообщает длину без нуля.
        let rc = unsafe { RegEnumKeyExW(key, i, Some(PWSTR(buf.as_mut_ptr())), &mut len, None, None, None, None) };
        if rc.is_err() {
            break;
        }
        out.push(String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]));
    }
    // SAFETY: ключ открыт выше и закрывается один раз.
    let _ = unsafe { RegCloseKey(key) };
    out
}

/// Программы из разделов Uninstall: 64- и 32-битные «для всех» и «для себя» у вошедших пользователей
/// (их ветви HKEY_USERS загружены, пока они в системе).
pub fn uninstall_entries() -> Vec<UninstallEntry> {
    const U: &str = r"Microsoft\Windows\CurrentVersion\Uninstall";
    let mut roots = vec![
        (HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\{U}")),
        (HKEY_LOCAL_MACHINE, format!(r"SOFTWARE\WOW6432Node\{U}")),
    ];
    for sid in reg_subkeys(HKEY_USERS, "") {
        if sid.starts_with("S-1-5-21-") && !sid.ends_with("_Classes") {
            roots.push((HKEY_USERS, format!(r"{sid}\Software\{U}")));
        }
    }
    let mut out = Vec::new();
    for (root, base) in roots {
        for k in reg_subkeys(root, &base) {
            let p = format!(r"{base}\{k}");
            out.push(UninstallEntry {
                name: reg_string(root, &p, "DisplayName"),
                version: reg_string(root, &p, "DisplayVersion"),
                publisher: reg_string(root, &p, "Publisher"),
                system_component: reg_dword(root, &p, "SystemComponent") == Some(1),
                has_parent: reg_string(root, &p, "ParentKeyName").is_some(),
                release_type: reg_string(root, &p, "ReleaseType"),
            });
        }
    }
    out
}

/// Таблицы SMBIOS целиком (модель, серийный номер, версия BIOS).
pub fn smbios() -> Option<Vec<u8>> {
    // SAFETY: без буфера функция только сообщает нужный размер.
    let n = unsafe { GetSystemFirmwareTable(RSMB, 0, None) };
    if n == 0 || n > 1 << 20 {
        return None;
    }
    let mut buf = vec![0u8; n as usize];
    // SAFETY: буфер на `n` байт; функция пишет не больше и возвращает записанное.
    let got = unsafe { GetSystemFirmwareTable(RSMB, 0, Some(buf.as_mut_slice())) };
    if got == 0 || got > n {
        return None;
    }
    buf.truncate(got as usize);
    Some(buf)
}

/// Установленная память (по SMBIOS, а не доступная ОС), МБ.
pub fn installed_memory_mb() -> Option<u64> {
    let mut kb: u64 = 0;
    // SAFETY: функция пишет одно u64.
    unsafe { GetPhysicallyInstalledSystemMemory(&mut kb) }.ok()?;
    Some(kb / 1024).filter(|m| *m > 0)
}

/// Локальные несъёмные диски с размером и свободным местом, ГБ (ГиБ, как в Проводнике).
pub fn fixed_disks() -> Vec<DeviceDisk> {
    const DRIVE_FIXED: u32 = 3;
    // SAFETY: без аргументов, возвращает битовую маску дисков.
    let mask = unsafe { GetLogicalDrives() };
    (0u8..26)
        .filter(|i| mask & (1 << i) != 0)
        .filter_map(|i| {
            let letter = char::from(b'A' + i);
            let root = HSTRING::from(format!("{letter}:\\"));
            // SAFETY: строка пути живёт до конца вызова.
            if unsafe { GetDriveTypeW(&root) } != DRIVE_FIXED {
                return None;
            }
            let (mut total, mut free) = (0u64, 0u64);
            // SAFETY: строка пути живёт до конца вызова; функция пишет по одному u64 в каждый указатель.
            unsafe { GetDiskFreeSpaceExW(&root, None, Some(&raw mut total), Some(&raw mut free)) }.ok()?;
            Some(DeviceDisk { name: format!("{letter}:"), size_gb: total >> 30, free_gb: free >> 30 })
        })
        .collect()
}

fn settings_at(path: &str) -> Settings {
    Settings {
        portal: reg_string(HKEY_LOCAL_MACHINE, path, "PortalUrl"),
        enroll_token: reg_string(HKEY_LOCAL_MACHINE, path, "EnrollToken").map(Secret::new),
        update_key: reg_string(HKEY_LOCAL_MACHINE, path, "UpdatePublicKey"),
    }
}

/// Настройки: GPO (`Policies`) главнее свойств MSI.
pub fn settings() -> Settings {
    Settings::merge(settings_at(r"SOFTWARE\Policies\Pult\Agent"), settings_at(r"SOFTWARE\Pult\Agent"))
}

fn computer_name(kind: COMPUTER_NAME_FORMAT) -> Option<String> {
    let mut len: u32 = 0;
    // SAFETY: первый вызов без буфера возвращает нужную длину (ошибка ERROR_MORE_DATA ожидаема).
    let _ = unsafe { GetComputerNameExW(kind, None, &mut len) };
    if len == 0 {
        return None;
    }
    let mut buf = vec![0u16; len as usize + 1];
    len = buf.len() as u32;
    // SAFETY: буфер на `len` символов; функция пишет не больше и сообщает длину без нуля.
    unsafe { GetComputerNameExW(kind, Some(PWSTR(buf.as_mut_ptr())), &mut len) }.ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize])).filter(|s| !s.is_empty())
}

pub fn hostname() -> Option<String> {
    computer_name(ComputerNameNetBIOS)
}

pub fn fqdn() -> Option<String> {
    computer_name(ComputerNameDnsFullyQualified)
}

pub fn dns_domain() -> Option<String> {
    computer_name(ComputerNameDnsDomain)
}

/// «Windows 11 Pro 23H2 (22631)». В реестре Windows 11 до сих пор называется «Windows 10».
pub fn os_version() -> Option<String> {
    const CV: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let product = reg_string(HKEY_LOCAL_MACHINE, CV, "ProductName")?;
    let build = reg_string(HKEY_LOCAL_MACHINE, CV, "CurrentBuild").unwrap_or_default();
    let display = reg_string(HKEY_LOCAL_MACHINE, CV, "DisplayVersion").unwrap_or_default();
    Some(describe_os(&product, &display, &build))
}

pub fn describe_os(product: &str, display: &str, build: &str) -> String {
    let win11 = build.parse::<u32>().is_ok_and(|b| b >= 22000);
    let product = if win11 { product.replacen("Windows 10", "Windows 11", 1) } else { product.to_string() };
    let mut s = product;
    if !display.is_empty() {
        s.push(' ');
        s.push_str(display);
    }
    if !build.is_empty() {
        s.push_str(&format!(" ({build})"));
    }
    s
}

/// Папка агента: полный доступ только SYSTEM и администраторам, наследование от `ProgramData` снято
/// (иначе ключ агента прочитал бы любой пользователь ПК).
pub fn protect(path: &Path) -> windows::core::Result<()> {
    let sddl = HSTRING::from("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)");
    let mut sd = PSECURITY_DESCRIPTOR::default();
    // SAFETY: строка SDDL живёт до конца вызова; дескриптор выделяет система, освобождаем ниже.
    unsafe { ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut sd, None)? };
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: путь — строка UTF-16 с нулём, дескриптор действителен до LocalFree.
    let ok = unsafe {
        SetFileSecurityW(PCWSTR(wide.as_ptr()), DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, sd)
    };
    // SAFETY: дескриптор выделен ConvertStringSecurityDescriptorToSecurityDescriptorW, освобождаем один раз.
    unsafe { LocalFree(Some(HLOCAL(sd.0))) };
    ok.ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_names() {
        assert_eq!(describe_os("Windows 10 Pro", "23H2", "22631"), "Windows 11 Pro 23H2 (22631)");
        assert_eq!(describe_os("Windows 10 Pro", "22H2", "19045"), "Windows 10 Pro 22H2 (19045)");
        assert_eq!(describe_os("Windows Server 2022", "", ""), "Windows Server 2022");
    }

    #[test]
    fn this_pc() {
        assert!(hostname().is_some_and(|h| !h.is_empty()));
        assert!(os_version().is_some_and(|v| v.starts_with("Windows")));
        assert!(reg_string(HKEY_LOCAL_MACHINE, r"SOFTWARE\Нет\Такого", "x").is_none());
    }

    #[test]
    fn this_pc_hardware() {
        assert!(fixed_disks().iter().any(|d| d.name == "C:" && d.size_gb > 0), "{:?}", fixed_disks());
        assert!(installed_memory_mb().is_some_and(|m| m >= 512));
        assert!(!reg_subkeys(HKEY_LOCAL_MACHINE, r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall").is_empty());
        assert!(reg_subkeys(HKEY_LOCAL_MACHINE, r"SOFTWARE\Нет\Такого").is_empty());
        assert_eq!(reg_dword(HKEY_LOCAL_MACHINE, r"SOFTWARE\Нет\Такого", "x"), None);
        assert!(!crate::inventory::software(uninstall_entries()).is_empty());
        assert!(smbios().is_some_and(|t| t.len() > 8));
    }
}
