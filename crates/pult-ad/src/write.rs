//! Запись в AD (ADR-0002): типизированные операции, без «произвольного LDAP modify».
//!
//! Пул для записи создаётся с учёткой записи (делегированные права на нужные OU и группы).
//! Каждая операция выполняется на одном соединении: прочитать состояние → изменить, если нужно →
//! прочитать снова. Результат — состояние «до» и «после»; проверку (повторное чтение) делает
//! вызывающий код по `after`. Пароль — только по LDAPS и не попадает в ошибки и журнал.

use crate::{config::TlsMode, errors::AdError, pool::DcPool};
use ldap3::{Ldap, Mod, Scope, SearchEntry};
use pult_core::Secret;
use std::collections::HashSet;

/// `ACCOUNTDISABLE` в `userAccountControl`.
pub const UAC_ACCOUNT_DISABLE: u32 = 0x2;
/// `DONT_EXPIRE_PASSWORD`.
pub const UAC_DONT_EXPIRE_PASSWORD: u32 = 0x1_0000;

/// Код LDAP `entryAlreadyExists`: AD так отвечает на добавление участника, который уже в группе.
const RC_ALREADY_EXISTS: u32 = 68;

const STATE_ATTRS: &[&str] =
    &["userAccountControl", "lockoutTime", "pwdLastSet", "lastLogonTimestamp", "memberOf", "adminCount"];

/// Отказ: учётка привилегированная. Текст — в каталоге переводов (`l10n/en.tsv`).
pub const PROTECTED: &str = "Учётка привилегированная (adminCount = 1, член административных групп) — Pult её не меняет. Это делает администратор домена.";

/// Состояние учётки — то, что меняют операции и что показывает карточка заявителя.
/// Время — как в AD: FILETIME (100 нс от 1601-01-01), 0 — «нет».
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AccountState {
    pub dn: String,
    pub user_account_control: u32,
    pub lockout_time: u64,
    pub pwd_last_set: u64,
    pub last_logon: u64,
    /// DN групп, где учётка — прямой участник.
    pub member_of: Vec<String>,
    /// `adminCount = 1`: учётка состоит (или состояла) в привилегированной группе и защищена AdminSDHolder.
    pub protected: bool,
}

impl AccountState {
    pub fn is_enabled(&self) -> bool {
        self.user_account_control & UAC_ACCOUNT_DISABLE == 0
    }

    pub fn is_locked(&self) -> bool {
        self.lockout_time != 0
    }

    pub fn in_group(&self, group_dn: &str) -> bool {
        self.member_of.iter().any(|g| g.eq_ignore_ascii_case(group_dn))
    }
}

/// Изменение одной учётки.
pub enum Change<'a> {
    AddToGroup {
        group_dn: &'a str,
    },
    Unlock,
    SetEnabled(bool),
    /// Новый пароль; `must_change` — сменить при следующем входе (`pwdLastSet = 0`).
    SetPassword {
        password: &'a Secret,
        must_change: bool,
    },
}

/// Итог: что было, что стало, меняли ли (повтор — «уже так», без записи).
#[derive(Debug, Clone)]
pub struct Applied {
    pub controller: String,
    pub before: AccountState,
    pub after: AccountState,
    pub changed: bool,
}

/// FILETIME → секунды Unix. 0 и «никогда» (`i64::MAX`) — `None`.
pub fn filetime_to_unix(ft: u64) -> Option<i64> {
    if ft == 0 || ft >= 0x7FFF_FFFF_FFFF_FFFF {
        return None;
    }
    i64::try_from(ft / 10_000_000).ok().map(|s| s - 11_644_473_600)
}

