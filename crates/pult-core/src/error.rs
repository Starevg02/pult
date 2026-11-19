//! Ошибки API: код для программы, текст для человека. Подробности внутренних ошибок
//! уходят в журнал, а не в ответ.

use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;

/// Тело ответа с ошибкой.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ErrorBody {
    /// Машинный код: `unauthorized`, `forbidden`, `conflict`…
    pub error: &'static str,
    /// Текст для пользователя.
    pub message: String,
    /// Дополнительные данные (например, текущая версия записи при конфликте).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("нужно войти")]
    Unauthorized,
    #[error("недостаточно прав")]
    Forbidden,
    /// Отказ с объяснением (например, ПК запрещён политикой): 403 со своим кодом.
    #[error("{message}")]
    Refused { code: &'static str, message: String },
    #[error("CSRF-токен отсутствует или неверен")]
    Csrf,
    #[error("не найдено")]
    NotFound,
    /// Одноразовое уже использовано (временный пароль показан).
    #[error("уже показано, второй раз не показывается")]
    Gone,
    #[error("{message}")]
    BadRequest { code: &'static str, message: String },
    /// Запись изменил кто-то другой (оптимистичная блокировка).
    #[error("запись уже изменена другим пользователем")]
    Conflict { current: serde_json::Value },
    #[error("{message}")]
    TooManyRequests { message: String, retry_after_secs: u64 },
    #[error("{message}")]
    Unavailable { code: &'static str, message: String },
    #[error("внутренняя ошибка")]
    Internal(String),
}

impl ApiError {
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::BadRequest { code, message: message.into() }
    }

    pub fn internal(e: impl std::fmt::Display) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => Self::NotFound,
            // Ссылка на несуществующую запись (площадка, шаблон, пользователь…) — ошибка ввода, а не сбой.
            sqlx::Error::Database(d) if d.code().as_deref() == Some("23503") => {
                // В журнал — ограничение: если это ошибка сервера, а не ввода, её видно.
                tracing::warn!(constraint = d.constraint().unwrap_or("?"), "нарушен внешний ключ — ответ 400");
                Self::bad_request("invalid_reference", "Связанная запись не найдена или ещё используется.")
            }
            other => Self::Internal(other.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Сообщение — на языке запроса (каталог `l10n/en.tsv`).
        let message = crate::l10n::tr(&self.to_string()).into_owned();
        let (status, error, details) = match &self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", None),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden", None),
            Self::Refused { code, .. } => (StatusCode::FORBIDDEN, *code, None),
            Self::Csrf => (StatusCode::FORBIDDEN, "csrf", None),
            Self::NotFound => (StatusCode::NOT_FOUND, "not_found", None),
            Self::Gone => (StatusCode::GONE, "gone", None),
            Self::BadRequest { code, .. } => (StatusCode::BAD_REQUEST, *code, None),
            Self::Conflict { current } => (StatusCode::CONFLICT, "conflict", Some(current.clone())),
            Self::TooManyRequests { .. } => (StatusCode::TOO_MANY_REQUESTS, "too_many_requests", None),
            Self::Unavailable { code, .. } => (StatusCode::SERVICE_UNAVAILABLE, *code, None),
            Self::Internal(why) => {
                tracing::error!(error = %why, "внутренняя ошибка API");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal", None)
            }
        };
        let mut resp = (status, Json(ErrorBody { error, message, details })).into_response();
        if let Self::TooManyRequests { retry_after_secs, .. } = self
            && let Ok(v) = HeaderValue::from_str(&retry_after_secs.to_string())
        {
            resp.headers_mut().insert(header::RETRY_AFTER, v);
        }
        resp
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
