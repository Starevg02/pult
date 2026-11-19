//! HTTP API справочника.

use crate::{
    ami,
    directory::{self, Dict, DictInput, DictItem},
    persons::{self, Duplicate, FieldSource, ListFilter, Person, PersonInput},
    telephony::{self, CallGateway, CallState, TelephonySettings},
};
use axum::{
    Json,
    extract::{FromRef, Path, Query, State},
    http::StatusCode,
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Module, Permission, Secret,
    audit::{self, Entry, Source},
    rbac::{ROLE_PHONEBOOK_EDITOR, ROLE_SUPERADMIN},
    settings,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub mod perm {
    use super::*;
    pub const EDIT: Permission = Permission {
        code: "phonebook.edit",
        description: "Править справочник: сотрудники, подразделения, должности, архив",
        default_roles: &[ROLE_PHONEBOOK_EDITOR, ROLE_SUPERADMIN],
    };
    pub const ADMIN: Permission = Permission {
        code: "phonebook.admin",
        description: "Настраивать справочник: источники полей, телефония, шаблон письма",
        default_roles: &[ROLE_SUPERADMIN],
    };
}

pub struct PhonebookModule;

impl<S> Module<S> for PhonebookModule
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
{
    fn name(&self) -> &'static str {
        "phonebook"
    }

    fn routes(&self) -> OpenApiRouter<S> {
        OpenApiRouter::new()
            .routes(routes!(list_persons, create_person))
            .routes(routes!(get_person, update_person))
            .routes(routes!(archive_person))
            .routes(routes!(restore_person))
            .routes(routes!(find_duplicates))
            .routes(routes!(add_favorite, remove_favorite))
            .routes(routes!(list_departments, create_department))
            .routes(routes!(update_department, delete_department))
            .routes(routes!(archive_department))
            .routes(routes!(list_positions, create_position))
            .routes(routes!(update_position, delete_position))
            .routes(routes!(archive_position))
            .routes(routes!(get_config))
            .routes(routes!(get_field_sources, put_field_sources))
            .routes(routes!(get_telephony, put_telephony))
            .routes(routes!(start_call))
            .routes(routes!(call_state))
            .routes(routes!(list_imports))
    }

    fn permissions(&self) -> Vec<Permission> {
        vec![perm::EDIT, perm::ADMIN]
    }
}

// ------------------------------------------------------------------ Сотрудники

#[derive(Deserialize, IntoParams)]
pub struct PersonQuery {
    /// Строка поиска (ФИО, должность, подразделение, email, телефон).
    pub q: Option<String>,
    pub department_id: Option<Uuid>,
    #[serde(default)]
    pub favorites: bool,
    /// Архив (только редактору).
    #[serde(default)]
    pub archived: bool,
    pub limit: Option<i64>,
}

/// Справочник: поиск и фильтры.
#[utoipa::path(get, path = "/api/phonebook/persons", tag = "phonebook", params(PersonQuery),
    responses((status = 200, body = Vec<Person>)))]
async fn list_persons(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<PersonQuery>,
) -> ApiResult<Json<Vec<Person>>> {
    if q.archived {
        user.require(&perm::EDIT)?;
    }
    let f = ListFilter {
        query: q.q.unwrap_or_default().chars().take(200).collect(),
        department_id: q.department_id,
        favorites_only: q.favorites,
        archived: q.archived,
        limit: q.limit.unwrap_or(2000),
    };
    Ok(Json(persons::list(&core.db, user.user_id, &f).await?))
}

/// Карточка сотрудника.
#[utoipa::path(get, path = "/api/phonebook/persons/{id}", tag = "phonebook", params(("id" = Uuid, Path)),
    responses((status = 200, body = Person), (status = 404)))]
