//! Синхронизация учёток AD .
//!
//! - Полная: постраничный поиск (paged results) по `user_filter`.
//! - Инкрементальная: `uSNChanged > сохранённого` на **том же** DC. USN у каждого DC свой,
//!   поэтому при смене DC вызывающий код делает полную синхронизацию — для этого в
//!   [`SyncBatch`] есть `controller`.

use crate::{errors::AdError, pool::DcPool};
use ldap3::{
    Ldap, Scope, SearchEntry,
    adapters::{Adapter, EntriesOnly, PagedResults},
};

/// Флаг `ACCOUNTDISABLE` в `userAccountControl`.
const UAC_ACCOUNT_DISABLE: u32 = 0x2;

/// Атрибуты, которые читаем (`ad_accounts`).
const ATTRS: &[&str] = &[
    "objectGUID",
    "sAMAccountName",
    "userPrincipalName",
    "displayName",
    "givenName",
    "sn",
    "mail",
    "title",
    "department",
    "telephoneNumber",
    "mobile",
    "userAccountControl",
    "uSNChanged",
    "whenChanged",
];

const COMPUTER_FILTER: &str = "(objectCategory=computer)";
const COMPUTER_ATTRS: &[&str] =
    &["objectGUID", "name", "dNSHostName", "description", "operatingSystem", "userAccountControl"];

/// Компьютер домена.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdComputer {
    pub object_guid: String,
    pub name: String,
    pub dns_host_name: Option<String>,
    pub description: Option<String>,
    pub operating_system: Option<String>,
    pub is_enabled: bool,
}

/// Учётка AD — строка будущей таблицы `ad_accounts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdAccount {
    pub object_guid: String,
    pub dn: String,
    pub sam_account_name: String,
    pub upn: Option<String>,
    pub display_name: Option<String>,
    pub given_name: Option<String>,
    pub sn: Option<String>,
    pub mail: Option<String>,
    pub title: Option<String>,
    pub department: Option<String>,
    pub telephone_number: Option<String>,
    pub mobile: Option<String>,
    pub user_account_control: u32,
    pub is_enabled: bool,
    pub usn_changed: u64,
    pub when_changed: Option<String>,
}

/// Результат одного прохода синхронизации.
#[derive(Debug, Clone)]
pub struct SyncBatch {
    /// DC, с которого читали. USN действителен только для него.
    pub controller: String,
    /// `highestCommittedUSN` этого DC на начало прохода — сохранить для следующей инкрементальной.
    pub highest_usn: u64,
    pub accounts: Vec<AdAccount>,
}

impl DcPool {
    /// Полная синхронизация, страницами по `page_size` записей (план: 500).
    pub async fn full_sync(&self, page_size: i32) -> Result<SyncBatch, AdError> {
        let filter = self.cfg.user_filter.clone();
        self.sync_with_filter(filter, page_size).await
    }

    /// Изменения после `since_usn` на текущем DC.
    pub async fn incremental_sync(&self, since_usn: u64, page_size: i32) -> Result<SyncBatch, AdError> {
        let filter = format!("(&{}(uSNChanged>={}))", self.cfg.user_filter, since_usn + 1);
        self.sync_with_filter(filter, page_size).await
    }

    /// DN участников группы по её имени (`cn` или `sAMAccountName`), без вложенных групп.
    /// Нужен для разового переноса ролей (например, `PhonebookAdmins` → «Редактор справочника»).
    /// `None` — группы нет.
    pub async fn group_member_dns(&self, group: &str) -> Result<Option<Vec<String>>, AdError> {
        let g = ldap3::ldap_escape(group.trim());
        let filter = format!("(&(objectCategory=group)(|(cn={g})(sAMAccountName={g})))");
        let base = self.cfg.base_dn.clone();
        let (_, members) = self
            .with_service_conn(|mut ldap| {
                let filter = filter.clone();
                let base = base.clone();
                async move {
                    let (rs, _) = ldap
                        .search(&base, Scope::Subtree, &filter, vec!["member"])
                        .await
                        .and_then(|r| r.success())
                        .map_err(to_ad)?;
                    let _ = ldap.unbind().await;
                    Ok(rs.into_iter().next().map(|e| {
                        let mut s = SearchEntry::construct(e);
                        s.attrs.remove("member").unwrap_or_default()
                    }))
                }
            })
            .await?;
        Ok(members)
    }

