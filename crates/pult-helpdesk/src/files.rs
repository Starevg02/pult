//! Вложения: белый список типов, проверка содержимого, хранение по sha256.

use crate::{HelpdeskState, settings::HelpdeskSettings};
use pult_core::{ApiError, ApiResult, Core};
use ring::digest;
use std::path::PathBuf;
use uuid::Uuid;

pub use pult_core::files::{check_type, clean_name, is_inline_image};

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
