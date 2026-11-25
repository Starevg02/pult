//! CMDB поверх каталога оборудования: связи между объектами (виды связей задаёт администратор),
//! анализ влияния и карта зависимостей, предложения связей с подтверждением инженером,
//! жизненный цикл по типам и учёт лицензий ПО по программам из отчётов агента.

use crate::{objects::MANAGE_ASSETS, perm, tickets::OPEN_STATUSES};
use axum::{
    Json,
    extract::{FromRef, Path, State},
    http::StatusCode,
};
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser,
    audit::{self, Entry, Source},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(list_relation_types, create_relation_type))
        .routes(routes!(update_relation_type))
        .routes(routes!(asset_relations, add_relation))
        .routes(routes!(remove_relation))
        .routes(routes!(asset_impact))
        .routes(routes!(asset_map))
        .routes(routes!(relation_suggestions))
        .routes(routes!(asset_alerts))
        .routes(routes!(service_status))
        .routes(routes!(get_lifecycle, put_lifecycle))
        .routes(routes!(list_licenses, create_license))
        .routes(routes!(get_license, update_license, delete_license))
}

// ------------------------------------------------------------------ Виды связей

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct RelationType {
    pub id: Uuid,
    /// Как читается связь от объекта: «Зависит от».
    pub name: String,
    /// Как она читается у второго объекта: «Нужен для».
    pub reverse_name: String,
    /// Отказ второго объекта затрагивает первый.
    pub impact: bool,
    pub is_active: bool,
    pub count: i64,
}

#[derive(Deserialize, ToSchema)]
pub struct RelationTypeInput {
    pub name: String,
    pub reverse_name: String,
    #[serde(default = "yes")]
    pub impact: bool,
    #[serde(default = "yes")]
    pub is_active: bool,
}

fn yes() -> bool {
    true
}

fn bad(m: &str) -> ApiError {
    ApiError::bad_request("invalid_relation", m.to_owned())
}

fn check_type(b: &RelationTypeInput) -> ApiResult<(String, String)> {
    let name = b.name.trim();
    let reverse = b.reverse_name.trim();
    if name.is_empty() || reverse.is_empty() || name.chars().count() > 100 || reverse.chars().count() > 100 {
        return Err(bad("Укажите оба названия связи, до 100 знаков."));
    }
    Ok((name.to_owned(), reverse.to_owned()))
}

