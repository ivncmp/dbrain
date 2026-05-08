use crate::config::{
  default_data_path, load_config, resolve_data_path, save_config, to_absolute, Config, Tiers,
};
use crate::dashboard::start_dashboard;
use crate::db::create_database;
use crate::server::serve_on;
use crate::utils::{gen_id, now_iso, today_date, NAME_REGEX};
use anyhow::{anyhow, Context, Result};
use clap::{Parser, Subcommand};
use dialoguer::{theme::ColorfulTheme, Input, Select};
use reqwest::StatusCode;
use rusqlite::params;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::Path;
use tracing::warn;

#[derive(Parser)]
#[command(name = "dbrain", version = crate::VERSION)]
struct Cli {
  #[command(subcommand)]
  command: Option<Command>,
}

const SUPPORTED_CLIENTS: &[&str] = &["claude", "opencode"];

#[derive(Subcommand)]
enum Command {
  Init {
    path: Option<String>,
    #[arg(long)]
    non_interactive: bool,
  },
  Start {
    path: Option<String>,
  },
  Connect {
    /// Client to configure (claude, opencode)
    client: Option<String>,
    url: Option<String>,
    #[arg(long)]
    token: Option<String>,
  },
  Status {
    path: Option<String>,
  },
}

pub(crate) async fn run() -> Result<()> {
  let cli = Cli::parse();

  match cli.command {
    Some(Command::Init {
      path,
      non_interactive,
    }) => init(path.as_deref(), non_interactive).await,
    Some(Command::Start { path }) => start(path.as_deref()).await,
    Some(Command::Connect { client, url, token }) => connect(client, url, token).await,
    Some(Command::Status { path }) => status(path.as_deref()),
    None => {
      println!(
        "\ndbrain — Your distributed mind. Wherever you go, I remember.\n\nUsage:\n  dbrain init [path]                           Initialize a new brain (server)\n  dbrain start [path]                          Wake up\n  dbrain connect <client> [url] [--token=]     Connect a client to a brain\n  dbrain status [path]                         Check brain status\n\nSupported clients: {}\n",
        SUPPORTED_CLIENTS.join(", ")
      );
      Ok(())
    }
  }
}

/// Mask a token, showing only the first 16 characters.
fn mask_token(token: &str) -> String {
  if token.len() > 16 {
    format!("{}...", &token[..16])
  } else {
    token.to_string()
  }
}

