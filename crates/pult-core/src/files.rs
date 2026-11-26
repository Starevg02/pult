//! Вложения: белый список типов, проверка содержимого по сигнатуре, чистое имя, заголовок скачивания.
//! Общие для заявок и сообщений.

use sqlx::PgExecutor;

/// Предел файла по умолчанию (настройка Helpdesk «Размер вложения», общая для заявок и сообщений).
pub const DEFAULT_MAX_FILE_MB: u32 = 25;

/// Разрешённое расширение: тип и сигнатура содержимого (пусто — текст и форматы без сигнатуры).
const ALLOWED: &[(&str, &str, &[u8])] = &[
    ("png", "image/png", b"\x89PNG"),
    ("jpg", "image/jpeg", b"\xFF\xD8\xFF"),
    ("jpeg", "image/jpeg", b"\xFF\xD8\xFF"),
    ("gif", "image/gif", b"GIF8"),
    ("webp", "image/webp", b"RIFF"),
    ("pdf", "application/pdf", b"%PDF"),
    ("txt", "text/plain; charset=utf-8", b""),
    ("log", "text/plain; charset=utf-8", b""),
    ("csv", "text/plain; charset=utf-8", b""),
    ("zip", "application/zip", b"PK"),
    ("7z", "application/x-7z-compressed", b"7z\xBC\xAF"),
    ("rar", "application/vnd.rar", b"Rar!"),
    ("docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", b"PK"),
    ("xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", b"PK"),
    ("pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation", b"PK"),
    ("odt", "application/vnd.oasis.opendocument.text", b"PK"),
    ("ods", "application/vnd.oasis.opendocument.spreadsheet", b"PK"),
    ("doc", "application/msword", b"\xD0\xCF\x11\xE0"),
    ("xls", "application/vnd.ms-excel", b"\xD0\xCF\x11\xE0"),
    ("msg", "application/vnd.ms-outlook", b"\xD0\xCF\x11\xE0"),
    ("eml", "message/rfc822", b""),
];

/// Картинки показываем в ленте, всё остальное — только скачивание.
pub fn is_inline_image(mime: &str) -> bool {
    matches!(mime, "image/png" | "image/jpeg" | "image/gif" | "image/webp")
}

/// Имя без пути и управляющих символов.
/// Символ, которым подменяют видимый текст: смена направления (`report\u{202E}fdp.zip` выглядит как
/// `reportpiz.pdf`) и невидимые пробелы. Соединитель эмодзи (U+200D) не трогается.
pub fn is_spoofing(c: char) -> bool {
    matches!(c, '\u{200B}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

pub fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let n: String = base.chars().filter(|c| !c.is_control() && !is_spoofing(*c)).take(200).collect();
    let n = n.trim().trim_matches('.').to_string();
    if n.is_empty() { "file".into() } else { n }
}

/// Тип файла, если он разрешён и содержимое соответствует расширению.
pub fn check_type(name: &str, data: &[u8]) -> Result<&'static str, String> {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let Some((_, mime, magic)) = ALLOWED.iter().find(|(e, _, _)| *e == ext) else {
        return Err(format!("Файлы типа «.{ext}» загружать нельзя."));
    };
    if !data.starts_with(magic) {
        return Err("Содержимое файла не соответствует его расширению.".into());
    }
    // Текст с разметкой браузер не должен принять за страницу, даже при скачивании.
    if magic.is_empty() && data.contains(&0) {
        return Err("Содержимое файла не соответствует его расширению.".into());
    }
    Ok(mime)
}

/// `Content-Disposition` с именем в UTF-8: картинки открываются в браузере, остальное скачивается.
pub fn content_disposition(kind: &str, name: &str) -> String {
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"._-".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }
        })
        .collect();
    format!("{kind}; filename*=UTF-8''{encoded}")
}

/// Предел размера вложения в МБ из настроек Helpdesk (они же действуют для сообщений).
pub async fn max_file_mb(db: impl PgExecutor<'_>) -> Result<u32, sqlx::Error> {
    let v: Option<i64> =
        sqlx::query_scalar("SELECT (value->>'max_file_mb')::bigint FROM settings WHERE key = 'helpdesk'")
            .fetch_optional(db)
            .await?
            .flatten();
    Ok(v.and_then(|n| u32::try_from(n).ok()).filter(|n| (1..=100).contains(n)).unwrap_or(DEFAULT_MAX_FILE_MB))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dangerous_types_are_refused() {
        for n in ["a.exe", "a.html", "a.svg", "a.js", "a.bat", "a", "a.png.exe", "a.ps1"] {
            assert!(check_type(n, b"MZ\x90").is_err(), "{n}");
        }
    }

    #[test]
    fn content_must_match_extension() {
        assert!(check_type("a.png", b"\x89PNG\r\n").is_ok());
        assert!(check_type("a.PNG", b"\x89PNG\r\n").is_ok());
        assert!(check_type("a.png", b"<html><script>").is_err());
        assert!(check_type("a.pdf", b"MZ").is_err());
        assert!(check_type("a.txt", b"\x00\x01").is_err());
        assert!(check_type("a.txt", "привет".as_bytes()).is_ok());
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name("C:\\Users\\x\\скрин.png"), "скрин.png");
        assert_eq!(clean_name("a\r\nb.txt"), "ab.txt");
        assert_eq!(clean_name("..."), "file");
    }

    #[test]
    fn direction_tricks_are_removed() {
        assert_eq!(clean_name("отчёт\u{202E}fdp.zip"), "отчётfdp.zip");
        assert_eq!(clean_name("\u{FEFF}a\u{200B}b.txt"), "ab.txt");
        assert!(!is_spoofing('\u{200D}'), "соединитель эмодзи остаётся");
    }
}
