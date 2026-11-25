//! Хранилище секретов: учётки служб, инвентаризации, мониторинга, оборудования.
//!
//! Значение зашифровано ключом портала и привязано к своему секрету: шифротекст, переставленный в другую
//! строку, не расшифруется. Права — на каждый секрет, пользователям и ролям портала (не группам AD):
//! `view` — карточка без значения, `reveal` — и значение, `manage` — правка, права, удаление. Значение
//! показывается только по отдельному запросу, при праве `helpdesk.vault.reveal`, и каждый показ — в журнале
//! аудита. Суперадминистратор управляет любым секретом (восстановить доступ, если владелец ушёл).

use axum::{
    Json,
    extract::{FromRef, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Permission, Secret,
    audit::{self, Entry, Source},
    rbac::{ROLE_ENGINEER, ROLE_SUPERADMIN},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub const USE: Permission = Permission {
    code: "helpdesk.vault.use",
    description: "Пользоваться хранилищем секретов: видеть открытые мне секреты и заводить свои",
    default_roles: &[ROLE_ENGINEER, ROLE_SUPERADMIN],
};

pub const REVEAL: Permission = Permission {
    code: "helpdesk.vault.reveal",
    description: "Смотреть значения секретов, к которым есть доступ (каждый просмотр — в журнале аудита)",
    default_roles: &[ROLE_ENGINEER, ROLE_SUPERADMIN],
};

const LEVELS: &[&str] = &["view", "reveal", "manage"];
const MAX_VALUE: usize = 8 * 1024;
const MAX_ACCESS: usize = 100;
/// Метка шифротекста: формат и секрет, к которому он относится.
const TAG: &str = "vault1:";

const USER_NAME: &str =
    "coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name)), ''), a.display_name, a.sam_account_name)";

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(list_secrets, create_secret))
        .routes(routes!(get_secret, update_secret, delete_secret))
        .routes(routes!(reveal_secret))
        .routes(routes!(get_secret_access, put_secret_access))
        .routes(routes!(vault_roles))
}

fn bad(m: &str) -> ApiError {
    ApiError::bad_request("invalid_secret", m.to_string())
}

