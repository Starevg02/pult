//! Windows: реестр, имена ПК, версия ОС, права на папку агента.
#![allow(unsafe_code)]

use crate::config::Settings;
use pult_core::Secret;
use std::{os::windows::ffi::OsStrExt, path::Path};
use windows::{
    Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::{
            Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
            DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SetFileSecurityW,
        },
        System::{
            Registry::{HKEY, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW},
            SystemInformation::{
                COMPUTER_NAME_FORMAT, ComputerNameDnsDomain, ComputerNameDnsFullyQualified, ComputerNameNetBIOS,
                GetComputerNameExW,
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
}
