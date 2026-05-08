use crate::utils::{gen_id, now_iso, sanitize_fts_query, today_date};
use anyhow::{anyhow, Result};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection};
use serde_json::{json, Value};

use crate::VERSION;

pub fn handle_request(conn: &Connection, request: Value) -> Result<Option<Value>> {
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Missing method"))?;

    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

    match method {
        "initialize" => Ok(Some(jsonrpc_result(
            id,
            json!({
              "protocolVersion": "2024-11-05",
              "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false }
              },
              "serverInfo": { "name": "dbrain", "version": VERSION }
            }),
        ))),
        "notifications/initialized" => Ok(None),
        "tools/list" => Ok(Some(jsonrpc_result(id, list_tools()))),
        "resources/list" => Ok(Some(jsonrpc_result(id, list_resources()))),
        "resources/read" => Ok(Some(jsonrpc_result(id, read_resource(conn, &params)?))),
        "tools/call" => Ok(Some(jsonrpc_result(id, call_tool(conn, &params)?))),
        _ => Ok(Some(jsonrpc_error(
            id,
            -32601,
            &format!("Method not found: {method}"),
        ))),
    }
}

fn jsonrpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn jsonrpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
      "jsonrpc": "2.0",
      "id": id,
      "error": { "code": code, "message": message }
    })
}

fn list_resources() -> Value {
    json!({
      "resources": [
        {
          "uri": "dbrain://brain",
          "name": "brain",
          "title": "dbrain",
          "description": "Your connected AI brain — identity, user, and how to use memory tools",
          "mimeType": "text/plain"
        }
      ]
    })
}

fn list_tools() -> Value {
    json!({
      "tools": [
        {
          "name": "wake_up",
          "description": "Call this ONCE at the start of every conversation. Returns your identity, the user profile, and behavioral rules. You are not ready to help until you've called this.",
          "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
          "name": "recall",
          "description": "Search your memory. Use this BEFORE answering any question about the user, their projects, preferences, or past conversations. If you're not sure, search first.",
          "inputSchema": {
            "type": "object",
            "properties": {
              "query": { "type": "string", "description": "What to search for" },
              "limit": { "type": "number", "description": "Max results", "default": 10 }
            },
            "required": ["query"]
          }
        },
        {
          "name": "remember",
          "description": "Save something important to memory. One atomic fact per call.",
          "inputSchema": {
            "type": "object",
            "properties": {
              "entityId": { "type": "string", "description": "Entity to attach the fact to" },
              "fact": { "type": "string", "description": "The fact to remember" },
              "category": { "type": "string", "description": "Fact category", "default": "context" }
            },
            "required": ["entityId", "fact"]
          }
        },
        {
          "name": "get_entity",
          "description": "Read an entity and all its facts. Use this to get full context about a project, person, system, or event.",
          "inputSchema": {
            "type": "object",
            "properties": { "id": { "type": "string", "description": "Entity ID" } },
            "required": ["id"]
          }
        },
        {
          "name": "list_entities",
          "description": "List all known entities. Filter by PARA category or type.",
          "inputSchema": {
            "type": "object",
            "properties": {
              "category": { "type": "string", "description": "PARA category" },
              "type": { "type": "string", "description": "Entity type" }
            }
          }
        },
        {
          "name": "create_entity",
          "description": "Create a new entity (project, person, system, event). Use this before remembering facts about something new.",
          "inputSchema": {
            "type": "object",
            "properties": {
              "id": { "type": "string" },
              "name": { "type": "string" },
              "type": { "type": "string", "enum": ["project", "person", "system", "event", "resource"] },
              "category": { "type": "string", "enum": ["projects", "areas", "resources", "archives"] }
            },
            "required": ["id", "name", "type", "category"]
          }
        },
        {
          "name": "bump",
          "description": "Touch a memory to keep it alive. Call this when you use a fact to answer a question.",
          "inputSchema": {
            "type": "object",
            "properties": { "factId": { "type": "string" } },
            "required": ["factId"]
          }
        },
        {
          "name": "log",
          "description": "Send conversation messages to the brain for storage.",
          "inputSchema": {
            "type": "object",
            "properties": {
              "source": { "type": "string" },
              "conversationId": { "type": "string" },
              "messages": {
                "type": "array",
                "items": {
                  "type": "object",
                  "properties": {
                    "role": { "type": "string", "enum": ["user", "assistant"] },
                    "content": { "type": "string" }
                  },
                  "required": ["role", "content"]
                }
              }
            },
            "required": ["source", "messages"]
          }
        },
        {
          "name": "overview",
          "description": "Get a summary of everything in the brain: entities, facts by tier, conversations.",
          "inputSchema": { "type": "object", "properties": {} }
        }
      ]
    })
}