#[utoipa::path(get, path = "/api/helpdesk/asset-relation-types", tag = "helpdesk", responses((status = 200, body = Vec<RelationType>)))]
async fn list_relation_types(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<RelationType>>> {
    user.require(&perm::WORK)?;
    Ok(Json(
        sqlx::query_as(
            "SELECT t.id, t.name, t.reverse_name, t.impact, t.is_active,
                    (SELECT count(*) FROM asset_relations r WHERE r.type_id = t.id) AS count
               FROM asset_relation_types t ORDER BY t.created_at",
        )
        .fetch_all(&core.db)
        .await?,
    ))
}

#[utoipa::path(post, path = "/api/admin/helpdesk/asset-relation-types", tag = "helpdesk-admin", request_body = RelationTypeInput, responses((status = 201, body = Object)))]
async fn create_relation_type(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<RelationTypeInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&perm::ADMIN)?;
    let (name, reverse) = check_type(&b)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO asset_relation_types (name, reverse_name, impact, is_active) VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(name)
    .bind(reverse)
    .bind(b.impact)
    .bind(b.is_active)
    .fetch_one(&core.db)
    .await
    .map_err(crate::objects::dup)?;
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("asset_relation_type.create", Source::Portal).object("asset_relation_type", id),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[utoipa::path(put, path = "/api/admin/helpdesk/asset-relation-types/{id}", tag = "helpdesk-admin", params(("id" = Uuid, Path)), request_body = RelationTypeInput, responses((status = 204)))]
async fn update_relation_type(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<RelationTypeInput>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let (name, reverse) = check_type(&b)?;
    let n = sqlx::query(
        "UPDATE asset_relation_types SET name = $2, reverse_name = $3, impact = $4, is_active = $5 WHERE id = $1",
    )
    .bind(id)
    .bind(name)
    .bind(reverse)
    .bind(b.impact)
    .bind(b.is_active)
    .execute(&core.db)
    .await
    .map_err(crate::objects::dup)?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(
        &core.db,
        &user.actor,
        Entry::new("asset_relation_type.update", Source::Portal).object("asset_relation_type", id),
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Связи объекта

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct AssetRelation {
    pub id: Uuid,
    pub type_id: Uuid,
    /// Название связи так, как она читается у этого объекта.
    pub label: String,
    /// `out` — связь идёт от этого объекта, `in` — к нему.
    pub direction: String,
    pub other_id: Uuid,
    pub other_name: String,
    pub other_type: String,
    pub other_status: String,
}

async fn exists(core: &Core, id: Uuid) -> ApiResult<()> {
    let found: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM assets WHERE id = $1)").bind(id).fetch_one(&core.db).await?;
    if found { Ok(()) } else { Err(ApiError::NotFound) }
}

#[utoipa::path(get, path = "/api/helpdesk/assets/{id}/relations", tag = "helpdesk", params(("id" = Uuid, Path)), responses((status = 200, body = Vec<AssetRelation>)))]
async fn asset_relations(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<AssetRelation>>> {
    user.require(&perm::WORK)?;
    exists(&core, id).await?;
    Ok(Json(
        sqlx::query_as(
            "SELECT r.id, r.type_id, CASE WHEN r.from_id = $1 THEN t.name ELSE t.reverse_name END AS label,
                    CASE WHEN r.from_id = $1 THEN 'out' ELSE 'in' END AS direction,
                    o.id AS other_id, o.name AS other_name, ot.name AS other_type, o.status AS other_status
               FROM asset_relations r
               JOIN asset_relation_types t ON t.id = r.type_id
               JOIN assets o ON o.id = CASE WHEN r.from_id = $1 THEN r.to_id ELSE r.from_id END
               JOIN asset_types ot ON ot.id = o.type_id
              WHERE r.from_id = $1 OR r.to_id = $1
              ORDER BY label, o.name",
        )
        .bind(id)
        .fetch_all(&core.db)
        .await?,
    ))
}

#[derive(Deserialize, ToSchema)]
pub struct RelationInput {
    pub type_id: Uuid,
    /// Второй объект связи.
    pub other_id: Uuid,
    /// `out` (по умолчанию): «этот объект — связь — второй»; `in` — наоборот.
    #[serde(default)]
    pub direction: Option<String>,
}

#[utoipa::path(post, path = "/api/helpdesk/assets/{id}/relations", tag = "helpdesk", params(("id" = Uuid, Path)), request_body = RelationInput, responses((status = 201, body = Object)))]
async fn add_relation(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<RelationInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&MANAGE_ASSETS)?;
    let (from, to) = match b.direction.as_deref().unwrap_or("out") {
        "out" => (id, b.other_id),
        "in" => (b.other_id, id),
        _ => return Err(bad("Неверное направление связи.")),
    };
    if from == to {
        return Err(bad("Объект не может быть связан сам с собой."));
    }
    exists(&core, id).await?;
    let ok: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM assets WHERE id = $1)
            AND EXISTS (SELECT 1 FROM asset_relation_types WHERE id = $2 AND is_active)",
    )
    .bind(b.other_id)
    .bind(b.type_id)
    .fetch_one(&core.db)
    .await?;
    if !ok {
        return Err(bad("Объект или вид связи не найден."));
    }
    let mut tx = core.db.begin().await?;
    let rid: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO asset_relations (from_id, to_id, type_id, created_by) VALUES ($1, $2, $3, $4)
         ON CONFLICT DO NOTHING RETURNING id",
    )
    .bind(from)
    .bind(to)
    .bind(b.type_id)
    .bind(user.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let rid = rid.ok_or_else(|| ApiError::bad_request("duplicate", "Такая связь уже есть."))?;
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("asset.relation.add", Source::Portal)
            .object("asset", from)
            .details(json!({"to": to, "type": b.type_id})),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"id": rid}))))
}

#[utoipa::path(delete, path = "/api/helpdesk/assets/{id}/relations/{relation}", tag = "helpdesk",
    params(("id" = Uuid, Path), ("relation" = Uuid, Path)), responses((status = 204)))]
