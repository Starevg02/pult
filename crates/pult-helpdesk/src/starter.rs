//! Стартовые данные helpdesk на языке установки (мастер).
//!
//! Миграции кладут категории, типы оборудования и шаблоны по-русски. Мастер переводит их
//! на выбранный язык через каталог `l10n`. Меняется только то, что администратор не трогал:
//! запись, совпадающая с исходной строкой на любом из языков. Повторный запуск ничего не портит.

use pult_core::l10n::{self, Lang};
use serde_json::Value;
use sqlx::PgConnection;

/// Категории заявок (`20261109000000_helpdesk.sql`).
const CATEGORIES: &[&str] = &[
    "Компьютер и периферия",
    "Принтеры и сканеры",
    "Сеть и Интернет",
    "Почта",
    "Учётная запись и пароль",
    "Программы и 1С",
    "Телефония",
    "Другое",
    "Не указана",
    "Запрос доступа",
];

/// Типы оборудования и подписи их полей (`20261110000002_stage3_catalog.sql`).
const ASSET_TYPES: &[&str] = &[
    "Принтер / МФУ",
    "Монитор",
    "Ноутбук",
    "ИБП",
    "Телефон",
    "Сетевое оборудование",
    "Программа / лицензия",
    "Прочее",
];
const ASSET_FIELDS: &[&str] = &[
    "Модель",
    "Картридж",
    "IP-адрес",
    "Замена батареи",
    "Внутренний номер",
    "Производитель",
    "Действует до",
    "Число мест",
];

/// Шаблоны заявок: название, тема, текст, пункты чек-листа.
const TEMPLATES: &[&str] = &[
    "Новый сотрудник",
    "Новый сотрудник: ФИО, должность, дата выхода",
    "ФИО:\nПодразделение и должность:\nДата выхода:\nРуководитель:\nНужные программы и доступы:",
    "Увольнение сотрудника",
    "Увольнение: ФИО, последний рабочий день",
    "ФИО:\nПоследний рабочий день:\nКому передать почту и файлы:",
    "Перевод в другой отдел",
    "Перевод: ФИО, новый отдел и должность",
    "ФИО:\nНовое подразделение и должность:\nДата перевода:",
];
const CHECKLIST: &[&str] = &[
    "Учётная запись в AD",
    "Почтовый ящик",
    "Группы и доступы к папкам",
    "Рабочее место: ПК, монитор, телефон",
    "Программы (1С и прочие)",
    "Запись в справочник, внутренний номер",
    "Отключить учётную запись в AD",
    "Почта: переадресация или архив",
    "Снять доступы и группы",
    "Забрать оборудование",
    "Отметить в справочнике",
    "Изменить подразделение и должность в AD",
    "Пересмотреть группы и доступы",
    "Перенести рабочее место",
    "Обновить справочник",
];

/// Шаблоны ответов: название и текст.
const RESPONSES: &[&str] = &[
    "Приветствие",
    "Здравствуйте, {{requester.first_name}}! Заявку #{{ticket.number}} взял в работу.",
    "Нужна информация",
    "Здравствуйте, {{requester.first_name}}! Чтобы решить заявку, пришлите, пожалуйста, скриншот ошибки и имя компьютера.",
    "Решено",
    "Готово. Если что-то не так — нажмите «Нет, не решена», заявка вернётся ко мне. {{engineer.name}}",
];

/// Строка на всех языках: исходная русская и переводы (по ним узнаём нетронутую запись).
fn variants(ru: &str) -> Vec<String> {
    Lang::CODES.iter().map(|c| l10n::tr_to(Lang::parse(c), ru).into_owned()).collect()
}

/// Перевод строки `s`, если она — одна из стартовых `list` на любом языке.
fn translate(list: &[&str], s: &str, lang: Lang) -> Option<String> {
    list.iter().find(|ru| variants(ru).iter().any(|v| v == s)).map(|ru| l10n::tr_to(lang, ru).into_owned())
}

/// Обновить колонку запросом `sql` (`$1` — новое значение, `$2` — прежние): стартовые строки — на язык `lang`.
async fn column(db: &mut PgConnection, sql: &'static str, list: &[&str], lang: Lang) -> Result<u64, sqlx::Error> {
    let mut n = 0;
    for ru in list {
        let to = l10n::tr_to(lang, ru).into_owned();
        let from: Vec<String> = variants(ru).into_iter().filter(|v| *v != to).collect();
        n += sqlx::query(sql).bind(&to).bind(&from).execute(&mut *db).await?.rows_affected();
    }
    Ok(n)
}

