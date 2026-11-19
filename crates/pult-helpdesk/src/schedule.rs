//! Рабочее время: график по дням недели, праздники и переносы из календаря
//! площадки. Нет календаря — общий график из настроек Helpdesk.
//! Расчёты — на местном времени площадки; переводы в UTC делает PostgreSQL, у него есть база поясов.

use crate::settings::HelpdeskSettings;
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool, Postgres, Transaction};
use std::collections::BTreeMap;
use time::{Date, Duration, OffsetDateTime, PrimitiveDateTime, Time};
use utoipa::ToSchema;
use uuid::Uuid;

/// Рабочие часы одного дня: с `start` до `end`.
type Hours = Option<(Time, Time)>;

/// Сколько дней вперёд ищем рабочее время: дальше — явно ошибка календаря.
const HORIZON_DAYS: u32 = 800;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    /// Индекс — день недели, 1 — понедельник … 7 — воскресенье.
    week: [Hours; 8],
    /// Праздники (`None`) и особые дни (перенос, сокращённый день).
    exceptions: BTreeMap<Date, Hours>,
}

pub fn parse_time(s: &str) -> Result<Time, String> {
    let bad = || format!("Время «{s}» — нужен формат ЧЧ:ММ.");
    let (h, m) = s.trim().split_once(':').ok_or_else(bad)?;
    Time::from_hms(h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?, 0).map_err(|_| bad())
}

/// Строка графика календаря.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DayHours {
    /// 1 — понедельник … 7 — воскресенье.
    pub day: u8,
    pub start: String,
    pub end: String,
}

fn span(start: &str, end: &str) -> Result<(Time, Time), String> {
    let (s, e) = (parse_time(start)?, parse_time(end)?);
    if s >= e {
        return Err("Начало рабочего дня должно быть раньше конца.".into());
    }
    Ok((s, e))
}

impl Schedule {
    pub fn from_settings(s: &HelpdeskSettings) -> Result<Self, String> {
        let hours = span(&s.work_start, &s.work_end)?;
        let mut week = [None; 8];
        for d in &s.work_days {
            *week.get_mut(usize::from(*d)).filter(|_| *d >= 1).ok_or("Дни недели — от 1 до 7.")? = Some(hours);
        }
        Self::new(week, BTreeMap::new())
    }

    pub fn from_week(rows: &[DayHours]) -> Result<Self, String> {
        let mut week = [None; 8];
        for r in rows {
            let slot = week.get_mut(usize::from(r.day)).filter(|_| r.day >= 1).ok_or("Дни недели — от 1 до 7.")?;
            if slot.is_some() {
                return Err("День недели указан дважды.".into());
            }
            *slot = Some(span(&r.start, &r.end)?);
        }
        Self::new(week, BTreeMap::new())
    }

    fn new(week: [Hours; 8], exceptions: BTreeMap<Date, Hours>) -> Result<Self, String> {
        if week.iter().all(Option::is_none) {
            return Err("Укажите хотя бы один рабочий день.".into());
        }
        Ok(Self { week, exceptions })
    }

    pub fn with_exceptions(mut self, days: impl IntoIterator<Item = (Date, Hours)>) -> Self {
        self.exceptions.extend(days);
        self
    }

    fn hours(&self, d: Date) -> Hours {
        match self.exceptions.get(&d) {
            Some(h) => *h,
            None => self.week[usize::from(d.weekday().number_from_monday())],
        }
    }

    fn working(&self, d: Date) -> bool {
        self.hours(d).is_some()
    }

    pub fn is_open(&self, t: PrimitiveDateTime) -> bool {
        self.hours(t.date()).is_some_and(|(s, e)| t.time() >= s && t.time() < e)
    }

    /// Начало ближайшего рабочего времени (сейчас открыто — само `t`).
    pub fn next_open(&self, t: PrimitiveDateTime) -> PrimitiveDateTime {
        if self.is_open(t) {
            return t;
        }
        let mut d = t.date();
        for _ in 0..HORIZON_DAYS {
            if let Some((s, _)) = self.hours(d)
                && (d != t.date() || t.time() < s)
            {
                return PrimitiveDateTime::new(d, s);
            }
            d = d.next_day().unwrap_or(d);
        }
        t
    }

