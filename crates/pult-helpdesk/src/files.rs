//! Вложения: белый список типов, проверка содержимого, хранение по sha256.

use crate::{HelpdeskState, settings::HelpdeskSettings};
use pult_core::{ApiError, ApiResult, Core};
use ring::digest;
use std::path::PathBuf;
use uuid::Uuid;

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
pub fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let n: String = base.chars().filter(|c| !c.is_control()).take(200).collect();
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

pub fn path_of(st: &HelpdeskState, sha: &str) -> PathBuf {
    st.files_dir.join(&sha[..2]).join(sha)
}

/// Сохраняет файл. Одинаковое содержимое лежит на диске один раз.
pub async fn store(
    core: &Core,
    st: &HelpdeskState,
    name: &str,
    data: &[u8],
    uploaded_by: Option<Uuid>,
) -> ApiResult<Uuid> {
    let cfg: HelpdeskSettings = pult_core::settings::load(&core.db).await?;
    if data.is_empty() {
        return Err(ApiError::bad_request("invalid_file", "Файл пустой."));
    }
    if data.len() as u64 > u64::from(cfg.max_file_mb) * 1024 * 1024 {
        return Err(ApiError::bad_request("file_too_large", format!("Файл больше {} МБ.", cfg.max_file_mb)));
    }
    let name = clean_name(name);
    let mime = check_type(&name, data).map_err(|m| ApiError::bad_request("invalid_file", m))?;
    let sha: String = digest::digest(&digest::SHA256, data).as_ref().iter().map(|b| format!("{b:02x}")).collect();
    let path = path_of(st, &sha);
    if !path.exists() {
        let dir = path.parent().ok_or_else(|| ApiError::internal("путь файла"))?;
        tokio::fs::create_dir_all(dir).await.map_err(ApiError::internal)?;
        let tmp = dir.join(format!("{sha}.{}.tmp", Uuid::now_v7()));
        tokio::fs::write(&tmp, data).await.map_err(ApiError::internal)?;
        tokio::fs::rename(&tmp, &path).await.map_err(ApiError::internal)?;
    }
    Ok(sqlx::query_scalar(
        "INSERT INTO files (sha256, size, mime, original_name, uploaded_by) VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(&sha)
    .bind(data.len() as i64)
    .bind(mime)
    .bind(&name)
    .bind(uploaded_by)
    .fetch_one(&core.db)
    .await?)
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
}