async fn get_person(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Person>> {
    let p = persons::get(&core.db, user.user_id, id).await?.ok_or(ApiError::NotFound)?;
    if p.is_archived && !user.grants.has(&perm::EDIT) {
        return Err(ApiError::NotFound);
    }
    Ok(Json(p))
}

#[derive(Deserialize, ToSchema)]
pub struct NewPerson {
    pub person: PersonInput,
}

/// Новый сотрудник (редактор справочника). Возможные дубли проверяются заранее (`/duplicates`).
#[utoipa::path(post, path = "/api/phonebook/persons", tag = "phonebook", request_body = NewPerson,
    responses((status = 201, body = Person)))]
async fn create_person(
    State(core): State<Core>,
    user: CurrentUser,
    Json(body): Json<NewPerson>,
) -> ApiResult<(StatusCode, Json<Person>)> {
    user.require(&perm::EDIT)?;
    let p = body.person.normalized().map_err(|m| ApiError::bad_request("invalid_person", m))?;
    let mut tx = core.db.begin().await?;
    let id = persons::create(&mut tx, &p, "manual", None, None).await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("person.create", Source::Portal).object("person", id).details(p.as_json()),
    )
    .await?;
    tx.commit().await?;
    let person = persons::get(&core.db, user.user_id, id).await?.ok_or(ApiError::NotFound)?;
    Ok((StatusCode::CREATED, Json(person)))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdatePerson {
    /// Версия, которую видел пользователь (оптимистичная блокировка).
    pub version: i32,
    pub person: PersonInput,
}

/// Правка. Устаревшая версия — 409 с текущей записью; поле из AD у связанного сотрудника — 400.
#[utoipa::path(put, path = "/api/phonebook/persons/{id}", tag = "phonebook", params(("id" = Uuid, Path)),
    request_body = UpdatePerson,
    responses((status = 200, body = Person), (status = 409, body = pult_core::error::ErrorBody)))]
async fn update_person(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdatePerson>,
) -> ApiResult<Json<Person>> {
    user.require(&perm::EDIT)?;
    let p = body.person.normalized().map_err(|m| ApiError::bad_request("invalid_person", m))?;
    let mut tx = core.db.begin().await?;
    let before = persons::get(&mut *tx, user.user_id, id).await?.ok_or(ApiError::NotFound)?;
    if before.version != body.version {
        return Err(ApiError::Conflict { current: json!(before) });
    }
    let (old, new) = (before.editable(), p.as_json());
    if before.ad_login.is_some() {
        let sources = persons::field_sources(&mut *tx).await?;
        if let Some((field, _)) =
            sources.iter().find(|(f, s)| **s == FieldSource::Ad && old.get(f.as_str()) != new.get(f.as_str()))
        {
            return Err(ApiError::bad_request(
                "field_from_ad",
                format!("Поле «{field}» берётся из AD и в портале не меняется."),
            ));
        }
    }
    if !persons::update(&mut tx, id, body.version, &p).await? {
        let current = persons::get(&mut *tx, user.user_id, id).await?;
        return Err(ApiError::Conflict { current: json!(current) });
    }
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("person.update", Source::Portal).object("person", id).details(audit::diff(&old, &new)),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(persons::get(&core.db, user.user_id, id).await?.ok_or(ApiError::NotFound)?))
}

#[derive(Deserialize, ToSchema)]
pub struct VersionBody {
    pub version: i32,
}

async fn set_archived(
    core: &Core,
    user: &CurrentUser,
    id: Uuid,
    version: i32,
    archived: bool,
) -> ApiResult<Json<Person>> {
    user.require(&perm::EDIT)?;
    let mut tx = core.db.begin().await?;
    if !persons::set_archived(&mut *tx, id, Some(version), archived, "manual").await? {
        let current = persons::get(&mut *tx, user.user_id, id).await?.ok_or(ApiError::NotFound)?;
        return Err(ApiError::Conflict { current: json!(current) });
    }
    let action = if archived { "person.archive" } else { "person.restore" };
    audit::record(&mut *tx, &user.actor, Entry::new(action, Source::Portal).object("person", id)).await?;
    tx.commit().await?;
    Ok(Json(persons::get(&core.db, user.user_id, id).await?.ok_or(ApiError::NotFound)?))
}