fn rank(level: &str) -> usize {
    LEVELS.iter().position(|l| *l == level).map_or(0, |i| i + 1)
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct VaultItem {
    pub id: Uuid,
    pub name: String,
    pub login: String,
    pub url: String,
    pub description: String,
    /// Мой доступ: `view`, `reveal` или `manage`.
    pub level: String,
    pub version: i32,
    pub created_by_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub value_changed_at: OffsetDateTime,
}

/// Секреты, видимые пользователю, с его уровнем доступа. `$1` — пользователь, `$2` — коды его ролей,
/// `$3` — суперадминистратор; `extra` — дополнительное условие.
fn visible(extra: &str) -> String {
    format!(
        "SELECT s.id, s.name, s.login, s.url, s.description, s.version, s.updated_at, s.value_changed_at,
                CASE WHEN $3 THEN 'manage' ELSE (ARRAY['view','reveal','manage'])[l.rank] END AS level,
                (SELECT {USER_NAME} FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id
                   LEFT JOIN persons p ON p.id = u.person_id WHERE u.id = s.created_by) AS created_by_name
           FROM vault_secrets s
           LEFT JOIN LATERAL (
                SELECT max(array_position(ARRAY['view','reveal','manage'], va.level)) AS rank
                  FROM vault_access va LEFT JOIN roles r ON r.id = va.role_id
                 WHERE va.secret_id = s.id AND (va.user_id = $1 OR r.code = ANY($2))) l ON true
          WHERE ($3 OR l.rank IS NOT NULL) {extra}
          ORDER BY lower(s.name)"
    )
}

fn role_codes(user: &CurrentUser) -> Vec<String> {
    user.grants.roles.iter().cloned().collect()
}

/// Секрет с уровнем не ниже `need`; нет доступа — как несуществующий.
async fn item(db: impl sqlx::PgExecutor<'_>, user: &CurrentUser, id: Uuid, need: &str) -> ApiResult<VaultItem> {
    let it: VaultItem = sqlx::query_as(&visible("AND s.id = $4"))
        .bind(user.user_id)
        .bind(role_codes(user))
        .bind(user.grants.is_superadmin())
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(ApiError::NotFound)?;
    if rank(&it.level) < rank(need) {
        // Карточку видит, но действие ему не положено.
        return Err(ApiError::Forbidden);
    }
    Ok(it)
}

/// Блокирует секрет до конца транзакции. Права проверяются уже под блокировкой: доступ, отозванный
/// параллельно, не даёт ни показать значение, ни изменить секрет.
async fn lock(tx: &mut sqlx::PgConnection, id: Uuid, share: bool) -> ApiResult<()> {
    let sql = if share {
        "SELECT 1 FROM vault_secrets WHERE id = $1 FOR SHARE"
    } else {
        "SELECT 1 FROM vault_secrets WHERE id = $1 FOR UPDATE"
    };
    sqlx::query(sql).bind(id).fetch_optional(&mut *tx).await?.ok_or(ApiError::NotFound)?;
    Ok(())
}

fn seal(core: &Core, id: Uuid, value: &Secret) -> ApiResult<Vec<u8>> {
    core.secrets.encrypt(&Secret::new(format!("{TAG}{id}:{}", value.expose()))).map_err(ApiError::internal)
}

fn unseal(core: &Core, id: Uuid, raw: &[u8]) -> ApiResult<Secret> {
    let s = core.secrets.decrypt(raw).map_err(ApiError::internal)?;
    let prefix = format!("{TAG}{id}:");
    s.expose()
        .strip_prefix(&prefix)
        .map(Secret::new)
        .ok_or_else(|| ApiError::internal("шифротекст секрета относится к другой записи"))
}

/// Поля карточки (без значения).
#[derive(Deserialize, ToSchema, Default)]
pub struct SecretFields {
    pub name: String,
    #[serde(default)]
    pub login: String,
    /// Адрес службы; ссылкой показывается только `http(s)://`.
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub description: String,
}

struct Clean {
    name: String,
    login: String,
    url: String,
    description: String,
}

fn clean(f: &SecretFields) -> ApiResult<Clean> {
    let one_line = |s: &str, max: usize, what: &str| -> ApiResult<String> {
        let s = s.trim();
        if s.chars().count() > max || s.chars().any(char::is_control) {
            return Err(bad(what));
        }
        Ok(s.to_string())
    };
    let name = one_line(&f.name, 200, "Название — одна строка до 200 символов.")?;
    if name.is_empty() {
        return Err(bad("Укажите название."));
    }
    let description = f.description.trim().to_string();
    if description.chars().count() > 2000
        || description.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(bad("Описание — до 2000 символов, без управляющих символов."));
    }
    Ok(Clean {
        name,
        login: one_line(&f.login, 200, "Логин — одна строка до 200 символов.")?,
        url: one_line(&f.url, 500, "Адрес — одна строка до 500 символов.")?,
        description,
    })
}

fn check_value(v: &Secret) -> ApiResult<()> {
    if v.is_empty() {
        return Err(bad("Значение секрета пустое."));
    }
    if v.expose().len() > MAX_VALUE {
        return Err(bad("Значение — не больше 8 КБ."));
    }
    Ok(())
}

/// Секреты, открытые мне.
#[utoipa::path(get, path = "/api/helpdesk/vault", tag = "helpdesk", responses((status = 200, body = Vec<VaultItem>)))]
async fn list_secrets(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<VaultItem>>> {
    user.require(&USE)?;
    let rows = sqlx::query_as(&visible(""))
        .bind(user.user_id)
        .bind(role_codes(&user))
        .bind(user.grants.is_superadmin())
        .fetch_all(&core.db)
        .await?;
    Ok(Json(rows))
}

#[derive(Deserialize, ToSchema)]
pub struct NewSecret {
    #[serde(flatten)]
    pub fields: SecretFields,
    #[schema(value_type = String, format = Password)]
    pub value: Secret,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedSecret {
    pub id: Uuid,
}

/// Завести секрет. Автор получает управление им.
#[utoipa::path(post, path = "/api/helpdesk/vault", tag = "helpdesk", request_body = NewSecret,
    responses((status = 201, body = CreatedSecret), (status = 400, body = pult_core::error::ErrorBody)))]
async fn create_secret(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<NewSecret>,
) -> ApiResult<(StatusCode, Json<CreatedSecret>)> {
    user.require(&USE)?;
    let f = clean(&b.fields)?;
    check_value(&b.value)?;
    let id = Uuid::now_v7();
    let sealed = seal(&core, id, &b.value)?;
    drop(b.value);
    let mut tx = core.db.begin().await?;
    sqlx::query(
        "INSERT INTO vault_secrets (id, name, login, url, description, sealed, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(id)
    .bind(&f.name)
    .bind(&f.login)
    .bind(&f.url)
    .bind(&f.description)
    .bind(&sealed)
    .bind(user.user_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO vault_access (secret_id, user_id, level) VALUES ($1, $2, 'manage')")
        .bind(id)
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("vault.create", Source::Portal).object("vault_secret", id).details(json!({"name": f.name})),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(CreatedSecret { id })))
}

