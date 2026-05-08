use crate::config::Config;
use crate::db::Db;
use crate::mcp;
use crate::utils::{gen_id, now_iso, sanitize_fts_query, today_date, validate_entity_id, NAME_REGEX};
use crate::VERSION;
use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, patch, post};
use axum::{Json, Router};
use rusqlite::params;
use rusqlite::params_from_iter;
use rusqlite::types::Value as SqlValue;
use serde::Deserialize;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info};

const CLAUDE_MD_CONTENT: &str = r#"# dbrain

You have an AI brain connected via MCP (dbrain). This is your persistent memory across all conversations and machines.

## What's in the brain

- **Documents**: Identity (who you are), User (who the user is), Soul (how to behave), Memory (narrative reflections). These are returned automatically with every `recall` call.
- **Entities + Facts**: Structured knowledge organized by PARA (Projects, Areas, Resources, Archives). Each entity (person, project, system, event) has facts attached. Facts have tiers: hot (recent/frequent), warm, cold.
- **Conversations**: Raw chat history from every AI session. Stored with source (e.g. 'claude-code-home', 'gemini-mobile').

## When to use each tool

- `recall` — Search before answering. Use this BEFORE responding to any question about the user, their preferences, projects, people they know, or past conversations. Also returns your identity docs. When in doubt, search first.
- `remember` — Save important facts. Use when the user shares preferences, makes decisions, mentions personal details, or says "remember this". One clear atomic fact per call.
- `log` — Store conversation messages. Use periodically to log what's happening in the conversation. Send both user and assistant messages.
- `get_entity` — Deep dive. When you need full context about a specific project, person, or system.
- `list_entities` — Discover. When you need to see what entities exist, filter by category or type.
- `create_entity` — New knowledge. When the user mentions a new project, person, or system worth tracking. Create the entity first, then `remember` facts about it.
- `bump` — Keep memories alive. When you use a fact to answer a question, bump it so it stays hot.
- `overview` — Brain stats. When the user asks "what do you know?" or you need a high-level picture.
- `wake_up` — Full identity load. Only needed if you want the complete identity documents outside of a recall.

## Rules

- At the start of every conversation, call `recall` with the user's first question or topic. This gives you both search results AND your identity.
- Never say "I don't know" about the user without searching first.
- When storing facts, be specific and atomic. "Favorite ice cream is pistachio" not "We talked about food preferences".
"#;

#[derive(Clone)]
pub struct AppState {
  pub config: Config,
  pub db: Db,
  /// Pre-computed "Bearer <token>" for constant-time comparison.
  pub bearer_token: String,
}

pub async fn serve_on(config: Config, db: Db) -> anyhow::Result<()> {
  let bearer_token = format!("Bearer {}", config.token);
  let state = AppState {
    config: config.clone(),
    db,
    bearer_token,
  };

  let public_routes = Router::new().route("/health", get(health));

  let protected_routes = Router::new()
    .route("/connect", get(connect))
    .route("/entities", get(list_entities).post(create_entity))
    .route("/entities/:id", get(get_entity).delete(delete_entity))
    .route(
      "/entities/:entity_id/facts",
      get(list_facts).post(create_fact),
    )
    .route("/facts/:id/access", patch(bump_fact_access))
    .route("/search", post(search))
    .route("/memory/summary", get(memory_summary))
    .route("/workspace", get(list_workspace))
    .route(
      "/workspace/:key",
      get(get_workspace).put(upsert_workspace).delete(delete_workspace),
    )
    .route(
      "/conversations",
      get(list_conversations).post(create_conversation),
    )
    .route("/conversations/pending", get(get_pending_conversations))
    .route("/conversations/:id", get(get_conversation))
    .route(
      "/conversations/:id/messages",
      get(get_conversation_messages).post(create_conversation_messages),
    )
    .route("/mcp", any(mcp_handler))
    .route_layer(middleware::from_fn_with_state(state.clone(), auth_middleware));

  let cors = CorsLayer::new()
    .allow_origin(Any)
    .allow_headers([
      axum::http::header::AUTHORIZATION,
      axum::http::header::CONTENT_TYPE,
      axum::http::header::ACCEPT,
    ])
    .allow_methods([
      Method::GET,
      Method::POST,
      Method::PUT,
      Method::PATCH,
      Method::DELETE,
      Method::OPTIONS,
    ]);

  let app = Router::new()
    .merge(public_routes)
    .merge(protected_routes)
    .with_state(state)
    .layer(cors);

  let addr = format!("{}:{}", config.host, config.port);
  info!("Listening on {addr}");
  let listener = tokio::net::TcpListener::bind(&addr).await?;
  axum::serve(listener, app).await?;
  Ok(())
}

