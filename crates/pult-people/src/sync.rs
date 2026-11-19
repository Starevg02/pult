//! Синхронизация AD → `ad_accounts` → сотрудники и пользователи.
//!
//! - Полная: все учётки по фильтру; кого нет — `is_missing`.
//! - Инкрементальная: по `uSNChanged` с того же DC; DC сменился — полная.
//! - Учётка отключена или пропала → пользователь неактивен, сеансы закрыты, сотрудник в архиве
//!   (`archived_reason = ad_disabled`); снова включена → всё возвращается.
//! - Связывание: по email — сразу; по ФИО — в очередь подтверждения; без пары — новый сотрудник
//!   (только для учёток с именем и фамилией: служебные учётки в справочник не попадают).

use crate::domain::{AdHandle, DomainRow};
use pult_ad::{AdAccount, AdError, DcPool};
use pult_core::{
    ApiError, Core,
    audit::{self, Actor, Entry, Source},
    notify,
    settings::{self, SettingsGroup},
};
use pult_phonebook::persons::{FieldSource, field_sources};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Full,
    Incremental,
}

/// Настройки связывания (админка → «Справочник»).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct LinkSettings {
    /// Новая учётка без пары создаёт сотрудника в справочнике (по умолчанию да).
    pub auto_create_persons: bool,
}

impl Default for LinkSettings {
    fn default() -> Self {
        Self { auto_create_persons: true }
    }
}

impl SettingsGroup for LinkSettings {
    const KEY: &'static str = "people_link";
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Summary {
    pub kind: Option<Kind>,
    pub controller: String,
    pub seen: usize,
    pub added: usize,
    pub updated: usize,
    pub disabled: usize,
    pub enabled: usize,
    pub linked: usize,
    pub candidates: usize,
    pub persons_created: usize,
}

#[derive(sqlx::FromRow)]
struct Prev {
    active: bool,
}

/// Состояние инкрементальной синхронизации в `ad_domains.sync_state`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct SyncState {
    controller: String,
    usn: u64,
}

/// Сколько учёток одна синхронизация может отключить без подтверждения администратора.
/// Больше — скорее сломан фильтр или пропали права служебной учётки (AD отдаёт только видимые
/// записи), чем массовое увольнение: отключение всех закрыло бы вход и администраторам.
pub fn mass_disable(active: i64, disabling: i64) -> bool {
    disabling > 0 && (disabling >= active || disabling > (active / 5).max(20))
}

/// `allow_mass_disable` — администратор подтвердил, что массовое отключение ожидаемо.
pub async fn run(core: &Core, ad: &AdHandle, requested: Kind, allow_mass_disable: bool) -> Result<Summary, ApiError> {
    let Some((domain, pool)) = ad.pool(core).await? else {
        return Err(ApiError::bad_request("ad_not_configured", "Домен AD не настроен."));
    };
    // Одна синхронизация домена за раз (несколько копий сервера, кнопка в админке и расписание).
    // Блокировка — на время транзакции: если задачу оборвут по тайм-ауту, транзакция откатится и
    // блокировка снимется сама (сеансовая осталась бы на соединении в пуле и молча останавливала
    // все следующие синхронизации).
    let mut lock = core.db.begin().await?;
    let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(7231020)").fetch_one(&mut *lock).await?;
    if !got {
        return Err(ApiError::bad_request("sync_running", "Синхронизация уже идёт."));
    }
    let res = run_locked(core, &domain, &pool, requested, allow_mass_disable).await;
    lock.rollback().await?;
    if let Err(ApiError::Internal(detail)) = &res {
        record_failure(core, domain.id, detail).await;
    }
    res
}

