//! Операции каталога (ADR-0002):
//! - [`request`] — проверки (основание, заявка, права, область), ключ идемпотентности, задача в той же транзакции;
//! - [`execute`] — фоновая задача: снова проверяет права запросившего, пишет учёткой записи, читает результат,
//!   пишет аудит «было/стало»; повторная доставка выполненной операции ничего не делает;
//! - [`reveal`] — временный пароль запросившему, один раз;
//! - [`account_state`] — состояние учётки для карточки заявителя (читается из AD при открытии).

use crate::{JOB, perm, zones::DirectorySettings};
use pult_ad::{AccountState, AdError, Change, DcPool, write::filetime_to_unix};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Permission, Secret,
    audit::{self, Actor, Entry, Source},
    rbac::{self, Grants},
    settings,
};
use pult_jobs::NewJob;
use pult_people::domain::{self, AdHandle};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

/// Операция записи. Других нет: произвольного LDAP modify модуль не умеет.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = OperationKind)]
pub enum Kind {
    #[serde(rename = "group.add_member")]
    AddMember,
    #[serde(rename = "user.unlock")]
    Unlock,
    #[serde(rename = "user.enable")]
    Enable,
    #[serde(rename = "user.disable")]
    Disable,
    #[serde(rename = "user.reset_password")]
    ResetPassword,
}

impl Kind {
    pub fn code(self) -> &'static str {
        match self {
            Self::AddMember => "group.add_member",
            Self::Unlock => "user.unlock",
            Self::Enable => "user.enable",
            Self::Disable => "user.disable",
            Self::ResetPassword => "user.reset_password",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Self::AddMember, Self::Unlock, Self::Enable, Self::Disable, Self::ResetPassword]
            .into_iter()
            .find(|k| k.code() == s)
    }

    /// Разрешение, без которого операцию не запросить и не выполнить.
    pub fn permission(self) -> &'static Permission {
        match self {
            Self::AddMember => &perm::ADD_MEMBER,
            Self::Unlock => &perm::UNLOCK,
            Self::Enable | Self::Disable => &perm::ENABLE,
            Self::ResetPassword => &perm::RESET_PASSWORD,
        }
    }

    fn audit_action(self) -> &'static str {
        match self {
            Self::AddMember => "directory.group.add_member",
            Self::Unlock => "directory.user.unlock",
            Self::Enable => "directory.user.enable",
            Self::Disable => "directory.user.disable",
            Self::ResetPassword => "directory.user.reset_password",
        }
    }
}

/// Право и область — одна проверка для API, кнопок заявки и очереди.
/// Учётку меняют только в разрешённых OU; в разрешённую группу добавляют тоже только учётку из этих OU
/// (иначе через Pult можно было бы дать доступ служебной или чужой учётке).
pub fn authorize(
    grants: &Grants,
    s: &DirectorySettings,
    kind: Kind,
    target_dn: &str,
    group_dn: Option<&str>,
) -> ApiResult<()> {
    if !grants.has(kind.permission()) {
        return Err(ApiError::Forbidden);
    }
    let in_zone = match kind {
        Kind::AddMember => group_dn.is_some_and(|g| s.group_allowed(g)) && s.user_allowed(target_dn),
        _ => s.user_allowed(target_dn),
    };
    if in_zone { Ok(()) } else { Err(ApiError::Forbidden) }
}

/// Чью учётку меняем: сотрудника справочника или учётку AD напрямую (новый сотрудник, которого
/// в справочнике ещё нет: отключённая учётка в справочник сама не попадает).
#[derive(Debug, Clone, Copy)]
pub enum Target {
    Person(Uuid),
    Account(Uuid),
}

