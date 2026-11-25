//! Оборудование и программы ПК: разбор таблиц SMBIOS и списка «Программы и компоненты».
//! Сам сбор — в `host` (Windows); здесь то, что проверяется тестами на любой платформе.

use pult_api_types::InstalledSoftware;
use std::collections::HashSet;

/// Программ в одном отчёте не больше (на обычном ПК — сотни).
pub const MAX_SOFTWARE: usize = 3000;

/// Сведения о ПК из SMBIOS: тип 0 (BIOS) и тип 1 (System Information).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Smbios {
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    pub bios_version: Option<String>,
}

/// Заглушки производителя вместо модели и серийного номера.
pub fn placeholder(s: &str) -> bool {
    let l = s.trim().to_ascii_lowercase();
    l.chars().all(|c| matches!(c, '0' | ' ' | '-' | '.'))
        || matches!(
            l.as_str(),
            "to be filled by o.e.m."
                | "default string"
                | "system serial number"
                | "system product name"
                | "system manufacturer"
                | "not specified"
                | "none"
                | "n/a"
                | "o.e.m."
        )
}

/// Разбор ответа `GetSystemFirmwareTable('RSMB')`: заголовок RawSMBIOSData (8 байт), затем
/// структуры: тип, длина форматной части, номер; за форматной частью — строки, каждая с нулём,
/// в конце ещё один ноль. Поле-строка хранит номер строки с 1 (0 — строки нет).
pub fn parse_smbios(raw: &[u8]) -> Smbios {
    let mut out = Smbios::default();
    let Some(len) = raw.get(4..8).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize) else {
        return out;
    };
    let data = &raw[8..];
    let data = &data[..len.min(data.len())];
    let mut i = 0;
    while i + 4 <= data.len() {
        let (kind, flen) = (data[i], usize::from(data[i + 1]));
        if flen < 4 || i + flen > data.len() {
            break;
        }
        let formatted = &data[i..i + flen];
        let mut j = i + flen;
        let mut strings: Vec<&[u8]> = Vec::new();
        while j < data.len() && data[j] != 0 {
            let end = data[j..].iter().position(|&b| b == 0).map_or(data.len(), |p| j + p);
            strings.push(&data[j..end]);
            j = end + 1;
        }
        // Завершающий ноль; у структуры без строк их два.
        j += if strings.is_empty() { 2 } else { 1 };
        let text = |off: usize| {
            let n = usize::from(*formatted.get(off)?);
            let s = String::from_utf8_lossy(strings.get(n.checked_sub(1)?)?).trim().to_string();
            Some(s).filter(|s| !placeholder(s))
        };
        match kind {
            0 => out.bios_version = out.bios_version.take().or_else(|| text(0x05)),
            1 => {
                out.manufacturer = text(0x04);
                out.product = text(0x05);
                out.serial = text(0x07);
            }
            127 => break,
            _ => {}
        }
        i = j;
    }
    out
}

/// Запись раздела Uninstall реестра.
#[derive(Debug, Default)]
pub struct UninstallEntry {
    pub name: Option<String>,
    pub version: Option<String>,
    pub publisher: Option<String>,
    /// `SystemComponent = 1`: Windows не показывает её в «Программах и компонентах».
    pub system_component: bool,
    /// Есть `ParentKeyName`: обновление или часть другой программы.
    pub has_parent: bool,
    pub release_type: Option<String>,
}