async fn auth_middleware(
  State(state): State<AppState>,
  request: Request,
  next: Next,
) -> Response {
  if request.method() == Method::OPTIONS {
    return StatusCode::NO_CONTENT.into_response();
  }

  let auth = request
    .headers()
    .get(axum::http::header::AUTHORIZATION)
    .and_then(|h| h.to_str().ok())
    .unwrap_or_default();

  // Constant-time comparison to prevent timing attacks
  let expected = state.bearer_token.as_bytes();
  let provided = auth.as_bytes();
  if expected.len() != provided.len() || expected.ct_eq(provided).unwrap_u8() != 1 {
    return (
      StatusCode::UNAUTHORIZED,
      Json(json!({ "error": "Unauthorized" })),
    )
      .into_response();
  }

  next.run(request).await
}

// --- Query/Body structs ---

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchBody {
  query: String,
  limit: Option<i64>,
  #[serde(rename = "entityId")]
  entity_id: Option<String>,
  tier: Option<String>,
}

#[derive(Deserialize)]
struct EntityListQuery {
  category: Option<String>,
  #[serde(rename = "type")]
  entity_type: Option<String>,
}

#[derive(Deserialize)]
struct FactsListQuery {
  tier: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateEntityBody {
  id: String,
  name: String,
  #[serde(rename = "type")]
  entity_type: String,
  category: String,
  metadata: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateFactBody {
  id: String,
  fact: String,
  category: Option<String>,
  timestamp: Option<String>,
  source: Option<String>,
  #[serde(rename = "relatedEntities")]
  related_entities: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpsertWorkspaceBody {
  title: String,
  content: String,
}

#[derive(Deserialize)]
struct ConversationListQuery {
  source: Option<String>,
  limit: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateConversationBody {
  id: Option<String>,
  source: String,
}

#[derive(Deserialize)]
struct ConversationMessagesQuery {
  since: Option<String>,
  processed: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageInput {
  role: String,
  content: String,
}

// --- Helper: run blocking DB work off the async runtime ---

async fn with_db<F, R>(db: &Db, f: F) -> Result<R, Response>
where
  F: FnOnce(&rusqlite::Connection) -> Result<R, Response> + Send + 'static,
  R: Send + 'static,
{
  let db = db.clone();
  tokio::task::spawn_blocking(move || {
    let conn = db
      .get()
      .map_err(|e| {
        error!("Failed to get DB connection from pool: {e}");
        internal_error("Failed to acquire DB connection")
      })?;
    f(&conn)
  })
  .await
  .map_err(|e| {
    error!("spawn_blocking panicked: {e}");
    internal_error("Internal error")
  })?
}

/// Collect query_map rows into a Vec, returning internal_error on failure.
fn collect_rows(
  rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<Value>>,
) -> Result<Vec<Value>, Response> {
  let mut out = Vec::new();
  for row in rows {
    out.push(row.map_err(|e| {
      error!("Row mapping error: {e}");
      internal_error("Failed to map row")
    })?);
  }
  Ok(out)
}

// --- Handlers ---

async fn health(State(state): State<AppState>) -> Response {
  match with_db(&state.db, |db| {
    let entities = count(db, "SELECT COUNT(*) FROM entities");
    let facts = count(db, "SELECT COUNT(*) FROM facts");
    let documents = count(db, "SELECT COUNT(*) FROM documents");
    let conversations = count(db, "SELECT COUNT(*) FROM conversations");
    let unprocessed = count(db, "SELECT COUNT(*) FROM messages WHERE processed = 0");

    let name = db
      .query_row(
        "SELECT content FROM documents WHERE key = 'identity'",
        [],
        |row| row.get::<_, String>(0),
      )
      .ok()
      .and_then(|content| {
        NAME_REGEX
          .captures(&content)
          .and_then(|caps| caps.get(1))
          .map(|m| m.as_str().to_string())
      })
      .unwrap_or_else(|| "dbrain".to_string());

    Ok(json!({
      "status": "awake",
      "name": name,
      "version": VERSION,
      "entities": entities,
      "facts": facts,
      "documents": documents,
      "conversations": conversations,
      "unprocessed": unprocessed
    }))
  })
  .await
  {
    Ok(value) => Json(value).into_response(),
    Err(resp) => resp,
  }
}

async fn connect(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Response {
  let host = headers
    .get(axum::http::header::HOST)
    .and_then(|h| h.to_str().ok())
    .unwrap_or("localhost");

  let proto = headers
    .get("x-forwarded-proto")
    .and_then(|h| h.to_str().ok())
    .unwrap_or("http");

  let normalized_host = if host.contains(':') {
    host.to_string()
  } else {
    format!("{host}:{}", state.config.port)
  };

  Json(json!({
    "mcp": {
      "dbrain": {
        "type": "http",
        "url": format!("{proto}://{normalized_host}/mcp"),
        "headers": { "Authorization": &state.bearer_token }
      }
    },
    "permissions": ["mcp__dbrain__*"],
    "claudeMd": CLAUDE_MD_CONTENT
  }))
  .into_response()
}

async fn list_entities(
  State(state): State<AppState>,
  Query(query): Query<EntityListQuery>,
) -> Response {
  match with_db(&state.db, move |db| {
    let mut sql = "SELECT * FROM entities WHERE status = 'active'".to_string();
    let mut params = Vec::<String>::new();
    if let Some(category) = query.category {
      sql.push_str(" AND category = ?");
      params.push(category);
    }
    if let Some(entity_type) = query.entity_type {
      sql.push_str(" AND type = ?");
      params.push(entity_type);
    }
    sql.push_str(" ORDER BY updated_at DESC");

    let mut stmt = db.prepare(&sql).map_err(|e| {
      error!("Prepare error: {e}");
      internal_error("Failed to prepare query")
    })?;

    let rows = stmt
      .query_map(params_from_iter(params.iter()), |row| map_entity_row(row))
      .map_err(|e| {
        error!("Query error: {e}");
        internal_error("Failed to query entities")
      })?;

    collect_rows(rows)
  })
  .await
  {
    Ok(entities) => Json(Value::Array(entities)).into_response(),
    Err(resp) => resp,
  }
}

async fn get_entity(State(state): State<AppState>, Path(id): Path<String>) -> Response {
  match with_db(&state.db, move |db| {
    let entity = db
      .query_row("SELECT * FROM entities WHERE id = ?", params![id], |row| {
        map_entity_row(row)
      })
      .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => {
          (StatusCode::NOT_FOUND, Json(json!({ "error": "Entity not found" }))).into_response()
        }
        _ => {
          error!("Query error: {e}");
          internal_error("Failed to query entity")
        }
      })?;

    let mut entity = entity;
    let entity_id = entity.get("id").and_then(Value::as_str).unwrap_or_default().to_string();

    let mut stmt = db
      .prepare("SELECT * FROM facts WHERE entity_id = ? ORDER BY tier ASC, access_count DESC")
      .map_err(|e| {
        error!("Prepare error: {e}");
        internal_error("Failed to prepare facts query")
      })?;

    let rows = stmt
      .query_map(params![entity_id], |row| map_fact_row(row))
      .map_err(|e| {
        error!("Query error: {e}");
        internal_error("Failed to query facts")
      })?;

    let facts = collect_rows(rows)?;

    if let Value::Object(ref mut object) = entity {
      object.insert("facts".to_string(), Value::Array(facts));
    }

    Ok(entity)
  })
  .await
  {
    Ok(entity) => Json(entity).into_response(),
    Err(resp) => resp,
  }
}

async fn create_entity(
  State(state): State<AppState>,
  Json(body): Json<CreateEntityBody>,
) -> Response {
  // Validate entity ID format
  if let Err(msg) = validate_entity_id(&body.id) {
    return (StatusCode::BAD_REQUEST, Json(json!({ "error": msg.to_string() }))).into_response();
  }

  match with_db(&state.db, move |db| {
    let now = now_iso();
    let metadata = body.metadata.as_ref().map(|m| m.to_string());

    db.execute(
      "INSERT INTO entities (id, name, type, category, created_at, updated_at, metadata) VALUES (?, ?, ?, ?, ?, ?, ?)",
      params![body.id, body.name, body.entity_type, body.category, now, now, metadata],
    )
    .map_err(|e| {
      error!("Insert error: {e}");
      internal_error("Failed to create entity")
    })?;

    Ok(json!({
      "id": body.id,
      "name": body.name,
      "type": body.entity_type,
      "category": body.category
    }))
  })
  .await
  {
    Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
    Err(resp) => resp,
  }
}

async fn delete_entity(State(state): State<AppState>, Path(id): Path<String>) -> Response {
  match with_db(&state.db, move |db| {
    let changed = db
      .execute(
        "UPDATE entities SET status = 'archived', updated_at = ? WHERE id = ?",
        params![now_iso(), id],
      )
      .map_err(|e| {
        error!("Update error: {e}");
        internal_error("Failed to archive entity")
      })?;

    if changed == 0 {
      Err(
        (StatusCode::NOT_FOUND, Json(json!({ "error": "Entity not found" }))).into_response(),
      )
    } else {
      Ok(json!({ "id": id, "status": "archived" }))
    }
  })
  .await
  {
    Ok(value) => Json(value).into_response(),
    Err(resp) => resp,
  }
}

async fn list_facts(
  State(state): State<AppState>,
  Path(entity_id): Path<String>,
  Query(query): Query<FactsListQuery>,
) -> Response {
  match with_db(&state.db, move |db| {
    let exists: bool = db
      .query_row(
        "SELECT 1 FROM entities WHERE id = ?",
        params![entity_id],
        |_| Ok(true),
      )
      .unwrap_or(false);

    if !exists {
      return Err(
        (StatusCode::NOT_FOUND, Json(json!({ "error": "Entity not found" }))).into_response(),
      );
    }

    let mut sql = "SELECT * FROM facts WHERE entity_id = ?".to_string();
    let mut sql_params = vec![entity_id.clone()];
    if let Some(tier) = query.tier {
      sql.push_str(" AND tier = ?");
      sql_params.push(tier);
    }
    sql.push_str(" ORDER BY access_count DESC, last_accessed DESC");

    let mut stmt = db.prepare(&sql).map_err(|e| {
      error!("Prepare error: {e}");
      internal_error("Failed to prepare facts query")
    })?;

    let rows = stmt
      .query_map(params_from_iter(sql_params.iter()), |row| map_fact_row(row))
      .map_err(|e| {
        error!("Query error: {e}");
        internal_error("Failed to query facts")
      })?;

    collect_rows(rows)
  })
  .await
  {
    Ok(facts) => Json(Value::Array(facts)).into_response(),
    Err(resp) => resp,
  }
}

async fn create_fact(
  State(state): State<AppState>,
  Path(entity_id): Path<String>,
  Json(body): Json<CreateFactBody>,
) -> Response {
  match with_db(&state.db, move |db| {
    let exists: bool = db
      .query_row(
        "SELECT 1 FROM entities WHERE id = ?",
        params![entity_id],
        |_| Ok(true),
      )
      .unwrap_or(false);

    if !exists {
      return Err(
        (StatusCode::NOT_FOUND, Json(json!({ "error": "Entity not found" }))).into_response(),
      );
    }

    let now = today_date();
    let category = body.category.as_deref().unwrap_or("context");
    let timestamp = body.timestamp.as_deref().unwrap_or(&now);
    let source = body.source.as_deref().unwrap_or("api");
    let related = serde_json::to_string(&body.related_entities.as_deref().unwrap_or(&[]))
      .unwrap_or_else(|_| "[]".to_string());

    db.execute(
      "INSERT INTO facts (id, entity_id, fact, category, timestamp, last_accessed, access_count, tier, source, related_entities) VALUES (?, ?, ?, ?, ?, ?, 1, 'hot', ?, ?)",
      params![body.id, entity_id, body.fact, category, timestamp, now, source, related],
    )
    .map_err(|e| {
      error!("Insert error: {e}");
      internal_error("Failed to create fact")
    })?;

    let _ = db.execute(
      "UPDATE entities SET updated_at = ? WHERE id = ?",
      params![now, entity_id],
    );

    Ok(json!({
      "id": body.id,
      "entityId": entity_id,
      "fact": body.fact
    }))
  })
  .await
  {
    Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
    Err(resp) => resp,
  }
}

async fn bump_fact_access(State(state): State<AppState>, Path(id): Path<String>) -> Response {
  match with_db(&state.db, move |db| {
    let now = today_date();
    let changed = db
      .execute(
        "UPDATE facts SET last_accessed = ?, access_count = access_count + 1, tier = 'hot' WHERE id = ?",
        params![now, id],
      )
      .map_err(|e| {
        error!("Update error: {e}");
        internal_error("Failed to bump fact")
      })?;

    if changed == 0 {
      Err((StatusCode::NOT_FOUND, Json(json!({ "error": "Fact not found" }))).into_response())
    } else {
      Ok(json!({ "id": id, "bumped": true }))
    }
  })
  .await
  {
    Ok(value) => Json(value).into_response(),
    Err(resp) => resp,
  }
}

async fn search(State(state): State<AppState>, Json(body): Json<SearchBody>) -> Response {
  match with_db(&state.db, move |db| {
    let fts_query = sanitize_fts_query(&body.query);

    let mut sql = r#"
      SELECT f.*, e.name as entity_name, e.type as entity_type, e.category as entity_category, rank
      FROM facts_fts fts
      JOIN facts f ON f.rowid = fts.rowid
      JOIN entities e ON e.id = f.entity_id
      WHERE facts_fts MATCH ?
    "#
    .to_string();

    let mut sql_params: Vec<SqlValue> = vec![SqlValue::from(fts_query)];
    if let Some(entity_id) = body.entity_id {
      sql.push_str(" AND f.entity_id = ?");
      sql_params.push(SqlValue::from(entity_id));
    }
    if let Some(tier) = body.tier {
      sql.push_str(" AND f.tier = ?");
      sql_params.push(SqlValue::from(tier));
    }
    sql.push_str(" ORDER BY rank LIMIT ?");
    sql_params.push(SqlValue::from(body.limit.unwrap_or(10)));

    let mut stmt = db.prepare(&sql).map_err(|e| {
      error!("Prepare error: {e}");
      internal_error("Failed to prepare search query")
    })?;

    let rows = stmt
      .query_map(params_from_iter(sql_params.iter()), |row| {
        let rank: f64 = row.get("rank")?;
        Ok(json!({
          "fact": {
            "id": row.get::<_, String>("id")?,
            "entityId": row.get::<_, String>("entity_id")?,
            "fact": row.get::<_, String>("fact")?,
            "category": row.get::<_, String>("category")?,
            "timestamp": row.get::<_, String>("timestamp")?,
            "status": row.get::<_, String>("status")?,
            "lastAccessed": row.get::<_, String>("last_accessed")?,
            "accessCount": row.get::<_, i64>("access_count")?,
            "tier": row.get::<_, String>("tier")?,
            "source": row.get::<_, Option<String>>("source")?,
          },
          "entity": {
            "id": row.get::<_, String>("entity_id")?,
            "name": row.get::<_, String>("entity_name")?,
            "type": row.get::<_, String>("entity_type")?,
            "category": row.get::<_, String>("entity_category")?,
          },
          "score": -rank,
        }))
      })
      .map_err(|e| {
        error!("Query error: {e}");
        internal_error("Failed to execute search")
      })?;

    collect_rows(rows)
  })
  .await
  {
    Ok(results) => Json(Value::Array(results)).into_response(),
    Err(resp) => resp,
  }
}

async fn memory_summary(State(state): State<AppState>) -> Response {
  match with_db(&state.db, |db| {
    let mut stmt = db
      .prepare(
        r#"
      SELECT e.id, e.name, e.type, e.category,
        COUNT(CASE WHEN f.tier = 'hot' THEN 1 END) as hot,
        COUNT(CASE WHEN f.tier = 'warm' THEN 1 END) as warm,
        COUNT(CASE WHEN f.tier = 'cold' THEN 1 END) as cold,
        COUNT(f.id) as total
      FROM entities e LEFT JOIN facts f ON f.entity_id = e.id
      WHERE e.status = 'active' GROUP BY e.id ORDER BY total DESC
      "#,
      )
      .map_err(|e| {
        error!("Prepare error: {e}");
        internal_error("Failed to prepare summary query")
      })?;

    let rows = stmt
      .query_map([], |row| {
        Ok(json!({
          "id": row.get::<_, String>("id")?,
          "name": row.get::<_, String>("name")?,
          "type": row.get::<_, String>("type")?,
          "category": row.get::<_, String>("category")?,
          "hot": row.get::<_, i64>("hot")?,
          "warm": row.get::<_, i64>("warm")?,
          "cold": row.get::<_, i64>("cold")?,
          "total": row.get::<_, i64>("total")?,
        }))
      })
      .map_err(|e| {
        error!("Query error: {e}");
        internal_error("Failed to query memory summary")
      })?;

    collect_rows(rows)
  })
  .await
  {
    Ok(summary) => Json(Value::Array(summary)).into_response(),
    Err(resp) => resp,
  }
}

async fn list_workspace(State(state): State<AppState>) -> Response {
  match with_db(&state.db, |db| {
    let mut stmt = db
      .prepare("SELECT key, title, updated_at FROM documents ORDER BY key")
      .map_err(|e| {
        error!("Prepare error: {e}");
        internal_error("Failed to prepare workspace query")
      })?;

    let rows = stmt
      .query_map([], |row| {
        Ok(json!({
          "key": row.get::<_, String>("key")?,
          "title": row.get::<_, String>("title")?,
          "updated_at": row.get::<_, String>("updated_at")?,
        }))
      })
      .map_err(|e| {
        error!("Query error: {e}");
        internal_error("Failed to query workspace")
      })?;

    collect_rows(rows)
  })
  .await
  {
    Ok(docs) => Json(Value::Array(docs)).into_response(),
    Err(resp) => resp,
  }
}

async fn get_workspace(State(state): State<AppState>, Path(key): Path<String>) -> Response {
  match with_db(&state.db, move |db| {
    db.query_row(
      "SELECT key, title, content, updated_at FROM documents WHERE key = ?",
      params![key],
      |row| {
        Ok(json!({
          "key": row.get::<_, String>("key")?,
          "title": row.get::<_, String>("title")?,
          "content": row.get::<_, String>("content")?,
          "updated_at": row.get::<_, String>("updated_at")?,
        }))
      },
    )
    .map_err(|e| match e {
      rusqlite::Error::QueryReturnedNoRows => {
        (StatusCode::NOT_FOUND, Json(json!({ "error": "Document not found" }))).into_response()
      }
      _ => {
        error!("Query error: {e}");
        internal_error("Failed to query document")
      }
    })
  })
  .await
  {
    Ok(doc) => Json(doc).into_response(),
    Err(resp) => resp,
  }
}

async fn upsert_workspace(
  State(state): State<AppState>,
  Path(key): Path<String>,
  Json(body): Json<UpsertWorkspaceBody>,
) -> Response {
  match with_db(&state.db, move |db| {
    let now = now_iso();

    db.execute(
      "INSERT OR REPLACE INTO documents (key, title, content, updated_at) VALUES (?, ?, ?, ?)",
      params![key, body.title, body.content, now],
    )
    .map_err(|e| {
      error!("Upsert error: {e}");
      internal_error("Failed to upsert document")
    })?;

    Ok(json!({ "key": key, "title": body.title, "updated_at": now }))
  })
  .await
  {
    Ok(value) => Json(value).into_response(),
    Err(resp) => resp,
  }
}

async fn delete_workspace(State(state): State<AppState>, Path(key): Path<String>) -> Response {
  match with_db(&state.db, move |db| {
    let changed = db
      .execute("DELETE FROM documents WHERE key = ?", params![key])
      .map_err(|e| {
        error!("Delete error: {e}");
        internal_error("Failed to delete document")
      })?;

    if changed == 0 {
      Err(
        (StatusCode::NOT_FOUND, Json(json!({ "error": "Document not found" }))).into_response(),
      )
    } else {
      Ok(json!({ "key": key, "deleted": true }))
    }
  })
  .await
  {
    Ok(value) => Json(value).into_response(),
    Err(resp) => resp,
  }
}

async fn list_conversations(
  State(state): State<AppState>,
  Query(query): Query<ConversationListQuery>,
) -> Response {
  match with_db(&state.db, move |db| {
    let mut sql = "SELECT id, source, started_at, ended_at, summary FROM conversations".to_string();
    let mut sql_params: Vec<SqlValue> = Vec::new();
    if let Some(source) = query.source {
      sql.push_str(" WHERE source = ?");
      sql_params.push(SqlValue::from(source));
    }
    sql.push_str(" ORDER BY started_at DESC LIMIT ?");
    sql_params.push(SqlValue::from(query.limit.unwrap_or(50)));

    let mut stmt = db.prepare(&sql).map_err(|e| {
      error!("Prepare error: {e}");
      internal_error("Failed to prepare conversations query")
    })?;

    let rows = stmt
      .query_map(params_from_iter(sql_params.iter()), |row| {
        Ok(json!({
          "id": row.get::<_, String>("id")?,
          "source": row.get::<_, String>("source")?,
          "started_at": row.get::<_, String>("started_at")?,
          "ended_at": row.get::<_, Option<String>>("ended_at")?,
          "summary": row.get::<_, Option<String>>("summary")?,
        }))
      })
      .map_err(|e| {
        error!("Query error: {e}");
        internal_error("Failed to query conversations")
      })?;

    collect_rows(rows)
  })
  .await
  {
    Ok(conversations) => Json(Value::Array(conversations)).into_response(),
    Err(resp) => resp,
  }
}

async fn get_conversation(State(state): State<AppState>, Path(id): Path<String>) -> Response {
  match with_db(&state.db, move |db| {
    let mut conversation = db
      .query_row("SELECT * FROM conversations WHERE id = ?", params![id], |row| {
        Ok(json!({
          "id": row.get::<_, String>("id")?,
          "source": row.get::<_, String>("source")?,
          "started_at": row.get::<_, String>("started_at")?,
          "ended_at": row.get::<_, Option<String>>("ended_at")?,
          "summary": row.get::<_, Option<String>>("summary")?,
        }))
      })
      .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => {
          (StatusCode::NOT_FOUND, Json(json!({ "error": "Conversation not found" }))).into_response()
        }
        _ => {
          error!("Query error: {e}");
          internal_error("Failed to query conversation")
        }
      })?;

    let mut stmt = db
      .prepare(
        "SELECT id, role, content, timestamp, processed FROM messages WHERE conversation_id = ? ORDER BY timestamp",
      )
      .map_err(|e| {
        error!("Prepare error: {e}");
        internal_error("Failed to prepare messages query")
      })?;

    let rows = stmt
      .query_map(params![id], |row| {
        Ok(json!({
          "id": row.get::<_, String>("id")?,
          "role": row.get::<_, String>("role")?,
          "content": row.get::<_, String>("content")?,
          "timestamp": row.get::<_, String>("timestamp")?,
          "processed": row.get::<_, i64>("processed")?,
        }))
      })
      .map_err(|e| {
        error!("Query error: {e}");
        internal_error("Failed to query messages")
      })?;

    let messages = collect_rows(rows)?;

    if let Value::Object(ref mut object) = conversation {
      object.insert("messages".to_string(), Value::Array(messages));
    }

    Ok(conversation)
  })
  .await
  {
    Ok(value) => Json(value).into_response(),
    Err(resp) => resp,
  }
}

async fn create_conversation(
  State(state): State<AppState>,
  Json(body): Json<CreateConversationBody>,
) -> Response {
  match with_db(&state.db, move |db| {
    let conversation_id = body.id.unwrap_or_else(|| gen_id("conv"));
    let now = now_iso();

    db.execute(
      "INSERT INTO conversations (id, source, started_at) VALUES (?, ?, ?)",
      params![conversation_id, body.source, now],
    )
    .map_err(|e| {
      error!("Insert error: {e}");
      internal_error("Failed to create conversation")
    })?;

    Ok(json!({
      "id": conversation_id,
      "source": body.source,
      "started_at": now
    }))
  })
  .await
  {
    Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
    Err(resp) => resp,
  }
}

async fn create_conversation_messages(
  State(state): State<AppState>,
  Path(id): Path<String>,
  Json(body): Json<Value>,
) -> Response {
  match with_db(&state.db, move |db| {
    let exists: bool = db
      .query_row(
        "SELECT 1 FROM conversations WHERE id = ?",
        params![id],
        |_| Ok(true),
      )
      .unwrap_or(false);

    if !exists {
      return Err(
        (StatusCode::NOT_FOUND, Json(json!({ "error": "Conversation not found" }))).into_response(),
      );
    }

    let messages: Vec<MessageInput> = if body.is_array() {
      serde_json::from_value(body).map_err(|_| {
        (StatusCode::BAD_REQUEST, Json(json!({ "error": "Invalid message array" }))).into_response()
      })?
    } else {
      let msg: MessageInput = serde_json::from_value(body).map_err(|_| {
        (StatusCode::BAD_REQUEST, Json(json!({ "error": "Invalid message body" }))).into_response()
      })?;
      vec![msg]
    };

    let now = now_iso();
    let mut saved = Vec::with_capacity(messages.len());
    for (index, message) in messages.iter().enumerate() {
      let msg_id = gen_id("msg");
      let ts = if index == 0 {
        now.clone()
      } else {
        (chrono::Utc::now() + chrono::Duration::milliseconds(index as i64))
          .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
      };

      db.execute(
        "INSERT INTO messages (id, conversation_id, role, content, timestamp) VALUES (?, ?, ?, ?, ?)",
        params![msg_id, id, message.role, message.content, ts],
      )
      .map_err(|e| {
        error!("Insert error: {e}");
        internal_error("Failed to insert conversation message")
      })?;

      saved.push(json!({ "id": msg_id, "role": message.role, "timestamp": ts }));
    }

    if let Some(last) = saved.last() {
      let end = last
        .get("timestamp")
        .and_then(|v| v.as_str())
        .unwrap_or(&now);
      let _ = db.execute(
        "UPDATE conversations SET ended_at = ? WHERE id = ?",
        params![end, id],
      );
    }

    Ok(Value::Array(saved))
  })
  .await
  {
    Ok(value) => (StatusCode::CREATED, Json(value)).into_response(),
    Err(resp) => resp,
  }
}

async fn get_conversation_messages(
  State(state): State<AppState>,
  Path(id): Path<String>,
  Query(query): Query<ConversationMessagesQuery>,
) -> Response {
  match with_db(&state.db, move |db| {
    let exists: bool = db
      .query_row(
        "SELECT 1 FROM conversations WHERE id = ?",
        params![id],
        |_| Ok(true),
      )
      .unwrap_or(false);

    if !exists {
      return Err(
        (StatusCode::NOT_FOUND, Json(json!({ "error": "Conversation not found" }))).into_response(),
      );
    }

    let mut sql =
      "SELECT id, role, content, timestamp, processed FROM messages WHERE conversation_id = ?"
        .to_string();
    let mut sql_params: Vec<SqlValue> = vec![SqlValue::from(id)];
    if let Some(since) = query.since {
      sql.push_str(" AND timestamp > ?");
      sql_params.push(SqlValue::from(since));
    }
    if let Some(processed) = query.processed {
      sql.push_str(" AND processed = ?");
      sql_params.push(SqlValue::from(if processed == "true" { 1 } else { 0 }));
    }
    sql.push_str(" ORDER BY timestamp");

    let mut stmt = db.prepare(&sql).map_err(|e| {
      error!("Prepare error: {e}");
      internal_error("Failed to prepare conversation messages query")
    })?;

    let rows = stmt
      .query_map(params_from_iter(sql_params.iter()), |row| {
        Ok(json!({
          "id": row.get::<_, String>("id")?,
          "role": row.get::<_, String>("role")?,
          "content": row.get::<_, String>("content")?,
          "timestamp": row.get::<_, String>("timestamp")?,
          "processed": row.get::<_, i64>("processed")?,
        }))
      })
      .map_err(|e| {
        error!("Query error: {e}");
        internal_error("Failed to query conversation messages")
      })?;

    collect_rows(rows)
  })
  .await
  {
    Ok(messages) => Json(Value::Array(messages)).into_response(),
    Err(resp) => resp,
  }
}

async fn get_pending_conversations(State(state): State<AppState>) -> Response {
  match with_db(&state.db, |db| {
    let total: i64 = db
      .query_row(
        "SELECT COUNT(*) FROM messages WHERE processed = 0",
        [],
        |row| row.get(0),
      )
      .unwrap_or(0);

    let mut stmt = db
      .prepare(
        r#"
      SELECT c.id, c.source, c.started_at, COUNT(m.id) as unprocessed
      FROM conversations c
      JOIN messages m ON m.conversation_id = c.id AND m.processed = 0
      GROUP BY c.id ORDER BY c.started_at DESC
      "#,
      )
      .map_err(|e| {
        error!("Prepare error: {e}");
        internal_error("Failed to prepare pending conversations query")
      })?;

    let rows = stmt
      .query_map([], |row| {
        Ok(json!({
          "id": row.get::<_, String>("id")?,
          "source": row.get::<_, String>("source")?,
          "started_at": row.get::<_, String>("started_at")?,
          "unprocessed": row.get::<_, i64>("unprocessed")?,
        }))
      })
      .map_err(|e| {
        error!("Query error: {e}");
        internal_error("Failed to query pending conversations")
      })?;

    let conversations = collect_rows(rows)?;

    Ok(json!({
      "total_unprocessed": total,
      "conversations": conversations
    }))
  })
  .await
  {
    Ok(value) => Json(value).into_response(),
    Err(resp) => resp,
  }
}

async fn mcp_handler(State(state): State<AppState>, method: Method, body: Bytes) -> Response {
  if method != Method::POST {
    return (
      StatusCode::METHOD_NOT_ALLOWED,
      Json(json!({
        "jsonrpc": "2.0",
        "error": { "code": -32000, "message": "Method not allowed." },
        "id": null
      })),
    )
      .into_response();
  }

  let request: Value = match serde_json::from_slice(&body) {
    Ok(value) => value,
    Err(_) => {
      return (
        StatusCode::BAD_REQUEST,
        Json(json!({
          "jsonrpc": "2.0",
          "error": { "code": -32700, "message": "Parse error" },
          "id": null
        })),
      )
        .into_response()
    }
  };

  // MCP handler: get a connection from pool and pass it directly — no deadlock
  match with_db(&state.db, move |conn| {
    mcp::handle_request(conn, request).map_err(|e| {
      error!("MCP error: {e}");
      internal_error(&e.to_string())
    })
  })
  .await
  {
    Ok(Some(response)) => Json(response).into_response(),
    Ok(None) => StatusCode::NO_CONTENT.into_response(),
    Err(resp) => resp,
  }
}

// --- Row mapping helpers ---

fn map_entity_row(row: &rusqlite::Row) -> rusqlite::Result<Value> {
  let metadata_raw: Option<String> = row.get("metadata")?;
  let metadata = metadata_raw
    .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
    .unwrap_or(Value::Null);

  Ok(json!({
    "id": row.get::<_, String>("id")?,
    "name": row.get::<_, String>("name")?,
    "type": row.get::<_, String>("type")?,
    "category": row.get::<_, String>("category")?,
    "status": row.get::<_, String>("status")?,
    "created_at": row.get::<_, String>("created_at")?,
    "updated_at": row.get::<_, String>("updated_at")?,
    "metadata": metadata,
  }))
}

fn map_fact_row(row: &rusqlite::Row) -> rusqlite::Result<Value> {
  let related_raw: String = row.get("related_entities")?;
  let related = serde_json::from_str::<Value>(&related_raw).unwrap_or_else(|_| json!([]));
  Ok(json!({
    "id": row.get::<_, String>("id")?,
    "entity_id": row.get::<_, String>("entity_id")?,
    "fact": row.get::<_, String>("fact")?,
    "category": row.get::<_, String>("category")?,
    "timestamp": row.get::<_, String>("timestamp")?,
    "status": row.get::<_, String>("status")?,
    "superseded_by": row.get::<_, Option<String>>("superseded_by")?,
    "related_entities": related,
    "last_accessed": row.get::<_, String>("last_accessed")?,
    "access_count": row.get::<_, i64>("access_count")?,
    "tier": row.get::<_, String>("tier")?,
    "source": row.get::<_, Option<String>>("source")?,
  }))
}

// --- Utility ---

fn internal_error(message: &str) -> Response {
  (
    StatusCode::INTERNAL_SERVER_ERROR,
    Json(json!({ "error": message })),
  )
    .into_response()
}

fn count(db: &rusqlite::Connection, sql: &str) -> i64 {
  db.query_row(sql, [], |row| row.get(0)).unwrap_or(0)
}