/// Новая операция: от API, «Выполнить» в запросе доступа или пункта чек-листа.
#[derive(Debug, Clone)]
pub struct NewOperation {
    pub kind: Kind,
    pub target: Target,
    pub group_dn: Option<String>,
    pub basis: String,
    pub ticket_id: Option<Uuid>,
    pub checklist_item_id: Option<Uuid>,
    pub idempotency_key: String,
    /// Можно и свою учётку: только для запроса доступа, согласованного владельцем ресурса.
    pub own_allowed: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
pub struct OperationView {
    pub id: Uuid,
    pub kind: String,
    /// `queued`, `running`, `done`, `failed`.
    pub status: String,
    pub person_id: Option<Uuid>,
    pub login: String,
    pub target_dn: String,
    pub group_dn: Option<String>,
    pub ticket_number: Option<i64>,
    pub basis: String,
    pub requested_by: Uuid,
    pub requested_by_name: Option<String>,
    /// Состояние учётки до и после: `enabled`, `locked`, `must_change_password`, `groups`.
    #[schema(value_type = Option<Object>)]
    pub before: Option<Value>,
    #[schema(value_type = Option<Object>)]
    pub after: Option<Value>,
    /// `false` — уже было так, AD не меняли.
    pub changed: Option<bool>,
    /// Повторное чтение подтвердило результат.
    pub verified: Option<bool>,
    pub error: Option<String>,
    /// Временный пароль ждёт показа запросившему.
    pub has_password: bool,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub finished_at: Option<OffsetDateTime>,
}

const VIEW: &str = "SELECT o.id, o.kind, o.status, p.id AS person_id, a.sam_account_name AS login, o.target_dn, o.group_dn,
        t.number AS ticket_number, o.basis, o.requested_by,
        (SELECT coalesce(nullif(trim(concat_ws(' ', rp.last_name, rp.first_name)), ''), ra.display_name, ra.sam_account_name)
           FROM users ru JOIN ad_accounts ra ON ra.id = ru.ad_account_id
           LEFT JOIN persons rp ON rp.id = ru.person_id WHERE ru.id = o.requested_by) AS requested_by_name,
        o.before, o.after, o.changed, o.verified, o.error, (o.secret IS NOT NULL) AS has_password, o.created_at, o.finished_at
   FROM directory_operations o
   JOIN ad_accounts a ON a.id = o.account_id
   LEFT JOIN LATERAL (SELECT id FROM persons WHERE ad_account_id = a.id LIMIT 1) p ON true
   LEFT JOIN tickets t ON t.id = o.ticket_id";

pub async fn view(db: &sqlx::PgPool, id: Uuid) -> ApiResult<OperationView> {
    Ok(sqlx::query_as(&format!("{VIEW} WHERE o.id = $1")).bind(id).fetch_one(db).await?) // SAFE: константа VIEW
}

/// Операции заявки — для карточки.
pub async fn for_ticket(db: &sqlx::PgPool, ticket: Uuid) -> ApiResult<Vec<OperationView>> {
    Ok(sqlx::query_as(&format!("{VIEW} WHERE o.ticket_id = $1 ORDER BY o.created_at")) // SAFE: константа VIEW
        .bind(ticket)
        .fetch_all(db)
        .await?)
}

/// Учётка записи текущего домена: DN и зашифрованный пароль.
async fn writer(db: &sqlx::PgPool, domain: Uuid) -> Result<Option<(String, Vec<u8>)>, sqlx::Error> {
    let row: Option<(Option<String>, Option<Vec<u8>>)> =
        sqlx::query_as("SELECT writer_bind_dn, writer_secret FROM ad_domains WHERE id = $1")
            .bind(domain)
            .fetch_optional(db)
            .await?;
    Ok(match row {
        Some((Some(dn), Some(secret))) if !dn.is_empty() => Some((dn, secret)),
        _ => None,
    })
}

/// Запросить операцию. Повтор с тем же ключом от того же пользователя — та же операция.
pub async fn request(core: &Core, who: &CurrentUser, n: NewOperation) -> ApiResult<OperationView> {
    // Выключенный каталог не пишет в AD и через заявки (выдача доступа, пункты чек-листа).
    if !pult_core::org::module_enabled(core, pult_core::org::Gated::Directory).await {
        return Err(ApiError::NotFound);
    }
    let key = n.idempotency_key.trim();
    if !(8..=200).contains(&key.chars().count()) {
        return Err(ApiError::bad_request("invalid_operation", "Ключ идемпотентности — от 8 до 200 символов."));
    }
    if let Some(op) = existing(core, who, key).await? {
        return Ok(op);
    }
    let Some(dom) = domain::current(&core.db).await? else {
        return Err(ApiError::bad_request("ad_not_configured", "Домен AD не настроен."));
    };
    if writer(&core.db, dom.id).await?.is_none() {
        return Err(ApiError::bad_request(
            "writer_not_configured",
            "Запись в AD не настроена: укажите учётку записи в разделе «Каталог AD → Запись».",
        ));
    }
    let basis = n.basis.trim();
    if basis.is_empty() {
        return Err(ApiError::bad_request("invalid_operation", "Укажите основание."));
    }
    if basis.chars().count() > 2000 {
        return Err(ApiError::bad_request("invalid_operation", "Основание — не длиннее 2000 символов."));
    }
    let s: DirectorySettings = settings::load(&core.db).await?;
    if s.require_ticket && n.ticket_id.is_none() {
        return Err(ApiError::bad_request("ticket_required", "Укажите заявку — основание операции."));
    }
    let target: Option<(Uuid, String)> = match n.target {
        Target::Person(p) => {
            sqlx::query_as(
                "SELECT a.id, a.dn FROM persons p JOIN ad_accounts a ON a.id = p.ad_account_id
                  WHERE p.id = $1 AND NOT a.is_missing",
            )
            .bind(p)
            .fetch_optional(&core.db)
            .await?
        }
        Target::Account(a) => {
            sqlx::query_as("SELECT id, dn FROM ad_accounts WHERE id = $1 AND NOT is_missing")
                .bind(a)
                .fetch_optional(&core.db)
                .await?
        }
    };
    let Some((account, dn)) = target else {
        return Err(ApiError::bad_request("no_account", "У сотрудника нет учётки AD."));
    };
    if !n.own_allowed && is_own(&core.db, who.user_id, account).await? {
        // Разделение обязанностей: свою учётку (пароль, группы, включение) меняет коллега, а не сам оператор.
        // Исключение — согласованный владельцем ресурса запрос доступа: решение уже принял другой человек.
        return Err(ApiError::Forbidden);
    }
    let group = match n.kind {
        Kind::AddMember => match n.group_dn.as_deref().map(str::trim).filter(|g| !g.is_empty()) {
            Some(g) => Some(g.to_string()),
            None => return Err(ApiError::bad_request("invalid_operation", "Укажите группу.")),
        },
        _ => None,
    };
    authorize(&who.grants, &s, n.kind, &dn, group.as_deref())?;

    let mut tx = core.db.begin().await?;
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO directory_operations (idempotency_key, kind, account_id, target_dn, group_dn, ticket_id,
                                           checklist_item_id, basis, requested_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (idempotency_key) DO NOTHING RETURNING id",
    )
    .bind(key)
    .bind(n.kind.code())
    .bind(account)
    .bind(&dn)
    .bind(&group)
    .bind(n.ticket_id)
    .bind(n.checklist_item_id)
    .bind(basis)
    .bind(who.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(id) = id else {
        // Тот же ключ успели занять параллельным запросом.
        tx.rollback().await?;
        return existing(core, who, key).await?.ok_or(ApiError::Forbidden);
    };
    pult_jobs::enqueue(
        &mut *tx,
        &NewJob::new(JOB, json!({"operation": id})).idempotency_key(format!("directory:{id}")),
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    view(&core.db, id).await
}

/// Учётка AD — самого пользователя Pult.
async fn is_own(db: &sqlx::PgPool, user: Uuid, account: Uuid) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND ad_account_id = $2)")
        .bind(user)
        .bind(account)
        .fetch_one(db)
        .await
}

/// Операция с этим ключом: своя — вернуть, чужая — отказ (ключ не открывает чужие операции).
async fn existing(core: &Core, who: &CurrentUser, key: &str) -> ApiResult<Option<OperationView>> {
    let row: Option<(Uuid, Uuid)> =
        sqlx::query_as("SELECT id, requested_by FROM directory_operations WHERE idempotency_key = $1")
            .bind(key)
            .fetch_optional(&core.db)
            .await?;
    match row {
        Some((id, by)) if by == who.user_id => Ok(Some(view(&core.db, id).await?)),
        Some(_) => Err(ApiError::Forbidden),
        None => Ok(None),
    }
}

/// Итог задачи. `fresh` — операция завершилась именно сейчас (комментарий в заявку пишется один раз).
#[derive(Debug, Clone)]
pub struct Finished {
    pub view: OperationView,
    pub fresh: bool,
}

struct Claimed {
    kind: Kind,
    account: Uuid,
    dn: String,
    group: Option<String>,
    requested_by: Uuid,
    basis: String,
    ticket_number: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct ClaimRow {
    status: String,
    kind: String,
    account_id: Uuid,
    target_dn: String,
    group_dn: Option<String>,
    requested_by: Uuid,
    basis: String,
    number: Option<i64>,
}

/// Состояние учётки для журнала операций и аудита.
fn state_json(s: &AccountState) -> Value {
    json!({
        "dn": s.dn,
        "enabled": s.is_enabled(),
        "locked": s.is_locked(),
        "must_change_password": s.pwd_last_set == 0,
        "groups": s.member_of,
    })
}

fn ts(ft: u64) -> Option<OffsetDateTime> {
    filetime_to_unix(ft).and_then(|s| OffsetDateTime::from_unix_timestamp(s).ok())
}

/// Неполученные временные пароли не хранятся дольше суток (вызывается и по расписанию).
pub async fn prune_secrets(db: &sqlx::PgPool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE directory_operations SET secret = NULL WHERE secret IS NOT NULL AND finished_at < now() - interval '1 day'",
    )
    .execute(db)
    .await?
    .rows_affected())
}