/// В архив.
#[utoipa::path(post, path = "/api/phonebook/persons/{id}/archive", tag = "phonebook", params(("id" = Uuid, Path)),
    request_body = VersionBody, responses((status = 200, body = Person)))]
async fn archive_person(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<VersionBody>,
) -> ApiResult<Json<Person>> {
    set_archived(&core, &user, id, b.version, true).await
}

/// Вернуть из архива.
#[utoipa::path(post, path = "/api/phonebook/persons/{id}/restore", tag = "phonebook", params(("id" = Uuid, Path)),
    request_body = VersionBody, responses((status = 200, body = Person)))]
async fn restore_person(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<VersionBody>,
) -> ApiResult<Json<Person>> {
    set_archived(&core, &user, id, b.version, false).await
}

#[derive(Deserialize, ToSchema)]
pub struct DuplicateQuery {
    pub person: PersonInput,
    /// При правке — сам сотрудник.
    pub exclude_id: Option<Uuid>,
}

/// Возможные дубли: совпадение ФИО, рабочего или мобильного телефона (предупреждение, не запрет).
#[utoipa::path(post, path = "/api/phonebook/duplicates", tag = "phonebook", request_body = DuplicateQuery,
    responses((status = 200, body = Vec<Duplicate>)))]
async fn find_duplicates(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<DuplicateQuery>,
) -> ApiResult<Json<Vec<Duplicate>>> {
    user.require(&perm::EDIT)?;
    Ok(Json(persons::duplicates(&core.db, &b.person, b.exclude_id).await?))
}

/// В избранное.
#[utoipa::path(put, path = "/api/phonebook/favorites/{id}", tag = "phonebook", params(("id" = Uuid, Path)),
    responses((status = 204)))]
async fn add_favorite(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    sqlx::query(
        "INSERT INTO person_favorites (user_id, person_id) SELECT $1, id FROM persons WHERE id = $2 ON CONFLICT DO NOTHING",
    )
    .bind(user.user_id)
    .bind(id)
    .execute(&core.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Убрать из избранного.
#[utoipa::path(delete, path = "/api/phonebook/favorites/{id}", tag = "phonebook", params(("id" = Uuid, Path)),
    responses((status = 204)))]
