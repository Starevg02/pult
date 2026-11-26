//! Шифрование секретов в БД и случайные токены.
//!
//! Ключ шифрования выводится из `PULT_SECRET_KEY` через HKDF-SHA256; шифр — AES-256-GCM.
//! Формат шифротекста: `[версия = 1][nonce, 12 байт][данные + тег]`. Сменить ключ без
//! перешифровки нельзя — поэтому версия формата хранится в каждом значении.

use crate::Secret;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey},
    digest, hkdf,
    rand::{SecureRandom, SystemRandom},
};

const FORMAT_V1: u8 = 1;
/// Контекст HKDF: при появлении других ключей (например, для подписи ссылок) — свой контекст.
const INFO_DB_SECRETS: &[u8] = b"pult/db-secrets/v1";
const SALT: &[u8] = b"pult";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("секрет повреждён или зашифрован другим ключом (PULT_SECRET_KEY)")]
    Decrypt,
    #[error("неизвестный формат зашифрованного секрета")]
    Format,
    #[error("системный генератор случайных чисел недоступен")]
    Rng,
}

/// Шифрует и расшифровывает секреты, которые хранятся в БД (пароль служебной учётки AD, SMTP…).
pub struct SecretBox {
    key: LessSafeKey,
    rng: SystemRandom,
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretBox(***)")
    }
}

struct Len32;
impl hkdf::KeyType for Len32 {
    fn len(&self) -> usize {
        32
    }
}

impl SecretBox {
    pub fn new(master: &Secret) -> Self {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, SALT).extract(master.expose().as_bytes());
        let mut bytes = [0u8; 32];
        // Ошибка возможна только при неверной длине — длина фиксирована.
        #[allow(clippy::expect_used)]
        prk.expand(&[INFO_DB_SECRETS], Len32).and_then(|okm| okm.fill(&mut bytes)).expect("HKDF: длина ключа 32");
        #[allow(clippy::expect_used)]
        let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &bytes).expect("AES-256: ключ 32 байта"));
        bytes.fill(0);
        Self { key, rng: SystemRandom::new() }
    }

    pub fn encrypt(&self, plain: &Secret) -> Result<Vec<u8>, CryptoError> {
        self.encrypt_bytes(plain.expose().as_bytes())
    }

    /// Зашифровать произвольные байты (вложения сообщений) тем же ключом и в том же формате.
    pub fn encrypt_bytes(&self, plain: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.encrypt_vec(plain.to_vec())
    }

    /// То же, что [`Self::encrypt_bytes`], но без копии: большой файл не держится в памяти дважды.
    pub fn encrypt_vec(&self, mut data: Vec<u8>) -> Result<Vec<u8>, CryptoError> {
        let mut nonce = [0u8; NONCE_LEN];
        self.rng.fill(&mut nonce).map_err(|_| CryptoError::Rng)?;
        self.key
            .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from([FORMAT_V1]), &mut data)
            .map_err(|_| CryptoError::Rng)?;
        // Заголовок (версия и nonce) — в начало того же буфера.
        data.splice(0..0, std::iter::once(FORMAT_V1).chain(nonce));
        Ok(data)
    }

    pub fn decrypt(&self, blob: &[u8]) -> Result<Secret, CryptoError> {
        let mut data = self.decrypt_bytes(blob)?;
        let s = String::from_utf8(data.clone()).map_err(|_| CryptoError::Decrypt);
        zeroize::Zeroize::zeroize(&mut data);
        s.map(Secret::new)
    }

    /// Расшифровать байты, зашифрованные [`Self::encrypt_bytes`] (или текст из [`Self::encrypt`]).
    pub fn decrypt_bytes(&self, blob: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let (&ver, rest) = blob.split_first().ok_or(CryptoError::Format)?;
        if ver != FORMAT_V1 || rest.len() < NONCE_LEN {
            return Err(CryptoError::Format);
        }
        let (nonce, ct) = rest.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| CryptoError::Format)?;
        let mut data = ct.to_vec();
        let len =
            self.key.open_in_place(nonce, Aad::from([FORMAT_V1]), &mut data).map_err(|_| CryptoError::Decrypt)?.len();
        data.truncate(len);
        Ok(data)
    }
}

/// Случайный токен: `bytes` случайных байт в base64url без `=`.
pub fn random_token(bytes: usize) -> Result<String, CryptoError> {
    let mut buf = vec![0u8; bytes];
    SystemRandom::new().fill(&mut buf).map_err(|_| CryptoError::Rng)?;
    Ok(URL_SAFE_NO_PAD.encode(&buf))
}

pub fn sha256(data: &[u8]) -> Vec<u8> {
    digest::digest(&digest::SHA256, data).as_ref().to_vec()
}

/// Сравнение строк за постоянное время (токены CSRF).
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn sb(k: &str) -> SecretBox {
        SecretBox::new(&Secret::new(k))
    }

    #[test]
    fn roundtrip_and_random_nonce() {
        let b = sb("0123456789abcdef0123456789abcdef");
        let a = b.encrypt(&Secret::new("svc-пароль")).unwrap();
        let c = b.encrypt(&Secret::new("svc-пароль")).unwrap();
        assert_ne!(a, c, "одинаковый текст — разные шифротексты");
        assert_eq!(b.decrypt(&a).unwrap().expose(), "svc-пароль");
        assert!(!a.windows(4).any(|w| w == b"svc-"), "открытый текст не виден в шифротексте");
    }

    #[test]
    fn wrong_key_or_tampering_fails() {
        let a = sb("0123456789abcdef0123456789abcdef").encrypt(&Secret::new("x")).unwrap();
        assert_eq!(sb("another-key-another-key-another-k").decrypt(&a).unwrap_err(), CryptoError::Decrypt);
        let mut t = a.clone();
        *t.last_mut().unwrap() ^= 1;
        assert_eq!(sb("0123456789abcdef0123456789abcdef").decrypt(&t).unwrap_err(), CryptoError::Decrypt);
        assert_eq!(sb("0123456789abcdef0123456789abcdef").decrypt(&[2, 0, 0]).unwrap_err(), CryptoError::Format);
        assert_eq!(sb("0123456789abcdef0123456789abcdef").decrypt(&[]).unwrap_err(), CryptoError::Format);
    }

    #[test]
    fn tokens() {
        let t = random_token(32).unwrap();
        assert_eq!(t.len(), 43);
        assert_ne!(t, random_token(32).unwrap());
        assert!(constant_time_eq("abc", "abc") && !constant_time_eq("abc", "abd") && !constant_time_eq("a", "ab"));
    }
}