/// Сбой при записи результата: не оставлять запуск «выполняется» и сообщить суперадминистраторам.
async fn record_failure(core: &Core, domain: Uuid, detail: &str) {
    tracing::error!(error = detail, "синхронизация AD не удалась");
    let r = sqlx::query("UPDATE ad_sync_runs SET status = 'error', error = $2, finished_at = now() WHERE domain_id = $1 AND status = 'running'")
        .bind(domain)
        .bind(detail.chars().take(500).collect::<String>())
        .execute(&core.db)
        .await;
    let n =
        notify::notify_superadmins(&core.db, notify::kind::AD_SYNC_FAILED, serde_json::json!({"error": detail})).await;
    if let Err(e) = r.map(|_| ()).and(n.map(|_| ())) {
        tracing::error!(error = %e, "сбой синхронизации не записан");
    }
}

async fn run_locked(
    core: &Core,
    domain: &DomainRow,
    pool: &DcPool,
    requested: Kind,
    allow_mass_disable: bool,
) -> Result<Summary, ApiError> {
    let state: SyncState = serde_json::from_value(domain.sync_state.clone()).unwrap_or_default();
    let kind = match requested {
        Kind::Incremental if state.usn > 0 && state.controller == pool.current().to_string() => Kind::Incremental,
        _ => Kind::Full,
    };
    let kind_s = if kind == Kind::Full { "full" } else { "incremental" };
    let run_id: Uuid = sqlx::query_scalar("INSERT INTO ad_sync_runs (domain_id, kind) VALUES ($1, $2) RETURNING id")
        .bind(domain.id)
        .bind(kind_s)
        .fetch_one(&core.db)
        .await?;
    // Начало — по часам БД: `synced_at` пишет она же (часы приложения могут отличаться).
    let started: time::OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&core.db).await?;
    let batch = match kind {
        Kind::Full => pool.full_sync(500).await,
        Kind::Incremental => pool.incremental_sync(state.usn, 500).await,
    };
    let batch = match batch {
        Ok(b) => b,
        Err(e) => {
            fail_run(core, run_id, &e.to_string()).await?;
            return Err(match e {
                AdError::Unavailable(_) => ApiError::Unavailable {
                    code: "ad_unavailable",
                    message: "Нет связи с доменом.".into(),
                },
                other => ApiError::bad_request("ad_sync_failed", other.to_string()),
            });
        }
    };
    // Инкрементальная пошла на другой DC (переключение во время прохода) — USN не годится, нужна полная.
    if kind == Kind::Incremental && batch.controller != state.controller {
        sqlx::query("UPDATE ad_sync_runs SET status = 'error', error = 'контроллер сменился — нужна полная', finished_at = now() WHERE id = $1")
            .bind(run_id)
            .execute(&core.db)
            .await?;
        return Box::pin(run_locked(core, domain, pool, Kind::Full, allow_mass_disable)).await;
    }

    let mut s = Summary {
        kind: Some(kind),
        controller: batch.controller.clone(),
        seen: batch.accounts.len(),
        ..Default::default()
    };
    let mut tx = core.db.begin().await?;
    let active: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ad_accounts WHERE domain_id = $1 AND is_enabled AND NOT is_missing")
            .bind(domain.id)
            .fetch_one(&mut *tx)
            .await?;
    let sources = field_sources(&mut *tx).await?;
    let link: LinkSettings = settings::load(&mut *tx).await?;
    let mut changed: Vec<(Uuid, bool, bool, bool)> = Vec::new(); // (учётка, была активна, стала активна, новая)
    for acc in &batch.accounts {
        let (id, prev_active, inserted) = upsert(&mut tx, domain.id, acc).await?;
        if inserted {
            s.added += 1;
        } else {
            s.updated += 1;
        }
        changed.push((id, prev_active, acc.is_enabled, inserted));
        apply_ad_fields(&mut tx, id, acc, &sources).await?;
    }
    if kind == Kind::Full {
        // Учётки, которых не было в полной выборке: удалены в AD или больше не проходят фильтр.
        let gone: Vec<(Uuid, bool)> = sqlx::query_as(
            "UPDATE ad_accounts SET is_missing = true
              WHERE domain_id = $1 AND synced_at < $2 AND NOT is_missing
          RETURNING id, is_enabled",
        )
        .bind(domain.id)
        .bind(started)
        .fetch_all(&mut *tx)
        .await?;
        changed.extend(gone.into_iter().map(|(id, was)| (id, was, false, false)));
    }
    let disabling = changed.iter().filter(|(_, was, now, _)| *was && !*now).count();
    let disabling = i64::try_from(disabling).unwrap_or(i64::MAX);
    if !allow_mass_disable && mass_disable(active, disabling) {
        tx.rollback().await?;
        let text = format!(
            "Синхронизация отключила бы {disabling} из {active} активных учёток (AD вернул {} записей) — ничего не изменено. \
             Проверьте фильтр, базовый DN и права служебной учётки или подтвердите отключение в админке.",
            s.seen
        );
        fail_run(core, run_id, &text).await?;
        return Err(ApiError::bad_request("ad_sync_mass_disable", text));
    }
    for (id, was, now, inserted) in changed {
        match (was, now) {
            (true, false) => {
                deactivate(&mut tx, id).await?;
                s.disabled += 1;
            }
            (false, true) if !inserted => {
                reactivate(&mut tx, id).await?;
                s.enabled += 1;
            }
            _ => {}
        }
        if now {
            match link_account(&mut tx, id, &link).await? {
                Linked::Linked => s.linked += 1,
                Linked::Candidates(n) => s.candidates += n,
                Linked::Created => s.persons_created += 1,
                Linked::Already | Linked::Skipped => {}
            }
        }
    }
    let new_state = serde_json::json!(SyncState { controller: batch.controller.clone(), usn: batch.highest_usn });
    sqlx::query(
        "UPDATE ad_domains SET sync_state = $2, last_sync_at = now(),
                last_full_sync_at = CASE WHEN $3 THEN now() ELSE last_full_sync_at END
          WHERE id = $1",
    )
    .bind(domain.id)
    .bind(new_state)
    .bind(kind == Kind::Full)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE ad_sync_runs SET status = 'ok', controller = $2, added = $3, updated = $4, disabled = $5, finished_at = now()
          WHERE id = $1",
    )
    .bind(run_id)
    .bind(&batch.controller)
    .bind(i32::try_from(s.added).unwrap_or(i32::MAX))
    .bind(i32::try_from(s.updated).unwrap_or(i32::MAX))
    .bind(i32::try_from(s.disabled).unwrap_or(i32::MAX))
    .execute(&mut *tx)
    .await?;
    if s.candidates > 0 {
        notify::notify_superadmins(&mut *tx, notify::kind::LINK_CANDIDATES, serde_json::json!({"count": s.candidates}))
            .await?;
    }
    // Каждая инкрементальная без изменений журнал не засоряет.
    if kind == Kind::Full || s.added + s.disabled + s.enabled + s.candidates + s.persons_created > 0 {
        audit::record(
            &mut *tx,
            &Actor::system(),
            Entry::new("ad.sync", Source::Job).object("ad_domain", domain.id).details(serde_json::json!(s)),
        )
        .await?;
    }
    tx.commit().await?;
    tracing::info!(kind = kind_s, dc = %s.controller, seen = s.seen, added = s.added, disabled = s.disabled, "синхронизация AD завершена");
    Ok(s)
}