/// Список, как в «Программах и компонентах»: без системных компонентов и обновлений, без повторов
/// (64- и 32-битный разделы, установка «для всех» и «для себя»), по алфавиту.
pub fn software(entries: impl IntoIterator<Item = UninstallEntry>) -> Vec<InstalledSoftware> {
    let mut seen = HashSet::new();
    let mut out: Vec<InstalledSoftware> = entries
        .into_iter()
        .filter(|e| !e.system_component && !e.has_parent)
        .filter(|e| {
            !e.release_type
                .as_deref()
                .is_some_and(|r| matches!(r.to_ascii_lowercase().as_str(), "update" | "hotfix" | "security update"))
        })
        .filter_map(|e| {
            let name = e.name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty())?;
            let version = e.version.map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
            seen.insert((name.to_lowercase(), version.clone())).then(|| InstalledSoftware {
                name,
                version,
                publisher: e.publisher.map(|p| p.trim().to_string()).filter(|p| !p.is_empty()),
            })
        })
        .take(MAX_SOFTWARE)
        .collect();
    out.sort_by_key(|s| s.name.to_lowercase());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Структура SMBIOS: форматная часть (тип, длина, номер, поля) и строки.
    fn structure(kind: u8, fields: &[u8], strings: &[&str]) -> Vec<u8> {
        let mut v = vec![kind, 4 + fields.len() as u8, 0, 0];
        v.extend_from_slice(fields);
        for s in strings {
            v.extend_from_slice(s.as_bytes());
            v.push(0);
        }
        v.push(0);
        if strings.is_empty() {
            v.push(0);
        }
        v
    }

    fn raw(structures: &[Vec<u8>]) -> Vec<u8> {
        let data: Vec<u8> = structures.concat();
        let mut v = vec![0, 3, 4, 0];
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend(data);
        v
    }

    #[test]
    fn smbios_system_and_bios() {
        let t = raw(&[
            // BIOS: поставщик (строка 1), версия (строка 2).
            structure(0, &[1, 2], &["Dell Inc.", "1.21.0"]),
            // Структура без строк — два нуля в конце.
            structure(32, &[0, 0, 0, 0, 0, 0, 0], &[]),
            // Система: производитель, модель, версия (нет), серийный номер.
            structure(1, &[1, 2, 0, 3], &["Dell Inc.", "OptiPlex 7090", " 5CG1234XYZ "]),
            structure(127, &[], &[]),
        ]);
        let s = parse_smbios(&t);
        assert_eq!(s.manufacturer.as_deref(), Some("Dell Inc."));
        assert_eq!(s.product.as_deref(), Some("OptiPlex 7090"));
        assert_eq!(s.serial.as_deref(), Some("5CG1234XYZ"));
        assert_eq!(s.bios_version.as_deref(), Some("1.21.0"));
    }

    #[test]
    fn smbios_placeholders_and_garbage() {
        let t = raw(&[structure(1, &[1, 2, 0, 3], &["To be filled by O.E.M.", "System Product Name", "0"])]);
        assert_eq!(parse_smbios(&t), Smbios::default());
        assert_eq!(parse_smbios(&[]), Smbios::default());
        assert_eq!(parse_smbios(&[0, 3, 4, 0, 200, 0, 0, 0, 1, 2]), Smbios::default(), "обрезанная таблица");
        let t = raw(&[structure(1, &[9, 0, 0, 0], &["x"])]);
        assert_eq!(parse_smbios(&t), Smbios::default(), "номер строки за пределами");
    }

    #[test]
    fn software_list() {
        let e = |name: &str, version: Option<&str>| UninstallEntry {
            name: Some(name.into()),
            version: version.map(Into::into),
            ..Default::default()
        };
        let list = software([
            e("Mozilla Firefox (x64 ru)", Some("133.0")),
            e("7-Zip 24.08 (x64)", Some("24.08")),
            e("7-zip 24.08 (x64)", Some("24.08")),
            UninstallEntry { system_component: true, ..e("Microsoft Edge Update", None) },
            UninstallEntry { has_parent: true, ..e("Office 16 Click-to-Run Licensing Component", None) },
            UninstallEntry { release_type: Some("Security Update".into()), ..e("Security Update for X", None) },
            e("  ", None),
        ]);
        let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["7-Zip 24.08 (x64)", "Mozilla Firefox (x64 ru)"]);
    }
}