async fn remove_relation(
    State(core): State<Core>,
    user: CurrentUser,
    Path((id, relation)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    user.require(&MANAGE_ASSETS)?;
    let mut tx = core.db.begin().await?;
    let gone: Option<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "DELETE FROM asset_relations WHERE id = $1 AND (from_id = $2 OR to_id = $2) RETURNING from_id, to_id, type_id",
    )
    .bind(relation)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let (from, to, ty) = gone.ok_or(ApiError::NotFound)?;
    // Удалённая связь остаётся только в журнале аудита.
    audit::record(
        &mut *tx,
        &user.actor,
        Entry::new("asset.relation.remove", Source::Portal)
            .object("asset", from)
            .details(json!({"to": to, "type": ty})),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Анализ влияния и карта

/// Сколько уровней зависимостей проходит анализ влияния.
const IMPACT_DEPTH: i32 = 6;
/// Сколько объектов показывает анализ влияния и карта.
const MAX_NODES: i64 = 200;

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ImpactedAsset {
    pub id: Uuid,
    pub name: String,
    pub type_name: String,
    pub status: String,
    /// 1 — зависит напрямую, 2 — через один объект и т. д.
    pub depth: i32,
    /// Объект, через который пришло влияние.
    pub via_id: Uuid,
    pub via_name: String,
    /// Связь так, как она читается у затронутого объекта.
    pub relation: String,
    pub responsible_name: Option<String>,
    pub open_tickets: i64,
}

/// Что затронет отказ объекта: объекты, которые зависят от него напрямую или через другие,
/// по связям с признаком «влияние». Списанные не учитываются.
#[utoipa::path(get, path = "/api/helpdesk/assets/{id}/impact", tag = "helpdesk", params(("id" = Uuid, Path)), responses((status = 200, body = Vec<ImpactedAsset>)))]
async fn asset_impact(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<ImpactedAsset>>> {
    user.require(&perm::WORK)?;
    exists(&core, id).await?;
    let rows = sqlx::query_as(
        "WITH RECURSIVE hit (id, depth, via, relation, path) AS (
             SELECT r.from_id, 1, r.to_id, t.name, ARRAY[$1, r.from_id]
               FROM asset_relations r JOIN asset_relation_types t ON t.id = r.type_id AND t.impact
              WHERE r.to_id = $1
             UNION ALL
             SELECT r.from_id, h.depth + 1, r.to_id, t.name, h.path || r.from_id
               FROM hit h
               JOIN asset_relations r ON r.to_id = h.id
               JOIN asset_relation_types t ON t.id = r.type_id AND t.impact
              WHERE h.depth < $2 AND NOT r.from_id = ANY (h.path)
         ), best AS (
             SELECT DISTINCT ON (id) id, depth, via, relation FROM hit ORDER BY id, depth
         )
         SELECT a.id, a.name, ty.name AS type_name, a.status, b.depth, b.via AS via_id, v.name AS via_name,
                b.relation, nullif(trim(concat_ws(' ', p.last_name, p.first_name)), '') AS responsible_name,
                (SELECT count(*) FROM ticket_objects o JOIN tickets x ON x.id = o.ticket_id
                  WHERE o.kind = 'asset' AND o.object_id = a.id AND x.status = ANY($3)) AS open_tickets
           FROM best b
           JOIN assets a ON a.id = b.id AND a.status <> 'retired'
           JOIN asset_types ty ON ty.id = a.type_id
           JOIN assets v ON v.id = b.via
           LEFT JOIN persons p ON p.id = a.responsible_person_id
          ORDER BY b.depth, ty.name, a.name
          LIMIT $4",
    )
    .bind(id)
    .bind(IMPACT_DEPTH)
    .bind(OPEN_STATUSES)
    .bind(MAX_NODES)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct MapNode {
    pub id: Uuid,
    pub name: String,
    pub type_name: String,
    pub status: String,
    /// Шагов от объекта карты; 0 — сам объект.
    pub distance: i32,
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct MapEdge {
    pub from_id: Uuid,
    pub to_id: Uuid,
    pub name: String,
    pub impact: bool,
}

#[derive(Serialize, ToSchema)]
pub struct AssetMap {
    pub nodes: Vec<MapNode>,
    pub edges: Vec<MapEdge>,
    /// Объектов больше, чем показано.
    pub truncated: bool,
}

/// Карта зависимостей: объекты в двух шагах от этого по любым связям в обе стороны.
#[utoipa::path(get, path = "/api/helpdesk/assets/{id}/map", tag = "helpdesk", params(("id" = Uuid, Path)), responses((status = 200, body = AssetMap)))]
async fn asset_map(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<AssetMap>> {
    user.require(&perm::WORK)?;
    exists(&core, id).await?;
    let nodes: Vec<MapNode> = sqlx::query_as(
        "WITH RECURSIVE near (id, distance) AS (
             SELECT $1::uuid, 0
             UNION
             SELECT CASE WHEN r.from_id = n.id THEN r.to_id ELSE r.from_id END, n.distance + 1
               FROM near n JOIN asset_relations r ON r.from_id = n.id OR r.to_id = n.id
              WHERE n.distance < 2
         ), best AS (SELECT id, min(distance) AS distance FROM near GROUP BY id)
         SELECT a.id, a.name, t.name AS type_name, a.status, b.distance
           FROM best b JOIN assets a ON a.id = b.id JOIN asset_types t ON t.id = a.type_id
          ORDER BY b.distance, t.name, a.name
          LIMIT $2",
    )
    .bind(id)
    .bind(MAX_NODES + 1)
    .fetch_all(&core.db)
    .await?;
    let truncated = nodes.len() as i64 > MAX_NODES;
    let nodes: Vec<MapNode> = nodes.into_iter().take(MAX_NODES as usize).collect();
    let ids: Vec<Uuid> = nodes.iter().map(|n| n.id).collect();
    let edges = sqlx::query_as(
        "SELECT r.from_id, r.to_id, t.name, t.impact
           FROM asset_relations r JOIN asset_relation_types t ON t.id = r.type_id
          WHERE r.from_id = ANY($1) AND r.to_id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(AssetMap { nodes, edges, truncated }))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct RelationSuggestion {
    pub id: Uuid,
    pub name: String,
    pub type_name: String,
    /// `same_person` — тот же ответственный, `same_room` — та же площадка и кабинет.
    pub reason: String,
}

/// Возможные связи: действующие объекты с тем же ответственным или в том же кабинете, ещё не
/// связанные с этим. Портал только предлагает — связь создаёт инженер.
#[utoipa::path(get, path = "/api/helpdesk/assets/{id}/relation-suggestions", tag = "helpdesk", params(("id" = Uuid, Path)), responses((status = 200, body = Vec<RelationSuggestion>)))]
async fn relation_suggestions(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<RelationSuggestion>>> {
    user.require(&MANAGE_ASSETS)?;
    exists(&core, id).await?;
    Ok(Json(
        sqlx::query_as(
            "SELECT DISTINCT ON (o.id) o.id, o.name, t.name AS type_name,
                    CASE WHEN o.responsible_person_id = a.responsible_person_id THEN 'same_person' ELSE 'same_room' END AS reason
               FROM assets a
               JOIN assets o ON o.id <> a.id AND o.status <> 'retired'
                AND ((a.responsible_person_id IS NOT NULL AND o.responsible_person_id = a.responsible_person_id)
                  OR (nullif(a.room, '') IS NOT NULL AND o.site_id IS NOT DISTINCT FROM a.site_id AND lower(o.room) = lower(a.room)))
               JOIN asset_types t ON t.id = o.type_id
              WHERE a.id = $1
                AND NOT EXISTS (SELECT 1 FROM asset_relations r
                                 WHERE (r.from_id = a.id AND r.to_id = o.id) OR (r.from_id = o.id AND r.to_id = a.id))
              ORDER BY o.id
              LIMIT 20",
        )
        .bind(id)
        .fetch_all(&core.db)
        .await?,
    ))
}

// ------------------------------------------------------------------ Мониторинг ↔ CMDB

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct AssetAlert {
    pub id: Uuid,
    /// Объект, к которому относится оповещение (сам объект или тот, от которого он зависит).
    pub asset_id: Uuid,
    pub asset_name: String,
    pub source_name: String,
    pub title: String,
    /// `info`, `warning`, `critical`.
    pub severity: String,
    pub host: Option<String>,
    pub ticket_number: Option<i64>,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub started_at: time::OffsetDateTime,
}

/// Открытые оповещения объекта — по узлу из оповещения (название, IP, ПК с агентом).
#[utoipa::path(get, path = "/api/helpdesk/assets/{id}/alerts", tag = "helpdesk", params(("id" = Uuid, Path)), responses((status = 200, body = Vec<AssetAlert>)))]
async fn asset_alerts(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<AssetAlert>>> {
    user.require(&perm::WORK)?;
    exists(&core, id).await?;
    Ok(Json(
        sqlx::query_as(
            "SELECT al.id, x.asset_id, a.name AS asset_name, s.name AS source_name, al.title, al.severity, al.host,
                    t.number AS ticket_number, al.started_at
               FROM alert_assets x
               JOIN alerts al ON al.id = x.alert_id
               JOIN alert_sources s ON s.id = al.source_id
               JOIN assets a ON a.id = x.asset_id
               LEFT JOIN tickets t ON t.id = al.ticket_id
              WHERE x.asset_id = $1
              ORDER BY al.started_at DESC LIMIT 100",
        )
        .bind(id)
        .fetch_all(&core.db)
        .await?,
    ))
}

#[derive(Serialize, ToSchema)]
pub struct ServiceState {
    pub id: Uuid,
    pub name: String,
    pub type_name: String,
    /// `ok`, `info`, `warning`, `critical` — по самому важному открытому оповещению у сервиса
    /// или у того, от чего он зависит.
    pub state: String,
    /// Открытые оповещения: сначала самые важные (до 5).
    pub causes: Vec<AssetAlert>,
}

fn rank(s: &str) -> u8 {
    match s {
        "critical" => 3,
        "warning" => 2,
        "info" => 1,
        _ => 0,
    }
}

/// «Состояние сервисов»: объекты типов с отметкой «показывать состояние» и оповещения по ним
/// и по их зависимостям (связи с влиянием, до шести уровней).
#[utoipa::path(get, path = "/api/helpdesk/monitoring/services", tag = "integrations", responses((status = 200, body = Vec<ServiceState>)))]
async fn service_status(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<ServiceState>>> {
    user.require(&perm::WORK)?;
    let services: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT a.id, a.name, t.name FROM assets a JOIN asset_types t ON t.id = a.type_id AND t.show_status
          WHERE a.status <> 'retired' ORDER BY a.name LIMIT 200",
    )
    .fetch_all(&core.db)
    .await?;
    let ids: Vec<Uuid> = services.iter().map(|s| s.0).collect();
    #[derive(sqlx::FromRow)]
    struct Hit {
        svc: Uuid,
        #[sqlx(flatten)]
        alert: AssetAlert,
    }
    let hits: Vec<Hit> = sqlx::query_as(
        "WITH RECURSIVE dep (svc, id, depth, path) AS (
             SELECT s, s, 0, ARRAY[s] FROM unnest($1::uuid[]) s
             UNION ALL
             SELECT d.svc, r.to_id, d.depth + 1, d.path || r.to_id
               FROM dep d
               JOIN asset_relations r ON r.from_id = d.id
               JOIN asset_relation_types rt ON rt.id = r.type_id AND rt.impact
              WHERE d.depth < $2 AND NOT r.to_id = ANY (d.path)
         )
         SELECT DISTINCT ON (d.svc, al.id) d.svc, al.id, x.asset_id, a.name AS asset_name, s.name AS source_name,
                al.title, al.severity, al.host, t.number AS ticket_number, al.started_at
           FROM dep d
           JOIN alert_assets x ON x.asset_id = d.id
           JOIN alerts al ON al.id = x.alert_id
           JOIN alert_sources s ON s.id = al.source_id
           JOIN assets a ON a.id = x.asset_id
           LEFT JOIN tickets t ON t.id = al.ticket_id
          ORDER BY d.svc, al.id",
    )
    .bind(&ids)
    .bind(IMPACT_DEPTH)
    .fetch_all(&core.db)
    .await?;
    let mut by: std::collections::HashMap<Uuid, Vec<AssetAlert>> = std::collections::HashMap::new();
    for h in hits {
        by.entry(h.svc).or_default().push(h.alert);
    }
    let mut out: Vec<ServiceState> = services
        .into_iter()
        .map(|(id, name, type_name)| {
            let mut causes = by.remove(&id).unwrap_or_default();
            causes.sort_by(|a, b| rank(&b.severity).cmp(&rank(&a.severity)).then(b.started_at.cmp(&a.started_at)));
            let state = causes.first().map_or("ok", |c| c.severity.as_str()).to_owned();
            causes.truncate(5);
            ServiceState { id, name, type_name, state, causes }
        })
        .collect();
    // Сначала проблемные.
    out.sort_by(|a, b| {
        rank(&b.state).cmp(&rank(&a.state)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(Json(out))
}

// ------------------------------------------------------------------ Жизненный цикл типа

pub const STATUSES: &[&str] = &["in_use", "in_stock", "repair", "retired"];

#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct Lifecycle {
    /// Состояния этого типа; «в работе» и «на складе» есть всегда (их ставят выдача и возврат).
    pub statuses: Vec<String>,
    /// Разрешённые переходы `[из, в]`; пусто — любые между состояниями типа.
    #[serde(default)]
    pub transitions: Vec<[String; 2]>,
}

impl Lifecycle {
    fn allows(&self, from: &str, to: &str) -> bool {
        from == to || self.transitions.is_empty() || self.transitions.iter().any(|[a, b]| a == from && b == to)
    }
}

fn check_lifecycle(l: &Lifecycle) -> ApiResult<Lifecycle> {
    let bad = |m: &str| ApiError::bad_request("invalid_asset", m.to_owned());
    let mut seen = HashSet::new();
    let mut statuses = Vec::new();
    for s in &l.statuses {
        if !STATUSES.contains(&s.as_str()) {
            return Err(bad("Неверное состояние."));
        }
        if seen.insert(s.clone()) {
            statuses.push(s.clone());
        }
    }
    for need in ["in_use", "in_stock"] {
        if !seen.contains(need) {
            statuses.push(need.to_owned());
            seen.insert(need.to_owned());
        }
    }
    statuses.sort_by_key(|s| STATUSES.iter().position(|x| x == s));
    let mut pairs = HashSet::new();
    let mut transitions = Vec::new();
    for [a, b] in &l.transitions {
        if !seen.contains(a) || !seen.contains(b) || a == b {
            return Err(bad("Переход возможен только между разными состояниями этого типа."));
        }
        if pairs.insert((a.clone(), b.clone())) {
            transitions.push([a.clone(), b.clone()]);
        }
    }
    // Выдача, возврат и передача должны оставаться возможными.
    if !transitions.is_empty() {
        for [a, b] in [["in_stock", "in_use"], ["in_use", "in_stock"]] {
            if !pairs.contains(&(a.to_owned(), b.to_owned())) {
                return Err(bad(
                    "Нужны переходы «на складе → в работе» и «в работе → на складе»: их делают выдача и возврат.",
                ));
            }
        }
    }
    Ok(Lifecycle { statuses, transitions })
}

pub(crate) async fn lifecycle_of(
    db: impl sqlx::PgExecutor<'_>,
    type_id: Uuid,
) -> Result<Option<Lifecycle>, sqlx::Error> {
    let v: Option<Value> =
        sqlx::query_scalar("SELECT lifecycle FROM asset_types WHERE id = $1").bind(type_id).fetch_optional(db).await?;
    Ok(v.map(|v| {
        serde_json::from_value(v)
            .unwrap_or(Lifecycle { statuses: STATUSES.iter().map(|s| (*s).to_owned()).collect(), transitions: vec![] })
    }))
}

/// Проверка состояния по жизненному циклу типа: оно есть у типа, и переход из прежнего разрешён.
pub(crate) fn check_status(l: &Lifecycle, old: Option<&str>, new: &str) -> ApiResult<()> {
    if !l.statuses.iter().any(|s| s == new) {
        return Err(ApiError::bad_request("invalid_asset", "У этого типа нет такого состояния."));
    }
    if let Some(old) = old
        && !l.allows(old, new)
    {
        return Err(ApiError::bad_request("invalid_asset", "Жизненный цикл типа не разрешает такой переход."));
    }
    Ok(())
}

#[utoipa::path(get, path = "/api/helpdesk/asset-types/{id}/lifecycle", tag = "helpdesk", params(("id" = Uuid, Path)), responses((status = 200, body = Lifecycle)))]
async fn get_lifecycle(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Lifecycle>> {
    user.require(&perm::WORK)?;
    lifecycle_of(&core.db, id).await?.map(Json).ok_or(ApiError::NotFound)
}

#[utoipa::path(put, path = "/api/admin/helpdesk/asset-types/{id}/lifecycle", tag = "helpdesk-admin", params(("id" = Uuid, Path)), request_body = Lifecycle, responses((status = 204)))]
async fn put_lifecycle(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<Lifecycle>,
) -> ApiResult<StatusCode> {
    user.require(&perm::ADMIN)?;
    let l = check_lifecycle(&b)?;
    let mut tx = core.db.begin().await?;
    // Объекты в состоянии, которого у типа больше нет, сначала нужно перевести.
    let stray: i64 = sqlx::query_scalar("SELECT count(*) FROM assets WHERE type_id = $1 AND NOT status = ANY($2)")
        .bind(id)
        .bind(&l.statuses)
        .fetch_one(&mut *tx)
        .await?;
    if stray > 0 {
        return Err(ApiError::bad_request(
            "invalid_asset",
            "Есть объекты этого типа в состоянии, которое вы убираете: сначала переведите их в другое.",
        ));
    }
    let n = sqlx::query("UPDATE asset_types SET lifecycle = $2 WHERE id = $1")
        .bind(id)
        .bind(json!(l))
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&mut *tx, &user.actor, Entry::new("asset_type.lifecycle", Source::Portal).object("asset_type", id))
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

// ------------------------------------------------------------------ Лицензии ПО

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct License {
    pub id: Uuid,
    pub name: String,
    pub vendor: Option<String>,
    /// Часть названия программы в «Программах и компонентах».
    pub pattern: String,
    /// Куплено мест; `null` — без ограничения.
    pub seats: Option<i32>,
    /// `ГГГГ-ММ-ДД`.
    pub expires_on: Option<String>,
    pub notes: Option<String>,
    pub is_active: bool,
    /// На скольких ПК с агентом стоит программа.
    pub installed: i64,
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct LicenseDevice {
    pub device_id: Uuid,
    pub hostname: String,
    pub program: String,
    pub version: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_seen_at: Option<time::OffsetDateTime>,
}

#[derive(Serialize, ToSchema)]
pub struct LicenseDetail {
    pub license: License,
    /// ПК, где найдена программа (до 500).
    pub devices: Vec<LicenseDevice>,
}

#[derive(Deserialize, ToSchema)]
pub struct LicenseInput {
    pub name: String,
    pub vendor: Option<String>,
    pub pattern: String,
    pub seats: Option<i32>,
    /// `ГГГГ-ММ-ДД`.
    pub expires_on: Option<String>,
    pub notes: Option<String>,
    #[serde(default = "yes")]
    pub is_active: bool,
}

const LICENSE_SELECT: &str = "SELECT l.id, l.name, l.vendor, l.pattern, l.seats, to_char(l.expires_on, 'YYYY-MM-DD') AS expires_on, l.notes, l.is_active,
        (SELECT count(DISTINCT s.device_id) FROM device_software s JOIN devices d ON d.id = s.device_id AND d.is_active
          WHERE strpos(lower(s.name), lower(l.pattern)) > 0) AS installed
   FROM software_licenses l";

fn clean(s: &Option<String>, max: usize) -> Option<String> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(max).collect())
}

fn check_license(b: &LicenseInput) -> ApiResult<(String, String, Option<time::Date>)> {
    let bad = |m: &str| ApiError::bad_request("invalid_license", m.to_owned());
    let name = b.name.trim();
    let m = b.pattern.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(bad("Укажите название лицензии, до 200 знаков."));
    }
    if m.chars().count() < 2 || m.chars().count() > 200 {
        return Err(bad("Часть названия программы — от 2 до 200 знаков."));
    }
    if b.seats.is_some_and(|s| s < 0) {
        return Err(bad("Число мест не может быть отрицательным."));
    }
    if b.notes.as_deref().is_some_and(|n| n.chars().count() > 2000) {
        return Err(bad("Заметки — не длиннее 2000 знаков."));
    }
    let expires = match b.expires_on.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(d) => Some(
            time::Date::parse(d, time::macros::format_description!("[year]-[month]-[day]"))
                .map_err(|_| bad("Дата окончания — в виде ГГГГ-ММ-ДД."))?,
        ),
    };
    Ok((name.to_owned(), m.to_owned(), expires))
}

#[utoipa::path(get, path = "/api/helpdesk/licenses", tag = "helpdesk", responses((status = 200, body = Vec<License>)))]
async fn list_licenses(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<License>>> {
    user.require(&perm::WORK)?;
    Ok(Json(
        sqlx::query_as(&format!("{LICENSE_SELECT} ORDER BY l.is_active DESC, l.name")) // SAFE: константа
            .fetch_all(&core.db)
            .await?,
    ))
}

#[utoipa::path(get, path = "/api/helpdesk/licenses/{id}", tag = "helpdesk", params(("id" = Uuid, Path)), responses((status = 200, body = LicenseDetail)))]
async fn get_license(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LicenseDetail>> {
    user.require(&perm::WORK)?;
    let license: License = sqlx::query_as(&format!("{LICENSE_SELECT} WHERE l.id = $1")) // SAFE: константа
        .bind(id)
        .fetch_optional(&core.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let devices = sqlx::query_as(
        "SELECT DISTINCT ON (d.id) d.id AS device_id, d.hostname, s.name AS program, s.version, d.last_seen_at
           FROM device_software s JOIN devices d ON d.id = s.device_id AND d.is_active
          WHERE strpos(lower(s.name), lower($1)) > 0
          ORDER BY d.id, s.name LIMIT 500",
    )
    .bind(&license.pattern)
    .fetch_all(&core.db)
    .await?;
    let mut devices: Vec<LicenseDevice> = devices;
    devices.sort_by(|a, b| a.hostname.to_lowercase().cmp(&b.hostname.to_lowercase()));
    Ok(Json(LicenseDetail { license, devices }))
}

#[utoipa::path(post, path = "/api/helpdesk/licenses", tag = "helpdesk", request_body = LicenseInput, responses((status = 201, body = Object)))]
async fn create_license(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<LicenseInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&MANAGE_ASSETS)?;
    let (name, m, expires) = check_license(&b)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO software_licenses (name, vendor, pattern, seats, expires_on, notes, is_active)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(name)
    .bind(clean(&b.vendor, 200))
    .bind(m)
    .bind(b.seats)
    .bind(expires)
    .bind(clean(&b.notes, 2000))
    .bind(b.is_active)
    .fetch_one(&core.db)
    .await
    .map_err(crate::objects::dup)?;
    audit::record(&core.db, &user.actor, Entry::new("license.create", Source::Portal).object("license", id)).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[utoipa::path(put, path = "/api/helpdesk/licenses/{id}", tag = "helpdesk", params(("id" = Uuid, Path)), request_body = LicenseInput, responses((status = 204)))]
async fn update_license(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<LicenseInput>,
) -> ApiResult<StatusCode> {
    user.require(&MANAGE_ASSETS)?;
    let (name, m, expires) = check_license(&b)?;
    let n = sqlx::query(
        "UPDATE software_licenses SET name = $2, vendor = $3, pattern = $4, seats = $5, expires_on = $6, notes = $7,
                is_active = $8, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(name)
    .bind(clean(&b.vendor, 200))
    .bind(m)
    .bind(b.seats)
    .bind(expires)
    .bind(clean(&b.notes, 2000))
    .bind(b.is_active)
    .execute(&core.db)
    .await
    .map_err(crate::objects::dup)?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("license.update", Source::Portal).object("license", id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/helpdesk/licenses/{id}", tag = "helpdesk", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn delete_license(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&MANAGE_ASSETS)?;
    let n =
        sqlx::query("DELETE FROM software_licenses WHERE id = $1").bind(id).execute(&core.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("license.delete", Source::Portal).object("license", id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lc(statuses: &[&str], transitions: &[[&str; 2]]) -> Lifecycle {
        Lifecycle {
            statuses: statuses.iter().map(|s| (*s).to_owned()).collect(),
            transitions: transitions.iter().map(|[a, b]| [(*a).to_owned(), (*b).to_owned()]).collect(),
        }
    }

    #[test]
    fn lifecycle_rules() {
        // Обязательные состояния добавляются сами, порядок — как в справочнике.
        let l = check_lifecycle(&lc(&["retired"], &[])).unwrap();
        assert_eq!(l.statuses, ["in_use", "in_stock", "retired"]);
        assert!(check_status(&l, None, "in_use").is_ok());
        assert!(check_status(&l, None, "repair").is_err());
        // Переходы: без выдачи и возврата нельзя.
        assert!(check_lifecycle(&lc(&["retired"], &[["in_use", "retired"]])).is_err());
        let l = check_lifecycle(&lc(
            &["retired"],
            &[["in_stock", "in_use"], ["in_use", "in_stock"], ["in_stock", "retired"]],
        ))
        .unwrap();
        assert!(check_status(&l, Some("in_stock"), "retired").is_ok());
        assert!(check_status(&l, Some("in_use"), "retired").is_err());
        assert!(check_status(&l, Some("in_use"), "in_use").is_ok());
        assert!(check_lifecycle(&lc(&["bogus"], &[])).is_err());
        assert!(check_lifecycle(&lc(&[], &[["in_use", "repair"]])).is_err());
    }
}