    /// Через `n` рабочих дней — в то же время суток.
    pub fn add_work_days(&self, t: PrimitiveDateTime, n: u32) -> PrimitiveDateTime {
        let mut d = t.date();
        let mut left = n;
        let mut guard = 0;
        while left > 0 && guard < HORIZON_DAYS {
            d = d.next_day().unwrap_or(d);
            guard += 1;
            if self.working(d) {
                left -= 1;
            }
        }
        PrimitiveDateTime::new(d, t.time())
    }

    /// Момент, когда истекут `minutes` рабочих минут, отсчитанных от `t`.
    pub fn add_work_minutes(&self, t: PrimitiveDateTime, minutes: i64) -> PrimitiveDateTime {
        let mut left = Duration::minutes(minutes.max(0));
        let mut d = t.date();
        for _ in 0..HORIZON_DAYS {
            if let Some((s, e)) = self.hours(d) {
                let from = if d == t.date() { t.time().max(s) } else { s };
                if from < e {
                    let avail = e - from;
                    if left <= avail {
                        return PrimitiveDateTime::new(d, from) + left;
                    }
                    left -= avail;
                }
            }
            d = d.next_day().unwrap_or(d);
        }
        t + Duration::minutes(minutes)
    }

    /// Рабочих минут между `a` и `b` (0, если `b` раньше).
    pub fn work_minutes_between(&self, a: PrimitiveDateTime, b: PrimitiveDateTime) -> i64 {
        if b <= a {
            return 0;
        }
        let mut total = Duration::ZERO;
        let mut d = a.date();
        for _ in 0..HORIZON_DAYS {
            if d > b.date() {
                break;
            }
            if let Some((s, e)) = self.hours(d) {
                let from = if d == a.date() { a.time().max(s) } else { s };
                let to = if d == b.date() { b.time().min(e) } else { e };
                if from < to {
                    total += to - from;
                }
            }
            d = d.next_day().unwrap_or(d);
        }
        total.whole_minutes()
    }
}

/// Пояс, известный PostgreSQL; иначе UTC — чтобы ошибка не оборвала транзакцию.
pub async fn known_tz(db: impl PgExecutor<'_>, tz: &str) -> Result<String, sqlx::Error> {
    let known: Option<String> =
        sqlx::query_scalar("SELECT name FROM pg_timezone_names WHERE name = $1").bind(tz).fetch_optional(db).await?;
    if known.is_none() {
        tracing::warn!(tz, "неизвестный часовой пояс — считаю по UTC");
    }
    Ok(known.unwrap_or_else(|| "UTC".into()))
}

/// Сейчас по местному времени пояса. Неизвестный пояс — UTC, чтобы заявка всё равно создалась.
pub async fn local_now(db: &PgPool, tz: &str) -> Result<(PrimitiveDateTime, String), sqlx::Error> {
    let tz = known_tz(db, tz).await?;
    let t = sqlx::query_scalar::<_, PrimitiveDateTime>("SELECT now() AT TIME ZONE $1").bind(&tz).fetch_one(db).await?;
    Ok((t, tz))
}

pub async fn to_local(db: impl PgExecutor<'_>, t: OffsetDateTime, tz: &str) -> Result<PrimitiveDateTime, sqlx::Error> {
    sqlx::query_scalar("SELECT $1::timestamptz AT TIME ZONE $2").bind(t).bind(tz).fetch_one(db).await
}

pub async fn to_utc(db: impl PgExecutor<'_>, t: PrimitiveDateTime, tz: &str) -> Result<OffsetDateTime, sqlx::Error> {
    sqlx::query_scalar("SELECT $1::timestamp AT TIME ZONE $2").bind(t).bind(tz).fetch_one(db).await
}

/// Вне рабочего времени: когда возьмут и куда звонить при аварии.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct AfterHours {
    /// Начало ближайшего рабочего дня, местное время: `2026-11-10 09:00`.
    pub next_open: String,
    pub emergency_phone: Option<String>,
}

fn format_local(t: PrimitiveDateTime) -> String {
    format!("{} {:02}:{:02}", t.date(), t.hour(), t.minute())
}

/// Пояс и аварийный номер площадки (или общие из настроек).
pub async fn site_tz(
    db: &PgPool,
    site: Option<Uuid>,
    s: &HelpdeskSettings,
) -> Result<(String, Option<String>), sqlx::Error> {
    let row: Option<(String, Option<String>)> = match site {
        Some(id) => {
            sqlx::query_as("SELECT timezone, emergency_phone FROM sites WHERE id = $1")
                .bind(id)
                .fetch_optional(db)
                .await?
        }
        None => None,
    };
    let (tz, phone) = row.unwrap_or_else(|| (s.timezone.clone(), None));
    let phone = phone.filter(|p| !p.trim().is_empty());
    Ok((tz, phone.or_else(|| Some(s.emergency_phone.clone()).filter(|p| !p.trim().is_empty()))))
}

