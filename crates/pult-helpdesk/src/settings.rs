//! Настройки Helpdesk (админка → Helpdesk, Почта).

use pult_core::settings::SettingsGroup;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Код закрытия: `code` хранится в заявке, `label` видят люди.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CloseCode {
    pub code: String,
    pub label: String,
}

/// Подписи кодов на языке запроса: стартовые русские подписи — по каталогу, свои — как есть.
pub fn localized(codes: Vec<CloseCode>) -> Vec<CloseCode> {
    codes.into_iter().map(|c| CloseCode { label: pult_core::l10n::tr(&c.label).into_owned(), ..c }).collect()
}

/// Стартовый список.
pub fn default_close_codes() -> Vec<CloseCode> {
    [
        ("solved", "Решено"),
        ("workaround", "Обходное решение"),
        ("not_reproduced", "Не воспроизводится"),
        ("duplicate", "Дубль"),
        ("out_of_scope", "Не наша зона"),
        ("rejected", "Отказ"),
        ("no_response", "Нет ответа"),
    ]
    .map(|(code, label)| CloseCode { code: code.into(), label: label.into() })
    .to_vec()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct HelpdeskSettings {
    /// Адрес портала для ссылок в письмах. Пусто — запомнится при первой заявке из браузера.
    pub portal_url: String,
    /// Начало и конец рабочего дня (`09:00`), пока нет календарей площадок.
    pub work_start: String,
    pub work_end: String,
    /// Рабочие дни: 1 — понедельник … 7 — воскресенье.
    pub work_days: Vec<u8>,
    /// Часовой пояс, если у заявки нет площадки.
    pub timezone: String,
    /// Через сколько рабочих дней решённая заявка закрывается сама.
    pub auto_close_days: u32,
    /// Максимальный размер вложения, МБ.
    pub max_file_mb: u32,
    /// Сети, из которых доступна форма «Не могу войти».
    pub login_help_subnets: Vec<String>,
    /// Номер для аварий, если у площадки свой не задан.
    pub emergency_phone: String,
    /// Коды закрытия; при «Решена» код обязателен.
    pub close_codes: Vec<CloseCode>,
    /// Срок SLA стоит, пока заявка «Ждёт ответа» («Отложена» — всегда).
    pub sla_pause_waiting: bool,
    /// Матрица приоритета: строки — влияние (только я, отдел, вся организация),
    /// столбцы — срочность (обычная, мешает работать, работа стоит).
    pub priority_matrix: Vec<Vec<String>>,
    /// VIP-сотрудник: приоритет на ступень выше.
    pub vip_bump: bool,
    /// «Ждёт ответа»: напоминание через столько рабочих дней (0 — не напоминать).
    pub waiting_remind_days: u32,
    /// …и решение с кодом «нет ответа» ещё через столько (0 — не решать).
    pub waiting_resolve_days: u32,
    /// Кого ещё уведомить об оценке «плохо» (руководитель IT). Пусто — суперадминистраторов.
    pub bad_rating_notify: Vec<uuid::Uuid>,
    /// Еженедельный отчёт: адреса получателей (пусто — не рассылать).
    pub weekly_report_to: Vec<String>,
}

impl Default for HelpdeskSettings {
    fn default() -> Self {
        Self {
            portal_url: String::new(),
            work_start: "09:00".into(),
            work_end: "18:00".into(),
            work_days: vec![1, 2, 3, 4, 5],
            timezone: "Europe/Moscow".into(),
            auto_close_days: 3,
            max_file_mb: pult_core::files::DEFAULT_MAX_FILE_MB,
            login_help_subnets: ["10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16", "127.0.0.0/8", "::1/128", "fc00::/7"]
                .map(String::from)
                .to_vec(),
            emergency_phone: String::new(),
            close_codes: default_close_codes(),
            sla_pause_waiting: true,
            priority_matrix: vec![
                vec!["normal".into(), "normal".into(), "high".into()],
                vec!["normal".into(), "high".into(), "critical".into()],
                vec!["high".into(), "critical".into(), "critical".into()],
            ],
            vip_bump: true,
            waiting_remind_days: 2,
            waiting_resolve_days: 3,
            bad_rating_notify: Vec::new(),
            weekly_report_to: Vec::new(),
        }
    }
}

impl SettingsGroup for HelpdeskSettings {
    const KEY: &'static str = "helpdesk";
}

impl HelpdeskSettings {
    pub fn validate(&self) -> Result<(), String> {
        crate::schedule::Schedule::from_settings(self)?;
        if !(1..=30).contains(&self.auto_close_days) {
            return Err("Автозакрытие — от 1 до 30 рабочих дней.".into());
        }
        if !(1..=100).contains(&self.max_file_mb) {
            return Err("Размер вложения — от 1 до 100 МБ.".into());
        }
        if !self.portal_url.is_empty()
            && !self.portal_url.starts_with("https://")
            && !self.portal_url.starts_with("http://")
        {
            return Err("Адрес портала начинается с https://".into());
        }
        let mut seen = std::collections::HashSet::new();
        for c in &self.close_codes {
            let ok = !c.code.is_empty()
                && c.code.len() <= 40
                && c.code.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
            if !ok {
                return Err(format!("Код закрытия «{}» — латиница в нижнем регистре, цифры и «_».", c.code));
            }
            if c.label.trim().is_empty() || !seen.insert(c.code.as_str()) {
                return Err(format!("Код закрытия «{}» повторяется или без названия.", c.code));
            }
        }
        if !self.close_codes.iter().any(|c| c.code == "no_response") {
            return Err("Код «no_response» нужен для автоматического решения по «Ждёт ответа».".into());
        }
        let rows_ok = self.priority_matrix.len() == 3
            && self
                .priority_matrix
                .iter()
                .all(|r| r.len() == 3 && r.iter().all(|p| crate::sla::PRIORITIES.contains(&p.as_str())));
        if !rows_ok {
            return Err("Матрица приоритета — 3 × 3 из low, normal, high, critical.".into());
        }
        if self.waiting_remind_days > 30 || self.waiting_resolve_days > 30 {
            return Err("Сроки напоминаний — до 30 рабочих дней.".into());
        }
        if self.weekly_report_to.iter().any(|a| !a.contains('@')) {
            return Err("Адрес для отчёта — электронная почта.".into());
        }
        for s in &self.login_help_subnets {
            s.parse::<sqlx::types::ipnetwork::IpNetwork>().map_err(|_| format!("Сеть «{s}» — нужен формат CIDR."))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        HelpdeskSettings::default().validate().unwrap();
    }

    #[test]
    fn bad_values_rejected() {
        let bad = |f: fn(&mut HelpdeskSettings)| {
            let mut s = HelpdeskSettings::default();
            f(&mut s);
            s.validate().is_err()
        };
        assert!(bad(|s| s.auto_close_days = 0));
        assert!(bad(|s| s.max_file_mb = 0));
        assert!(bad(|s| s.portal_url = "ftp://x".into()));
        assert!(bad(|s| s.login_help_subnets = vec!["офис".into()]));
        assert!(bad(|s| s.work_start = "25:00".into()));
        assert!(bad(|s| s.close_codes.push(CloseCode { code: "Решено".into(), label: "x".into() })));
        assert!(bad(|s| s.close_codes.push(CloseCode { code: "solved".into(), label: "x".into() })));
        assert!(bad(|s| s.close_codes.retain(|c| c.code != "no_response")));
    }
}