impl OperationView {
    /// Тексты (ошибка, основание) — на языке запроса.
    pub fn localized(mut self) -> Self {
        self.error = self.error.map(|e| pult_core::l10n::tr(&e).into_owned());
        self.basis = pult_core::l10n::tr(&self.basis).into_owned();
        self
    }
}

/// Выполнить операцию (фоновая задача `directory.operation`).
pub async fn execute(core: &Core, op: Uuid) -> ApiResult<Finished> {
    prune_secrets(&core.db).await?;
    let mut tx = core.db.begin().await?;
    let row: Option<ClaimRow> = sqlx::query_as(
        "SELECT o.status, o.kind, o.account_id, o.target_dn, o.group_dn, o.requested_by, o.basis, t.number
           FROM directory_operations o LEFT JOIN tickets t ON t.id = o.ticket_id
          WHERE o.id = $1 FOR UPDATE OF o",
    )
    .bind(op)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(ClaimRow {
        status,
        kind,
        account_id: account,
        target_dn: dn,
        group_dn: group,
        requested_by,
        basis,
        number: ticket_number,
    }) = row
    else {
        return Err(ApiError::NotFound);
    };
    let kind = Kind::parse(&kind).ok_or_else(|| ApiError::internal(format!("неизвестная операция {kind}")))?;
    if status == "done" || status == "failed" {
        // Повторная доставка задачи: операция уже выполнена — ничего не делаем.
        tx.commit().await?;
        return Ok(Finished { view: view(&core.db, op).await?, fresh: false });
    }
    let c = Claimed { kind, account, dn, group, requested_by, basis, ticket_number };
    if status == "running" && kind == Kind::ResetPassword {
        // Прошлая попытка оборвалась на середине: пароль мог смениться, второй раз не сбрасываем.
        tx.commit().await?;
        let msg = "Выполнение прервалось: пароль мог измениться. Проверьте учётку и запросите сброс заново.";
        return finish(core, op, &c, Err(msg.to_string()), None).await;
    }
    sqlx::query("UPDATE directory_operations SET status = 'running' WHERE id = $1").bind(op).execute(&mut *tx).await?;
    tx.commit().await?;

    match perform(core, &c).await {
        Ok((applied, secret)) => finish(core, op, &c, Ok(applied), secret).await,
        Err(Perform::Retry(e)) => {
            // Нет связи с контроллерами: вернуть в очередь, задача повторится.
            sqlx::query("UPDATE directory_operations SET status = 'queued' WHERE id = $1")
                .bind(op)
                .execute(&core.db)
                .await?;
            Err(e)
        }
        Err(Perform::Failed(msg)) => finish(core, op, &c, Err(msg), None).await,
    }
}