#[utoipa::path(get, path = "/api/helpdesk/vault/{id}", tag = "helpdesk", params(("id" = Uuid, Path)),
    responses((status = 200, body = VaultItem), (status = 404)))]
async fn get_secret(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<VaultItem>> {
    user.require(&USE)?;
    Ok(Json(item(&core.db, &user, id, "view").await?))
}

#[derive(Deserialize, ToSchema)]
pub struct SecretUpdate {
    #[serde(flatten)]
    pub fields: SecretFields,
    /// Новое значение; не передано — прежнее.
    #[schema(value_type = Option<String>, format = Password)]
    pub value: Option<Secret>,
    /// Версия, которую правили: чужая правка между чтением и сохранением — 409.
    pub version: i32,
}

/// Изменить карточку и, если передано, значение (уровень `manage`).
#[utoipa::path(put, path = "/api/helpdesk/vault/{id}", tag = "helpdesk", params(("id" = Uuid, Path)),
    request_body = SecretUpdate, responses((status = 200, body = VaultItem), (status = 409)))]
async fn update_secret(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<SecretUpdate>,
) -> ApiResult<Json<VaultItem>> {
    user.require(&USE)?;
    let mut tx = core.db.begin().await?;
    lock(&mut tx, id, false).await?;
    item(&mut *tx, &user, id, "manage").await?;
    let f = clean(&b.fields)?;
    let sealed = match &b.value {
        Some(v) => {
            check_value(v)?;
            Some(seal(&core, id, v)?)
        }
        None => None,
    };
    drop(b.value);
    type Before = (String, String, String, String, i32);
    let before: Before =
        sqlx::query_as("SELECT name, login, url, description, version FROM vault_secrets WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(ApiError::NotFound)?;
    if before.4 != b.version {
        return Err(ApiError::Conflict { current: json!({"version": before.4}) });
    }
    sqlx::query(
        "UPDATE vault_secrets SET name = $2, login = $3, url = $4, description = $5, version = version + 1,
                updated_at = now(), sealed = coalesce($6, sealed),
                value_changed_at = CASE WHEN $6 IS NULL THEN value_changed_at ELSE now() END
          WHERE id = $1",
    )
    .bind(id)
    .bind(&f.name)
    .bind(&f.login)
    .bind(&f.url)
    .bind(&f.description)
    .bind(&sealed)
    .execute(&mut *tx)
    .await?;
    let mut details = audit::diff(
        &json!({"name": before.0, "login": before.1, "url": before.2}),
        &json!({"name": f.name, "login": f.login, "url": f.url}),
    );
    if let Some(o) = details.as_object_mut() {
        // Текст описания в журнал не пишется: туда могли по ошибке вставить пароль.
        if before.3 != f.description {
            o.insert("description".into(), json!("изменено"));
        }
        if sealed.is_some() {
            o.insert("value".into(), json!("изменено"));
        }
    }
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("vault.update", Source::Portal).object("vault_secret", id).details(details),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(item(&core.db, &user, id, "view").await?))
}

#[utoipa::path(delete, path = "/api/helpdesk/vault/{id}", tag = "helpdesk", params(("id" = Uuid, Path)),
    responses((status = 204)))]
async fn delete_secret(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&USE)?;
    let mut tx = core.db.begin().await?;
    lock(&mut tx, id, false).await?;
    let it = item(&mut *tx, &user, id, "manage").await?;
    sqlx::query("DELETE FROM vault_secrets WHERE id = $1").bind(id).execute(&mut *tx).await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("vault.delete", Source::Portal).object("vault_secret", id).details(json!({"name": it.name})),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Самая короткая причина показа: пустая или «.» в журнале ничего не объясняет.
const MIN_REASON: usize = 3;

#[derive(Deserialize, ToSchema, Default)]
#[serde(default)]
pub struct RevealRequest {
    /// Зачем нужно значение (пишется в журнал аудита).
    pub reason: String,
}

#[derive(Serialize, ToSchema)]
pub struct RevealedSecret {
    pub value: String,
}

/// Показать значение. Нужны право `helpdesk.vault.reveal` и доступ `reveal` к секрету; показ — в аудите.
#[utoipa::path(post, path = "/api/helpdesk/vault/{id}/reveal", tag = "helpdesk", params(("id" = Uuid, Path)),
    request_body = RevealRequest, responses((status = 200, body = RevealedSecret), (status = 403)))]