    /// Одна учётка по DN (точечная синхронизация при первом входе).
    /// `None` — учётки нет или она не проходит `user_filter`.
    pub async fn account_by_dn(&self, dn: &str) -> Result<Option<AdAccount>, AdError> {
        let dn = dn.to_string();
        let filter = self.cfg.user_filter.clone();
        let (_, acc) = self
            .with_service_conn(|mut ldap| {
                let dn = dn.clone();
                let filter = filter.clone();
                async move {
                    let res = ldap.search(&dn, Scope::Base, &filter, ATTRS.to_vec()).await.map_err(to_ad)?;
                    // 32 (noSuchObject) — учётку успели удалить: это не ошибка, а «нет».
                    let entries = match res.success() {
                        Ok((rs, _)) => rs,
                        Err(ldap3::LdapError::LdapResult { result }) if result.rc == 32 => Vec::new(),
                        Err(e) => return Err(to_ad(e)),
                    };
                    let _ = ldap.unbind().await;
                    Ok(entries.into_iter().next().map(SearchEntry::construct).and_then(account_from))
                }
            })
            .await?;
        Ok(acc)
    }

    async fn sync_with_filter(&self, filter: String, page_size: i32) -> Result<SyncBatch, AdError> {
        let base = self.cfg.base_dn.clone();
        let (dc, (highest_usn, accounts)) = self
            .with_service_conn(|mut ldap| {
                let filter = filter.clone();
                let base = base.clone();
                async move {
                    // USN фиксируем до чтения: изменения во время прохода попадут в следующую инкрементальную.
                    let (root, _) = ldap
                        .search("", Scope::Base, "(objectClass=*)", vec!["highestCommittedUSN"])
                        .await
                        .and_then(|r| r.success())
                        .map_err(to_ad)?;
                    let highest_usn = root
                        .into_iter()
                        .next()
                        .map(SearchEntry::construct)
                        .and_then(|e| e.attrs.get("highestCommittedUSN").and_then(|v| v.first()?.parse().ok()))
                        .unwrap_or(0);
                    let entries = read_all(&mut ldap, &base, &filter, ATTRS, page_size).await?;
                    let _ = ldap.unbind().await;
                    Ok((highest_usn, entries.into_iter().filter_map(account_from).collect::<Vec<_>>()))
                }
            })
            .await?;
        tracing::info!(dc = %dc, count = accounts.len(), highest_usn, "синхронизация AD: проход завершён");
        Ok(SyncBatch { controller: dc.to_string(), highest_usn, accounts })
    }

    /// Все компьютеры домена, страницами по `page_size` записей.
    pub async fn computers(&self, page_size: i32) -> Result<Vec<AdComputer>, AdError> {
        let base = self.cfg.base_dn.clone();
        let (_, computers) = self
            .with_service_conn(|mut ldap| {
                let base = base.clone();
                async move {
                    let entries = read_all(&mut ldap, &base, COMPUTER_FILTER, COMPUTER_ATTRS, page_size).await?;
                    let _ = ldap.unbind().await;
                    Ok(entries.into_iter().filter_map(computer_from).collect::<Vec<_>>())
                }
            })
            .await?;
        Ok(computers)
    }
}

/// Постраничный поиск по всему поддереву.
async fn read_all(
    ldap: &mut Ldap,
    base: &str,
    filter: &str,
    attrs: &'static [&'static str],
    page_size: i32,
) -> Result<Vec<SearchEntry>, AdError> {
    let adapters: Vec<Box<dyn Adapter<_, _>>> =
        vec![Box::new(EntriesOnly::new()), Box::new(PagedResults::new(page_size))];
    let mut stream =
        ldap.streaming_search_with(adapters, base, Scope::Subtree, filter, attrs.to_vec()).await.map_err(to_ad)?;
    let mut entries = Vec::new();
    while let Some(entry) = stream.next().await.map_err(to_ad)? {
        entries.push(SearchEntry::construct(entry));
    }
    stream.finish().await.success().map_err(to_ad)?;
    Ok(entries)
}

fn to_ad(e: ldap3::LdapError) -> AdError {
    match e {
        ldap3::LdapError::LdapResult { result } => {
            AdError::Ldap { rc: result.rc, text: result.text.trim_end_matches('\0').to_string() }
        }
        other => AdError::Ldap { rc: 0, text: other.to_string() },
    }
}

/// `objectGUID` записи в виде строки.
///
/// ldap3 кладёт значение в `bin_attrs`, только если оно не UTF-8. GUID, случайно оказавшийся
/// корректным UTF-8 (все байты < 0x80 и т. п.), лежит в `attrs` — без этой проверки такая
/// учётка молча выпадала бы из синхронизации.
pub(crate) fn entry_guid(e: &SearchEntry) -> Option<String> {
    let raw = e
        .bin_attrs
        .get("objectGUID")
        .and_then(|v| v.first().cloned())
        .or_else(|| e.attrs.get("objectGUID").and_then(|v| v.first()).map(|s| s.as_bytes().to_vec()));
    raw.as_deref().and_then(guid_to_string)
}