async fn fail_run(core: &Core, run_id: Uuid, text: &str) -> Result<(), sqlx::Error> {
    tracing::error!(error = %text, "синхронизация AD не удалась");
    sqlx::query("UPDATE ad_sync_runs SET status = 'error', error = $2, finished_at = now() WHERE id = $1")
        .bind(run_id)
        .bind(text)
        .execute(&core.db)
        .await?;
    // Письмо — только вместе с новым уведомлением (не на каждую неудачную попытку раз в 5 минут).
    if notify::notify_superadmins(&core.db, notify::kind::AD_SYNC_FAILED, serde_json::json!({"error": text})).await? > 0
    {
        // Одно письмо всем суперадминистраторам — на языке установки.
        let lang = pult_core::l10n::default_lang(&core.db).await;
        let tr = |s: &str| pult_core::l10n::tr_to(lang, s).into_owned();
        pult_core::mail::enqueue_superadmins(
            &core.db,
            &tr("Pult: синхронизация с AD не удалась"),
            &tr(&format!(
                "Синхронизация с Active Directory не удалась:

{text}

Подробности — в админке, раздел «Каталог AD»."
            )),
        )
        .await?;
    }
    Ok(())
}

/// Логин занят другой записью: учётку в AD удалили и создали заново (новый objectGUID, тот же логин).
/// Это другой объект AD: роли, сеансы и карточка прежнего владельца к нему не переходят — логин
/// мог достаться другому человеку. Старая запись остаётся для истории, с изменённым логином.
/// Без этого одна пересозданная учётка останавливала синхронизацию всего домена.
async fn release_login(
    tx: &mut Transaction<'_, Postgres>,
    domain_id: Uuid,
    guid: Uuid,
    sam: &str,
) -> Result<(), ApiError> {
    let stale: Option<(Uuid, bool, Uuid)> = sqlx::query_as(
        "SELECT id, is_enabled AND NOT is_missing, object_guid FROM ad_accounts
          WHERE domain_id = $1 AND lower(sam_account_name) = lower($2) AND object_guid <> $3 FOR UPDATE",
    )
    .bind(domain_id)
    .bind(sam)
    .bind(guid)
    .fetch_optional(&mut **tx)
    .await?;
    let Some((old, active, old_guid)) = stale else { return Ok(()) };
    sqlx::query(
        "UPDATE ad_accounts SET sam_account_name = sam_account_name || '~' || $2, is_missing = true WHERE id = $1",
    )
    .bind(old)
    .bind(old_guid.to_string().chars().take(8).collect::<String>())
    .execute(&mut **tx)
    .await?;
    if active {
        deactivate(tx, old).await?;
    }
    audit::record(
        &mut **tx,
        &Actor::system(),
        Entry::new("ad.account.replaced", Source::Job)
            .object("ad_account", old)
            .details(serde_json::json!({"login": sam})),
    )
    .await?;
    tracing::warn!(login = sam, "учётка пересоздана в AD: прежняя запись закрыта");
    Ok(())
}