enum Perform {
    Retry(ApiError),
    Failed(String),
}

impl From<ApiError> for Perform {
    fn from(e: ApiError) -> Self {
        Self::Retry(e)
    }
}

impl From<sqlx::Error> for Perform {
    fn from(e: sqlx::Error) -> Self {
        Self::Retry(e.into())
    }
}

/// Проверить права, записать, прочитать. Пароль (для сброса) — зашифрованный, для показа запросившему.
async fn perform(core: &Core, c: &Claimed) -> Result<(pult_ad::Applied, Option<Vec<u8>>), Perform> {
    // Права проверяются снова: между запросом и выполнением их могли отозвать (или отключить пользователя).
    let active: bool = sqlx::query_scalar("SELECT coalesce((SELECT is_active FROM users WHERE id = $1), false)")
        .bind(c.requested_by)
        .fetch_one(&core.db)
        .await?;
    let grants = if active { rbac::load_grants(&core.db, c.requested_by).await? } else { Grants::default() };
    let s: DirectorySettings = settings::load(&core.db).await?;
    if authorize(&grants, &s, c.kind, &c.dn, c.group.as_deref()).is_err() {
        return Err(Perform::Failed(
            "У запросившего больше нет права на эту операцию или учётка вне разрешённой области.".into(),
        ));
    }
    let Some(dom) = domain::current(&core.db).await? else {
        return Err(Perform::Failed("Домен AD не настроен.".into()));
    };
    let Some((writer_dn, sealed)) = writer(&core.db, dom.id).await? else {
        return Err(Perform::Failed("Учётка записи в AD не настроена.".into()));
    };
    let mut cfg = dom.to_config(core)?;
    cfg.bind_dn = writer_dn;
    cfg.bind_password = core.secrets.decrypt(&sealed).map_err(ApiError::internal)?;
    let pool = DcPool::new(cfg).map_err(|e| Perform::Failed(e.to_string()))?;
    let password = match c.kind {
        Kind::ResetPassword => Some(temp_password()?),
        _ => None,
    };
    let change = match (c.kind, &password) {
        (Kind::AddMember, _) => Change::AddToGroup { group_dn: c.group.as_deref().unwrap_or_default() },
        (Kind::Unlock, _) => Change::Unlock,
        (Kind::Enable, _) => Change::SetEnabled(true),
        (Kind::Disable, _) => Change::SetEnabled(false),
        (Kind::ResetPassword, Some(p)) => Change::SetPassword { password: p, must_change: true },
        (Kind::ResetPassword, None) => return Err(Perform::Failed("нет пароля".into())),
    };
    match pool.apply(&c.dn, change).await {
        Ok(a) => {
            let secret = match &password {
                Some(p) => Some(core.secrets.encrypt(p).map_err(ApiError::internal)?),
                None => None,
            };
            Ok((a, secret))
        }
        Err(e @ AdError::Unavailable(_)) => {
            Err(Perform::Retry(ApiError::Unavailable { code: "ad_unavailable", message: e.to_string() }))
        }
        Err(e) => Err(Perform::Failed(e.to_string())),
    }
}

