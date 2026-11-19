//! Обновление агента по команде портала: проверка SHA-256 и подписи Ed25519.
//!
//! Подпись делает администратор вне сервера (`pult-agent sign`), ключом, созданным `pult-agent keygen`
//! и хранящимся офлайн, как ключ корневого УЦ. Открытый ключ задаётся при установке агента
//! (свойство MSI `UPDATEKEY` или параметр GPO). Без ключа агент обновления не принимает.

use base64::{Engine, engine::general_purpose::STANDARD};
use pult_api_types::{AgentUpdate, update_message};
use ring::{
    digest,
    rand::SystemRandom,
    signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey},
};

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UpdateError {
    #[error("хэш пакета не совпадает с объявленным")]
    HashMismatch,
    #[error("подпись пакета неверна")]
    BadSignature,
    #[error("открытый ключ обновлений неверен (нужно 32 байта в base64)")]
    BadKey,
    #[error("ключ подписи не читается (нужен PKCS#8 из pult-agent keygen)")]
    BadPrivateKey,
}

pub fn sha256_hex(data: &[u8]) -> String {
    digest::digest(&digest::SHA256, data).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Открытый ключ из настройки (base64, 32 байта).
pub fn parse_public_key(b64: &str) -> Result<Vec<u8>, UpdateError> {
    let k = STANDARD.decode(b64.trim()).map_err(|_| UpdateError::BadKey)?;
    if k.len() == 32 { Ok(k) } else { Err(UpdateError::BadKey) }
}

/// Пакет годится, только если его хэш объявлен порталом и подписан ключом обновлений.
pub fn verify(data: &[u8], u: &AgentUpdate, public_key: &[u8]) -> Result<(), UpdateError> {
    let sha = sha256_hex(data);
    if !sha.eq_ignore_ascii_case(u.sha256.trim()) {
        return Err(UpdateError::HashMismatch);
    }
    let sig = STANDARD.decode(u.signature.trim()).map_err(|_| UpdateError::BadSignature)?;
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(&update_message(&u.version, &sha), &sig)
        .map_err(|_| UpdateError::BadSignature)
}

/// Версия `candidate` новее `current` (числа через точку). Обновление на ту же или старую версию
/// не ставится: иначе по команде портала можно вернуть старую подписанную сборку с известной уязвимостью.
pub fn newer(candidate: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<Vec<u64>> {
        v.trim().split('.').map(|p| p.parse::<u64>().ok()).collect()
    }
    match (parse(candidate), parse(current)) {
        (Some(mut a), Some(mut b)) => {
            let n = a.len().max(b.len());
            a.resize(n, 0);
            b.resize(n, 0);
            a > b
        }
        _ => false,
    }
}

/// Новый ключ подписи: PKCS#8 (хранить офлайн) и открытый ключ (base64 — в настройку агентов).
pub fn keygen() -> Result<(Vec<u8>, String), UpdateError> {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).map_err(|_| UpdateError::BadPrivateKey)?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| UpdateError::BadPrivateKey)?;
    Ok((pkcs8.as_ref().to_vec(), STANDARD.encode(pair.public_key().as_ref())))
}

/// Подпись пакета: (подпись base64, SHA-256 hex) — для загрузки в админке.
pub fn sign(pkcs8: &[u8], version: &str, data: &[u8]) -> Result<(String, String), UpdateError> {
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8).map_err(|_| UpdateError::BadPrivateKey)?;
    let sha = sha256_hex(data);
    let sig = pair.sign(&update_message(version, &sha));
    Ok((STANDARD.encode(sig.as_ref()), sha))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(version: &str, data: &[u8], pkcs8: &[u8]) -> AgentUpdate {
        let (signature, sha256) = sign(pkcs8, version, data).unwrap();
        AgentUpdate { package_id: "p".into(), version: version.into(), sha256, signature }
    }

    #[test]
    fn signed_package_verifies() {
        let (pkcs8, public) = keygen().unwrap();
        let key = parse_public_key(&public).unwrap();
        let data = b"MZ new agent";
        let u = update("0.6.0", data, &pkcs8);
        assert_eq!(verify(data, &u, &key), Ok(()));
    }

    #[test]
    fn tampering_is_rejected() {
        let (pkcs8, public) = keygen().unwrap();
        let key = parse_public_key(&public).unwrap();
        let data = b"MZ new agent";
        let u = update("0.6.0", data, &pkcs8);
        assert_eq!(verify(b"MZ evil agent", &u, &key), Err(UpdateError::HashMismatch), "подменили файл");
        let evil = AgentUpdate { sha256: sha256_hex(b"MZ evil agent"), ..u.clone() };
        assert_eq!(verify(b"MZ evil agent", &evil, &key), Err(UpdateError::BadSignature), "подменили файл и хэш");
        let other = AgentUpdate { version: "9.9.9".into(), ..u.clone() };
        assert_eq!(verify(data, &other, &key), Err(UpdateError::BadSignature), "подпись привязана к версии");
        let (_, stranger) = keygen().unwrap();
        assert_eq!(
            verify(data, &u, &parse_public_key(&stranger).unwrap()),
            Err(UpdateError::BadSignature),
            "чужой ключ"
        );
        let garbage = AgentUpdate { signature: "не base64".into(), ..u };
        assert_eq!(verify(data, &garbage, &key), Err(UpdateError::BadSignature));
    }

    #[test]
    fn keys() {
        assert_eq!(parse_public_key("AAAA"), Err(UpdateError::BadKey));
        assert_eq!(parse_public_key("%%%"), Err(UpdateError::BadKey));
        assert_eq!(sign(b"not pkcs8", "1", b"x"), Err(UpdateError::BadPrivateKey));
    }

    #[test]
    fn only_newer_versions_are_installed() {
        assert!(newer("0.6.0", "0.5.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.9.9"));
        assert!(!newer("0.5.0", "0.5.0"), "та же версия — не обновление");
        assert!(!newer("0.4.9", "0.5.0"), "откат на старую сборку не принимается");
        assert!(!newer("0.6.0-beta", "0.5.0"), "непонятная версия не принимается");
        assert!(!newer("", "0.5.0"));
    }
}