/// Учётка в `ad_accounts`. Возвращает id, была ли она активна до этого и новая ли она.
pub async fn upsert(
    tx: &mut Transaction<'_, Postgres>,
    domain_id: Uuid,
    a: &AdAccount,
) -> Result<(Uuid, bool, bool), ApiError> {
    let guid = Uuid::parse_str(&a.object_guid).map_err(ApiError::internal)?;
    release_login(tx, domain_id, guid, &a.sam_account_name).await?;
    let prev: Option<Prev> =
        sqlx::query_as("SELECT is_enabled AND NOT is_missing AS active FROM ad_accounts WHERE object_guid = $1")
            .bind(guid)
            .fetch_optional(&mut **tx)
            .await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO ad_accounts (domain_id, object_guid, sam_account_name, upn, display_name, given_name, sn, mail, title,
                                  department, telephone_number, mobile, dn, user_account_control, is_enabled, is_missing,
                                  usn_changed, when_changed, synced_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, false, $16, $17, now())
         ON CONFLICT (object_guid) DO UPDATE SET
             sam_account_name = EXCLUDED.sam_account_name, upn = EXCLUDED.upn, display_name = EXCLUDED.display_name,
             given_name = EXCLUDED.given_name, sn = EXCLUDED.sn, mail = EXCLUDED.mail, title = EXCLUDED.title,
             department = EXCLUDED.department, telephone_number = EXCLUDED.telephone_number, mobile = EXCLUDED.mobile,
             dn = EXCLUDED.dn, user_account_control = EXCLUDED.user_account_control, is_enabled = EXCLUDED.is_enabled,
             is_missing = false, usn_changed = EXCLUDED.usn_changed, when_changed = EXCLUDED.when_changed, synced_at = now()
         RETURNING id",
    )
    .bind(domain_id)
    .bind(guid)
    .bind(&a.sam_account_name)
    .bind(&a.upn)
    .bind(&a.display_name)
    .bind(&a.given_name)
    .bind(&a.sn)
    .bind(&a.mail)
    .bind(&a.title)
    .bind(&a.department)
    .bind(&a.telephone_number)
    .bind(&a.mobile)
    .bind(&a.dn)
    .bind(i32::from_ne_bytes(a.user_account_control.to_ne_bytes()))
    .bind(a.is_enabled)
    .bind(i64::try_from(a.usn_changed).unwrap_or(i64::MAX))
    .bind(&a.when_changed)
    .fetch_one(&mut **tx)
    .await?;
    Ok(match prev {
        Some(p) => (id, p.active, false),
        // Новая учётка «была неактивна» — включённая новая пройдёт как появившаяся.
        None => (id, false, true),
    })
}