/// Записать итог: состояние до/после, проверку, аудит, состояние учётки в `ad_accounts`.
async fn finish(
    core: &Core,
    op: Uuid,
    c: &Claimed,
    result: Result<pult_ad::Applied, String>,
    secret: Option<Vec<u8>>,
) -> ApiResult<Finished> {
    let (before, after, changed, verified, error, controller) = match &result {
        Ok(a) => {
            let ok = match c.kind {
                Kind::AddMember => c.group.as_deref().is_some_and(|g| a.after.in_group(g)),
                Kind::Unlock => !a.after.is_locked(),
                Kind::Enable => a.after.is_enabled(),
                Kind::Disable => !a.after.is_enabled(),
                Kind::ResetPassword => a.after.pwd_last_set == 0,
            };
            let err = (!ok).then(|| "Повторное чтение из AD не подтвердило изменение.".to_string());
            (
                Some(state_json(&a.before)),
                Some(state_json(&a.after)),
                Some(a.changed),
                Some(ok),
                err,
                Some(a.controller.clone()),
            )
        }
        Err(msg) => (None, None, None, None, Some(msg.clone()), None),
    };
    let status = if verified == Some(true) { "done" } else { "failed" };
    // Пароль, который не подтвердился, не показываем.
    let secret = secret.filter(|_| status == "done");
    let mut tx = core.db.begin().await?;
    sqlx::query(
        "UPDATE directory_operations SET status = $2, before = $3, after = $4, changed = $5, verified = $6, error = $7,
                secret = $8, finished_at = now() WHERE id = $1",
    )
    .bind(op)
    .bind(status)
    .bind(&before)
    .bind(&after)
    .bind(changed)
    .bind(verified)
    .bind(&error)
    .bind(&secret)
    .execute(&mut *tx)
    .await?;
    if let Ok(a) = &result {
        store_state(&mut *tx, c.account, &a.after).await?;
    }
    let actor = Actor { user_id: Some(c.requested_by), ..Actor::default() };
    audit::record(
        &mut *tx,
        &actor,
        Entry::new(c.kind.audit_action(), Source::Job).object("ad_account", c.account).details(json!({
            "operation": op,
            "ticket": c.ticket_number,
            "basis": c.basis,
            "group": c.group,
            "before": before,
            "after": after,
            "changed": changed,
            "verified": verified,
            "controller": controller,
            "error": error,
        })),
    )
    .await?;
    tx.commit().await?;
    Ok(Finished { view: view(&core.db, op).await?, fresh: true })
}

