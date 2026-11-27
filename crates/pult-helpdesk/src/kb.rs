//! База знаний: статьи для всех или только для инженеров, разделы, поиск,
//! подсказки «Возможно, это поможет» при создании заявки, «Создать статью из заявки».
//! Вложения статей — файлы из общей таблицы `files` (загрузка как у заявок); видит их тот, кому видна статья.

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
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

pub const EDIT: Permission = Permission {
    code: "helpdesk.kb.edit",
    description: "Писать и публиковать статьи базы знаний",
    default_roles: &[ROLE_ENGINEER, ROLE_SUPERADMIN],
};

/// Вложений в одной статье.
pub const MAX_FILES: usize = 50;

/// Столбцы строки списка. Анонс — начало текста (разметку убирает [`plain_excerpt`]), обложка —
/// первая картинка статьи, вставленная в текст.
const ROW_COLS: &str = "a.id, a.title, left(a.body, 3000) AS excerpt, a.audience, a.status,
                a.section_id, s.name AS section_name, a.views, a.updated_at,
                (SELECT k.file_id FROM kb_attachments k JOIN files f ON f.id = k.file_id
                  WHERE k.article_id = a.id AND f.mime IN ('image/png', 'image/jpeg', 'image/gif', 'image/webp')
                    AND strpos(a.body, k.file_id::text) > 0
                  ORDER BY strpos(a.body, k.file_id::text) LIMIT 1) AS cover";

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
    /// Начало текста без разметки.
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
    /// Обложка: первая картинка в тексте статьи.
    pub cover: Option<Uuid>,
}