/// Поля с источником «AD» у связанного сотрудника — из учётки. Пустое значение в AD не стирает
/// значение в справочнике (служебные атрибуты в AD часто пусты).
async fn apply_ad_fields(
    tx: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    a: &AdAccount,
    sources: &std::collections::BTreeMap<String, FieldSource>,
) -> Result<(), sqlx::Error> {
    let ad = |f: &str| sources.get(f) == Some(&FieldSource::Ad);
    let val = |v: &Option<String>| v.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let last = if ad("last_name") { val(&a.sn) } else { None };
    let first = if ad("first_name") { val(&a.given_name) } else { None };
    let mail = if ad("email") { val(&a.mail).map(|m| m.to_lowercase()) } else { None };
    let work = if ad("work_phone") { val(&a.telephone_number) } else { None };
    let mobile = if ad("mobile_phone") { val(&a.mobile) } else { None };
    if (&last, &first, &mail, &work, &mobile) == (&None, &None, &None, &None, &None) {
        return Ok(());
    }
    sqlx::query(
        "UPDATE persons SET last_name = coalesce($2, last_name), first_name = coalesce($3, first_name),
                email = coalesce($4, email), work_phone = coalesce($5, work_phone), mobile_phone = coalesce($6, mobile_phone),
                version = version + 1
          WHERE ad_account_id = $1
            AND (last_name, first_name, email, work_phone, mobile_phone)
                IS DISTINCT FROM (coalesce($2, last_name), coalesce($3, first_name), coalesce($4, email),
                                  coalesce($5, work_phone), coalesce($6, mobile_phone))",
    )
    .bind(account_id)
    .bind(last)
    .bind(first)
    .bind(mail)
    .bind(work)
    .bind(mobile)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Учётка отключена или пропала: доступ закрыт сразу, сотрудник — в архив.
async fn deactivate(tx: &mut Transaction<'_, Postgres>, account_id: Uuid) -> Result<(), sqlx::Error> {
    let users: Vec<Uuid> =
        sqlx::query_scalar("UPDATE users SET is_active = false WHERE ad_account_id = $1 AND is_active RETURNING id")
            .bind(account_id)
            .fetch_all(&mut **tx)
            .await?;
    for u in &users {
        pult_core::session::destroy_all(&mut **tx, *u).await?;
    }
    sqlx::query(
        "UPDATE persons SET is_archived = true, archived_at = now(), archived_reason = 'ad_disabled', version = version + 1
          WHERE ad_account_id = $1 AND NOT is_archived",
    )
    .bind(account_id)
    .execute(&mut **tx)
    .await?;
    audit::record(
        &mut **tx,
        &Actor::system(),
        Entry::new("ad.account.disabled", Source::Job).object("ad_account", account_id),
    )
    .await?;
    Ok(())
}

async fn reactivate(tx: &mut Transaction<'_, Postgres>, account_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE users SET is_active = true WHERE ad_account_id = $1")
        .bind(account_id)
        .execute(&mut **tx)
        .await?;
    sqlx::query(
        "UPDATE persons SET is_archived = false, archived_at = NULL, archived_reason = NULL, version = version + 1
          WHERE ad_account_id = $1 AND is_archived AND archived_reason = 'ad_disabled'",
    )
    .bind(account_id)
    .execute(&mut **tx)
    .await?;
    audit::record(
        &mut **tx,
        &Actor::system(),
        Entry::new("ad.account.enabled", Source::Job).object("ad_account", account_id),
    )
    .await?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Linked {
    Already,
    Linked,
    Candidates(usize),
    Created,
    Skipped,
}

#[derive(sqlx::FromRow)]
struct AccountNames {
    sam_account_name: String,
    display_name: Option<String>,
    sn: Option<String>,
    given_name: Option<String>,
    mail: Option<String>,
}

fn non_empty(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

/// Ставит связь учётки с сотрудником и пишет в аудит, каким правилом.
async fn set_link(
    tx: &mut Transaction<'_, Postgres>,
    person: Uuid,
    account_id: Uuid,
    by: &str,
) -> Result<Linked, sqlx::Error> {
    sqlx::query("UPDATE persons SET ad_account_id = $2, version = version + 1 WHERE id = $1")
        .bind(person)
        .bind(account_id)
        .execute(&mut **tx)
        .await?;
    sync_user_person(tx, account_id).await?;
    audit::record(
        &mut **tx,
        &Actor::system(),
        Entry::new("person.link", Source::Job)
            .object("person", person)
            .details(serde_json::json!({"ad_account": account_id, "by": by})),
    )
    .await?;
    Ok(Linked::Linked)
}

/// Предложить пары «учётка — сотрудник» с такими фамилией и именем (подтверждает администратор).
async fn propose_by_name(
    tx: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    last: &str,
    first: &str,
    reason: &str,
) -> Result<usize, sqlx::Error> {
    Ok(sqlx::query(
        "INSERT INTO person_link_candidates (ad_account_id, person_id, reason)
         SELECT $1, p.id, $4
           FROM persons p
          WHERE p.ad_account_id IS NULL AND NOT p.is_archived
            AND pult_norm(p.last_name) = pult_norm($2) AND pult_norm(p.first_name) = pult_norm($3)
         ON CONFLICT (ad_account_id, person_id) DO NOTHING",
    )
    .bind(account_id)
    .bind(last)
    .bind(first)
    .bind(reason)
    .execute(&mut **tx)
    .await?
    .rows_affected() as usize)
}

/// Связывает учётку с сотрудником.
///
/// 1. Email учётки совпал ровно с одним сотрудником — связь сразу.
/// 2. Логин почты сотрудника (до `@`) совпал с логином AD ровно у одного сотрудника и фамилия
///    сотрудника есть в имени учётки (`displayName` или `sn`) — связь сразу; фамилия расходится —
///    пара на подтверждение. Во многих организациях ящик заводят под логином AD, а `mail` в AD пуст.
/// 3. Совпали фамилия и имя (`sn`/`givenName`, а если они пусты — первые два слова `displayName`
///    в любом порядке) — пары на подтверждение администратору.
/// 4. Пары нет — по настройке новый сотрудник (только при заполненных `sn` и `givenName`:
///    служебные учётки обычно без них и в справочник не попадают).
pub async fn link_account(
    tx: &mut Transaction<'_, Postgres>,
    account_id: Uuid,
    s: &LinkSettings,
) -> Result<Linked, sqlx::Error> {
    let linked: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM persons WHERE ad_account_id = $1)")
        .bind(account_id)
        .fetch_one(&mut **tx)
        .await?;
    if linked {
        sync_user_person(tx, account_id).await?;
        return Ok(Linked::Already);
    }
    let a: AccountNames =
        sqlx::query_as("SELECT sam_account_name, display_name, sn, given_name, mail FROM ad_accounts WHERE id = $1")
            .bind(account_id)
            .fetch_one(&mut **tx)
            .await?;
    // 1. Email — однозначная пара.
    if let Some(mail) = a.mail.as_deref().filter(|m| m.contains('@')) {
        let by_mail: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM persons WHERE lower(email) = lower($1) AND ad_account_id IS NULL AND NOT is_archived LIMIT 2",
        )
        .bind(mail)
        .fetch_all(&mut **tx)
        .await?;
        if let [only] = by_mail.as_slice() {
            return set_link(tx, *only, account_id, "email").await;
        }
    }
    // 2. Логин почты = логин AD; фамилия — проверка, что это тот же человек.
    let account_name =
        [non_empty(&a.display_name), non_empty(&a.sn)].into_iter().flatten().collect::<Vec<_>>().join(" ");
    let by_login: Vec<(Uuid, bool)> = sqlx::query_as(
        "SELECT id, length(pult_norm(last_name)) > 1 AND pult_norm($2) LIKE '%' || pult_norm(last_name) || '%'
           FROM persons
          WHERE email LIKE '%@%' AND lower(split_part(email, '@', 1)) = lower($1)
            AND ad_account_id IS NULL AND NOT is_archived
          LIMIT 2",
    )
    .bind(a.sam_account_name.trim())
    .bind(&account_name)
    .fetch_all(&mut **tx)
    .await?;
    let mut proposed = 0;
    if let [(person, names_agree)] = by_login.as_slice() {
        if *names_agree {
            return set_link(tx, *person, account_id, "email_login").await;
        }
        proposed += sqlx::query(
            "INSERT INTO person_link_candidates (ad_account_id, person_id, reason)
             VALUES ($1, $2, 'Логин почты совпадает с логином AD, фамилия — нет')
             ON CONFLICT (ad_account_id, person_id) DO NOTHING",
        )
        .bind(account_id)
        .bind(person)
        .execute(&mut **tx)
        .await?
        .rows_affected() as usize;
    }
    // 3. ФИО — только с подтверждением администратора.
    let names = match (non_empty(&a.sn), non_empty(&a.given_name)) {
        (Some(sn), Some(given)) => Some((sn.to_string(), given.to_string())),
        _ => None,
    };
    if let Some((sn, given)) = &names {
        proposed += propose_by_name(tx, account_id, sn, given, "Совпадают фамилия и имя").await?;
    } else if let Some(display) = non_empty(&a.display_name) {
        let words: Vec<&str> = display.split_whitespace().collect();
        if let [w0, w1, ..] = words.as_slice() {
            let reason = "Совпадают фамилия и имя (displayName)";
            proposed += propose_by_name(tx, account_id, w0, w1, reason).await?;
            proposed += propose_by_name(tx, account_id, w1, w0, reason).await?;
        }
    }
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM person_link_candidates WHERE ad_account_id = $1 AND status = 'pending'",
    )
    .bind(account_id)
    .fetch_one(&mut **tx)
    .await?;
    if pending > 0 {
        return Ok(Linked::Candidates(proposed));
    }
    // Без фамилии и имени в AD — служебная или техническая учётка: в справочник сама не попадает.
    // Администратор уже отклонил все предложенные пары — значит, это другой человек.
    let (Some((sn, given)), true) = (names, s.auto_create_persons) else {
        return Ok(Linked::Skipped);
    };
    // 4. Новый сотрудник из AD.
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO persons (last_name, first_name, email, source, ad_account_id) VALUES ($1, $2, lower($3), 'ad', $4) RETURNING id",
    )
    .bind(sn.trim())
    .bind(given.trim())
    .bind(a.mail.as_deref().filter(|m| m.contains('@')))
    .bind(account_id)
    .fetch_one(&mut **tx)
    .await?;
    sync_user_person(tx, account_id).await?;
    audit::record(
        &mut **tx,
        &Actor::system(),
        Entry::new("person.create", Source::Job)
            .object("person", id)
            .details(serde_json::json!({"ad_account": account_id})),
    )
    .await?;
    Ok(Linked::Created)
}