/// Пароль для `unicodePwd`: UTF-16LE в кавычках.
fn unicode_pwd(p: &str) -> Vec<u8> {
    format!("\"{p}\"").encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn to_ad(e: ldap3::LdapError) -> AdError {
    match e {
        ldap3::LdapError::LdapResult { result } => {
            AdError::Ldap { rc: result.rc, text: result.text.trim_end_matches('\0').trim().to_string() }
        }
        other => AdError::Ldap { rc: 0, text: other.to_string() },
    }
}

async fn read_state(ldap: &mut Ldap, dn: &str) -> Result<Option<AccountState>, AdError> {
    let res = ldap.search(dn, Scope::Base, "(objectClass=user)", STATE_ATTRS.to_vec()).await.map_err(to_ad)?;
    let entries = match res.success() {
        Ok((rs, _)) => rs,
        Err(ldap3::LdapError::LdapResult { result }) if result.rc == 32 => Vec::new(),
        Err(e) => return Err(to_ad(e)),
    };
    Ok(entries.into_iter().next().map(|e| {
        let e = SearchEntry::construct(e);
        let num = |a: &str| e.attrs.get(a).and_then(|v| v.first()).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        AccountState {
            dn: e.dn.clone(),
            user_account_control: u32::try_from(num("userAccountControl")).unwrap_or(0),
            lockout_time: num("lockoutTime"),
            pwd_last_set: num("pwdLastSet"),
            last_logon: num("lastLogonTimestamp"),
            member_of: e.attrs.get("memberOf").cloned().unwrap_or_default(),
            protected: num("adminCount") != 0,
        }
    }))
}

async fn modify(ldap: &mut Ldap, dn: &str, mods: Vec<Mod<Vec<u8>>>) -> Result<u32, AdError> {
    let r = ldap.modify(dn, mods).await.map_err(to_ad)?;
    match r.rc {
        0 | RC_ALREADY_EXISTS => Ok(r.rc),
        rc => Err(AdError::Ldap { rc, text: r.text.trim_end_matches('\0').trim().to_string() }),
    }
}

fn replace(attr: &str, value: Vec<u8>) -> Mod<Vec<u8>> {
    Mod::Replace(attr.as_bytes().to_vec(), HashSet::from([value]))
}

impl DcPool {
    pub fn tls_mode(&self) -> TlsMode {
        self.cfg.tls
    }

    /// Состояние учётки по DN (`None` — нет такой).
    pub async fn account_state(&self, dn: &str) -> Result<Option<AccountState>, AdError> {
        let dn = dn.to_string();
        let (_, st) = self
            .with_service_conn(|mut ldap| {
                let dn = dn.clone();
                async move {
                    let st = read_state(&mut ldap, &dn).await?;
                    let _ = ldap.unbind().await;
                    Ok(st)
                }
            })
            .await?;
        Ok(st)
    }

    /// Максимальный срок пароля в домене (`maxPwdAge`), секунд; `None` — не ограничен или не прочитан.
    pub async fn max_password_age(&self) -> Result<Option<i64>, AdError> {
        let base = self.cfg.base_dn.clone();
        let (_, age) = self
            .with_service_conn(|mut ldap| {
                let base = base.clone();
                async move {
                    let res =
                        ldap.search(&base, Scope::Base, "(objectClass=*)", vec!["maxPwdAge"]).await.map_err(to_ad)?;
                    let _ = ldap.unbind().await;
                    let Ok((rs, _)) = res.success() else { return Ok(None) };
                    Ok(rs.into_iter().next().and_then(|e| {
                        let e = SearchEntry::construct(e);
                        // Отрицательное число интервалов по 100 нс; «никогда» — минимальное i64.
                        let v: i64 = e.attrs.get("maxPwdAge")?.first()?.parse().ok()?;
                        (v < 0 && v != i64::MIN)
                            .then(|| v.unsigned_abs() / 10_000_000)
                            .and_then(|s| i64::try_from(s).ok())
                    }))
                }
            })
            .await?;
        Ok(age)
    }

    /// Есть ли группа с таким DN.
    pub async fn group_exists(&self, dn: &str) -> Result<bool, AdError> {
        let dn = dn.to_string();
        let (_, ok) = self
            .with_service_conn(|mut ldap| {
                let dn = dn.clone();
                async move {
                    let res = ldap.search(&dn, Scope::Base, "(objectClass=group)", vec!["cn"]).await.map_err(to_ad)?;
                    let _ = ldap.unbind().await;
                    Ok(matches!(res.success(), Ok((rs, _)) if !rs.is_empty()))
                }
            })
            .await?;
        Ok(ok)
    }

    /// Изменить учётку `dn`: прочитать, изменить (если ещё не так), прочитать снова — на одном DC.
    pub async fn apply(&self, dn: &str, change: Change<'_>) -> Result<Applied, AdError> {
        if matches!(change, Change::SetPassword { .. }) && self.cfg.tls != TlsMode::Ldaps {
            return Err(AdError::Ldap { rc: 0, text: "смена пароля — только по LDAPS".into() });
        }
        let change = &change;
        let (dc, (before, after, changed)) = self
            .with_service_conn(|mut ldap| {
                let dn = dn.to_string();
                async move {
                    let before = read_state(&mut ldap, &dn)
                        .await?
                        .ok_or_else(|| AdError::Ldap {
                            rc: 32, text: "учётка не найдена в AD".into()
                        })?;
                    // Делегирование на OU на такие учётки и так не действует (AdminSDHolder), но если права
                    // выданы шире, Pult всё равно не трогает администраторов домена: сброс пароля одному из них —
                    // захват домена.
                    if before.protected {
                        return Err(AdError::Ldap { rc: 50, text: PROTECTED.into() });
                    }
                    let plan: Option<(String, Vec<Mod<Vec<u8>>>)> = match change {
                        Change::AddToGroup { group_dn } if before.in_group(group_dn) => None,
                        Change::AddToGroup { group_dn } => Some((
                            (*group_dn).to_string(),
                            vec![Mod::Add(b"member".to_vec(), HashSet::from([dn.as_bytes().to_vec()]))],
                        )),
                        Change::Unlock if !before.is_locked() => None,
                        Change::Unlock => Some((dn.clone(), vec![replace("lockoutTime", b"0".to_vec())])),
                        Change::SetEnabled(on) if before.is_enabled() == *on => None,
                        Change::SetEnabled(on) => {
                            let uac = if *on {
                                before.user_account_control & !UAC_ACCOUNT_DISABLE
                            } else {
                                before.user_account_control | UAC_ACCOUNT_DISABLE
                            };
                            Some((dn.clone(), vec![replace("userAccountControl", uac.to_string().into_bytes())]))
                        }
                        Change::SetPassword { password, must_change } => {
                            let mut m = vec![replace("unicodePwd", unicode_pwd(password.expose()))];
                            if *must_change {
                                m.push(replace("pwdLastSet", b"0".to_vec()));
                            }
                            Some((dn.clone(), m))
                        }
                    };
                    let changed = match plan {
                        Some((target, m)) => modify(&mut ldap, &target, m).await? != RC_ALREADY_EXISTS,
                        None => false,
                    };
                    let after = read_state(&mut ldap, &dn)
                        .await?
                        .ok_or_else(|| AdError::Ldap {
                            rc: 32, text: "учётка не найдена в AD".into()
                        })?;
                    let _ = ldap.unbind().await;
                    Ok((before, after, changed))
                }
            })
            .await?;
        Ok(Applied { controller: dc.to_string(), before, after, changed })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime() {
        assert_eq!(filetime_to_unix(0), None);
        assert_eq!(filetime_to_unix(0x7FFF_FFFF_FFFF_FFFF), None);
        // 2026-01-01T00:00:00Z
        assert_eq!(filetime_to_unix(134_116_992_000_000_000), Some(1_767_225_600));
    }

    #[test]
    fn password_encoding() {
        assert_eq!(unicode_pwd("Ab1"), vec![b'"', 0, b'A', 0, b'b', 0, b'1', 0, b'"', 0]);
    }
}