fn account_from(e: SearchEntry) -> Option<AdAccount> {
    let one = |a: &str| e.attrs.get(a).and_then(|v| v.first().cloned());
    let guid = entry_guid(&e)?;
    // Нет атрибута (нет прав на чтение) — считаем учётку отключённой, а не включённой: отключённый
    // в AD сотрудник не должен сохранить вход. Массовую потерю атрибута ловит порог синхронизации.
    let uac: u32 = one("userAccountControl").and_then(|s| s.parse().ok()).unwrap_or(UAC_ACCOUNT_DISABLE);
    Some(AdAccount {
        object_guid: guid,
        dn: e.dn.clone(),
        sam_account_name: one("sAMAccountName")?,
        upn: one("userPrincipalName"),
        display_name: one("displayName"),
        given_name: one("givenName"),
        sn: one("sn"),
        mail: one("mail"),
        title: one("title"),
        department: one("department"),
        telephone_number: one("telephoneNumber"),
        mobile: one("mobile"),
        user_account_control: uac,
        is_enabled: uac & UAC_ACCOUNT_DISABLE == 0,
        usn_changed: one("uSNChanged").and_then(|s| s.parse().ok()).unwrap_or(0),
        when_changed: one("whenChanged"),
    })
}

fn computer_from(e: SearchEntry) -> Option<AdComputer> {
    let one = |a: &str| e.attrs.get(a).and_then(|v| v.first().cloned()).filter(|s| !s.trim().is_empty());
    let uac: u32 = one("userAccountControl").and_then(|s| s.parse().ok()).unwrap_or(0);
    Some(AdComputer {
        object_guid: entry_guid(&e)?,
        name: one("name")?,
        dns_host_name: one("dNSHostName"),
        description: one("description"),
        operating_system: one("operatingSystem"),
        is_enabled: uac & UAC_ACCOUNT_DISABLE == 0,
    })
}

/// `objectGUID` (16 байт, первые три поля little-endian) → `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
pub fn guid_to_string(b: &[u8]) -> Option<String> {
    let b: &[u8; 16] = b.try_into().ok()?;
    Some(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn guid_byte_order() {
        // GUID 00112233-4455-6677-8899-aabbccddeeff в формате AD.
        let raw = [0x33, 0x22, 0x11, 0x00, 0x55, 0x44, 0x77, 0x66, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
        assert_eq!(guid_to_string(&raw).as_deref(), Some("00112233-4455-6677-8899-aabbccddeeff"));
        assert_eq!(guid_to_string(&raw[..15]), None);
    }

    #[test]
    fn disabled_flag() {
        let mut attrs = HashMap::new();
        attrs.insert("sAMAccountName".to_string(), vec!["ivanov".to_string()]);
        attrs.insert("userAccountControl".to_string(), vec!["514".to_string()]); // 512 + ACCOUNTDISABLE
        let mut bin = HashMap::new();
        bin.insert("objectGUID".to_string(), vec![vec![0u8; 16]]);
        let acc = account_from(SearchEntry { dn: "CN=Иванов,OU=Users,DC=x".into(), attrs, bin_attrs: bin }).unwrap();
        assert!(!acc.is_enabled);
        assert_eq!(acc.user_account_control, 514);
    }

    #[test]
    fn guid_that_happens_to_be_utf8_is_not_lost() {
        let mut attrs = HashMap::new();
        attrs.insert("sAMAccountName".to_string(), vec!["ivanov".to_string()]);
        // 16 байт < 0x80 — ldap3 положит их в `attrs`, а не в `bin_attrs`.
        attrs.insert("objectGUID".to_string(), vec!["\u{1}\u{2}\u{3}\u{4}abcdefghijkl".to_string()]);
        let acc = account_from(SearchEntry { dn: "CN=x".into(), attrs, bin_attrs: HashMap::new() }).unwrap();
        assert_eq!(acc.object_guid, "04030201-6261-6463-6566-6768696a6b6c");
    }

    #[test]
    fn computer_fields() {
        let mut attrs = HashMap::new();
        attrs.insert("name".to_string(), vec!["WS-1".to_string()]);
        attrs.insert("description".to_string(), vec!["  ".to_string()]);
        attrs.insert("userAccountControl".to_string(), vec!["4098".to_string()]);
        let mut bin = HashMap::new();
        bin.insert("objectGUID".to_string(), vec![vec![0u8; 16]]);
        let c = computer_from(SearchEntry { dn: "CN=WS-1,DC=x".into(), attrs, bin_attrs: bin }).unwrap();
        assert_eq!(c.name, "WS-1");
        assert_eq!(c.dns_host_name, None);
        assert_eq!(c.description, None, "пустое описание — нет описания");
        assert!(!c.is_enabled);
    }

    #[test]
    fn entry_without_guid_is_skipped() {
        let e = SearchEntry { dn: "CN=x".into(), attrs: HashMap::new(), bin_attrs: HashMap::new() };
        assert!(account_from(e).is_none());
    }
}
