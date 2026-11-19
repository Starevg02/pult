//! Расписания в формате cron (5 полей: минута, час, день месяца, месяц, день недели).
//!
//! Поддерживается: `*`, числа, диапазоны `a-b`, шаг `*/n` и `a-b/n`, списки через запятую,
//! имена месяцев и дней (`jan`, `mon`), день недели 0–7 (0 и 7 — воскресенье),
//! сокращения `@hourly`, `@daily`, `@weekly`, `@monthly`, `@quarterly`, `@yearly`.
//! Если заданы и день месяца, и день недели (оба не `*`), срабатывает любой из них — как в Vixie cron.
//!
//! Время — в фиксированном смещении от UTC (`+03:00` по умолчанию): в России нет перехода
//! на летнее время, поэтому часовой пояс с правилами не нужен.

use time::{Date, Month, OffsetDateTime, Time, UtcOffset};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CronError {
    #[error("в расписании «{0}» должно быть 5 полей: минута час день месяц день_недели")]
    FieldCount(String),
    #[error("поле «{field}» расписания: «{value}» — {reason}")]
    Field { field: &'static str, value: String, reason: &'static str },
    #[error("расписание «{0}» никогда не срабатывает")]
    Never(String),
    #[error("смещение «{0}» — ожидается вида +03:00")]
    Offset(String),
}

/// Разобранное расписание.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    source: String,
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    weekdays: u64,
    /// День месяца и день недели заданы оба — тогда «или».
    day_or_weekday: bool,
}

struct Field {
    name: &'static str,
    min: u8,
    max: u8,
    names: &'static [&'static str],
}

const MINUTE: Field = Field { name: "минута", min: 0, max: 59, names: &[] };
const HOUR: Field = Field { name: "час", min: 0, max: 23, names: &[] };
const DAY: Field = Field { name: "день месяца", min: 1, max: 31, names: &[] };
const MONTH: Field = Field {
    name: "месяц",
    min: 1,
    max: 12,
    names: &["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"],
};
const WEEKDAY: Field =
    Field { name: "день недели", min: 0, max: 7, names: &["sun", "mon", "tue", "wed", "thu", "fri", "sat"] };