async fn reveal_secret(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<RevealRequest>,
) -> ApiResult<(HeaderMap, Json<RevealedSecret>)> {
    user.require(&USE)?;
    user.require(&REVEAL)?;
    let reason: String = b.reason.trim().chars().filter(|c| !c.is_control()).take(500).collect();
    if reason.chars().count() < MIN_REASON {
        return Err(ApiError::bad_request(
            "reason_required",
            "Укажите, зачем нужно значение: причина попадёт в журнал.",
        ));
    }
    let mut tx = core.db.begin().await?;
    lock(&mut tx, id, true).await?;
    let it = item(&mut *tx, &user, id, "reveal").await?;
    // Значение и запись о показе — в одной транзакции: без записи в журнале значение не уходит.
    let raw: Vec<u8> = sqlx::query_scalar("SELECT sealed FROM vault_secrets WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApiError::NotFound)?;
    let value = unseal(&core, id, &raw)?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("vault.reveal", Source::Portal)
            .object("vault_secret", id)
            .details(json!({"name": it.name, "reason": reason})),
    )
    .await?;
    tx.commit().await?;
    let mut h = HeaderMap::new();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok((h, Json(RevealedSecret { value: value.expose().to_string() })))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct AccessEntry {
    pub user_id: Option<Uuid>,
    pub user_name: Option<String>,
    pub role: Option<String>,
    pub role_name: Option<String>,
    /// `view`, `reveal`, `manage`.
    pub level: String,
}

async fn access_list(db: impl sqlx::PgExecutor<'_>, id: Uuid) -> Result<Vec<AccessEntry>, sqlx::Error> {
    sqlx::query_as(&format!(
        "SELECT va.user_id, (SELECT {USER_NAME} FROM users u JOIN ad_accounts a ON a.id = u.ad_account_id
                  LEFT JOIN persons p ON p.id = u.person_id WHERE u.id = va.user_id) AS user_name,
                r.code AS role, r.name AS role_name, va.level
           FROM vault_access va LEFT JOIN roles r ON r.id = va.role_id
          WHERE va.secret_id = $1
          ORDER BY va.role_id IS NULL, r.name, user_name" // SAFE: константа
    ))
    .bind(id)
    .fetch_all(db)
    .await
}

#[derive(Serialize, ToSchema)]
pub struct VaultAccessList {
    /// Версия списка: сохранение поверх чужой правки — 409.
    pub version: i32,
    pub entries: Vec<AccessEntry>,
}

/// Кому открыт секрет (уровень `manage`).
#[utoipa::path(get, path = "/api/helpdesk/vault/{id}/access", tag = "helpdesk", params(("id" = Uuid, Path)),
    responses((status = 200, body = VaultAccessList)))]
async fn get_secret_access(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<VaultAccessList>> {
    user.require(&USE)?;
    item(&core.db, &user, id, "manage").await?;
    let version: i32 = sqlx::query_scalar("SELECT access_version FROM vault_secrets WHERE id = $1")
        .bind(id)
        .fetch_one(&core.db)
        .await?;
    Ok(Json(VaultAccessList { version, entries: access_list(&core.db, id).await? }))
}

#[derive(Deserialize, ToSchema)]
pub struct AccessInput {
    /// Активный пользователь портала (обычно из прежнего списка доступа).
    pub user_id: Option<Uuid>,
    /// Или сотрудник из справочника с учёткой портала.
    pub person_id: Option<Uuid>,
    /// Или роль портала (код).
    pub role: Option<String>,
    pub level: String,
}

#[derive(Deserialize, ToSchema)]
pub struct VaultAccessUpdate {
    /// Версия списка, который правили.
    pub version: i32,
    pub entries: Vec<AccessInput>,
}

/// Заменить список доступа целиком. Хотя бы одна запись `manage` — у сотрудника, иначе секретом может
/// никто не управлять (роль бывает пустой).
#[utoipa::path(put, path = "/api/helpdesk/vault/{id}/access", tag = "helpdesk", params(("id" = Uuid, Path)),
    request_body = VaultAccessUpdate,
    responses((status = 200, body = VaultAccessList), (status = 400), (status = 409)))]