async fn remove_favorite(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    sqlx::query("DELETE FROM person_favorites WHERE user_id = $1 AND person_id = $2")
        .bind(user.user_id)
        .bind(id)
        .execute(&core.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Подразделения и должности

#[derive(Deserialize, IntoParams)]
pub struct DictQuery {
    #[serde(default)]
    pub archived: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct DictUpdate {
    pub version: i32,
    pub name: String,
    pub parent_id: Option<Uuid>,
}

#[derive(Deserialize, ToSchema)]
pub struct DictArchive {
    pub version: i32,
    pub archived: bool,
}

async fn dict_list(core: &Core, user: &CurrentUser, d: Dict, archived: bool) -> ApiResult<Json<Vec<DictItem>>> {
    if archived {
        user.require(&perm::EDIT)?;
    }
    Ok(Json(directory::list(&core.db, d, archived).await?))
}

async fn dict_create(
    core: &Core,
    user: &CurrentUser,
    d: Dict,
    b: DictInput,
) -> ApiResult<(StatusCode, Json<DictItem>)> {
    user.require(&perm::EDIT)?;
    let name = directory::clean_name(&b.name).map_err(|m| ApiError::bad_request("invalid_name", m))?;
    if directory::name_taken(&core.db, d, &name, None).await? {
        return Err(ApiError::bad_request("name_taken", format!("«{name}» уже есть.")));
    }
    let mut tx = core.db.begin().await?;
    let id: Uuid = match d {
        Dict::Departments => {
            sqlx::query_scalar("INSERT INTO departments (name, parent_id) VALUES ($1, $2) RETURNING id")
                .bind(&name)
                .bind(b.parent_id)
                .fetch_one(&mut *tx)
                .await?
        }
        Dict::Positions => {
            sqlx::query_scalar("INSERT INTO positions (name) VALUES ($1) RETURNING id")
                .bind(&name)
                .fetch_one(&mut *tx)
                .await?
        }
    };
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("phonebook.dict.create", Source::Portal).object(d.object_type(), id).details(json!({"name": name})),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(directory::get(&core.db, d, id).await?.ok_or(ApiError::NotFound)?)))
}

async fn dict_update(core: &Core, user: &CurrentUser, d: Dict, id: Uuid, b: DictUpdate) -> ApiResult<Json<DictItem>> {
    user.require(&perm::EDIT)?;
    let name = directory::clean_name(&b.name).map_err(|m| ApiError::bad_request("invalid_name", m))?;
    if b.parent_id == Some(id) {
        return Err(ApiError::bad_request("invalid_parent", "Подразделение не может входить само в себя."));
    }
    let before = directory::get(&core.db, d, id).await?.ok_or(ApiError::NotFound)?;
    if directory::name_taken(&core.db, d, &name, Some(id)).await? {
        return Err(ApiError::bad_request("name_taken", format!("«{name}» уже есть.")));
    }
    let mut tx = core.db.begin().await?;
    let sql = match d {
        Dict::Departments => {
            "UPDATE departments SET name = $3, parent_id = $4, version = version + 1 WHERE id = $1 AND version = $2"
        }
        Dict::Positions => {
            "UPDATE positions SET name = $3, version = version + 1 WHERE id = $1 AND version = $2 AND $4::uuid IS NULL"
        }
    };
    let parent = if d == Dict::Departments { b.parent_id } else { None };
    let n = sqlx::query(sql).bind(id).bind(b.version).bind(&name).bind(parent).execute(&mut *tx).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::Conflict { current: json!(before) });
    }
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("phonebook.dict.update", Source::Portal).object(d.object_type(), id).details(audit::diff(
            &json!({"name": before.name, "parent_id": before.parent_id}),
            &json!({"name": name, "parent_id": parent}),
        )),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(directory::get(&core.db, d, id).await?.ok_or(ApiError::NotFound)?))
}

async fn dict_archive(core: &Core, user: &CurrentUser, d: Dict, id: Uuid, b: DictArchive) -> ApiResult<Json<DictItem>> {
    user.require(&perm::EDIT)?;
    let before = directory::get(&core.db, d, id).await?.ok_or(ApiError::NotFound)?;
    let mut tx = core.db.begin().await?;
    let sql =
        format!("UPDATE {} SET is_archived = $3, version = version + 1 WHERE id = $1 AND version = $2", d.table()); // SAFE: имя таблицы из Dict
    if sqlx::query(&sql).bind(id).bind(b.version).bind(b.archived).execute(&mut *tx).await?.rows_affected() == 0 {
        return Err(ApiError::Conflict { current: json!(before) });
    }
    let action = if b.archived { "phonebook.dict.archive" } else { "phonebook.dict.restore" };
    audit::record(&mut *tx, &user.actor, Entry::new(action, Source::Portal).object(d.object_type(), id)).await?;
    tx.commit().await?;
    Ok(Json(directory::get(&core.db, d, id).await?.ok_or(ApiError::NotFound)?))
}

async fn dict_delete(core: &Core, user: &CurrentUser, d: Dict, id: Uuid) -> ApiResult<StatusCode> {
    user.require(&perm::EDIT)?;
    let used_sql = format!("SELECT EXISTS (SELECT 1 FROM persons WHERE {} = $1)", d.person_column()); // SAFE: имя столбца из Dict
    let used: bool = sqlx::query_scalar(&used_sql).bind(id).fetch_one(&core.db).await?;
    let child: bool = d == Dict::Departments
        && sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM departments WHERE parent_id = $1)")
            .bind(id)
            .fetch_one(&core.db)
            .await?;
    if used || child {
        return Err(ApiError::bad_request("in_use", "Используется — можно только отправить в архив."));
    }
    let mut tx = core.db.begin().await?;
    let del = format!("DELETE FROM {} WHERE id = $1 RETURNING name", d.table()); // SAFE: имя таблицы из Dict
    let name: Option<String> = sqlx::query_scalar(&del).bind(id).fetch_optional(&mut *tx).await?;
    let name = name.ok_or(ApiError::NotFound)?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("phonebook.dict.delete", Source::Portal).object(d.object_type(), id).details(json!({"name": name})),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Подразделения со счётчиками сотрудников.
