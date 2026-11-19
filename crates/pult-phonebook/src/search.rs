//! Поиск в справочнике (как `ContactSearch.cs` в Phonebook):
//! - каждое слово запроса должно найтись в ФИО, должности, подразделении, email, телефонах, кабинете;
//! - `ё` = `е`, регистр не важен;
//! - автоисправление раскладки: запрос, набранный не в той раскладке, ищется ещё и исправленным
//!   (в Phonebook — только латиница → кириллица; здесь — в обе стороны);
//! - слово из цифр (от 3) с разделителями `+-().` ищется по цифрам телефонов без учёта форматирования.

/// Клавиши в одинаковом порядке: латинская раскладка и ЙЦУКЕН.
const LATIN: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`";
const RUSSIAN: &str = "йцукенгшщзхъфывапролджэячсмитьбюё";

/// Нормализация как у `pult_norm` в БД: нижний регистр, ё → е, пробелы схлопнуты.
pub fn normalize(s: &str) -> String {
    s.to_lowercase().replace('ё', "е").split_whitespace().collect::<Vec<_>>().join(" ")
}

fn convert(s: &str, from: &str, to: &str) -> String {
    let to: Vec<char> = to.chars().collect();
    s.chars().map(|c| from.chars().position(|f| f == c).map(|i| to[i]).unwrap_or(c)).collect()
}

/// Набрано латиницей, а имелась в виду кириллица: `bdfyjd` → `иванов`.
pub fn latin_to_russian(s: &str) -> String {
    normalize(&convert(&s.to_lowercase(), LATIN, RUSSIAN))
}

/// Набрано кириллицей, а имелась в виду латиница: `шмфтщм` → `ivanov` (например, email).
pub fn russian_to_latin(s: &str) -> String {
    normalize(&convert(&s.to_lowercase(), RUSSIAN, LATIN))
}

/// Одно слово запроса для SQL: текст (с экранированием для LIKE) и цифры, если это номер.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub like: String,
    /// Цифры от 3 и больше, если слово похоже на номер телефона; иначе пусто.
    pub digits: String,
}

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn tokens(q: &str) -> Vec<Token> {
    q.split_whitespace()
        .map(|t| {
            let digits: String = t.chars().filter(char::is_ascii_digit).collect();
            let numeric = digits.len() >= 3 && t.chars().all(|c| c.is_ascii_digit() || "+-().".contains(c));
            Token { like: escape_like(t), digits: if numeric { digits } else { String::new() } }
        })
        .collect()
}

/// Варианты запроса: исходный и исправленные раскладки (без повторов). Пустой запрос — пусто.
pub fn variants(raw: &str) -> Vec<Vec<Token>> {
    let q = normalize(raw);
    if q.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = vec![q.clone()];
    for v in [latin_to_russian(&q), russian_to_latin(&q)] {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out.iter().map(|v| tokens(v)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_fix_both_ways() {
        assert_eq!(latin_to_russian("Bdfyjd"), "иванов");
        assert_eq!(russian_to_latin("шмфтщм"), "ivanov");
        assert_eq!(latin_to_russian("ctvtyjd"), "семенов");
        // Цифры и прочее не трогаем.
        assert_eq!(latin_to_russian("4512"), "4512");
    }

    #[test]
    fn normalization_matches_db() {
        assert_eq!(normalize("  Семёнов   ПЁТР "), "семенов петр");
    }

    #[test]
    fn variants_without_duplicates() {
        let v = variants("4512");
        assert_eq!(v.len(), 1, "цифры одинаковы во всех раскладках");
        assert_eq!(v[0], vec![Token { like: "4512".into(), digits: "4512".into() }]);
        let v = variants("bdfyjd");
        assert_eq!(v.len(), 2, "латиница → кириллица; обратное преобразование совпало с исходным");
        assert_eq!(v[1][0].like, "иванов");
        assert!(variants("   ").is_empty());
    }

    #[test]
    fn phone_tokens() {
        let t = tokens("+7(900)100-00-00 12 abc123");
        assert_eq!(t[0].digits, "79001000000");
        assert_eq!(t[1].digits, "", "меньше трёх цифр — не номер");
        assert_eq!(t[2].digits, "", "буквы — не номер");
    }

    #[test]
    fn like_is_escaped() {
        assert_eq!(tokens("50%_a\\b")[0].like, "50\\%\\_a\\\\b");
    }
}