impl Schedule {
    pub fn parse(expr: &str) -> Result<Self, CronError> {
        let expanded = match expr.trim() {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            "@quarterly" => "0 0 1 1,4,7,10 *",
            "@yearly" | "@annually" => "0 0 1 1 *",
            other => other,
        };
        let parts: Vec<&str> = expanded.split_whitespace().collect();
        let [m, h, d, mo, wd] = parts.as_slice() else {
            return Err(CronError::FieldCount(expr.to_string()));
        };
        let mut weekdays = parse_field(wd, &WEEKDAY)?;
        if weekdays & (1 << 7) != 0 {
            weekdays = (weekdays & !(1 << 7)) | 1;
        }
        Ok(Self {
            source: expr.trim().to_string(),
            minutes: parse_field(m, &MINUTE)?,
            hours: parse_field(h, &HOUR)?,
            days: parse_field(d, &DAY)?,
            months: parse_field(mo, &MONTH)?,
            weekdays,
            day_or_weekday: !d.starts_with('*') && !wd.starts_with('*'),
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    fn day_matches(&self, date: Date) -> bool {
        let by_day = bit(self.days, date.day());
        let by_weekday = bit(self.weekdays, date.weekday().number_days_from_sunday());
        if self.day_or_weekday { by_day || by_weekday } else { by_day && by_weekday }
    }

    /// Первое срабатывание строго после `after`, в смещении `offset`. `None` — не нашлось за 5 лет.
    pub fn next_after(&self, after: OffsetDateTime, offset: UtcOffset) -> Option<OffsetDateTime> {
        let t = after.to_offset(offset);
        let mut t = t.replace_time(Time::from_hms(t.hour(), t.minute(), 0).ok()?) + time::Duration::MINUTE;
        let limit = t.year() + 5;
        while t.year() <= limit {
            if !bit(self.months, u8::from(t.month())) {
                let (y, m) = if t.month() == Month::December {
                    (t.year() + 1, Month::January)
                } else {
                    (t.year(), t.month().next())
                };
                t = Date::from_calendar_date(y, m, 1).ok()?.with_time(Time::MIDNIGHT).assume_offset(offset);
            } else if !self.day_matches(t.date()) {
                t = t.date().next_day()?.with_time(Time::MIDNIGHT).assume_offset(offset);
            } else if !bit(self.hours, t.hour()) {
                t = t.replace_time(Time::from_hms(t.hour(), 0, 0).ok()?) + time::Duration::HOUR;
            } else if !bit(self.minutes, t.minute()) {
                t += time::Duration::MINUTE;
            } else {
                return Some(t);
            }
        }
        None
    }

    /// Последнее срабатывание в полуинтервале `(from, to]` — для пропущенного запуска «один раз».
    pub fn latest_in(&self, from: OffsetDateTime, to: OffsetDateTime, offset: UtcOffset) -> Option<OffsetDateTime> {
        let mut last = None;
        let mut cursor = from;
        while let Some(next) = self.next_after(cursor, offset) {
            if next > to {
                break;
            }
            last = Some(next);
            cursor = next;
        }
        last
    }
}

fn bit(mask: u64, n: u8) -> bool {
    mask & (1u64 << n) != 0
}

fn parse_field(s: &str, f: &Field) -> Result<u64, CronError> {
    let err = |reason| CronError::Field { field: f.name, value: s.to_string(), reason };
    let mut mask = 0u64;
    for item in s.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((r, st)) => (r, st.parse::<u8>().ok().filter(|&n| n > 0).ok_or_else(|| err("неверный шаг"))?),
            None => (item, 1),
        };
        let (lo, hi) = if range == "*" {
            (f.min, f.max)
        } else if let Some((a, b)) = range.split_once('-') {
            (value(a, f).ok_or_else(|| err("неверное значение"))?, value(b, f).ok_or_else(|| err("неверное значение"))?)
        } else {
            let v = value(range, f).ok_or_else(|| err("неверное значение"))?;
            // `5/15` — с 5 до конца диапазона с шагом 15.
            (v, if step > 1 { f.max } else { v })
        };
        if lo > hi {
            return Err(err("начало диапазона больше конца"));
        }
        let mut v = lo;
        while v <= hi {
            mask |= 1 << v;
            v = match v.checked_add(step) {
                Some(n) => n,
                None => break,
            };
        }
    }
    Ok(mask)
}

fn value(s: &str, f: &Field) -> Option<u8> {
    let v = match s.parse::<u8>() {
        Ok(n) => n,
        Err(_) => {
            let i = f.names.iter().position(|n| n.eq_ignore_ascii_case(s))?;
            u8::try_from(i).ok()? + if f.min == 1 { 1 } else { 0 }
        }
    };
    (f.min..=f.max).contains(&v).then_some(v)
}

/// Смещение вида `+03:00`, `-05:30`, `Z`.
pub fn parse_offset(s: &str) -> Result<UtcOffset, CronError> {
    let bad = || CronError::Offset(s.to_string());
    if s.eq_ignore_ascii_case("z") || s == "+00:00" {
        return Ok(UtcOffset::UTC);
    }
    let (sign, rest) = match s.as_bytes().first() {
        Some(b'+') => (1i8, &s[1..]),
        Some(b'-') => (-1i8, &s[1..]),
        _ => return Err(bad()),
    };
    let (h, m) = rest.split_once(':').ok_or_else(bad)?;
    let (h, m): (i8, i8) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
    if !(0..=14).contains(&h) || !(0..60).contains(&m) {
        return Err(bad());
    }
    UtcOffset::from_hms(sign * h, sign * m, 0).map_err(|_| bad())
}

#[cfg(test)]
mod tests {
    use super::*;

    const MSK: UtcOffset = match UtcOffset::from_hms(3, 0, 0) {
        Ok(o) => o,
        Err(_) => UtcOffset::UTC,
    };

    /// Местное время МСК → момент.
    fn msk(y: i32, mo: u8, d: u8, h: u8, mi: u8) -> OffsetDateTime {
        let date = Date::from_calendar_date(y, Month::try_from(mo).unwrap(), d).unwrap();
        date.with_time(Time::from_hms(h, mi, 0).unwrap()).assume_offset(MSK)
    }