#[utoipa::path(get, path = "/api/phonebook/departments", tag = "phonebook", params(DictQuery),
    responses((status = 200, body = Vec<DictItem>)))]
async fn list_departments(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<DictQuery>,
) -> ApiResult<Json<Vec<DictItem>>> {
    dict_list(&core, &user, Dict::Departments, q.archived).await
}

#[utoipa::path(post, path = "/api/phonebook/departments", tag = "phonebook", request_body = DictInput,
    responses((status = 201, body = DictItem)))]
async fn create_department(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<DictInput>,
) -> ApiResult<(StatusCode, Json<DictItem>)> {
    dict_create(&core, &user, Dict::Departments, b).await
}

#[utoipa::path(put, path = "/api/phonebook/departments/{id}", tag = "phonebook", params(("id" = Uuid, Path)),
    request_body = DictUpdate, responses((status = 200, body = DictItem)))]
async fn update_department(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<DictUpdate>,
) -> ApiResult<Json<DictItem>> {
    dict_update(&core, &user, Dict::Departments, id, b).await
}

#[utoipa::path(delete, path = "/api/phonebook/departments/{id}", tag = "phonebook", params(("id" = Uuid, Path)),
    responses((status = 204)))]
async fn delete_department(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    dict_delete(&core, &user, Dict::Departments, id).await
}

#[utoipa::path(post, path = "/api/phonebook/departments/{id}/archive", tag = "phonebook", params(("id" = Uuid, Path)),
    request_body = DictArchive, responses((status = 200, body = DictItem)))]
async fn archive_department(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<DictArchive>,
) -> ApiResult<Json<DictItem>> {
    dict_archive(&core, &user, Dict::Departments, id, b).await
}

/// Должности со счётчиками сотрудников.
#[utoipa::path(get, path = "/api/phonebook/positions", tag = "phonebook", params(DictQuery),
    responses((status = 200, body = Vec<DictItem>)))]
async fn list_positions(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<DictQuery>,
) -> ApiResult<Json<Vec<DictItem>>> {
    dict_list(&core, &user, Dict::Positions, q.archived).await
}

#[utoipa::path(post, path = "/api/phonebook/positions", tag = "phonebook", request_body = DictInput,
    responses((status = 201, body = DictItem)))]
async fn create_position(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<DictInput>,
) -> ApiResult<(StatusCode, Json<DictItem>)> {
    dict_create(&core, &user, Dict::Positions, b).await
}

#[utoipa::path(put, path = "/api/phonebook/positions/{id}", tag = "phonebook", params(("id" = Uuid, Path)),
    request_body = DictUpdate, responses((status = 200, body = DictItem)))]
async fn update_position(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<DictUpdate>,
) -> ApiResult<Json<DictItem>> {
    dict_update(&core, &user, Dict::Positions, id, b).await
}

#[utoipa::path(delete, path = "/api/phonebook/positions/{id}", tag = "phonebook", params(("id" = Uuid, Path)),
    responses((status = 204)))]
async fn delete_position(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    dict_delete(&core, &user, Dict::Positions, id).await
}

#[utoipa::path(post, path = "/api/phonebook/positions/{id}/archive", tag = "phonebook", params(("id" = Uuid, Path)),
    request_body = DictArchive, responses((status = 200, body = DictItem)))]