/// Вложение статьи (или черновика из заявки).
#[derive(Serialize, sqlx::FromRow, ToSchema)]
pub struct KbFile {
    pub id: Uuid,
    pub name: String,
    pub mime: String,
    pub size: i64,
    /// Картинка: её можно вставить в текст.
    pub is_image: bool,
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
    #[serde(with = "time::serde::rfc3339")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: OffsetDateTime,
    pub can_edit: bool,
    #[sqlx(skip)]
    pub files: Vec<KbFile>,
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

/// Анонс без разметки markdown: картинки убираются, у ссылок остаётся текст, заголовки и списки —
/// просто текстом. Не длиннее 220 знаков.
pub fn plain_excerpt(md: &str) -> String {
    let mut out = String::new();
    let mut code = false;
    for line in md.lines() {
        let l = line.trim_start();
        if l.starts_with("```") {
            code = !code;
            continue;
        }
        let l = if code { l } else { block_marker(l) };
        out.push_str(&inline_plain(l));
        out.push(' ');
        if out.chars().count() > 400 {
            break;
        }
    }
    let flat = out.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 220 {
        return flat;
    }
    let mut cut: String = flat.chars().take(219).collect::<String>().trim_end().to_string();
    cut.push('…');
    cut
}

/// Без значков начала строки: `#`, `>`, маркеров списка.
fn block_marker(l: &str) -> &str {
    let l = l.trim_start_matches(['#', '>']).trim_start();
    for m in ["- ", "* ", "+ "] {
        if let Some(r) = l.strip_prefix(m) {
            return r;
        }
    }
    let digits = l.chars().take_while(char::is_ascii_digit).count();
    if digits > 0
        && let Some(r) = l[digits..].strip_prefix(". ")
    {
        return r;
    }
    l
}

/// `[текст](адрес)` с позиции `[`: позиция `]` и позиция после `)`.
/// Ссылка `[текст](адрес)` с позиции `[`. Скобки ищутся не дальше LINK_SCAN знаков: длинная строка из
/// одних `[` не превращает разбор в квадратичный.
fn link_parts(chars: &[char], start: usize) -> Option<(usize, usize)> {
    const LINK_SCAN: usize = 300;
    let close = start + chars[start..].iter().take(LINK_SCAN).position(|&c| c == ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let paren = close + 1 + chars[close + 1..].iter().take(LINK_SCAN).position(|&c| c == ')')?;
    Some((close, paren + 1))
}

fn inline_plain(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '!'
            && chars.get(i + 1) == Some(&'[')
            && let Some((_, end)) = link_parts(&chars, i + 1)
        {
            i = end;
            continue;
        }
        if c == '['
            && let Some((close, end)) = link_parts(&chars, i)
        {
            out.extend(&chars[i + 1..close]);
            i = end;
            continue;
        }
        if !matches!(c, '*' | '`' | '~') {
            out.push(c);
        }
        i += 1;
    }
    out
}

/// Ссылки на вложения в тексте: `/api/helpdesk/files/{id}`.
fn file_refs(body: &str) -> Vec<Uuid> {
    const PREFIX: &str = "/api/helpdesk/files/";
    let mut v: Vec<Uuid> = body
        .match_indices(PREFIX)
        .filter_map(|(i, _)| body.get(i + PREFIX.len()..i + PREFIX.len() + 36))
        .filter_map(|s| Uuid::parse_str(s).ok())
        .collect();
    v.sort();
    v.dedup();
    v
}

const IMAGE_MIME: &str = "f.mime IN ('image/png', 'image/jpeg', 'image/gif', 'image/webp')";

async fn article_files(db: impl sqlx::PgExecutor<'_>, article: Uuid) -> ApiResult<Vec<KbFile>> {
    let sql = format!(
        "SELECT f.id, f.original_name AS name, f.mime, f.size, {IMAGE_MIME} AS is_image
           FROM kb_attachments k JOIN files f ON f.id = k.file_id
          WHERE k.article_id = $1 ORDER BY k.created_at, f.id"
    );
    Ok(sqlx::query_as(&sql).bind(article).fetch_all(db).await?)
}

/// Вложения статьи — ровно `files` и вставленные в текст. Новый файл — только свой и нигде ещё не
/// прикреплённый; снятый со статьи файл через сутки удалит уборка.
async fn sync_files(
    tx: &mut Transaction<'_, Postgres>,
    article: Uuid,
    files: &[Uuid],
    body: &str,
    user: Uuid,
) -> ApiResult<()> {
    let too_many = || ApiError::bad_request("too_many_files", format!("Не больше {MAX_FILES} вложений в статье."));
    let mut wanted = files.to_vec();
    wanted.sort();
    wanted.dedup();
    if wanted.len() > MAX_FILES {
        return Err(too_many());
    }
    let refs = file_refs(body);
    let mut keep = wanted.clone();
    keep.extend(&refs);
    sqlx::query("DELETE FROM kb_attachments WHERE article_id = $1 AND NOT (file_id = ANY($2))")
        .bind(article)
        .bind(&keep)
        .execute(&mut **tx)
        .await?;
    let attach = "INSERT INTO kb_attachments (article_id, file_id)
                  SELECT $1, f.id FROM files f
                   WHERE f.id = ANY($2) AND f.uploaded_by = $3
                     AND NOT EXISTS (SELECT 1 FROM ticket_attachments t WHERE t.file_id = f.id)
                  ON CONFLICT DO NOTHING";
    sqlx::query(attach).bind(article).bind(&wanted).bind(user).execute(&mut **tx).await?;
    // Картинки из текста — по возможности: чужая ссылка просто не станет вложением.
    sqlx::query(attach).bind(article).bind(&refs).bind(user).execute(&mut **tx).await?;
    let have: i64 =
        sqlx::query_scalar("SELECT count(*) FROM kb_attachments WHERE article_id = $1 AND file_id = ANY($2)")
            .bind(article)
            .bind(&wanted)
            .fetch_one(&mut **tx)
            .await?;
    if have != wanted.len() as i64 {
        return Err(ApiError::bad_request("invalid_file", "Вложение не найдено или уже использовано."));
    }
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM kb_attachments WHERE article_id = $1")
        .bind(article)
        .fetch_one(&mut **tx)
        .await?;
    if total > MAX_FILES as i64 {
        return Err(too_many());
    }
    Ok(())
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
    let sql = format!(
        "SELECT {ROW_COLS}
           FROM kb_articles a LEFT JOIN kb_sections s ON s.id = a.section_id
          WHERE ($1 OR a.audience = 'all') AND (a.status = 'published' OR ($2 AND $3))
            AND ($4::uuid IS NULL OR a.section_id = $4)
            AND ($5::text IS NULL OR a.search_tsv @@ websearch_to_tsquery('russian', $5)
                 OR strpos(lower(a.title), lower($5)) > 0)
          ORDER BY CASE WHEN $5::text IS NULL THEN 0 ELSE -ts_rank(a.search_tsv, websearch_to_tsquery('russian', $5)) END,
                   a.updated_at DESC
          LIMIT 200"
    );
    let mut rows: Vec<ArticleRow> = sqlx::query_as(&sql)
        .bind(engineer)
        .bind(editor)
        .bind(q.drafts)
        .bind(q.section_id)
        .bind(needle)
        .fetch_all(&core.db)
        .await?;
    for r in &mut rows {
        r.excerpt = plain_excerpt(&r.excerpt);
    }
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
    let sql = format!(
        "SELECT {ROW_COLS}
           FROM kb_articles a LEFT JOIN kb_sections s ON s.id = a.section_id
          WHERE a.status = 'published' AND ($1 OR a.audience = 'all')
            AND a.search_tsv @@ websearch_to_tsquery('russian', $2)
          ORDER BY ts_rank(a.search_tsv, websearch_to_tsquery('russian', $2)) DESC, a.views DESC
          LIMIT 5"
    );
    let mut rows: Vec<ArticleRow> = sqlx::query_as(&sql).bind(engineer).bind(query).fetch_all(&core.db).await?;
    for r in &mut rows {
        r.excerpt = plain_excerpt(&r.excerpt);
    }
    Ok(Json(rows))
}

#[utoipa::path(get, path = "/api/kb/articles/{id}", tag = "kb", params(("id" = Uuid, Path)), responses((status = 200, body = Article)))]
async fn get_article(State(core): State<Core>, user: CurrentUser, Path(id): Path<Uuid>) -> ApiResult<Json<Article>> {
    let (engineer, editor) = scope(&user);
    let mut a: Article = sqlx::query_as(
        "SELECT a.id, a.title, a.body, a.audience, a.status, a.section_id, s.name AS section_name, a.views, a.updated_at,
                a.created_at,
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
    a.files = article_files(&core.db, id).await?;
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
    /// Вложения статьи (загруженные через `/api/helpdesk/files`); картинки из текста добавляются сами.
    #[serde(default)]
    pub files: Vec<Uuid>,
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
    let mut tx = core.db.begin().await?;
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
    .fetch_one(&mut *tx)
    .await?;
    sync_files(&mut tx, id, &b.files, &b.body, user.user_id).await?;
    audit::record(&mut *tx, &user.actor, Entry::new("kb.create", Source::Portal).object("kb_article", id)).await?;
    tx.commit().await?;
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
    let mut tx = core.db.begin().await?;
    let n = sqlx::query(
        "UPDATE kb_articles SET title = $2, body = $3, audience = $4, status = $5, section_id = $6, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(b.title.trim())
    .bind(&b.body)
    .bind(&b.audience)
    .bind(&b.status)
    .bind(b.section_id)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(ApiError::NotFound);
    }
    sync_files(&mut tx, id, &b.files, &b.body, user.user_id).await?;
    audit::record(&mut *tx, &user.actor, Entry::new("kb.update", Source::Portal).object("kb_article", id)).await?;
    tx.commit().await?;
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
    /// Открытые вложения заявки, скопированные для статьи (ссылки в тексте ведут на копии).
    pub files: Vec<KbFile>,
}

/// Копий вложений заявки в черновике.
const DRAFT_FILES: i64 = 20;

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
    // Вложения заявки видны только её участникам: для статьи — копии (то же содержимое на диске),
    // не попавшие в статью уберёт суточная уборка. Вложения внутренних заметок не копируются.
    let atts: Vec<(Uuid, String, i64, String, String)> = sqlx::query_as(
        "SELECT f.id, f.sha256, f.size, f.mime, f.original_name
           FROM ticket_attachments a JOIN files f ON f.id = a.file_id
           LEFT JOIN ticket_comments c ON c.id = a.comment_id
          WHERE a.ticket_id = $1 AND (a.comment_id IS NULL OR c.visibility = 'public')
          ORDER BY a.created_at LIMIT $2",
    )
    .bind(id)
    .bind(DRAFT_FILES)
    .fetch_all(&core.db)
    .await?;
    let mut body = body.trim_end().to_string();
    let mut files = Vec::new();
    // Повторное открытие черновика берёт уже сделанные копии (свои, ещё никуда не прикреплённые).
    let mut tx = core.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('kb-draft:' || $1::text, 0))")
        .bind(user.user_id)
        .execute(&mut *tx)
        .await?;
    let mut taken: Vec<Uuid> = Vec::new();
    for (old, sha, size, mime, name) in atts {
        let reuse: Option<Uuid> = sqlx::query_scalar(
            "SELECT f.id FROM files f
              WHERE f.uploaded_by = $1 AND f.sha256 = $2 AND f.original_name = $3 AND f.id <> ALL($4)
                AND f.created_at > now() - interval '1 day'
                AND NOT EXISTS (SELECT 1 FROM ticket_attachments a WHERE a.file_id = f.id)
                AND NOT EXISTS (SELECT 1 FROM kb_attachments k WHERE k.file_id = f.id)
              ORDER BY f.created_at LIMIT 1",
        )
        .bind(user.user_id)
        .bind(&sha)
        .bind(&name)
        .bind(&taken)
        .fetch_optional(&mut *tx)
        .await?;
        let new = match reuse {
            Some(id) => id,
            None => {
                sqlx::query_scalar(
                    "INSERT INTO files (sha256, size, mime, original_name, uploaded_by)
                     VALUES ($1, $2, $3, $4, $5) RETURNING id",
                )
                .bind(&sha)
                .bind(size)
                .bind(&mime)
                .bind(&name)
                .bind(user.user_id)
                .fetch_one(&mut *tx)
                .await?
            }
        };
        taken.push(new);
        body = body.replace(&old.to_string(), &new.to_string());
        files.push(KbFile { id: new, is_image: crate::files::is_inline_image(&mime), name, mime, size });
    }
    tx.commit().await?;
    Ok(Json(Draft { title, body, source_ticket_number: number, files }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_has_no_markup() {
        let md = "## Проблема\n\n![скрин](/api/helpdesk/files/0190a0b0-0000-7000-8000-000000000001)\n\
                  Не работает **VPN**, см. [инструкцию](https://example.test/vpn).\n\n- раз\n1. два\n> цитата";
        assert_eq!(plain_excerpt(md), "Проблема Не работает VPN, см. инструкцию. раз два цитата");
        let e = plain_excerpt(&"слово ".repeat(100));
        assert!(e.chars().count() <= 220, "{e}");
        assert!(e.ends_with('…'));
    }

    #[test]
    fn file_refs_are_found() {
        let a = "0190a0b0-0000-7000-8000-000000000001";
        let body = format!("![x](/api/helpdesk/files/{a}) и [файл](/api/helpdesk/files/{a}) /api/helpdesk/files/плохо");
        assert_eq!(file_refs(&body), vec![Uuid::parse_str(a).unwrap()]);
    }
}