fn read_resource(conn: &Connection, params: &Value) -> Result<Value> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if uri != "dbrain://brain" {
        return Ok(json!({ "contents": [] }));
    }

    let identity: Option<String> = conn
        .query_row(
            "SELECT content FROM documents WHERE key = 'identity'",
            [],
            |row| row.get(0),
        )
        .ok();
    let user: Option<String> = conn
        .query_row(
            "SELECT content FROM documents WHERE key = 'user'",
            [],
            |row| row.get(0),
        )
        .ok();
    let entities: i64 = conn
        .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
        .unwrap_or(0);
    let facts: i64 = conn
        .query_row("SELECT COUNT(*) FROM facts", [], |row| row.get(0))
        .unwrap_or(0);

    let text = [
    "You have an AI brain connected (dbrain).",
    "",
    identity.as_deref().unwrap_or_default(),
    "",
    user.as_deref().unwrap_or_default(),
    "",
    &format!("Brain stats: {entities} entities, {facts} facts stored."),
    "",
    "IMPORTANT: Use the `recall` tool BEFORE answering any question about the user, their preferences, projects, or past conversations.",
    "Use `remember` to save new facts when the user shares something worth remembering.",
  ]
  .join("\n");

    Ok(json!({
      "contents": [
        {
          "uri": "dbrain://brain",
          "mimeType": "text/plain",
          "text": text
        }
      ]
    }))
}

fn call_tool(conn: &Connection, params: &Value) -> Result<Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Missing tool name"))?;
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let payload = match name {
        "wake_up" => tool_wake_up(conn)?,
        "recall" => tool_recall(conn, &args)?,
        "remember" => tool_remember(conn, &args)?,
        "get_entity" => tool_get_entity(conn, &args)?,
        "list_entities" => tool_list_entities(conn, &args)?,
        "create_entity" => tool_create_entity(conn, &args)?,
        "bump" => tool_bump(conn, &args)?,
        "log" => tool_log(conn, &args)?,
        "overview" => tool_overview(conn)?,
        _ => json!({ "error": format!("Unknown tool: {name}") }),
    };

    Ok(json!({
      "content": [
        {
          "type": "text",
          "text": payload.to_string()
        }
      ]
    }))
}

fn tool_wake_up(conn: &Connection) -> Result<Value> {
    let mut stmt = conn.prepare("SELECT key, title, content FROM documents ORDER BY key")?;
    let rows = stmt.query_map([], |row| {
        Ok(json!({
          "key": row.get::<_, String>(0)?,
          "title": row.get::<_, String>(1)?,
          "content": row.get::<_, String>(2)?
        }))
    })?;

    let mut documents = Vec::new();
    for row in rows {
        documents.push(row?);
    }

    let entities: i64 = conn
        .query_row("SELECT COUNT(*) FROM entities", [], |row| row.get(0))
        .unwrap_or(0);
    let facts: i64 = conn
        .query_row("SELECT COUNT(*) FROM facts", [], |row| row.get(0))
        .unwrap_or(0);

    Ok(json!({
      "documents": documents,
      "stats": { "entities": entities, "facts": facts }
    }))
}