async fn init(path_arg: Option<&str>, flag_non_interactive: bool) -> Result<()> {
  let non_interactive = flag_non_interactive || env::var("DBRAIN_NON_INTERACTIVE").unwrap_or_default() == "1";

  let existing_path = resolve_data_path(path_arg)?;
  let existing_config = existing_path.join("config.json");
  if existing_config.exists() {
    println!("Brain already exists at {}", existing_path.display());
    println!("To connect a client, run: dbrain connect <url>");
    println!("Done.");
    return Ok(());
  }

  if non_interactive {
    return init_non_interactive(path_arg);
  }

  println!("dbrain — Your distributed mind");
  let theme = ColorfulTheme::default();

  let initial_path = path_arg
    .map(ToString::to_string)
    .unwrap_or_else(|| default_data_path().map(|p| p.display().to_string()).unwrap_or_else(|_| ".dbrain".to_string()));

  let data_path_input: String = Input::with_theme(&theme)
    .with_prompt("Where should the brain live?")
    .default(initial_path)
    .interact_text()
    .context("Init cancelled")?;

  let port_input: String = Input::with_theme(&theme)
    .with_prompt("API port?")
    .default("7878".to_string())
    .validate_with(|value: &String| -> Result<(), &str> {
      match value.parse::<u16>() {
        Ok(p) if p < 65535 => Ok(()), // Reserve room for dashboard port + 1
        Ok(_) => Err("Port must be less than 65535 (dashboard uses port+1)"),
        Err(_) => Err("Invalid port"),
      }
    })
    .interact_text()
    .context("Init cancelled")?;

  let host_options = [
    "0.0.0.0 — All interfaces (accessible from network)",
    "127.0.0.1 — Localhost only",
  ];
  let host_index = Select::with_theme(&theme)
    .with_prompt("Bind address?")
    .default(0)
    .items(&host_options)
    .interact()
    .context("Init cancelled")?;
  let host = if host_index == 0 {
    "0.0.0.0".to_string()
  } else {
    "127.0.0.1".to_string()
  };

  let token: String = Input::with_theme(&theme)
    .with_prompt("Access token (leave default to auto-generate)")
    .default(generate_token())
    .interact_text()
    .context("Init cancelled")?;

  let agent_name: String = Input::with_theme(&theme)
    .with_prompt("What's my name? (your AI's identity)")
    .default("dBrain".to_string())
    .interact_text()
    .context("Init cancelled")?;

  let owner_name: String = Input::with_theme(&theme)
    .with_prompt("And you are...? (your name)")
    .validate_with(|value: &String| -> Result<(), &str> {
      if value.trim().is_empty() {
        return Err("Name is required");
      }
      Ok(())
    })
    .interact_text()
    .context("Init cancelled")?;

  let owner_timezone: String = Input::with_theme(&theme)
    .with_prompt("Your timezone")
    .default(env::var("TZ").unwrap_or_else(|_| "UTC".to_string()))
    .interact_text()
    .context("Init cancelled")?;

  let data_path = to_absolute(crate::config::expand_tilde(&data_path_input));
  if data_path.join("config.json").exists() {
    println!("Brain already exists at {}", data_path.display());
    println!("To connect a client, run: dbrain connect <url>");
    println!("Done.");
    return Ok(());
  }

  let port = port_input.parse::<u16>().map_err(|_| anyhow!("Invalid port"))?;
  let config = Config {
    data_path: data_path.display().to_string(),
    port,
    host,
    token: token.clone(),
    tiers: Tiers {
      hot_days: 7,
      hot_min_access: 10,
      warm_days: 30,
    },
  };

  save_config(&config)?;
  let db = create_database(&config)?;
  seed_database(&db, &agent_name, &owner_name, &owner_timezone)?;

  println!("Brain ready — I'm {}, and I know {}", agent_name, owner_name);
  println!("\nConfiguration");
  println!("Data:   {}", config.data_path);
  println!("Port:   {}", config.port);
  println!("Host:   {}", config.host);
  println!("Token:  {}", mask_token(&config.token));

  println!("\nNext steps");
  println!("Start the server:");
  println!("  dbrain start");
  println!();
  println!("Then connect clients:");
  println!("  dbrain connect http://localhost:{}", config.port);
  println!();
  println!("Brain online. Wherever you go, I'll remember.");

  Ok(())
}