async fn put_secret_access(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<VaultAccessUpdate>,
) -> ApiResult<Json<VaultAccessList>> {
    user.require(&USE)?;
    if b.entries.len() > MAX_ACCESS {
        return Err(bad("Не больше 100 записей доступа."));
    }
    let mut tx = core.db.begin().await?;
    lock(&mut tx, id, false).await?;
    let it = item(&mut *tx, &user, id, "manage").await?;
    let version: i32 = sqlx::query_scalar("SELECT access_version FROM vault_secrets WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if version != b.version {
        return Err(ApiError::Conflict { current: json!({"version": version}) });
    }
    let before = access_list(&mut *tx, id).await?;
    // Цель: (пользователь, роль); дубли — с наибольшим уровнем.
    let mut rows: Vec<(Option<Uuid>, Option<Uuid>, &str)> = Vec::new();
    for a in &b.entries {
        let Some(level) = LEVELS.iter().copied().find(|l| *l == a.level) else {
            return Err(bad("Уровень доступа — view, reveal или manage."));
        };
        let target: (Option<Uuid>, Option<Uuid>) = match (a.user_id, a.person_id, a.role.as_deref()) {
            (Some(u), None, None) => {
                let ok: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM users WHERE id = $1 AND is_active)")
                    .bind(u)
                    .fetch_one(&mut *tx)
                    .await?;
                if !ok {
                    return Err(bad("Пользователь не найден."));
                }
                (Some(u), None)
            }
            (None, Some(p), None) => {
                let u: Option<Uuid> =
                    sqlx::query_scalar("SELECT id FROM users WHERE person_id = $1 AND is_active ORDER BY last_login_at DESC NULLS LAST LIMIT 1")
                        .bind(p)
                        .fetch_optional(&mut *tx)
                        .await?;
                (Some(u.ok_or_else(|| bad("У сотрудника нет учётки в портале: доступ ему не открыть."))?), None)
            }
            (None, None, Some(code)) => {
                let r: Option<Uuid> = sqlx::query_scalar("SELECT id FROM roles WHERE code = $1")
                    .bind(code)
                    .fetch_optional(&mut *tx)
                    .await?;
                (None, Some(r.ok_or_else(|| bad("Нет такой роли."))?))
            }
            _ => return Err(bad("Укажите либо сотрудника, либо роль.")),
        };
        match rows.iter_mut().find(|r| (r.0, r.1) == target) {
            Some(r) if rank(level) > rank(r.2) => r.2 = level,
            Some(_) => {}
            None => rows.push((target.0, target.1, level)),
        }
    }
    if !rows.iter().any(|r| r.2 == "manage" && r.0.is_some()) {
        return Err(bad("Кто-то должен управлять секретом: оставьте хотя бы один доступ «Управление» у сотрудника."));
    }
    sqlx::query("DELETE FROM vault_access WHERE secret_id = $1").bind(id).execute(&mut *tx).await?;
    for (u, r, level) in &rows {
        sqlx::query("INSERT INTO vault_access (secret_id, user_id, role_id, level) VALUES ($1, $2, $3, $4)")
            .bind(id)
            .bind(u)
            .bind(r)
            .bind(level)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE vault_secrets SET access_version = access_version + 1 WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    let summary = |list: &[AccessEntry]| -> Vec<String> {
        list.iter()
            .map(|e| {
                let who =
                    e.role.clone().map(|r| format!("role:{r}")).or_else(|| e.user_id.map(|u| format!("user:{u}")));
                format!("{}={}", who.unwrap_or_default(), e.level)
            })
            .collect()
    };
    let after = access_list(&mut *tx, id).await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("vault.access", Source::Portal)
            .object("vault_secret", id)
            .details(json!({"name": it.name, "before": summary(&before), "after": summary(&after)})),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(VaultAccessList { version: version + 1, entries: after }))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct VaultRole {
    pub code: String,
    pub name: String,
}

/// Роли портала для списка доступа.
#[utoipa::path(get, path = "/api/helpdesk/vault-roles", tag = "helpdesk", responses((status = 200, body = Vec<VaultRole>)))]
async fn vault_roles(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<VaultRole>>> {
    user.require(&USE)?;
    Ok(Json(sqlx::query_as("SELECT code, name FROM roles ORDER BY name").fetch_all(&core.db).await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        assert!(rank("manage") > rank("reveal") && rank("reveal") > rank("view") && rank("view") > rank("x"));
        let f =
            |name: &str, url: &str| clean(&SecretFields { name: name.into(), url: url.into(), ..Default::default() });
        assert!(f("Сервер мониторинга", "https://zabbix.local").is_ok());
        assert!(f(" ", "").is_err());
        assert!(f("a\nb", "").is_err());
        assert!(f("ok", "https://x/\u{0}").is_err());
        assert!(check_value(&Secret::new("")).is_err());
        assert!(check_value(&Secret::new("x".repeat(MAX_VALUE + 1))).is_err());
        assert!(
            check_value(&Secret::new("-----BEGIN KEY-----\nabc\n-----END KEY-----")).is_ok(),
            "ключи — в несколько строк"
        );
    }
}