/// Перевести подписи внутри JSON-массива `[{key: …}]`.
fn relabel(v: &mut Value, key: &str, list: &[&str], lang: Lang) -> bool {
    let mut changed = false;
    for item in v.as_array_mut().into_iter().flatten() {
        if let Some(s) = item.get(key).and_then(Value::as_str)
            && let Some(t) = translate(list, s, lang)
            && t != s
        {
            item[key] = Value::String(t);
            changed = true;
        }
    }
    changed
}

/// Перевести стартовые данные helpdesk на язык `lang`. Возвращает число изменённых записей.
/// Вызывать в транзакции: перевод применяется целиком или никак.
pub async fn localize(db: &mut PgConnection, lang: Lang) -> Result<u64, sqlx::Error> {
    let mut n = column(db, "UPDATE ticket_categories SET name = $1 WHERE name = ANY($2)", CATEGORIES, lang).await?;
    n += column(
        db,
        "UPDATE asset_types SET name = $1 WHERE name = ANY($2)
          AND NOT EXISTS (SELECT 1 FROM asset_types o WHERE lower(o.name) = lower($1))",
        ASSET_TYPES,
        lang,
    )
    .await?;
    n += column(db, "UPDATE ticket_templates SET name = $1 WHERE name = ANY($2)", TEMPLATES, lang).await?;
    n += column(db, "UPDATE ticket_templates SET title = $1 WHERE title = ANY($2)", TEMPLATES, lang).await?;
    n +=
        column(db, "UPDATE ticket_templates SET description = $1 WHERE description = ANY($2)", TEMPLATES, lang).await?;
    n += column(db, "UPDATE response_templates SET title = $1 WHERE title = ANY($2)", RESPONSES, lang).await?;
    n += column(db, "UPDATE response_templates SET body = $1 WHERE body = ANY($2)", RESPONSES, lang).await?;

    let rows: Vec<(sqlx::types::Uuid, Value)> =
        sqlx::query_as("SELECT id, fields FROM asset_types").fetch_all(&mut *db).await?;
    for (id, mut fields) in rows {
        if relabel(&mut fields, "label", ASSET_FIELDS, lang) {
            sqlx::query("UPDATE asset_types SET fields = $2 WHERE id = $1")
                .bind(id)
                .bind(&fields)
                .execute(&mut *db)
                .await?;
            n += 1;
        }
    }
    let rows: Vec<(sqlx::types::Uuid, Value)> =
        sqlx::query_as("SELECT id, checklist FROM ticket_templates").fetch_all(&mut *db).await?;
    for (id, mut items) in rows {
        if relabel(&mut items, "title", CHECKLIST, lang) {
            sqlx::query("UPDATE ticket_templates SET checklist = $2 WHERE id = $1")
                .bind(id)
                .bind(&items)
                .execute(&mut *db)
                .await?;
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_starter_string_is_translated() {
        for list in [CATEGORIES, ASSET_TYPES, ASSET_FIELDS, TEMPLATES, CHECKLIST, RESPONSES] {
            for ru in list {
                for lang in [Lang::En, Lang::Zh] {
                    let t = l10n::tr_to(lang, ru);
                    assert!(!t.chars().any(l10n::is_cyrillic), "{ru} ({lang:?}): {t}");
                }
            }
        }
    }

    #[test]
    fn placeholders_survive_translation() {
        for ru in RESPONSES.iter().filter(|s| s.contains("{{")) {
            for lang in [Lang::En, Lang::Zh] {
                let t = l10n::tr_to(lang, ru);
                for ph in ["{{requester.first_name}}", "{{ticket.number}}", "{{engineer.name}}"] {
                    assert_eq!(ru.contains(ph), t.contains(ph), "{ph} в {t}");
                }
            }
        }
    }

    #[test]
    fn edited_entries_are_left_alone() {
        assert_eq!(translate(CATEGORIES, "Почта", Lang::En).as_deref(), Some("Email"));
        assert_eq!(translate(CATEGORIES, "Email", Lang::Ru).as_deref(), Some("Почта"));
        assert_eq!(translate(CATEGORIES, "Почта и календарь", Lang::En), None);
    }
}