/// График площадки и её пояс: календарь площадки, иначе общий график из настроек.
pub async fn for_site(
    tx: &mut Transaction<'_, Postgres>,
    site: Option<Uuid>,
    s: &HelpdeskSettings,
) -> Result<(Schedule, String), sqlx::Error> {
    let row: Option<(String, Option<Uuid>, Option<serde_json::Value>)> =
        match site {
            Some(id) => sqlx::query_as(
                "SELECT s.timezone, s.calendar_id, c.hours FROM sites s LEFT JOIN calendars c ON c.id = s.calendar_id
                  WHERE s.id = $1",
            )
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?,
            None => None,
        };
    let (tz, calendar, hours) = row.unwrap_or_else(|| (s.timezone.clone(), None, None));
    let tz = known_tz(&mut **tx, &tz).await?;
    let fallback = || {
        Schedule::from_settings(s)
            .unwrap_or_else(|_| Schedule::from_settings(&HelpdeskSettings::default()).expect("график по умолчанию"))
    };
    let Some(cal) = calendar else { return Ok((fallback(), tz)) };
    let week: Vec<DayHours> = hours.and_then(|h| serde_json::from_value(h).ok()).unwrap_or_default();
    let base = Schedule::from_week(&week).unwrap_or_else(|_| fallback());
    // Год назад — тоже: сроки и паузы SLA считаются от даты создания старой заявки, а не от сегодня.
    let days: Vec<(Date, Option<Time>, Option<Time>)> = sqlx::query_as(
        "SELECT day, start_time, end_time FROM calendar_days
          WHERE calendar_id = $1 AND day BETWEEN current_date - 400 AND current_date + 800",
    )
    .bind(cal)
    .fetch_all(&mut **tx)
    .await?;
    let ex = days.into_iter().map(|(d, s, e)| (d, s.zip(e).filter(|(s, e)| s < e)));
    Ok((base.with_exceptions(ex), tz))
}

pub async fn after_hours(
    db: &PgPool,
    site: Option<Uuid>,
    s: &HelpdeskSettings,
) -> Result<Option<AfterHours>, sqlx::Error> {
    let (_, emergency_phone) = site_tz(db, site, s).await?;
    let mut tx = db.begin().await?;
    let (sch, tz) = for_site(&mut tx, site, s).await?;
    let now: PrimitiveDateTime =
        sqlx::query_scalar("SELECT now() AT TIME ZONE $1").bind(&tz).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    if sch.is_open(now) {
        return Ok(None);
    }
    Ok(Some(AfterHours { next_open: format_local(sch.next_open(now)), emergency_phone }))
}

/// Когда закроется решённая заявка.
pub async fn auto_close_at(
    db: &PgPool,
    site: Option<Uuid>,
    s: &HelpdeskSettings,
) -> Result<OffsetDateTime, sqlx::Error> {
    let mut tx = db.begin().await?;
    let at = add_days_from_now(&mut tx, site, s, s.auto_close_days).await?;
    tx.commit().await?;
    Ok(at)
}

