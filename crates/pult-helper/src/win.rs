//! Реестр текущего пользователя: схема ссылок `pult:` и адрес портала (без прав администратора).
#![allow(unsafe_code)]

use windows::{
    Win32::System::Registry::{
        HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteTreeW, RegGetValueW, RegSetKeyValueW,
    },
    core::{HSTRING, PCWSTR},
};

pub const SCHEME_KEY: &str = r"Software\Classes\pult";
pub const SETTINGS_KEY: &str = r"Software\Pult\Helper";

/// Записать строку (`name = None` — значение по умолчанию). Ключ создаётся при необходимости.
pub fn set(path: &str, name: Option<&str>, value: &str) -> windows::core::Result<()> {
    let wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    let path = HSTRING::from(path);
    let name = name.map(HSTRING::from);
    let name_ptr = name.as_ref().map_or(PCWSTR::null(), |n| PCWSTR(n.as_ptr()));
    // SAFETY: строки UTF-16 с нулём живут до конца вызова; размер — в байтах вместе с нулём.
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            &path,
            name_ptr,
            REG_SZ.0,
            Some(wide.as_ptr().cast()),
            (wide.len() * 2) as u32,
        )
    }
    .ok()
}

pub fn get(path: &str, name: &str) -> Option<String> {
    let (path, name) = (HSTRING::from(path), HSTRING::from(name));
    let mut bytes: u32 = 0;
    // SAFETY: первый вызов узнаёт размер; строки живут до конца функции.
    unsafe { RegGetValueW(HKEY_CURRENT_USER, &path, &name, RRF_RT_REG_SZ, None, None, Some(&mut bytes)) }.ok().ok()?;
    let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
    // SAFETY: буфер на `bytes` байт, функция пишет не больше и завершает строку нулём.
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            &path,
            &name,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
    }
    .ok()
    .ok()?;
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end])).filter(|s| !s.is_empty())
}

pub fn delete_tree(path: &str) {
    let path = HSTRING::from(path);
    // SAFETY: строка живёт до конца вызова; удаляется только ключ текущего пользователя.
    let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &path) };
}

/// Схема `pult:` → `"<helper.exe>" open "%1"`, адрес портала — в настройках helper.
pub fn register(exe: &str, portal: &str, prompt_credentials: bool) -> windows::core::Result<()> {
    set(SCHEME_KEY, None, "URL:Pult")?;
    set(SCHEME_KEY, Some("URL Protocol"), "")?;
    set(&format!(r"{SCHEME_KEY}\DefaultIcon"), None, &format!("\"{exe}\",0"))?;
    set(&format!(r"{SCHEME_KEY}\shell\open\command"), None, &format!("\"{exe}\" open \"%1\""))?;
    set(SETTINGS_KEY, Some("Portal"), portal)?;
    set(SETTINGS_KEY, Some("PromptCredentials"), if prompt_credentials { "1" } else { "0" })
}

pub fn unregister() {
    delete_tree(SCHEME_KEY);
    delete_tree(SETTINGS_KEY);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_round_trip() {
        let key = r"Software\PultHelperTest";
        set(key, Some("Portal"), "https://pult.example.local/").unwrap();
        set(&format!(r"{key}\shell"), None, "x").unwrap();
        assert_eq!(get(key, "Portal").as_deref(), Some("https://pult.example.local/"));
        delete_tree(key);
        assert_eq!(get(key, "Portal"), None);
    }
}