fn init_non_interactive(path_arg: Option<&str>) -> Result<()> {
  let data_path = if let Some(path) = path_arg {
    to_absolute(crate::config::expand_tilde(path))
  } else if let Ok(path) = env::var("DBRAIN_DATA") {
    to_absolute(crate::config::expand_tilde(&path))
  } else {
    default_data_path()?
  };

  let port = env::var("DBRAIN_PORT")
    .ok()
    .and_then(|value| value.parse::<u16>().ok())
    .unwrap_or(7878);
  let host = env::var("DBRAIN_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
  let token = env::var("DBRAIN_TOKEN").unwrap_or_else(|_| generate_token());
  let agent_name = env::var("DBRAIN_AGENT_NAME").unwrap_or_else(|_| "dBrain".to_string());
  let owner_name = env::var("DBRAIN_OWNER_NAME").unwrap_or_else(|_| "Human".to_string());
  let owner_timezone = env::var("DBRAIN_TIMEZONE").unwrap_or_else(|_| "UTC".to_string());

  if data_path.join("config.json").exists() {
    println!("Config already exists at {}, skipping init.", data_path.display());
    return Ok(());
  }

  let config = Config {
    data_path: data_path.display().to_string(),
    port,
    host,
    token: token.clone(),
    tiers: Tiers {
      hot_days: 7,
      hot_min_access: 10,
      warm_days: 30,
    },
  };

  save_config(&config)?;
  let db = create_database(&config)?;
  seed_database(&db, &agent_name, &owner_name, &owner_timezone)?;

  println!(
    "Brain initialized at {} (agent: {}, owner: {})",
    config.data_path, agent_name, owner_name
  );
  println!("Token: {}", mask_token(&token));
  println!("Connect clients with: dbrain connect http://localhost:{}", port);

  Ok(())
}

async fn start(path_arg: Option<&str>) -> Result<()> {
  // Initialize structured logging (respects RUST_LOG env var, defaults to info)
  tracing_subscriber::fmt()
    .with_env_filter(
      tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
    )
    .init();

  let data_path = resolve_data_path(path_arg)?;
  let config = load_config(&data_path)?;
  let db = create_database(&config)?;

  let dashboard_port = config.port.checked_add(1).ok_or_else(|| {
    anyhow!("Port overflow: API port {} is too high for dashboard (needs port+1)", config.port)
  })?;
  start_dashboard(dashboard_port, &config.host);

  let name = {
    let conn = db
      .get()
      .map_err(|e| anyhow!("Failed to get DB connection: {e}"))?;
    let identity: Option<String> = conn
      .query_row(
        "SELECT content FROM documents WHERE key = 'identity'",
        [],
        |row| row.get(0),
      )
      .ok();
    identity
      .and_then(|content| {
        NAME_REGEX
          .captures(&content)
          .and_then(|caps| caps.get(1))
          .map(|m| m.as_str().to_string())
      })
      .unwrap_or_else(|| "dbrain".to_string())
  };

  println!(
    "\n{} is awake\n\n  API:       http://{}:{}\n  Dashboard: http://{}:{}\n  Brain:     {}\n  Token:     {}...\n",
    name,
    config.host,
    config.port,
    config.host,
    dashboard_port,
    config.data_path,
    config.token.chars().take(16).collect::<String>()
  );

  serve_on(config, db).await
}

async fn connect(client_arg: Option<String>, url_arg: Option<String>, token_arg: Option<String>) -> Result<()> {
  let theme = ColorfulTheme::default();
  println!("dbrain — Connect to a brain");

  // Validate client
  let client = match client_arg {
    Some(c) if SUPPORTED_CLIENTS.contains(&c.as_str()) => c,
    Some(c) => {
      println!("Unknown client \"{c}\". Supported: {}", SUPPORTED_CLIENTS.join(", "));
      return Ok(());
    }
    None => {
      println!("Client is required. Usage: dbrain connect <client> [url] [--token=...]");
      println!("Supported clients: {}", SUPPORTED_CLIENTS.join(", "));
      return Ok(());
    }
  };

  if client == "opencode" {
    println!("opencode support is not available yet. Coming soon.");
    return Ok(());
  }

  let mut url = match url_arg {
    Some(url) => url,
    None => Input::with_theme(&theme)
      .with_prompt("Brain URL")
      .default("http://your-server:7878".to_string())
      .interact_text()
      .context("Connect cancelled")?,
  };
  url = url.trim_end_matches('/').to_string();

  let token = match token_arg {
    Some(token) => token,
    None => Input::with_theme(&theme)
      .with_prompt("Access token")
      .default("sk-dbr_...".to_string())
      .interact_text()
      .context("Connect cancelled")?,
  };

  println!("Connecting to brain...");
  let client = reqwest::Client::new();
  let connect_response = client
    .get(format!("{url}/connect"))
    .header("Authorization", format!("Bearer {token}"))
    .send()
    .await
    .with_context(|| format!("Could not reach {url}/connect"))?;

  if connect_response.status() == StatusCode::UNAUTHORIZED {
    return Err(anyhow!("Invalid token"));
  }

  if !connect_response.status().is_success() {
    return Err(anyhow!("Connection failed with HTTP {}", connect_response.status()));
  }

  let connect_payload: Value = connect_response.json().await.context("Invalid /connect response")?;
  println!("Brain found");

  let health_payload = if let Ok(response) = client.get(format!("{url}/health")).send().await {
    response.json::<Value>().await.ok()
  } else {
    None
  };

  if let Some(health) = health_payload {
    if let Some(name) = health.get("name").and_then(Value::as_str) {
      let entities = health.get("entities").and_then(Value::as_i64).unwrap_or(0);
      let facts = health.get("facts").and_then(Value::as_i64).unwrap_or(0);
      println!("Brain: {} — {} entities, {} facts", name, entities, facts);
    }
  }

  println!("Configuring Claude Code...");

  let home = dirs::home_dir().ok_or_else(|| anyhow!("Could not resolve home directory"))?;
  let claude_dir = home.join(".claude");
  let claude_json = home.join(".claude.json");
  let settings_json = claude_dir.join("settings.json");
  let claude_md = claude_dir.join("CLAUDE.md");

  fs::create_dir_all(&claude_dir).with_context(|| format!("Failed to create {}", claude_dir.display()))?;

  let mut claude_config = read_json_or_empty(&claude_json)?;
  ensure_object(&mut claude_config);
  ensure_path_object(&mut claude_config, &["mcpServers"]);
  if let Some(mcp) = connect_payload.get("mcp").and_then(Value::as_object) {
    let target = claude_config
      .get_mut("mcpServers")
      .and_then(Value::as_object_mut)
      .ok_or_else(|| anyhow!("Invalid ~/.claude.json format"))?;
    for (key, value) in mcp {
      target.insert(key.clone(), value.clone());
    }
  }
  write_json_pretty(&claude_json, &claude_config)?;

  let mut settings = read_json_or_empty(&settings_json)?;
  ensure_object(&mut settings);
  ensure_path_object(&mut settings, &["permissions"]);
  ensure_path_array(&mut settings, &["permissions", "allow"]);
  if let Some(perms) = connect_payload.get("permissions").and_then(Value::as_array) {
    let allow = settings
      .get_mut("permissions")
      .and_then(Value::as_object_mut)
      .and_then(|permissions| permissions.get_mut("allow"))
      .and_then(Value::as_array_mut)
      .ok_or_else(|| anyhow!("Invalid ~/.claude/settings.json format"))?;

    for perm in perms {
      if let Some(perm_str) = perm.as_str() {
        if !allow.iter().any(|value| value.as_str() == Some(perm_str)) {
          allow.push(Value::String(perm_str.to_string()));
        }
      }
    }
  }
  write_json_pretty(&settings_json, &settings)?;

  let claude_md_content = connect_payload
    .get("claudeMd")
    .and_then(Value::as_str)
    .unwrap_or_default();
  fs::write(&claude_md, claude_md_content)
    .with_context(|| format!("Failed to write {}", claude_md.display()))?;

  println!("Claude Code configured");
  println!("\nFiles updated");
  println!("{}          MCP server registered", claude_json.display());
  println!("{}  Permissions granted", settings_json.display());
  println!("{}      Behavioral instructions installed", claude_md.display());
  println!();
  println!("Connected. Restart Claude Code to activate.");

  Ok(())
}

fn status(path_arg: Option<&str>) -> Result<()> {
  let data_path = resolve_data_path(path_arg)?;
  if !data_path.join("config.json").exists() {
    println!("Not initialized. Run: dbrain init");
    return Ok(());
  }

  let config = load_config(&data_path)?;
  let db_path = std::path::PathBuf::from(&config.data_path).join("dbrain.db");
  let db_metadata = fs::metadata(&db_path).ok();
  let db_size_kb = db_metadata.map(|m| m.len() as f64 / 1024.0);

  println!("\ndbrain status\n");
  println!("  Data:     {}", config.data_path);
  println!("  Port:     {}", config.port);
  println!("  Host:     {}", config.host);
  println!(
    "  Database: {}",
    db_size_kb
      .map(|size| format!("{size:.1} KB"))
      .unwrap_or_else(|| "not created".to_string())
  );

  Ok(())
}

fn generate_token() -> String {
  let raw = gen_id("tok");
  let token = raw.trim_start_matches("tok_");
  format!("sk-dbr_{token}")
}

fn seed_database(
  db: &crate::db::Db,
  agent_name: &str,
  owner_name: &str,
  owner_timezone: &str,
) -> Result<()> {
  let now = now_iso();
  let today = today_date();

  let identity = [
    "# Identity".to_string(),
    "".to_string(),
    format!("- **Name:** {agent_name}"),
    format!("- **Created:** {today}"),
  ]
  .join("\n");

  let user = [
    "# User".to_string(),
    "".to_string(),
    format!("- **Name:** {owner_name}"),
    format!("- **Timezone:** {owner_timezone}"),
  ]
  .join("\n");

  let soul = [
    "# Soul".to_string(),
    "".to_string(),
    "Be genuinely helpful, not performatively helpful.".to_string(),
    "Have opinions. Be resourceful before asking.".to_string(),
    "Private things stay private. When in doubt, ask before acting externally.".to_string(),
  ]
  .join("\n");

  let memory = [
    "# Memory".to_string(),
    "".to_string(),
    "Narrative memory: reflections, decisions, learnings.".to_string(),
  ]
  .join("\n");

  let owner_id = slugify(owner_name);
  let agent_id = slugify(agent_name);

  let conn = db.get().map_err(|e| anyhow!("Failed to get DB connection: {e}"))?;
  conn.execute(
    "INSERT INTO documents (key, title, content, updated_at) VALUES (?, ?, ?, ?)",
    params!["identity", "Identity", identity, now],
  )?;
  conn.execute(
    "INSERT INTO documents (key, title, content, updated_at) VALUES (?, ?, ?, ?)",
    params!["user", "User", user, now],
  )?;
  conn.execute(
    "INSERT INTO documents (key, title, content, updated_at) VALUES (?, ?, ?, ?)",
    params!["soul", "Soul", soul, now],
  )?;
  conn.execute(
    "INSERT INTO documents (key, title, content, updated_at) VALUES (?, ?, ?, ?)",
    params!["memory", "Memory", memory, now],
  )?;
  conn.execute(
    "INSERT INTO entities (id, name, type, category, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
    params![owner_id, owner_name, "person", "areas", now, now],
  )?;
  conn.execute(
    "INSERT INTO entities (id, name, type, category, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
    params![agent_id, agent_name, "system", "areas", now, now],
  )?;

  Ok(())
}

/// Slugify a string: lowercase, strip non-alphanumeric (except spaces/hyphens), join with hyphens.
fn slugify(value: &str) -> String {
  value
    .to_lowercase()
    .chars()
    .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { ' ' })
    .collect::<String>()
    .split_whitespace()
    .collect::<Vec<_>>()
    .join("-")
}

