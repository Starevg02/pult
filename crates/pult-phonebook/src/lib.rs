//! Справочник: перенос из PhonebookDB .
//!
//! Схема Phonebook 1.0 (по `PhonebookRepository.cs`):
//! - `dbo.Departments (DepartmentID, DepartmentName)`, `dbo.Positions (PositionID, PositionName)`;
//! - `dbo.Employees (EmployeeID, LastName, FirstName, MiddleName, DepartmentID, PositionID,
//!   WorkPhone, InternalPhone, MobilePhone [, Email] [, IsArchived, ArchivedAtUtc, ArchivedBy])`;
//! - `dbo.PhonebookAudit (AuditId, OccurredAt, ActionName, EntityName, SubjectName, LoginName,
//!   ApplicationName, BeforeXml, AfterXml)` — есть не во всех установках.
//!
//! Правила переноса — [`map_employee`]; чтение с SQL Server — [`reader`]; запись в БД Pult — [`import`].
//! Справочник в портале — [`persons`], [`search`], [`api`]; звонки — [`telephony`].

pub mod ami;
pub mod api;
pub mod directory;
pub mod import;
pub mod persons;
pub mod reader;
pub mod search;
pub mod telephony;

use serde::Serialize;

/// Строка `dbo.PhonebookAudit` (журнал изменений Phonebook) — переносится в `audit_log` как есть.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct LegacyAudit {
    pub id: i64,
    pub occurred_at: String,
    pub action: String,
    pub entity: String,
    pub subject: String,
    pub login: String,
    pub application: String,
    pub before_xml: String,
    pub after_xml: String,
}

/// Строка `dbo.Employees` как есть.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct LegacyEmployee {
    pub id: i32,
    pub last_name: String,
    pub first_name: String,
    pub middle_name: String,
    pub department_id: Option<i32>,
    pub position_id: Option<i32>,
    pub work_phone: String,
    pub internal_phone: String,
    pub mobile_phone: String,
    pub email: String,
    pub is_archived: bool,
}

/// Запись справочника Pult (будущая строка `persons`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PersonImport {
    pub legacy_phonebook_id: i32,
    pub last_name: String,
    pub first_name: String,
    pub middle_name: Option<String>,
    pub legacy_department_id: Option<i32>,
    pub legacy_position_id: Option<i32>,
    pub work_phone: Option<String>,
    /// Добавочный для звонка (3–4 цифры).
    pub internal_ext: Option<String>,
    pub mobile_phone: Option<String>,
    pub email: Option<String>,
    pub is_archived: bool,
}

/// Пометка для отчёта импорта.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ImportNote {
    /// `WorkPhone` пуст, взят устаревший `InternalPhone`.
    UsedLegacyInternalPhone,
    /// Пустое ФИО — запись перенесена, но требует проверки.
    MissingName,
    /// Email не похож на адрес — не перенесён.
    InvalidEmail(String),
}

/// Переносит одного сотрудника. Ничего не теряет молча: всё необычное — в пометках.
pub fn map_employee(e: &LegacyEmployee) -> (PersonImport, Vec<ImportNote>) {
    let mut notes = Vec::new();
    let work = non_empty(&e.work_phone);
    let internal = non_empty(&e.internal_phone);
    let phone = match (work, internal) {
        (Some(w), _) => Some(w),
        (None, Some(i)) => {
            notes.push(ImportNote::UsedLegacyInternalPhone);
            Some(i)
        }
        (None, None) => None,
    };
    let email = non_empty(&e.email).map(|m| m.to_lowercase());
    let email = match email {
        Some(m) if looks_like_email(&m) => Some(m),
        Some(m) => {
            notes.push(ImportNote::InvalidEmail(m));
            None
        }
        None => None,
    };
    let (last, first) = (e.last_name.trim().to_string(), e.first_name.trim().to_string());
    if last.is_empty() && first.is_empty() {
        notes.push(ImportNote::MissingName);
    }
    let person = PersonImport {
        legacy_phonebook_id: e.id,
        last_name: last,
        first_name: first,
        middle_name: non_empty(&e.middle_name),
        legacy_department_id: e.department_id,
        legacy_position_id: e.position_id,
        internal_ext: phone.as_deref().and_then(internal_ext),
        work_phone: phone,
        mobile_phone: non_empty(&e.mobile_phone),
        email,
        is_archived: e.is_archived,
    };
    (person, notes)
}

/// Добавочный номер: в телефоне ровно 3–4 цифры (разделители не считаются).
pub fn internal_ext(phone: &str) -> Option<String> {
    let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
    let only_digits_and_separators = phone.chars().all(|c| c.is_ascii_digit() || " -().".contains(c));
    ((3..=4).contains(&digits.len()) && only_digits_and_separators).then_some(digits)
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

pub(crate) fn looks_like_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else { return false };
    !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.') && !s.contains(' ')
}

