//! Секрет, который нельзя случайно вывести: пароль AD, ключ шифрования, пароль служебной учётки.
//!
//! План, раздел 20: пароли запрещены в логах, трассировке, панике и ответах API.
//! `Debug` и `Display` печатают заглушку, `Serialize` не реализован, значение
//! доступно только через явный вызов [`Secret::expose`].

use std::fmt;

/// Строка-секрет. Значение затирается при освобождении памяти.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Оборачивает значение.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Даёт доступ к значению. Каждый вызов — осознанное место, где секрет используется.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Пустой ли секрет.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // `zeroize` гарантирует, что запись нулей не будет выброшена оптимизатором.
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

impl<'de> serde::Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        String::deserialize(d).map(Secret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_hide_value() {
        let s = Secret::new("P@ssw0rd-very-secret");
        assert_eq!(format!("{s:?}"), "Secret(***)");
        assert_eq!(format!("{s}"), "***");
        assert!(!format!("{s:?} {s}").contains("P@ssw0rd"));
    }

    #[test]
    fn hidden_inside_structs() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Login {
            user: String,
            password: Secret,
        }
        let l = Login { user: "ivanov".into(), password: Secret::new("hunter2") };
        let out = format!("{l:?}");
        assert!(out.contains("ivanov"));
        assert!(!out.contains("hunter2"));
    }

    #[test]
    fn expose_returns_value() {
        assert_eq!(Secret::new("x").expose(), "x");
    }
}