async fn store_state(ex: impl sqlx::PgExecutor<'_>, account: Uuid, s: &AccountState) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE ad_accounts SET user_account_control = $2, is_enabled = $3, lockout_time = $4, pwd_last_set = $5,
                last_logon = $6, state_read_at = now() WHERE id = $1",
    )
    .bind(account)
    .bind(i32::try_from(s.user_account_control).unwrap_or(i32::MAX))
    .bind(s.is_enabled())
    .bind(ts(s.lockout_time))
    .bind(ts(s.pwd_last_set))
    .bind(ts(s.last_logon))
    .execute(ex)
    .await?;
    Ok(())
}

/// Временный пароль: 16 символов, все четыре класса символов (требования сложности AD), без похожих букв.
fn temp_password() -> ApiResult<Secret> {
    use ring::rand::{SecureRandom, SystemRandom};
    const SETS: [&[u8]; 4] = [b"ABCDEFGHJKLMNPQRSTUVWXYZ", b"abcdefghijkmnopqrstuvwxyz", b"23456789", b"!#%+-=?@"];
    let rng = SystemRandom::new();
    // Без смещения: байты за пределами кратного `n` отбрасываются.
    let pick = |n: usize| -> ApiResult<usize> {
        loop {
            let mut b = [0u8; 1];
            rng.fill(&mut b).map_err(|_| ApiError::internal("нет источника случайных чисел"))?;
            let limit = 256 - 256 % n;
            if usize::from(b[0]) < limit {
                return Ok(usize::from(b[0]) % n);
            }
        }
    };
    let all = SETS.concat();
    let mut out = Vec::with_capacity(16);
    for set in SETS {
        out.push(set[pick(set.len())?]);
    }
    while out.len() < 16 {
        out.push(all[pick(all.len())?]);
    }
    for i in (1..out.len()).rev() {
        let j = pick(i + 1)?;
        out.swap(i, j);
    }
    Ok(Secret::new(String::from_utf8(out).map_err(ApiError::internal)?))
}

/// Показать временный пароль запросившему — один раз; потом он стирается.
pub async fn reveal(core: &Core, who: &CurrentUser, op: Uuid) -> ApiResult<Secret> {
    let mut tx = core.db.begin().await?;
    let row: Option<(Uuid, Option<Vec<u8>>)> = sqlx::query_as(
        "SELECT requested_by, secret FROM directory_operations
          WHERE id = $1 AND (finished_at IS NULL OR finished_at >= now() - interval '1 day') FOR UPDATE",
    )
    .bind(op)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((by, secret)) = row else { return Err(ApiError::NotFound) };
    if by != who.user_id {
        return Err(ApiError::Forbidden);
    }
    let Some(secret) = secret else { return Err(ApiError::Gone) };
    let password = core.secrets.decrypt(&secret).map_err(ApiError::internal)?;
    sqlx::query("UPDATE directory_operations SET secret = NULL, secret_shown_at = now() WHERE id = $1")
        .bind(op)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        &who.actor,
        Entry::new("directory.password.shown", Source::Portal).object("directory_operation", op),
    )
    .await?;
    tx.commit().await?;
    Ok(password)
}

/// Состояние учётки AD сотрудника — для карточки заявителя.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AccountView {
    pub account_id: Uuid,
    pub login: String,
    pub dn: String,
    pub enabled: bool,
    pub locked: bool,
    /// Сменить пароль при следующем входе.
    pub must_change_password: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub password_last_set_at: Option<OffsetDateTime>,
    /// Когда истекает пароль; `None` — не истекает (или сменить при входе).
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub password_expires_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_logon_at: Option<OffsetDateTime>,
    /// Прямые группы (DN).
    pub groups: Vec<String>,
}

