//! Сессии через Windows Terminal Services API.
#![allow(unsafe_code)]

use crate::{Session, SessionState};
use windows::{
    Win32::System::RemoteDesktop::{
        WTS_CURRENT_SERVER_HANDLE, WTS_INFO_CLASS, WTS_SESSION_INFOW, WTSClientName, WTSDomainName,
        WTSEnumerateSessionsW, WTSFreeMemory, WTSINFOW, WTSQuerySessionInformationW, WTSSessionInfo, WTSUserName,
    },
    core::PWSTR,
};

/// Все сессии этого ПК.
pub fn sessions() -> windows::core::Result<Vec<Session>> {
    let mut info: *mut WTS_SESSION_INFOW = std::ptr::null_mut();
    let mut count: u32 = 0;
    // SAFETY: передаём указатели на локальные переменные; при успехе WTS выделяет массив
    // из `count` элементов, который освобождается через WTSFreeMemory ниже.
    unsafe { WTSEnumerateSessionsW(Some(WTS_CURRENT_SERVER_HANDLE), 0, 1, &mut info, &mut count)? };
    let mut out = Vec::with_capacity(count as usize);
    // SAFETY: `info` указывает на `count` инициализированных структур (контракт WTSEnumerateSessionsW).
    let items = unsafe { std::slice::from_raw_parts(info, count as usize) };
    for it in items {
        let id = it.SessionId;
        out.push(Session {
            id,
            // SAFETY: pWinStationName — строка UTF-16 с нулём, живёт до WTSFreeMemory(info).
            station: unsafe { it.pWinStationName.to_string() }.unwrap_or_default(),
            state: SessionState::from_wts(it.State.0),
            user: query(id, WTSUserName),
            domain: query(id, WTSDomainName),
            client_name: query(id, WTSClientName),
            logon_at: logon_time(id),
        });
    }
    // SAFETY: освобождаем память, выделенную WTSEnumerateSessionsW, ровно один раз.
    unsafe { WTSFreeMemory(info.cast()) };
    Ok(out)
}

/// Строковое свойство сессии; пустая строка → `None`.
fn query(session: u32, class: WTS_INFO_CLASS) -> Option<String> {
    let mut buf = PWSTR::null();
    let mut bytes: u32 = 0;
    // SAFETY: при успехе WTS выделяет строку и пишет указатель в `buf`; освобождаем ниже.
    unsafe { WTSQuerySessionInformationW(Some(WTS_CURRENT_SERVER_HANDLE), session, class, &mut buf, &mut bytes) }
        .ok()?;
    // SAFETY: `buf` — строка UTF-16 с нулём, выделенная WTS.
    let value = unsafe { buf.to_string() }.ok();
    // SAFETY: освобождаем строку, выделенную WTSQuerySessionInformationW.
    unsafe { WTSFreeMemory(buf.0.cast()) };
    value.filter(|v| !v.is_empty())
}

/// FILETIME (сотни наносекунд с 1601 года) → секунды Unix; ноль — времени нет.
pub fn filetime_to_unix(ft: i64) -> Option<i64> {
    const EPOCH_DIFF: i64 = 116_444_736_000_000_000;
    (ft > EPOCH_DIFF).then(|| (ft - EPOCH_DIFF) / 10_000_000)
}

/// Время входа в сессию.
fn logon_time(session: u32) -> Option<i64> {
    let mut buf = PWSTR::null();
    let mut bytes: u32 = 0;
    // SAFETY: при успехе WTS выделяет структуру WTSINFOW и пишет указатель в `buf`; освобождаем ниже.
    unsafe {
        WTSQuerySessionInformationW(Some(WTS_CURRENT_SERVER_HANDLE), session, WTSSessionInfo, &mut buf, &mut bytes)
    }
    .ok()?;
    let value = if (bytes as usize) >= std::mem::size_of::<WTSINFOW>() {
        // SAFETY: буфер не меньше WTSINFOW (проверено), выделен WTS и выровнен для этой структуры.
        let info = unsafe { &*(buf.0 as *const WTSINFOW) };
        filetime_to_unix(info.LogonTime)
    } else {
        None
    };
    // SAFETY: освобождаем память, выделенную WTSQuerySessionInformationW, ровно один раз.
    unsafe { WTSFreeMemory(buf.0.cast()) };
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime() {
        assert_eq!(filetime_to_unix(0), None);
        assert_eq!(filetime_to_unix(116_444_736_000_000_000 + 10_000_000), Some(1));
    }

    #[test]
    fn this_pc_sessions() {
        let s = sessions().unwrap();
        assert!(s.iter().any(|x| x.id == 0), "сессия служб есть всегда");
    }
}