    fn next(expr: &str, from: OffsetDateTime) -> OffsetDateTime {
        Schedule::parse(expr).unwrap().next_after(from, MSK).unwrap()
    }

    #[test]
    fn daily_at_local_time() {
        // 03:17 МСК — это 00:17 UTC; после 03:17 — уже следующий день.
        assert_eq!(next("17 3 * * *", msk(2026, 11, 3, 1, 0)), msk(2026, 11, 3, 3, 17));
        assert_eq!(next("17 3 * * *", msk(2026, 11, 3, 3, 17)), msk(2026, 11, 4, 3, 17));
        assert_eq!(next("17 3 * * *", msk(2026, 11, 3, 3, 17)).to_offset(UtcOffset::UTC).hour(), 0);
    }

    #[test]
    fn steps_ranges_lists() {
        assert_eq!(next("*/15 * * * *", msk(2026, 11, 3, 10, 7)), msk(2026, 11, 3, 10, 15));
        assert_eq!(next("*/15 * * * *", msk(2026, 11, 3, 10, 59)), msk(2026, 11, 3, 11, 0));
        assert_eq!(next("0 9-18/3 * * *", msk(2026, 11, 3, 13, 0)), msk(2026, 11, 3, 15, 0));
        assert_eq!(next("5/20 * * * *", msk(2026, 11, 3, 10, 26)), msk(2026, 11, 3, 10, 45));
        assert_eq!(next("0 8 * * mon-fri", msk(2026, 11, 6, 9, 0)), msk(2026, 11, 9, 8, 0)); // пт → пн
    }

    #[test]
    fn month_and_year_rollover() {
        assert_eq!(next("@quarterly", msk(2026, 11, 3, 0, 0)), msk(2027, 1, 1, 0, 0));
        assert_eq!(next("0 9 31 * *", msk(2026, 11, 3, 0, 0)), msk(2026, 12, 31, 9, 0)); // в ноябре 30 дней
        assert_eq!(next("0 0 29 2 *", msk(2026, 3, 1, 0, 0)), msk(2028, 2, 29, 0, 0)); // високосный
    }

    #[test]
    fn day_of_month_or_weekday() {
        // 1-го числа ИЛИ по понедельникам: от вт 03.11 ближайший — пн 09.11, а не 01.12.
        assert_eq!(next("0 9 1 * 1", msk(2026, 11, 3, 12, 0)), msk(2026, 11, 9, 9, 0));
        // День недели 7 = воскресенье.
        assert_eq!(next("0 0 * * 7", msk(2026, 11, 3, 0, 0)), msk(2026, 11, 8, 0, 0));
    }

    #[test]
    fn never_and_bad_expressions() {
        assert_eq!(Schedule::parse("0 0 30 2 *").unwrap().next_after(msk(2026, 1, 1, 0, 0), MSK), None);
        for bad in [
            "",
            "* * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "*/0 * * * *",
            "5-1 * * * *",
            "x * * * *",
        ] {
            assert!(Schedule::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn latest_missed_slot_only() {
        let s = Schedule::parse("*/10 * * * *").unwrap();
        // Сервер стоял с 10:00 до 10:45: из 10:10…10:40 запускаем только 10:40.
        assert_eq!(s.latest_in(msk(2026, 11, 3, 10, 0), msk(2026, 11, 3, 10, 45), MSK), Some(msk(2026, 11, 3, 10, 40)));
        // Граница справа включена, слева — нет.
        assert_eq!(s.latest_in(msk(2026, 11, 3, 10, 0), msk(2026, 11, 3, 10, 10), MSK), Some(msk(2026, 11, 3, 10, 10)));
        assert_eq!(s.latest_in(msk(2026, 11, 3, 10, 0), msk(2026, 11, 3, 10, 9), MSK), None);
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_offset("+03:00").unwrap(), MSK);
        assert_eq!(parse_offset("Z").unwrap(), UtcOffset::UTC);
        assert_eq!(parse_offset("-05:30").unwrap(), UtcOffset::from_hms(-5, -30, 0).unwrap());
        for bad in ["3", "+3", "+25:00", "MSK", ""] {
            assert!(parse_offset(bad).is_err(), "{bad:?}");
        }
    }
}