fn tool_recall(conn: &Connection, args: &Value) -> Result<Value> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("query is required"))?;
    let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(10);

    let fts_query = sanitize_fts_query(query);

    let mut stmt = conn.prepare(
        r#"
    SELECT f.id, f.fact, f.category, f.tier, f.access_count,
           e.name as entity_name, e.type as entity_type, rank
    FROM facts_fts fts
    JOIN facts f ON f.rowid = fts.rowid
    JOIN entities e ON e.id = f.entity_id
    WHERE facts_fts MATCH ?
    ORDER BY rank LIMIT ?
    "#,
    )?;

    let today = today_date();
    let rows = stmt.query_map(params![fts_query, limit], |row| {
        Ok((
            row.get::<_, String>("id")?,
            json!({
              "fact": row.get::<_, String>("fact")?,
              "entity": row.get::<_, String>("entity_name")?,
              "entityType": row.get::<_, String>("entity_type")?,
              "category": row.get::<_, String>("category")?,
              "tier": row.get::<_, String>("tier")?,
              "score": -row.get::<_, f64>("rank")?
            }),
        ))
    })?;

    // Collect first, then batch-update access counts to avoid write-during-read
    let mut results = Vec::new();
    let mut ids_to_bump = Vec::new();
    for row in rows {
        let (id, value) = row?;
        ids_to_bump.push(id);
        results.push(value);
    }

    for id in &ids_to_bump {
        let _ = conn.execute(
            "UPDATE facts SET last_accessed = ?, access_count = access_count + 1 WHERE id = ?",
            params![today, id],
        );
    }

    let mut docs_stmt = conn.prepare("SELECT key, content FROM documents ORDER BY key")?;
    let docs_rows = docs_stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;

    let mut identity = serde_json::Map::new();
    for row in docs_rows {
        let (key, content) = row?;
        identity.insert(key, Value::String(content));
    }

    Ok(json!({
      "identity": identity,
      "results": results
    }))
}

fn tool_remember(conn: &Connection, args: &Value) -> Result<Value> {
    let entity_id = args
        .get("entityId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("entityId is required"))?;
    let fact = args
        .get("fact")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("fact is required"))?;
    let category = args
        .get("category")
        .and_then(Value::as_str)
        .unwrap_or("context");

    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM entities WHERE id = ?",
            params![entity_id],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if !exists {
        return Ok(
            json!({ "error": format!("Entity '{entity_id}' not found. Create it first or use list_entities to find the right one.") }),
        );
    }

    let id = gen_id("fact");
    let today = today_date();
    conn.execute(
    "INSERT INTO facts (id, entity_id, fact, category, timestamp, last_accessed, access_count, tier, source, related_entities) VALUES (?, ?, ?, ?, ?, ?, 1, 'hot', 'mcp', '[]')",
    params![id, entity_id, fact, category, today, today],
  )?;
    conn.execute(
        "UPDATE entities SET updated_at = ? WHERE id = ?",
        params![today, entity_id],
    )?;

    Ok(json!({ "saved": true, "id": id, "entityId": entity_id, "fact": fact }))
}

fn tool_get_entity(conn: &Connection, args: &Value) -> Result<Value> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("id is required"))?;

    let entity = conn.query_row("SELECT * FROM entities WHERE id = ?", params![id], |row| {
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
          "metadata": metadata
        }))
    });

    let mut entity = match entity {
        Ok(entity) => entity,
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            return Ok(json!({ "error": "Entity not found" }))
        }
        Err(error) => return Err(error.into()),
    };

    let mut stmt = conn.prepare(
    "SELECT id, fact, category, tier, access_count, last_accessed FROM facts WHERE entity_id = ? ORDER BY tier ASC, access_count DESC",
  )?;
    let rows = stmt.query_map(params![id], |row| {
        Ok(json!({
          "id": row.get::<_, String>("id")?,
          "fact": row.get::<_, String>("fact")?,
          "category": row.get::<_, String>("category")?,
          "tier": row.get::<_, String>("tier")?,
          "access_count": row.get::<_, i64>("access_count")?,
          "last_accessed": row.get::<_, String>("last_accessed")?
        }))
    })?;

    let mut facts = Vec::new();
    for row in rows {
        facts.push(row?);
    }

    if let Value::Object(ref mut object) = entity {
        object.insert("facts".to_string(), Value::Array(facts));
    }

    Ok(entity)
}

fn tool_list_entities(conn: &Connection, args: &Value) -> Result<Value> {
    let category = args.get("category").and_then(Value::as_str);
    let entity_type = args.get("type").and_then(Value::as_str);

    let mut sql =
        "SELECT id, name, type, category, status FROM entities WHERE status = 'active'".to_string();
    let mut sql_params: Vec<SqlValue> = Vec::new();
    if let Some(category) = category {
        sql.push_str(" AND category = ?");
        sql_params.push(SqlValue::from(category.to_string()));
    }
    if let Some(entity_type) = entity_type {
        sql.push_str(" AND type = ?");
        sql_params.push(SqlValue::from(entity_type.to_string()));
    }
    sql.push_str(" ORDER BY updated_at DESC");

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(sql_params.iter()), |row| {
        Ok(json!({
          "id": row.get::<_, String>("id")?,
          "name": row.get::<_, String>("name")?,
          "type": row.get::<_, String>("type")?,
          "category": row.get::<_, String>("category")?,
          "status": row.get::<_, String>("status")?
        }))
    })?;

    let mut entities = Vec::new();
    for row in rows {
        entities.push(row?);
    }

    Ok(Value::Array(entities))
}

