//! Токены и ключи: случайные 32 байта в base64url с префиксом вида; в базе — только SHA-256.

use axum::http::{HeaderMap, header};
use pult_core::{ApiError, crypto};

/// Префикс токена регистрации агентов.
pub const ENROLL_PREFIX: &str = "pae_";
/// Префикс ключа агента.
pub const AGENT_KEY_PREFIX: &str = "pak_";
/// Префикс одноразового токена подключения (ссылка `pult://connect?t=…`).
pub const CONNECT_PREFIX: &str = "pc_";

/// Новый токен и его хэш.
pub fn issue(prefix: &str) -> Result<(String, Vec<u8>), ApiError> {
    let t = format!("{prefix}{}", crypto::random_token(32).map_err(ApiError::internal)?);
    let h = hash(&t);
    Ok((t, h))
}

pub fn hash(token: &str) -> Vec<u8> {
    crypto::sha256(token.as_bytes())
}

/// Токен годного вида: свой префикс, base64url, разумная длина. Остальное — сразу отказ, без запроса в БД.
pub fn well_formed(token: &str, prefix: &str) -> bool {
    token.len() <= 128
        && token.strip_prefix(prefix).is_some_and(|rest| {
            rest.len() >= 32 && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

/// `Authorization: Bearer <ключ агента>`.
pub fn bearer(h: &HeaderMap) -> Option<&str> {
    h.get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| well_formed(t, AGENT_KEY_PREFIX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_and_check() {
        let (t, h) = issue(CONNECT_PREFIX).unwrap();
        assert!(well_formed(&t, CONNECT_PREFIX));
        assert!(!well_formed(&t, AGENT_KEY_PREFIX), "чужой префикс");
        assert_eq!(h, hash(&t));
        assert_eq!(h.len(), 32);
        let (t2, _) = issue(CONNECT_PREFIX).unwrap();
        assert_ne!(t, t2);
        // Ссылку разбирает helper: токен должен пройти его проверку.
        assert!(t.len() <= 128 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn malformed() {
        assert!(!well_formed("pak_short", AGENT_KEY_PREFIX));
        assert!(!well_formed(&format!("pak_{}", "a".repeat(200)), AGENT_KEY_PREFIX));
        assert!(!well_formed(&format!("pak_{}/", "a".repeat(40)), AGENT_KEY_PREFIX));
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, format!("Bearer pak_{}", "a".repeat(43)).parse().unwrap());
        assert!(bearer(&h).is_some());
        h.insert(header::AUTHORIZATION, "Bearer pult_xxx".parse().unwrap());
        assert!(bearer(&h).is_none());
    }
}