async fn archive_position(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<DictArchive>,
) -> ApiResult<Json<DictItem>> {
    dict_archive(&core, &user, Dict::Positions, id, b).await
}

// ------------------------------------------------------------------ Настройки

#[derive(Serialize, ToSchema)]
pub struct PhonebookConfig {
    /// Шаблон «Написать письмо» с `{email}`.
    pub mail_template: String,
    /// Показывать ли кнопку «Позвонить».
    pub call_enabled: bool,
    pub field_sources: BTreeMap<String, FieldSource>,
    pub can_edit: bool,
}

/// Что нужно экрану справочника: шаблон письма, телефония, источники полей, права.
#[utoipa::path(get, path = "/api/phonebook/config", tag = "phonebook", responses((status = 200, body = PhonebookConfig)))]
async fn get_config(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<PhonebookConfig>> {
    let t: TelephonySettings = settings::load(&core.db).await?;
    Ok(Json(PhonebookConfig {
        mail_template: t.mail_template,
        call_enabled: t.enabled,
        field_sources: persons::field_sources(&core.db).await?,
        can_edit: user.grants.has(&perm::EDIT),
    }))
}

/// Источники полей справочника (AD или портал).
#[utoipa::path(get, path = "/api/admin/phonebook/field-sources", tag = "admin",
    responses((status = 200, body = BTreeMap<String, FieldSource>)))]
async fn get_field_sources(
    State(core): State<Core>,
    user: CurrentUser,
) -> ApiResult<Json<BTreeMap<String, FieldSource>>> {
    user.require(&perm::ADMIN)?;
    Ok(Json(persons::field_sources(&core.db).await?))
}

#[utoipa::path(put, path = "/api/admin/phonebook/field-sources", tag = "admin", request_body = BTreeMap<String, FieldSource>,
    responses((status = 200, body = BTreeMap<String, FieldSource>)))]
async fn put_field_sources(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<BTreeMap<String, FieldSource>>,
) -> ApiResult<Json<BTreeMap<String, FieldSource>>> {
    user.require(&perm::ADMIN)?;
    if let Some(bad) = b.keys().find(|k| !persons::FIELDS.contains(&k.as_str())) {
        return Err(ApiError::bad_request("unknown_field", format!("Неизвестное поле: {bad}")));
    }
    let mut tx = core.db.begin().await?;
    let before = persons::field_sources(&mut *tx).await?;
    for (field, source) in &b {
        let s = match source {
            FieldSource::Ad => "ad",
            FieldSource::Portal => "portal",
        };
        sqlx::query(
            "INSERT INTO person_field_sources (field, source) VALUES ($1, $2)
             ON CONFLICT (field) DO UPDATE SET source = EXCLUDED.source, updated_at = now()",
        )
        .bind(field)
        .bind(s)
        .execute(&mut *tx)
        .await?;
    }
    let after = persons::field_sources(&mut *tx).await?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("settings.update", Source::Portal)
            .object("settings", "field_sources")
            .details(audit::diff(&json!(before), &json!(after))),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(after))
}

/// Токен CallGateway или пароль AMI — отдельно и зашифрованным; наружу не отдаётся.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct TelephonyToken {
    /// base64(шифротекст).
    sealed: Option<String>,
}