/// Итоги переноса — для `--dry-run` и для сверки после переноса (число сотрудников,
/// архивных, подразделений, должностей).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    pub employees: usize,
    pub archived: usize,
    pub departments: usize,
    pub positions: usize,
    pub audit_records: usize,
    pub used_legacy_internal_phone: usize,
    pub with_internal_ext: usize,
    /// Нет ни рабочего, ни внутреннего телефона.
    pub without_phone: usize,
    /// Телефон есть, но не добавочный (городской или с текстом) — звонок не по добавочному.
    pub phone_not_extension: usize,
    pub invalid_emails: usize,
    pub missing_names: usize,
    pub has_email_column: bool,
    pub has_archive_columns: bool,
    pub has_audit_table: bool,
}

/// Снимок PhonebookDB.
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize)]
pub struct Snapshot {
    pub departments: Vec<(i32, String)>,
    pub positions: Vec<(i32, String)>,
    pub employees: Vec<LegacyEmployee>,
    pub audit_records: usize,
    pub audit: Vec<LegacyAudit>,
    pub has_email_column: bool,
    pub has_archive_columns: bool,
    pub has_audit_table: bool,
}

impl Snapshot {
    /// Переносит всех сотрудников и считает итоги.
    pub fn plan(&self) -> (Vec<PersonImport>, ImportReport) {
        let mut r = ImportReport {
            departments: self.departments.len(),
            positions: self.positions.len(),
            audit_records: self.audit_records,
            has_email_column: self.has_email_column,
            has_archive_columns: self.has_archive_columns,
            has_audit_table: self.has_audit_table,
            ..Default::default()
        };
        let mut people = Vec::with_capacity(self.employees.len());
        for e in &self.employees {
            let (p, notes) = map_employee(e);
            r.employees += 1;
            r.archived += usize::from(p.is_archived);
            r.with_internal_ext += usize::from(p.internal_ext.is_some());
            r.without_phone += usize::from(p.work_phone.is_none());
            r.phone_not_extension += usize::from(p.work_phone.is_some() && p.internal_ext.is_none());
            for n in &notes {
                match n {
                    ImportNote::UsedLegacyInternalPhone => r.used_legacy_internal_phone += 1,
                    ImportNote::InvalidEmail(_) => r.invalid_emails += 1,
                    ImportNote::MissingName => r.missing_names += 1,
                }
            }
            people.push(p);
        }
        (people, r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emp() -> LegacyEmployee {
        LegacyEmployee {
            id: 7,
            last_name: " Иванов ".into(),
            first_name: "Иван".into(),
            middle_name: "".into(),
            work_phone: "4512".into(),
            mobile_phone: "+7 900 100-00-00".into(),
            email: "Ivanov@Corp.Local ".into(),
            ..Default::default()
        }
    }

    #[test]
    fn work_phone_wins_and_ext_extracted() {
        let (p, notes) = map_employee(&emp());
        assert_eq!(p.work_phone.as_deref(), Some("4512"));
        assert_eq!(p.internal_ext.as_deref(), Some("4512"));
        assert_eq!(p.last_name, "Иванов");
        assert_eq!(p.middle_name, None);
        assert_eq!(p.email.as_deref(), Some("ivanov@corp.local"));
        assert!(notes.is_empty());
    }

    #[test]
    fn legacy_internal_phone_used_and_noted() {
        let mut e = emp();
        e.work_phone = "  ".into();
        e.internal_phone = "214".into();
        let (p, notes) = map_employee(&e);
        assert_eq!(p.work_phone.as_deref(), Some("214"));
        assert_eq!(notes, vec![ImportNote::UsedLegacyInternalPhone]);
    }

    #[test]
    fn city_number_is_not_an_extension() {
        assert_eq!(internal_ext("8 (8182) 20-45-12"), None);
        assert_eq!(internal_ext("45-12"), Some("4512".into()));
        assert_eq!(internal_ext("доб. 214"), None);
        assert_eq!(internal_ext("12"), None);
    }

    #[test]
    fn bad_email_is_dropped_with_note() {
        let mut e = emp();
        e.email = "ivanov at corp".into();
        let (p, notes) = map_employee(&e);
        assert_eq!(p.email, None);
        assert!(matches!(notes.as_slice(), [ImportNote::InvalidEmail(_)]));
    }

    #[test]
    fn report_counts() {
        let mut archived = emp();
        archived.id = 8;
        archived.is_archived = true;
        archived.work_phone = String::new();
        archived.internal_phone = "300".into();
        let mut city = emp();
        city.id = 9;
        city.work_phone = "8 (8182) 20-45-12".into();
        let mut none = emp();
        none.id = 10;
        none.work_phone = " ".into();
        let s = Snapshot {
            departments: vec![(1, "Бухгалтерия".into())],
            positions: vec![(1, "Бухгалтер".into()), (2, "Инженер".into())],
            employees: vec![emp(), archived, city, none],
            has_archive_columns: true,
            ..Default::default()
        };
        let (people, r) = s.plan();
        assert_eq!(people.len(), 4);
        assert_eq!((r.employees, r.archived, r.departments, r.positions), (4, 1, 1, 2));
        assert_eq!((r.used_legacy_internal_phone, r.with_internal_ext), (1, 2));
        // Без добавочного двое: у одного городской номер, у другого телефона нет.
        assert_eq!((r.phone_not_extension, r.without_phone), (1, 1));
        assert_eq!(r.with_internal_ext + r.phone_not_extension + r.without_phone, r.employees);
    }
}