pub async fn account_state(core: &Core, ad: &AdHandle, person: Uuid) -> ApiResult<AccountView> {
    let row: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT a.id, a.dn, a.sam_account_name FROM persons p JOIN ad_accounts a ON a.id = p.ad_account_id WHERE p.id = $1",
    )
    .bind(person)
    .fetch_optional(&core.db)
    .await?;
    let Some((account, dn, login)) = row else {
        return Err(ApiError::bad_request("no_account", "У сотрудника нет учётки AD."));
    };
    let Some((_, pool)) = ad.pool(core).await? else {
        return Err(ApiError::bad_request("ad_not_configured", "Домен AD не настроен."));
    };
    let unavailable = |e: AdError| ApiError::Unavailable { code: "ad_unavailable", message: e.to_string() };
    let Some(st) = pool.account_state(&dn).await.map_err(unavailable)? else {
        return Err(ApiError::bad_request("no_account", "Учётки нет в AD."));
    };
    let max_age = pool.max_password_age().await.ok().flatten();
    store_state(&core.db, account, &st).await?;
    let set = ts(st.pwd_last_set);
    let expires = match (set, max_age) {
        (Some(set), Some(age)) if st.user_account_control & pult_ad::write::UAC_DONT_EXPIRE_PASSWORD == 0 => {
            Some(set + time::Duration::seconds(age))
        }
        _ => None,
    };
    sqlx::query("UPDATE ad_accounts SET password_expires_at = $2 WHERE id = $1")
        .bind(account)
        .bind(expires)
        .execute(&core.db)
        .await?;
    Ok(AccountView {
        account_id: account,
        login,
        dn: st.dn.clone(),
        enabled: st.is_enabled(),
        locked: st.is_locked(),
        must_change_password: st.pwd_last_set == 0,
        password_last_set_at: set,
        password_expires_at: expires,
        last_logon_at: ts(st.last_logon),
        groups: st.member_of,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants(perms: &[&str]) -> Grants {
        Grants { roles: Default::default(), permissions: perms.iter().map(|p| (*p).to_string()).collect() }
    }

    #[test]
    fn permission_and_zone() {
        let s = DirectorySettings {
            user_ous: vec!["OU=Users,DC=x".into()],
            groups: vec!["CN=Buh,OU=Groups,DC=x".into()],
            require_ticket: false,
        };
        let user = "CN=Ivan,OU=Users,DC=x";
        let g = grants(&["directory.user.unlock", "directory.group.add_member"]);
        assert!(authorize(&g, &s, Kind::Unlock, user, None).is_ok());
        assert!(authorize(&g, &s, Kind::Unlock, "CN=Adm,OU=Admins,DC=x", None).is_err(), "вне OU");
        assert!(authorize(&g, &s, Kind::Enable, user, None).is_err(), "нет разрешения");
        assert!(authorize(&g, &s, Kind::AddMember, user, Some("cn=buh,ou=groups,dc=x")).is_ok());
        assert!(authorize(&g, &s, Kind::AddMember, user, Some("CN=Domain Admins,DC=x")).is_err(), "чужая группа");
        assert!(authorize(&g, &s, Kind::AddMember, user, None).is_err());
        assert!(
            authorize(&g, &s, Kind::AddMember, "CN=svc-backup,OU=Service,DC=x", Some("CN=Buh,OU=Groups,DC=x")).is_err(),
            "в группу — только учётку из разрешённых OU"
        );
        assert!(authorize(&grants(&[]), &DirectorySettings::default(), Kind::Unlock, user, None).is_err());
    }

    #[test]
    fn temp_passwords_are_complex_and_different() {
        let a = temp_password().unwrap();
        let b = temp_password().unwrap();
        assert_ne!(a.expose(), b.expose());
        let p = a.expose();
        assert_eq!(p.chars().count(), 16);
        assert!(p.chars().any(|c| c.is_ascii_uppercase()) && p.chars().any(|c| c.is_ascii_lowercase()));
        assert!(p.chars().any(|c| c.is_ascii_digit()) && p.chars().any(|c| "!#%+-=?@".contains(c)));
    }

    #[test]
    fn kinds_round_trip() {
        for k in [Kind::AddMember, Kind::Unlock, Kind::Enable, Kind::Disable, Kind::ResetPassword] {
            assert_eq!(Kind::parse(k.code()), Some(k));
            assert!(k.permission().code.starts_with("directory."));
        }
    }
}
