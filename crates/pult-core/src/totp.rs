//! Одноразовые коды по времени (RFC 6238, TOTP): второй фактор для входа из интернета.
//!
//! HMAC-SHA1, 6 цифр, шаг 30 секунд — так работают Яндекс Ключ, Google Authenticator, Microsoft
//! Authenticator, FreeOTP. Принимается код текущего шага и соседних (часы телефона расходятся), но только
//! шаг новее последнего принятого: перехваченный код второй раз не войдёт.

use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};

/// Длина шага, секунд.
pub const STEP_SECS: u64 = 30;
/// Длина секрета, байт (RFC 4226 советует не меньше 160 бит).
pub const SECRET_LEN: usize = 20;
const DIGITS: u32 = 6;
const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Шаг для момента `unix_secs`.
pub fn step_at(unix_secs: u64) -> u64 {
    unix_secs / STEP_SECS
}

/// Код шага: 6 цифр (RFC 4226, динамическое усечение).
pub fn code_at(secret: &[u8], step: u64) -> u32 {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = hmac::sign(&key, &step.to_be_bytes());
    let h = tag.as_ref();
    let off = usize::from(h[h.len() - 1] & 0x0f);
    let bin = u32::from_be_bytes([h[off] & 0x7f, h[off + 1], h[off + 2], h[off + 3]]);
    bin % 10u32.pow(DIGITS)
}

/// Проверяет код: шаг `now_step` ± 1, строго новее `last_step`. Возвращает принятый шаг.
/// Код — ровно 6 цифр; пробелы (`123 456`) допускаются.
pub fn verify(secret: &[u8], code: &str, now_step: u64, last_step: u64) -> Option<u64> {
    let digits: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    if digits.len() != DIGITS as usize || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let wanted: u32 = digits.parse().ok()?;
    let mut found = None;
    // Все три шага проверяются всегда: время ответа не подсказывает, какой из них совпал.
    for step in [now_step.saturating_sub(1), now_step, now_step + 1] {
        let ok = constant_eq(code_at(secret, step), wanted);
        if ok && step > last_step && found.is_none() {
            found = Some(step);
        }
    }
    found
}

fn constant_eq(a: u32, b: u32) -> bool {
    (a ^ b) == 0
}

/// Новый случайный секрет.
pub fn new_secret() -> Result<Vec<u8>, crate::crypto::CryptoError> {
    let mut s = vec![0u8; SECRET_LEN];
    SystemRandom::new().fill(&mut s).map_err(|_| crate::crypto::CryptoError::Rng)?;
    Ok(s)
}

/// Base32 (RFC 4648) без `=`: так секрет вводят в приложение вручную.
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((buf >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((buf << (5 - bits)) & 31) as usize]));
    }
    out
}

/// Обратно из base32; регистр, пробелы и `=` не важны.
pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.chars().filter(|c| !c.is_whitespace() && *c != '=') {
        let c = u8::try_from(c.to_ascii_uppercase()).ok()?;
        let v = ALPHABET.iter().position(|&a| a == c)?;
        buf = (buf << 5) | u32::try_from(v).ok()?;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buf >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

/// Ссылка `otpauth://` для приложения-аутентификатора.
pub fn otpauth_uri(issuer: &str, account: &str, secret_b32: &str) -> String {
    let issuer = percent(issuer);
    format!(
        "otpauth://totp/{issuer}:{}?secret={secret_b32}&issuer={issuer}&algorithm=SHA1&digits={DIGITS}&period={STEP_SECS}",
        percent(account)
    )
}

fn percent(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// Секрет из приложения B RFC 6238 (вариант SHA1).
    const RFC_SECRET: &[u8] = b"12345678901234567890";

    #[test]
    fn rfc6238_vectors() {
        // RFC даёт 8 цифр; 6 цифр — те же младшие разряды.
        for (t, code) in
            [(59u64, 287_082u32), (1_111_111_109, 81_804), (1_234_567_890, 5_924), (2_000_000_000, 279_037)]
        {
            assert_eq!(code_at(RFC_SECRET, step_at(t)), code, "t = {t}");
        }
    }

    #[test]
    fn neighbour_steps_accepted_once() {
        let now = step_at(1_234_567_890);
        let prev = format!("{:06}", code_at(RFC_SECRET, now - 1));
        let cur = format!("{:06}", code_at(RFC_SECRET, now));
        assert_eq!(verify(RFC_SECRET, &prev, now, 0), Some(now - 1), "часы телефона отстают на шаг");
        assert_eq!(verify(RFC_SECRET, &cur, now, 0), Some(now));
        assert_eq!(verify(RFC_SECRET, &cur, now, now), None, "тот же код второй раз не принимается");
        assert_eq!(verify(RFC_SECRET, &prev, now, now - 1), None);
        let far = format!("{:06}", code_at(RFC_SECRET, now - 2));
        assert_eq!(verify(RFC_SECRET, &far, now, 0), None, "старый код не принимается");
    }

    #[test]
    fn malformed_codes_rejected() {
        let now = step_at(59);
        assert_eq!(verify(RFC_SECRET, "287 082", now, 0), Some(now), "пробел внутри — можно");
        for bad in ["", "28708", "2870822", "28708a", "+87082", "２８７０８２"] {
            assert_eq!(verify(RFC_SECRET, bad, now, 0), None, "{bad:?}");
        }
    }

    #[test]
    fn base32_roundtrip_and_rfc4648() {
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_decode("mzxw 6ytb oi==").unwrap(), b"foobar");
        assert_eq!(base32_decode("MZ1"), None, "1 не из алфавита");
        let s = new_secret().unwrap();
        assert_eq!(s.len(), SECRET_LEN);
        let enc = base32_encode(&s);
        assert_eq!(enc.len(), 32);
        assert_eq!(base32_decode(&enc).unwrap(), s);
        assert_ne!(new_secret().unwrap(), s);
    }

    #[test]
    fn otpauth_uri_is_escaped() {
        assert_eq!(
            otpauth_uri("Пульт", "ivanov", "MZXW6YTBOI"),
            "otpauth://totp/%D0%9F%D1%83%D0%BB%D1%8C%D1%82:ivanov?secret=MZXW6YTBOI&issuer=%D0%9F%D1%83%D0%BB%D1%8C%D1%82&algorithm=SHA1&digits=6&period=30"
        );
        assert!(otpauth_uri("a b", "x@y", "S").contains("totp/a%20b:x%40y?"));
    }
}