fn tool_create_entity(conn: &Connection, args: &Value) -> Result<Value> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("id is required"))?;

    // Validate entity ID format
    crate::utils::validate_entity_id(id)?;

    let name = args
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("name is required"))?;
    let entity_type = args
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("type is required"))?;
    let category = args
        .get("category")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("category is required"))?;

    let exists: bool = conn
        .query_row("SELECT 1 FROM entities WHERE id = ?", params![id], |_| {
            Ok(true)
        })
        .unwrap_or(false);

    if exists {
        return Ok(json!({ "error": format!("Entity '{id}' already exists") }));
    }

    let now = now_iso();
    conn.execute(
    "INSERT INTO entities (id, name, type, category, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
    params![id, name, entity_type, category, now, now],
  )?;

    Ok(
        json!({ "created": true, "id": id, "name": name, "type": entity_type, "category": category }),
    )
}

fn tool_bump(conn: &Connection, args: &Value) -> Result<Value> {
    let fact_id = args
        .get("factId")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("factId is required"))?;

    let today = today_date();
    let changed = conn.execute(
    "UPDATE facts SET last_accessed = ?, access_count = access_count + 1, tier = 'hot' WHERE id = ?",
    params![today, fact_id],
  )?;

    if changed == 0 {
        return Ok(json!({ "error": "Fact not found" }));
    }
    Ok(json!({ "bumped": true, "factId": fact_id }))
}

fn tool_log(conn: &Connection, args: &Value) -> Result<Value> {
    let source = args
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("source is required"))?;
    let messages = args
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("messages is required"))?;

    let conversation_id = args
        .get("conversationId")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| gen_id("conv"));

    let exists: bool = conn
        .query_row(
            "SELECT 1 FROM conversations WHERE id = ?",
            params![conversation_id],
            |_| Ok(true),
        )
        .unwrap_or(false);

    if !exists {
        conn.execute(
            "INSERT INTO conversations (id, source, started_at) VALUES (?, ?, ?)",
            params![conversation_id, source, now_iso()],
        )?;
    }

    let now = now_iso();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("assistant");
        let content = message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        conn.execute(
      "INSERT INTO messages (id, conversation_id, role, content, timestamp) VALUES (?, ?, ?, ?, ?)",
      params![gen_id("msg"), conversation_id, role, content, now],
    )?;
    }

    conn.execute(
        "UPDATE conversations SET ended_at = ? WHERE id = ?",
        params![now_iso(), conversation_id],
    )?;

    Ok(json!({ "logged": true, "conversationId": conversation_id, "count": messages.len() }))
}

fn tool_overview(conn: &Connection) -> Result<Value> {
    let mut stmt = conn.prepare(
        r#"
    SELECT e.id, e.name, e.type, e.category,
      COUNT(CASE WHEN f.tier = 'hot' THEN 1 END) as hot,
      COUNT(CASE WHEN f.tier = 'warm' THEN 1 END) as warm,
      COUNT(CASE WHEN f.tier = 'cold' THEN 1 END) as cold,
      COUNT(f.id) as total
    FROM entities e LEFT JOIN facts f ON f.entity_id = e.id
    WHERE e.status = 'active' GROUP BY e.id ORDER BY total DESC
    "#,
    )?;
    let rows = stmt.query_map([], |row| {
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
    })?;

    let mut entities = Vec::new();
    for row in rows {
        entities.push(row?);
    }

    let conversations: i64 = conn
        .query_row("SELECT COUNT(*) FROM conversations", [], |row| row.get(0))
        .unwrap_or(0);
    let messages: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap_or(0);
    let unprocessed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE processed = 0",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    Ok(json!({
      "entities": entities,
      "conversations": conversations,
      "messages": messages,
      "unprocessed": unprocessed
    }))
}