impl settings::SettingsGroup for TelephonyToken {
    const KEY: &'static str = "telephony_token";
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct TelephonyView {
    pub settings: TelephonySettings,
    /// Токен задан (значение не показывается).
    pub has_token: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct TelephonyUpdate {
    pub settings: TelephonySettings,
    /// Новый токен; не передан — прежний; пустая строка — удалить.
    pub token: Option<String>,
}

/// Телефония и шаблон письма.
#[utoipa::path(get, path = "/api/admin/telephony", tag = "admin", responses((status = 200, body = TelephonyView)))]
async fn get_telephony(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<TelephonyView>> {
    user.require(&perm::ADMIN)?;
    let token: TelephonyToken = settings::load(&core.db).await?;
    Ok(Json(TelephonyView { settings: settings::load(&core.db).await?, has_token: token.sealed.is_some() }))
}

#[utoipa::path(put, path = "/api/admin/telephony", tag = "admin", request_body = TelephonyUpdate,
    responses((status = 200, body = TelephonyView)))]
async fn put_telephony(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<TelephonyUpdate>,
) -> ApiResult<Json<TelephonyView>> {
    use base64::Engine;
    user.require(&perm::ADMIN)?;
    b.settings.validate().map_err(|m| ApiError::bad_request("invalid_settings", m))?;
    let mut tx = core.db.begin().await?;
    let before: TelephonySettings = settings::load(&mut *tx).await?;
    let mut token: TelephonyToken = settings::load(&mut *tx).await?;
    // Сохранённый токен или пароль не уходит на другой адрес или другому пользователю.
    let target = |t: &TelephonySettings| {
        (
            t.provider.clone(),
            t.base_url.trim_end_matches('/').to_string(),
            t.ami_host.clone(),
            t.ami_port,
            t.ami_user.clone(),
        )
    };
    if token.sealed.is_some() && b.token.is_none() && target(&before) != target(&b.settings) {
        return Err(ApiError::bad_request(
            "secret_required",
            "Адрес или пользователь телефонии изменились — введите токен (пароль) заново или удалите его.",
        ));
    }
    settings::save(&mut *tx, &b.settings, Some(user.user_id)).await?;
    let mut details = audit::diff(&json!(before), &json!(b.settings));
    if let Some(t) = &b.token {
        token.sealed = if t.is_empty() {
            None
        } else {
            let sealed = core.secrets.encrypt(&Secret::new(t.clone())).map_err(ApiError::internal)?;
            Some(base64::engine::general_purpose::STANDARD.encode(sealed))
        };
        settings::save(&mut *tx, &token, Some(user.user_id)).await?;
        if let Some(o) = details.as_object_mut() {
            o.insert("token".into(), json!(if t.is_empty() { "удалён" } else { "изменён" }));
        }
    }
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("settings.update", Source::Portal).object("settings", "telephony").details(details),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(TelephonyView { settings: b.settings, has_token: token.sealed.is_some() }))
}

/// Как звонить: через CallGateway или напрямую в AMI.
enum Provider {
    Gateway(CallGateway),
    Ami(ami::AmiConfig),
}

async fn provider(core: &Core) -> ApiResult<Provider> {
    use base64::Engine;
    let t: TelephonySettings = settings::load(&core.db).await?;
    if !t.enabled {
        return Err(ApiError::bad_request("telephony_disabled", telephony::CallError::Disabled.to_string()));
    }
    let token: TelephonyToken = settings::load(&core.db).await?;
    let token = match token.sealed {
        Some(s) => {
            let raw = base64::engine::general_purpose::STANDARD.decode(s).map_err(ApiError::internal)?;
            Some(core.secrets.decrypt(&raw).map_err(ApiError::internal)?)
        }
        None => None,
    };
    if t.provider == "ami" {
        let secret = token.ok_or_else(|| call_err(telephony::CallError::Disabled))?;
        return Ok(Provider::Ami(ami::AmiConfig {
            host: t.ami_host,
            port: t.ami_port,
            user: t.ami_user,
            secret,
            channel: t.ami_channel,
            context: t.ami_context,
        }));
    }
    Ok(Provider::Gateway(CallGateway::new(&t.base_url, token).map_err(ApiError::internal)?))
}

fn call_err(e: telephony::CallError) -> ApiError {
    use telephony::CallError as E;
    match e {
        E::Unavailable => ApiError::Unavailable { code: "pbx_unavailable", message: e.to_string() },
        E::Busy => ApiError::bad_request("call_busy", e.to_string()),
        E::Denied => ApiError::bad_request("call_denied", e.to_string()),
        E::NoCallerExt => ApiError::bad_request("call_no_extension", e.to_string()),
        E::NotFound => ApiError::NotFound,
        other => ApiError::bad_request("call_failed", other.to_string()),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct StartCall {
    pub person_id: Uuid,
}

#[derive(Serialize, ToSchema)]
pub struct CallStarted {
    pub id: String,
}

/// Позвонить сотруднику на внутренний номер (с сервера портала: CallGateway или AMI).
#[utoipa::path(post, path = "/api/phonebook/calls", tag = "phonebook", request_body = StartCall,
    responses((status = 200, body = CallStarted)))]