/// У пользователя портала — тот же сотрудник, что связан с его учёткой.
pub async fn sync_user_person(tx: &mut Transaction<'_, Postgres>, account_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE users u SET person_id = p.id FROM persons p
          WHERE u.ad_account_id = $1 AND p.ad_account_id = $1 AND u.person_id IS DISTINCT FROM p.id",
    )
    .bind(account_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Связывает все активные несвязанные учётки (после импорта Phonebook).
pub async fn link_all(core: &Core) -> Result<Summary, ApiError> {
    let mut tx = core.db.begin().await?;
    let link: LinkSettings = settings::load(&mut *tx).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT a.id FROM ad_accounts a WHERE a.is_enabled AND NOT a.is_missing
            AND NOT EXISTS (SELECT 1 FROM persons p WHERE p.ad_account_id = a.id)",
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut s = Summary::default();
    for id in ids {
        match link_account(&mut tx, id, &link).await? {
            Linked::Linked => s.linked += 1,
            Linked::Candidates(n) => s.candidates += n,
            Linked::Created => s.persons_created += 1,
            _ => {}
        }
    }
    tx.commit().await?;
    Ok(s)
}

/// Разовый перенос роли участникам группы AD (при импорте Phonebook участники
/// `PhonebookAdmins` получают «Редактор справочника»). Повторный вызов ничего не делает —
/// дальше роли ведутся только в портале. Возвращает, скольким выдана роль.
pub async fn grant_role_to_group_once(
    core: &Core,
    ad: &AdHandle,
    group: &str,
    role: &str,
) -> Result<Option<usize>, ApiError> {
    let flag = format!("role_migration:{}:{role}", group.to_lowercase());
    let done: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM settings WHERE key = $1)")
        .bind(&flag)
        .fetch_one(&core.db)
        .await?;
    if done {
        return Ok(None);
    }
    let Some((_, pool)) = ad.pool(core).await? else { return Ok(None) };
    let Some(members) =
        pool.group_member_dns(group).await.map_err(|e| ApiError::bad_request("ad_error", e.to_string()))?
    else {
        tracing::info!(group, "группы нет в AD — перенос роли пропущен");
        return Ok(None);
    };
    let dns: Vec<String> = members.iter().map(|d| d.to_lowercase()).collect();
    let mut tx = core.db.begin().await?;
    // Пользователь портала создаётся заранее: роль будет ждать его первого входа.
    sqlx::query(
        "INSERT INTO users (ad_account_id, person_id, locale)
         SELECT a.id, (SELECT p.id FROM persons p WHERE p.ad_account_id = a.id), coalesce((SELECT value->>'default_locale' FROM settings WHERE key = 'locale'), 'ru')
           FROM ad_accounts a WHERE lower(a.dn) = ANY($1) AND a.is_enabled AND NOT a.is_missing
         ON CONFLICT (ad_account_id) DO NOTHING",
    )
    .bind(&dns)
    .execute(&mut *tx)
    .await?;
    let granted: Vec<Uuid> = sqlx::query_scalar(
        "INSERT INTO user_roles (user_id, role_id)
         SELECT u.id, r.id FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id, roles r
          WHERE lower(a.dn) = ANY($1) AND r.code = $2
         ON CONFLICT DO NOTHING RETURNING user_id",
    )
    .bind(&dns)
    .bind(role)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO settings (key, value) VALUES ($1, $2)")
        .bind(&flag)
        .bind(serde_json::json!({"members": members.len(), "granted": granted.len()}))
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        &Actor::system(),
        Entry::new("user.roles.migrated", Source::Cli)
            .details(serde_json::json!({"group": group, "role": role, "users": granted})),
    )
    .await?;
    tx.commit().await?;
    Ok(Some(granted.len()))
}

#[cfg(test)]
mod tests {
    use super::mass_disable;

    #[test]
    fn mass_disable_threshold() {
        assert!(!mass_disable(1000, 0));
        assert!(!mass_disable(1000, 200), "до 20% — обычные увольнения");
        assert!(mass_disable(1000, 201));
        assert!(!mass_disable(50, 20), "в небольшом домене — до 20 учёток");
        assert!(mass_disable(50, 21));
        assert!(mass_disable(8, 8), "отключить всех — никогда без подтверждения");
        assert!(!mass_disable(8, 1));
    }
}
