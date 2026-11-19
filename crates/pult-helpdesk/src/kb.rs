//! База знаний: статьи для всех или только для инженеров, разделы, поиск,
//! подсказки «Возможно, это поможет» при создании заявки, «Создать статью из заявки».

use crate::{HelpdeskState, perm, tickets::Who};
use axum::{
    Json,
    extract::{FromRef, Path, Query, State},
    http::StatusCode,
};
use pult_core::l10n::tr;
use pult_core::{
    ApiError, ApiResult, Core, CurrentUser, Permission,
    audit::{self, Entry, Source},
    rbac::{ROLE_ENGINEER, ROLE_SUPERADMIN},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub const EDIT: Permission = Permission {
    code: "helpdesk.kb.edit",
    description: "Писать и публиковать статьи базы знаний",
    default_roles: &[ROLE_ENGINEER, ROLE_SUPERADMIN],
};

pub fn routes<S>() -> OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
    Core: FromRef<S>,
    HelpdeskState: FromRef<S>,
{
    OpenApiRouter::new()
        .routes(routes!(list_articles, create_article))
        .routes(routes!(get_article, update_article, delete_article))
        .routes(routes!(suggest))
        .routes(routes!(list_sections, create_section))
        .routes(routes!(update_section, delete_section))
        .routes(routes!(draft_from_ticket))
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct ArticleRow {
    pub id: Uuid,
    pub title: String,
    pub excerpt: String,
    /// `all` или `engineers`.
    pub audience: String,
    /// `draft` или `published`.
    pub status: String,
    pub section_id: Option<Uuid>,
    pub section_name: Option<String>,
    pub views: i32,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct Article {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub audience: String,
    pub status: String,
    pub section_id: Option<Uuid>,
    pub section_name: Option<String>,
    pub author_name: Option<String>,
    pub source_ticket_number: Option<i64>,
    pub views: i32,
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: OffsetDateTime,
    pub can_edit: bool,
}

#[derive(Deserialize, IntoParams)]
pub struct KbQuery {
    pub q: Option<String>,
    pub section_id: Option<Uuid>,
    /// С черновиками (для редакторов).
    #[serde(default)]
    pub drafts: bool,
}

/// Кто что видит: сотрудник — опубликованные «для всех»; инженер — и «для инженеров»; редактор — и черновики.
fn scope(user: &CurrentUser) -> (bool, bool) {
    (user.grants.has(&perm::WORK), user.grants.has(&EDIT))
}

#[utoipa::path(get, path = "/api/kb/articles", tag = "kb", params(KbQuery), responses((status = 200, body = Vec<ArticleRow>)))]
async fn list_articles(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<KbQuery>,
) -> ApiResult<Json<Vec<ArticleRow>>> {
    let (engineer, editor) = scope(&user);
    let needle =
        q.q.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(200).collect::<String>());
    let rows = sqlx::query_as(
        "SELECT a.id, a.title, left(regexp_replace(a.body, '\\s+', ' ', 'g'), 200) AS excerpt, a.audience, a.status,
                a.section_id, s.name AS section_name, a.views, a.updated_at
           FROM kb_articles a LEFT JOIN kb_sections s ON s.id = a.section_id
          WHERE ($1 OR a.audience = 'all') AND (a.status = 'published' OR ($2 AND $3))
            AND ($4::uuid IS NULL OR a.section_id = $4)
            AND ($5::text IS NULL OR a.search_tsv @@ websearch_to_tsquery('russian', $5)
                 OR strpos(lower(a.title), lower($5)) > 0)
          ORDER BY CASE WHEN $5::text IS NULL THEN 0 ELSE -ts_rank(a.search_tsv, websearch_to_tsquery('russian', $5)) END,
                   a.updated_at DESC
          LIMIT 200",
    )
    .bind(engineer)
    .bind(editor)
    .bind(q.drafts)
    .bind(q.section_id)
    .bind(needle)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[derive(Deserialize, IntoParams)]
pub struct SuggestQuery {
    /// Тема новой заявки.
    pub q: String,
}

/// «Возможно, это поможет»: опубликованные статьи по теме заявки.
#[utoipa::path(get, path = "/api/kb/suggest", tag = "kb", params(SuggestQuery), responses((status = 200, body = Vec<ArticleRow>)))]
async fn suggest(
    State(core): State<Core>,
    user: CurrentUser,
    Query(q): Query<SuggestQuery>,
) -> ApiResult<Json<Vec<ArticleRow>>> {
    let (engineer, _) = scope(&user);
    let words: Vec<String> =
        q.q.split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 3)
            .take(8)
            .map(str::to_lowercase)
            .collect();
    if words.is_empty() {
        return Ok(Json(Vec::new()));
    }
    // Любое из слов: «принтер не печатает» найдёт статью «Принтер зажевал бумагу».
    let query = words.join(" OR ");
    let rows = sqlx::query_as(
        "SELECT a.id, a.title, left(regexp_replace(a.body, '\\s+', ' ', 'g'), 200) AS excerpt, a.audience, a.status,
                a.section_id, s.name AS section_name, a.views, a.updated_at
           FROM kb_articles a LEFT JOIN kb_sections s ON s.id = a.section_id
          WHERE a.status = 'published' AND ($1 OR a.audience = 'all')
            AND a.search_tsv @@ websearch_to_tsquery('russian', $2)
          ORDER BY ts_rank(a.search_tsv, websearch_to_tsquery('russian', $2)) DESC, a.views DESC
          LIMIT 5",
    )
    .bind(engineer)
    .bind(query)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[utoipa::path(get, path = "/api/kb/articles/{id}", tag = "kb", params(("id" = Uuid, Path)), responses((status = 200, body = Article)))]
async fn get_article(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Article>> {
    let (engineer, editor) = scope(&user);
    let a: Article = sqlx::query_as(
        "SELECT a.id, a.title, a.body, a.audience, a.status, a.section_id, s.name AS section_name, a.views, a.updated_at,
                (SELECT coalesce(nullif(trim(concat_ws(' ', p.last_name, p.first_name)), ''), ad.display_name)
                   FROM users u JOIN ad_accounts ad ON ad.id = u.ad_account_id LEFT JOIN persons p ON p.id = u.person_id
                  WHERE u.id = a.author_id) AS author_name,
                CASE WHEN $2 THEN (SELECT number FROM tickets t WHERE t.id = a.source_ticket_id) END AS source_ticket_number,
                $3::bool AS can_edit
           FROM kb_articles a LEFT JOIN kb_sections s ON s.id = a.section_id
          WHERE a.id = $1 AND ($2 OR a.audience = 'all') AND (a.status = 'published' OR $3)",
    )
    .bind(id)
    .bind(engineer)
    .bind(editor)
    .fetch_optional(&core.db)
    .await?
    .ok_or(ApiError::NotFound)?;
    if a.status == "published" {
        sqlx::query("UPDATE kb_articles SET views = views + 1 WHERE id = $1").bind(id).execute(&core.db).await?;
    }
    Ok(Json(a))
}

#[derive(Deserialize, ToSchema)]
pub struct ArticleInput {
    pub title: String,
    pub body: String,
    pub audience: String,
    pub status: String,
    pub section_id: Option<Uuid>,
    /// Из какой заявки (кнопка «Создать статью из заявки»).
    pub source_ticket_number: Option<i64>,
}

fn check(b: &ArticleInput) -> ApiResult<()> {
    let bad = |m: &str| Err(ApiError::bad_request("invalid_article", m));
    if b.title.trim().is_empty() || b.title.chars().count() > 300 {
        return bad("Укажите заголовок (до 300 знаков).");
    }
    if b.body.len() > 500_000 {
        return bad("Текст слишком длинный.");
    }
    if !matches!(b.audience.as_str(), "all" | "engineers") || !matches!(b.status.as_str(), "draft" | "published") {
        return bad("Неверная аудитория или состояние.");
    }
    Ok(())
}

#[utoipa::path(post, path = "/api/kb/articles", tag = "kb", request_body = ArticleInput, responses((status = 201, body = Object)))]
async fn create_article(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<ArticleInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&EDIT)?;
    check(&b)?;
    let source: Option<Uuid> = match b.source_ticket_number {
        Some(n) => {
            sqlx::query_scalar("SELECT id FROM tickets WHERE number = $1").bind(n).fetch_optional(&core.db).await?
        }
        None => None,
    };
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO kb_articles (title, body, audience, status, section_id, author_id, source_ticket_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(b.title.trim())
    .bind(&b.body)
    .bind(&b.audience)
    .bind(&b.status)
    .bind(b.section_id)
    .bind(user.user_id)
    .bind(source)
    .fetch_one(&core.db)
    .await?;
    audit::record(&core.db, &user.actor, Entry::new("kb.create", Source::Portal).object("kb_article", id)).await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[utoipa::path(put, path = "/api/kb/articles/{id}", tag = "kb", params(("id" = Uuid, Path)), request_body = ArticleInput, responses((status = 204)))]
async fn update_article(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<ArticleInput>,
) -> ApiResult<StatusCode> {
    user.require(&EDIT)?;
    check(&b)?;
    let n = sqlx::query(
        "UPDATE kb_articles SET title = $2, body = $3, audience = $4, status = $5, section_id = $6, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(b.title.trim())
    .bind(&b.body)
    .bind(&b.audience)
    .bind(&b.status)
    .bind(b.section_id)
    .execute(&core.db)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("kb.update", Source::Portal).object("kb_article", id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(delete, path = "/api/kb/articles/{id}", tag = "kb", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn delete_article(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&EDIT)?;
    let n = sqlx::query("DELETE FROM kb_articles WHERE id = $1").bind(id).execute(&core.db).await?.rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    audit::record(&core.db, &user.actor, Entry::new("kb.delete", Source::Portal).object("kb_article", id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct Section {
    pub id: Uuid,
    pub name: String,
    pub sort: i32,
    pub articles: i64,
}

#[derive(Deserialize, ToSchema)]
pub struct SectionInput {
    pub name: String,
    #[serde(default)]
    pub sort: i32,
}

#[utoipa::path(get, path = "/api/kb/sections", tag = "kb", responses((status = 200, body = Vec<Section>)))]
async fn list_sections(State(core): State<Core>, user: CurrentUser) -> ApiResult<Json<Vec<Section>>> {
    let (engineer, _) = scope(&user);
    let rows = sqlx::query_as(
        "SELECT s.id, s.name, s.sort,
                (SELECT count(*) FROM kb_articles a WHERE a.section_id = s.id AND a.status = 'published' AND ($1 OR a.audience = 'all')) AS articles
           FROM kb_sections s ORDER BY s.sort, s.name",
    )
    .bind(engineer)
    .fetch_all(&core.db)
    .await?;
    Ok(Json(rows))
}

#[utoipa::path(post, path = "/api/kb/sections", tag = "kb", request_body = SectionInput, responses((status = 201, body = Object)))]
async fn create_section(
    State(core): State<Core>,
    user: CurrentUser,
    Json(b): Json<SectionInput>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    user.require(&EDIT)?;
    if b.name.trim().is_empty() {
        return Err(ApiError::bad_request("invalid_section", "Укажите название раздела."));
    }
    let id: Uuid = sqlx::query_scalar("INSERT INTO kb_sections (name, sort) VALUES ($1, $2) RETURNING id")
        .bind(b.name.trim())
        .bind(b.sort)
        .fetch_one(&core.db)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

#[utoipa::path(put, path = "/api/kb/sections/{id}", tag = "kb", params(("id" = Uuid, Path)), request_body = SectionInput, responses((status = 204)))]
async fn update_section(
    State(core): State<Core>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
    Json(b): Json<SectionInput>,
) -> ApiResult<StatusCode> {
    user.require(&EDIT)?;
    let n = sqlx::query("UPDATE kb_sections SET name = $2, sort = $3 WHERE id = $1")
        .bind(id)
        .bind(b.name.trim())
        .bind(b.sort)
        .execute(&core.db)
        .await?
        .rows_affected();
    if n == 0 { Err(ApiError::NotFound) } else { Ok(StatusCode::NO_CONTENT) }
}

#[utoipa::path(delete, path = "/api/kb/sections/{id}", tag = "kb", params(("id" = Uuid, Path)), responses((status = 204)))]
async fn delete_section(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    user.require(&EDIT)?;
    let n = sqlx::query("DELETE FROM kb_sections WHERE id = $1").bind(id).execute(&core.db).await?.rows_affected();
    if n == 0 { Err(ApiError::NotFound) } else { Ok(StatusCode::NO_CONTENT) }
}

#[derive(Serialize, ToSchema)]
pub struct Draft {
    pub title: String,
    pub body: String,
    pub source_ticket_number: i64,
}

/// Черновик статьи из заявки: тема, описание проблемы и текст решения (без внутренних заметок).
#[utoipa::path(get, path = "/api/helpdesk/tickets/{number}/kb-draft", tag = "kb", params(("number" = i64, Path)), responses((status = 200, body = Draft)))]
async fn draft_from_ticket(
    State(core): State<Core>,
    user: CurrentUser,
    Path(number): Path<i64>,
) -> ApiResult<Json<Draft>> {
    user.require(&EDIT)?;
    let who = Who::from_user(&user);
    if !who.engineer {
        return Err(ApiError::Forbidden);
    }
    let row: Option<(Uuid, String, String)> =
        sqlx::query_as("SELECT id, title, description FROM tickets WHERE number = $1")
            .bind(number)
            .fetch_optional(&core.db)
            .await?;
    let (id, title, description) = row.ok_or(ApiError::NotFound)?;
    let replies: Vec<String> = sqlx::query_scalar(
        "SELECT c.body FROM ticket_comments c JOIN users u ON u.id = c.author_id
          WHERE c.ticket_id = $1 AND c.visibility = 'public'
            AND EXISTS (SELECT 1 FROM site_engineers se WHERE se.user_id = u.id)
          ORDER BY c.created_at DESC LIMIT 3",
    )
    .bind(id)
    .fetch_all(&core.db)
    .await?;
    let mut body = tr("## Проблема\n\n").into_owned();
    body.push_str(description.trim());
    body.push_str(&tr("\n\n## Решение\n\n"));
    for r in replies.iter().rev() {
        body.push_str(r.trim());
        body.push_str("\n\n");
    }
    Ok(Json(Draft { title, body: body.trim_end().to_string(), source_ticket_number: number }))
}