async fn start_call(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<StartCall>,
) -> ApiResult<Json<CallStarted>> {
    let gw = provider(&core).await?;
    let row: Option<(Option<String>, Option<i32>)> =
        sqlx::query_as("SELECT internal_ext, legacy_phonebook_id FROM persons WHERE id = $1 AND NOT is_archived")
            .bind(b.person_id)
            .fetch_optional(&core.db)
            .await?;
    let (ext, legacy) = row.ok_or(ApiError::NotFound)?;
    if !telephony::callable(ext.as_deref()) {
        return Err(call_err(telephony::CallError::NotInternal));
    }
    let id = match gw {
        Provider::Ami(cfg) => {
            // Сначала звонит телефон самого звонящего — его внутренний номер из его карточки.
            let mine: Option<String> = match user.person_id {
                Some(p) => sqlx::query_scalar("SELECT internal_ext FROM persons WHERE id = $1")
                    .bind(p)
                    .fetch_optional(&core.db)
                    .await?
                    .flatten(),
                None => None,
            };
            let mine =
                mine.filter(|e| telephony::callable(Some(e))).ok_or(call_err(telephony::CallError::NoCallerExt))?;
            let target = ext.unwrap_or_default();
            if mine == target {
                return Err(ApiError::bad_request("call_self", "Это ваш собственный номер."));
            }
            ami::start_call(cfg, user.user_id, &mine, &target).await.map_err(call_err)?
        }
        Provider::Gateway(gw) => {
            // CallGateway знает сотрудников по номеру Phonebook.
            let legacy = legacy.ok_or_else(|| {
                ApiError::bad_request("call_unknown", "Сотрудник не перенесён из Phonebook — CallGateway его не знает.")
            })?;
            gw.start_call(&user.login, legacy).await.map_err(call_err)?
        }
    };
    audit::record(&core.db, &user.actor, Entry::new("phonebook.call", Source::Portal).object("person", b.person_id))
        .await?;
    Ok(Json(CallStarted { id }))
}

/// Состояние звонка (окно звонка опрашивает раз в секунду).
#[utoipa::path(get, path = "/api/phonebook/calls/{id}", tag = "phonebook", params(("id" = String, Path)),
    responses((status = 200, body = CallState)))]
async fn call_state(State(core): State<Core>, user: CurrentUser, Path(id): Path<String>) -> ApiResult<Json<CallState>> {
    Ok(Json(match provider(&core).await? {
        Provider::Ami(_) => {
            let mut s = ami::state(user.user_id, &id).map_err(call_err)?;
            s.error = s.error.map(|e| pult_core::l10n::tr(&e).into_owned());
            s
        }
        Provider::Gateway(gw) => gw.call_state(&id).await.map_err(call_err)?,
    }))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ImportRun {
    pub id: Uuid,
    pub source: String,
    pub dry_run: bool,
    #[schema(value_type = Object)]
    pub counts: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub started_at: time::OffsetDateTime,
}

/// Журнал переносов из PhonebookDB.
#[utoipa::path(get, path = "/api/admin/phonebook/imports", tag = "admin", responses((status = 200, body = Vec<ImportRun>)))]
async fn list_imports(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<ImportRun>>> {
    user.require(&perm::ADMIN)?;
    Ok(Json(
        sqlx::query_as("SELECT id, source, dry_run, counts, started_at FROM phonebook_import_runs ORDER BY started_at DESC LIMIT 50")
            .fetch_all(&core.db)
            .await?,
    ))
}
