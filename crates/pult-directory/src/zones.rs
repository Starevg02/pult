//! Область записи: в каких OU можно менять учётки и в какие группы добавлять (ADR-0002).
//! Пустой список — ничего нельзя: установка по умолчанию только читает.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct DirectorySettings {
    /// OU (DN), учётки внутри которых (на любой глубине) можно разблокировать, включать, сбрасывать пароль.
    pub user_ous: Vec<String>,
    /// Группы (DN), в которые можно добавлять участников.
    pub groups: Vec<String>,
    /// Без заявки операцию не запросить (только основание-комментарий — нельзя).
    pub require_ticket: bool,
}

impl pult_core::settings::SettingsGroup for DirectorySettings {
    const KEY: &'static str = "directory";
}

/// Части DN (RDN) по неэкранированным запятым: `CN=Smith\, John` — одна часть, а не две.
/// Без этого объект `CN=a,OU=Admins\,OU=Users,DC=x` (OU «Admins,OU=Users» в корне домена)
/// сошёл бы за учётку внутри `OU=Users,DC=x`.
fn rdns(dn: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = dn.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ',' => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out.into_iter().map(|p| p.trim().to_lowercase()).collect()
}

/// `CN=Иванов,OU=Users,DC=x` внутри `OU=Users,DC=x`. Сравнение без учёта регистра и пробелов у запятых.
pub fn within(dn: &str, ou: &str) -> bool {
    let (d, o) = (rdns(dn), rdns(ou));
    !o.is_empty() && !o[0].is_empty() && d.len() > o.len() && d[d.len() - o.len()..] == o[..]
}

pub fn same_dn(a: &str, b: &str) -> bool {
    rdns(a) == rdns(b)
}

/// Похоже на DN: каждая часть — `тип=значение`, без управляющих символов, не длиннее 500 символов.
pub fn is_dn(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= 500
        && !s.chars().any(char::is_control)
        && rdns(s).iter().all(|p| p.split_once('=').is_some_and(|(t, _)| !t.trim().is_empty()))
}

impl DirectorySettings {
    pub fn user_allowed(&self, dn: &str) -> bool {
        self.user_ous.iter().any(|ou| within(dn, ou))
    }

    pub fn group_allowed(&self, dn: &str) -> bool {
        self.groups.iter().any(|g| same_dn(g, dn))
    }

    /// Убрать пустые строки и повторы; DN проверяется на вид `X=…`.
    pub fn validate(mut self) -> Result<Self, String> {
        for list in [&mut self.user_ous, &mut self.groups] {
            list.retain(|s| !s.trim().is_empty());
            for s in list.iter_mut() {
                *s = s.trim().to_string();
                if !is_dn(s) {
                    return Err(format!("Это не DN: {s}"));
                }
            }
            list.dedup_by(|a, b| same_dn(a, b));
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ou_scope() {
        assert!(within("CN=Иванов,OU=Users,DC=x,DC=y", "OU=Users,DC=x,DC=y"));
        assert!(within("CN=a,OU=Sub,OU=Users,DC=x", "ou=users, dc=x"), "вложенные OU и регистр");
        assert!(!within("OU=Users,DC=x", "OU=Users,DC=x"), "сама OU — не учётка внутри");
        assert!(!within("CN=a,OU=Admins,DC=x", "OU=Users,DC=x"));
        assert!(!within("CN=a,OU=NotUsers,DC=x", "OU=Users,DC=x"), "только целые части DN");
        assert!(!within("CN=a,DC=x", ""));
        // Экранированная запятая — часть имени, а не граница DN.
        assert!(!within(r"CN=a,OU=Admins\,OU=Users,DC=x", "OU=Users,DC=x"), "OU «Admins,OU=Users» в корне");
        assert!(!within(r"CN=a\,OU=Users,DC=x", "OU=Users,DC=x"), "объект «a,OU=Users» в корне");
        assert!(within(r"CN=Smith\, John,OU=Users,DC=x", "OU=Users,DC=x"));
        assert!(same_dn(r"CN=Smith\, John,OU=G,DC=x", r"cn=smith\, john, ou=g, dc=x"));
        assert!(!same_dn(r"CN=G\,OU=Groups,DC=x", "CN=G,OU=Groups,DC=x"));
    }

    #[test]
    fn validation() {
        let s = DirectorySettings { user_ous: vec![" OU=A,DC=x ".into(), String::new()], ..Default::default() };
        assert_eq!(s.validate().unwrap().user_ous, vec!["OU=A,DC=x"]);
        let bad = DirectorySettings { groups: vec!["Бухгалтерия".into()], ..Default::default() };
        assert!(bad.validate().is_err());
        for bad in ["CN=a\n,DC=x", "=a,DC=x", ""] {
            assert!(!is_dn(bad), "{bad:?}");
        }
        assert!(is_dn(r"CN=Smith\, John,OU=G,DC=x"));
    }
}