fn read_json_or_empty(path: &Path) -> Result<Value> {
  if !path.exists() {
    return Ok(json!({}));
  }

  let raw = fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
  match serde_json::from_str::<Value>(&raw) {
    Ok(value) => Ok(value),
    Err(e) => {
      warn!("Failed to parse {} as JSON ({}), treating as empty object", path.display(), e);
      Ok(json!({}))
    }
  }
}

fn write_json_pretty(path: &Path, value: &Value) -> Result<()> {
  let raw = serde_json::to_string_pretty(value)? + "\n";
  fs::write(path, raw).with_context(|| format!("Failed to write {}", path.display()))
}

fn ensure_object(value: &mut Value) {
  if !value.is_object() {
    *value = json!({});
  }
}

fn ensure_path_object(root: &mut Value, path: &[&str]) {
  let mut current = root;
  for key in path {
    if !current.is_object() {
      *current = json!({});
    }
    let map = current.as_object_mut().expect("object checked");
    if !map.contains_key(*key) || !map.get(*key).is_some_and(Value::is_object) {
      map.insert((*key).to_string(), json!({}));
    }
    current = map.get_mut(*key).expect("inserted key exists");
  }
}

fn ensure_path_array(root: &mut Value, path: &[&str]) {
  if path.is_empty() {
    return;
  }

  let parents = &path[..path.len() - 1];
  ensure_path_object(root, parents);

  let mut current = root;
  for key in parents {
    current = current
      .as_object_mut()
      .expect("parent object ensured")
      .get_mut(*key)
      .expect("parent key exists");
  }

  if let Some(last) = path.last() {
    let map = current.as_object_mut().expect("object ensured");
    if !map.contains_key(*last) || !map.get(*last).is_some_and(Value::is_array) {
      map.insert((*last).to_string(), json!([]));
    }
  }
}