/// Сейчас + `n` рабочих дней по графику площадки, в UTC.
pub async fn add_days_from_now(
    tx: &mut Transaction<'_, Postgres>,
    site: Option<Uuid>,
    s: &HelpdeskSettings,
    n: u32,
) -> Result<OffsetDateTime, sqlx::Error> {
    let (sch, tz) = for_site(tx, site, s).await?;
    let now: PrimitiveDateTime =
        sqlx::query_scalar("SELECT now() AT TIME ZONE $1").bind(&tz).fetch_one(&mut **tx).await?;
    to_utc(&mut **tx, sch.add_work_days(now, n), &tz).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::{date, datetime, time};

    fn sch() -> Schedule {
        Schedule::from_settings(&HelpdeskSettings::default()).unwrap()
    }

    #[test]
    fn open_hours() {
        // 9 ноября 2026 — понедельник.
        assert!(sch().is_open(datetime!(2026-11-09 09:00)));
        assert!(sch().is_open(datetime!(2026-11-09 17:59)));
        assert!(!sch().is_open(datetime!(2026-11-09 18:00)));
        assert!(!sch().is_open(datetime!(2026-11-09 08:59)));
        assert!(!sch().is_open(datetime!(2026-11-14 12:00)), "суббота");
    }

    #[test]
    fn next_open_skips_evening_and_weekend() {
        let s = sch();
        assert_eq!(s.next_open(datetime!(2026-11-09 07:30)), datetime!(2026-11-09 09:00), "утром — сегодня");
        assert_eq!(s.next_open(datetime!(2026-11-09 19:00)), datetime!(2026-11-10 09:00), "вечером — завтра");
        assert_eq!(s.next_open(datetime!(2026-11-13 19:00)), datetime!(2026-11-16 09:00), "пятница → понедельник");
        assert_eq!(s.next_open(datetime!(2026-11-14 10:00)), datetime!(2026-11-16 09:00), "суббота → понедельник");
        assert_eq!(s.next_open(datetime!(2026-11-09 12:00)), datetime!(2026-11-09 12:00), "сейчас открыто");
    }

    #[test]
    fn three_working_days_skip_weekend() {
        let s = sch();
        assert_eq!(s.add_work_days(datetime!(2026-11-09 15:00), 3), datetime!(2026-11-12 15:00), "пн → чт");
        assert_eq!(s.add_work_days(datetime!(2026-11-12 15:00), 3), datetime!(2026-11-17 15:00), "чт → вт");
        assert_eq!(s.add_work_days(datetime!(2026-11-14 10:00), 3), datetime!(2026-11-18 10:00), "из субботы");
    }

    #[test]
    fn invalid_schedules() {
        let with = |f: fn(&mut HelpdeskSettings)| {
            let mut s = HelpdeskSettings::default();
            f(&mut s);
            Schedule::from_settings(&s).is_err()
        };
        assert!(with(|s| s.work_days = vec![]));
        assert!(with(|s| s.work_days = vec![0]));
        assert!(with(|s| s.work_days = vec![8]));
        assert!(with(|s| s.work_end = "08:00".into()));
        let row = |day, start: &str, end: &str| DayHours { day, start: start.into(), end: end.into() };
        assert!(Schedule::from_week(&[row(1, "09:00", "18:00"), row(1, "10:00", "12:00")]).is_err(), "дубль дня");
        assert!(Schedule::from_week(&[]).is_err());
    }

    /// Пример: пятница 17:50, 8 рабочих часов при графике 9–18 — понедельник 16:50.
    #[test]
    fn sla_friday_evening() {
        let s = sch();
        // 13 ноября 2026 — пятница.
        assert_eq!(s.add_work_minutes(datetime!(2026-11-13 17:50), 8 * 60), datetime!(2026-11-16 16:50));
        // Понедельник — праздник: срок во вторник.
        let h = sch().with_exceptions([(date!(2026 - 11 - 16), None)]);
        assert_eq!(h.add_work_minutes(datetime!(2026-11-13 17:50), 8 * 60), datetime!(2026-11-17 16:50));
        // Перенос: рабочая суббота.
        let w = sch().with_exceptions([(date!(2026 - 11 - 14), Some((time!(9:00), time!(18:00))))]);
        assert_eq!(w.add_work_minutes(datetime!(2026-11-13 17:50), 8 * 60), datetime!(2026-11-14 16:50));
    }

    #[test]
    fn work_minutes() {
        let s = sch();
        assert_eq!(s.add_work_minutes(datetime!(2026-11-09 07:00), 30), datetime!(2026-11-09 09:30), "до начала дня");
        assert_eq!(s.add_work_minutes(datetime!(2026-11-09 12:00), 0), datetime!(2026-11-09 12:00));
        assert_eq!(s.work_minutes_between(datetime!(2026-11-13 17:50), datetime!(2026-11-16 16:50)), 480);
        assert_eq!(s.work_minutes_between(datetime!(2026-11-09 12:00), datetime!(2026-11-09 11:00)), 0);
        assert_eq!(s.work_minutes_between(datetime!(2026-11-14 10:00), datetime!(2026-11-15 10:00)), 0, "выходные");
        let week = Schedule::from_week(&[
            DayHours { day: 1, start: "08:00".into(), end: "17:00".into() },
            DayHours { day: 5, start: "08:00".into(), end: "16:00".into() },
        ])
        .unwrap();
        assert!(
            week.is_open(datetime!(2026-11-13 15:30)) && !week.is_open(datetime!(2026-11-13 16:30)),
            "пятница короче"
        );
        assert!(!week.is_open(datetime!(2026-11-10 10:00)), "вторник выходной");
    }
}
