use std::{
    collections::HashSet,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, RwLock},
    time::Duration,
};

use base64::Engine;
use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use chrono::Utc;
use libc::umask;
use regex::Regex;
use reqwest::{header, Client};
use rusqlite::{named_params, params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tera::{Context, Tera};
use tokio::{net::TcpListener, task::JoinHandle, time::sleep};
use tower_http::services::ServeDir;
use tracing::{info, warn};

const CONFIG_PATH_DEFAULT: &str = "/data/config.json";
const DOWNLOADS_DIR: &str = "/downloads";
const LIBRARY_DIR: &str = "/library";
const EBOOKS_DIR: &str = "/ebooks";
const DEFAULT_AUTO_IMPORT_POLL_INTERVAL: u64 = 30;
const MEDIA_TYPE_AUDIOBOOK: &str = "audiobook";
const MEDIA_TYPE_EBOOK: &str = "ebook";
const HISTORY_DB_PATH: &str = "/data/history.db";

const MAM_MAIN_CATEGORIES_AUDIOBOOK: &str = "13";
const MAM_MAIN_CATEGORIES_EBOOK: &str = "14";

#[derive(Clone)]
struct Settings {
    mam_base: String,
    mam_cookie: String,
    transmission_url: String,
    transmission_user: String,
    transmission_pass: String,
    transmission_label: String,
    auto_import_enabled: bool,
    auto_import_poll_interval: u64,
    umask: Option<String>,
}

impl Settings {
    fn load() -> Self {
        let cfg = load_json_config();

        let mam_base = cfg
            .get("MAM_BASE")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| env_or_default("MAM_BASE", "https://www.myanonamouse.net"));

        let raw_cookie = cfg
            .get("MAM_COOKIE")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| env_or_default("MAM_COOKIE", ""));

        let transmission_url = cfg
            .get("TRANSMISSION_URL")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| env_or_default("TRANSMISSION_URL", "http://transmission:9091/transmission/rpc"))
            .trim_end_matches('/')
            .to_owned();

        let transmission_user = cfg
            .get("TRANSMISSION_USER")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| env_or_default("TRANSMISSION_USER", ""));
        let transmission_pass = cfg
            .get("TRANSMISSION_PASS")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| env_or_default("TRANSMISSION_PASS", ""));
        let transmission_label = cfg
            .get("TRANSMISSION_LABEL")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| env_or_default("TRANSMISSION_LABEL", "mam-audiofinder"));

        let auto_import_enabled = if let Some(v) = cfg.get("AUTO_IMPORT_ENABLED") {
            is_truthy_json(v)
        } else {
            is_truthy_str(&env_or_default("AUTO_IMPORT_ENABLED", ""))
        };

        let auto_import_poll_interval = parse_positive_int(
            &env_or_default("AUTO_IMPORT_POLL_INTERVAL", &DEFAULT_AUTO_IMPORT_POLL_INTERVAL.to_string()),
            DEFAULT_AUTO_IMPORT_POLL_INTERVAL,
        );

        let umask = cfg
            .get("UMASK")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                let v = env_or_default("UMASK", "");
                if v.trim().is_empty() {
                    None
                } else {
                    Some(v)
                }
            });

        Self {
            mam_base,
            mam_cookie: build_mam_cookie(&raw_cookie),
            transmission_url,
            transmission_user,
            transmission_pass,
            transmission_label,
            auto_import_enabled,
            auto_import_poll_interval,
            umask,
        }
    }

    fn apply_umask(&self) {
        if let Some(raw) = &self.umask {
            if let Ok(mask) = u32::from_str_radix(raw.trim(), 8) {
                unsafe {
                    umask(mask as _);
                }
            }
        }
    }

    fn setup_context(&self, app_version: &str) -> Context {
        let mut ctx = Context::new();
        ctx.insert("app_version", app_version);
        ctx.insert("transmission_url", &self.transmission_url);
        ctx.insert("transmission_user", &self.transmission_user);
        ctx.insert("transmission_label", &self.transmission_label);
        ctx.insert("auto_import_enabled", &self.auto_import_enabled);
        ctx
    }
}

struct AppState {
    app_version: String,
    config_path: String,
    settings: RwLock<Settings>,
    templates: Tera,
    auto_import_task: Mutex<Option<JoinHandle<()>>>,
}

impl AppState {
    fn load() -> anyhow::Result<Arc<Self>> {
        let app_version = env_or_default("APP_VERSION", "unknown");
        let config_path = env_or_default("APP_CONFIG_PATH", CONFIG_PATH_DEFAULT);
        let settings = Settings::load();
        settings.apply_umask();

        let templates = Tera::new("app/templates/**/*")?;
        init_db()?;

        Ok(Arc::new(Self {
            app_version,
            config_path,
            settings: RwLock::new(settings),
            templates,
            auto_import_task: Mutex::new(None),
        }))
    }

    fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    fn reload_settings(&self) {
        let settings = Settings::load();
        settings.apply_umask();
        *self.settings.write().unwrap() = settings;
    }
}

#[derive(Debug)]
struct AppError {
    status: StatusCode,
    detail: String,
}

impl AppError {
    fn new(status: StatusCode, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
        }
    }

    fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, detail)
    }

    fn not_found(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, detail)
    }

    fn bad_gateway(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_GATEWAY, detail)
    }

    fn internal(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, detail)
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "detail": self.detail }))).into_response()
    }
}

type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Clone, Serialize)]
struct SearchResult {
    id: String,
    title: Option<String>,
    author_info: String,
    narrator_info: String,
    format: String,
    size: Option<Value>,
    seeders: Option<Value>,
    leechers: Option<Value>,
    catname: Option<Value>,
    added: Option<Value>,
    dl: Option<Value>,
    media_type: String,
    is_freeleech: bool,
    is_vip: bool,
}

#[derive(Debug, Clone, Serialize)]
struct HistoryItem {
    id: i64,
    mam_id: Option<String>,
    title: Option<String>,
    author: Option<String>,
    narrator: Option<String>,
    media_type: Option<String>,
    dl: Option<String>,
    torrent_hash: Option<String>,
    added_at: Option<String>,
    imported_at: Option<String>,
    torrent_status: Option<String>,
    status_detail: Option<String>,
    status_updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CompletedTorrent {
    hash: String,
    name: Option<String>,
    download_dir: Option<String>,
    root: String,
    single_file: bool,
    size: Option<Value>,
    added_on: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct SetupPayload {
    mam_cookie: Option<String>,
    transmission_url: Option<String>,
    transmission_user: Option<String>,
    transmission_pass: Option<String>,
    transmission_label: Option<String>,
    auto_import_enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct AddBody {
    id: Option<Value>,
    title: Option<String>,
    dl: Option<String>,
    author: Option<String>,
    narrator: Option<String>,
    media_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImportBody {
    author: String,
    title: String,
    hash: String,
    history_id: Option<i64>,
    media_type: Option<String>,
}

fn env_or_default(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn is_truthy_json(value: &Value) -> bool {
    match value {
        Value::Bool(v) => *v,
        Value::Number(n) => n.as_i64().map(|v| v != 0).unwrap_or(false),
        Value::String(s) => is_truthy_str(s),
        _ => false,
    }
}

fn is_truthy_str(value: &str) -> bool {
    matches!(
        value.trim().to_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn parse_positive_int(value: &str, default: u64) -> u64 {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

fn build_mam_cookie(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    if raw.contains("mam_id=") || raw.contains("mam_session=") {
        return raw.to_owned();
    }
    if !raw.contains('=') && !raw.contains(';') {
        return format!("mam_id={raw}");
    }
    raw.to_owned()
}

fn normalize_media_type(value: Option<&str>) -> AppResult<String> {
    let media_type = value.unwrap_or(MEDIA_TYPE_AUDIOBOOK).trim().to_lowercase();
    match media_type.as_str() {
        "audiobook" | "audiobooks" | "audio" => Ok(MEDIA_TYPE_AUDIOBOOK.to_owned()),
        "ebook" | "ebooks" | "e-book" | "e-books" => Ok(MEDIA_TYPE_EBOOK.to_owned()),
        _ => Err(AppError::bad_request("media_type must be audiobook or ebook")),
    }
}

fn load_json_config() -> Map<String, Value> {
    let path = env_or_default("APP_CONFIG_PATH", CONFIG_PATH_DEFAULT);
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default(),
        Err(_) => Map::new(),
    }
}

fn write_json_config(path: &str, cfg: &Map<String, Value>) -> AppResult<()> {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent)
            .map_err(|e| AppError::internal(format!("Failed to create config directory: {e}")))?;
    }

    let text = serde_json::to_string_pretty(cfg)
        .map_err(|e| AppError::internal(format!("Failed to serialize config: {e}")))?;
    fs::write(path, text).map_err(|e| AppError::internal(format!("Failed to write config: {e}")))
}

fn init_db() -> anyhow::Result<()> {
    if let Some(parent) = Path::new(HISTORY_DB_PATH).parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(HISTORY_DB_PATH)?;
    ensure_history_schema(&conn)?;
    Ok(())
}

fn db_conn() -> AppResult<Connection> {
    if let Some(parent) = Path::new(HISTORY_DB_PATH).parent() {
        fs::create_dir_all(parent)
            .map_err(|e| AppError::internal(format!("Failed to create database directory: {e}")))?;
    }
    let conn = Connection::open(HISTORY_DB_PATH)
        .map_err(|e| AppError::internal(format!("Failed to open database: {e}")))?;
    ensure_history_schema(&conn)
        .map_err(|e| AppError::internal(format!("Failed to prepare database schema: {e}")))?;
    Ok(conn)
}

fn ensure_history_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS history (
          id INTEGER PRIMARY KEY,
          mam_id   TEXT,
          title    TEXT,
          author   TEXT,
          narrator TEXT,
          media_type TEXT,
          dl       TEXT,
          added_at TEXT DEFAULT (datetime('now')),
          imported_at TEXT,
          torrent_status TEXT,
          torrent_hash   TEXT
        );
        "#,
    )?;

    let mut cols = HashSet::new();
    let mut stmt = conn.prepare("PRAGMA table_info(history)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for col in rows {
        cols.insert(col?);
    }

    if !cols.contains("status_detail") {
        conn.execute("ALTER TABLE history ADD COLUMN status_detail TEXT", [])?;
    }
    if !cols.contains("status_updated_at") {
        conn.execute("ALTER TABLE history ADD COLUMN status_updated_at TEXT", [])?;
    }
    if !cols.contains("media_type") {
        conn.execute("ALTER TABLE history ADD COLUMN media_type TEXT", [])?;
    }
    conn.execute(
        "UPDATE history SET media_type = 'audiobook' WHERE media_type IS NULL OR trim(media_type) = ''",
        [],
    )?;
    conn.execute(
        "UPDATE history SET torrent_status = 'added' WHERE torrent_status IS NULL OR trim(torrent_status) = ''",
        [],
    )?;
    conn.execute(
        "UPDATE history SET status_updated_at = COALESCE(status_updated_at, imported_at, added_at) WHERE status_updated_at IS NULL",
        [],
    )?;

    Ok(())
}

fn utcnow_str() -> String {
    Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn setup_enabled() -> bool {
    !is_truthy_str(&env_or_default("DISABLE_SETUP", ""))
}

fn needs_setup(state: &AppState) -> bool {
    state.settings.read().unwrap().mam_cookie.is_empty()
}

fn render_template(state: &AppState, name: &str, context: Context) -> AppResult<String> {
    state
        .templates
        .render(name, &context)
        .map_err(|e| AppError::internal(format!("Template render failed: {e}")))
}

fn setup_context(state: &AppState) -> Context {
    state.settings.read().unwrap().setup_context(&state.app_version)
}

fn sanitize_name(name: &str) -> String {
    static WHITESPACE: OnceLock<Regex> = OnceLock::new();
    let whitespace = WHITESPACE.get_or_init(|| Regex::new(r"\s+").unwrap());
    let s = name.trim().replace(':', " -").replace('\\', "﹨").replace('/', "﹨");
    let s = whitespace.replace_all(&s, " ").to_string();
    let s = s.chars().take(200).collect::<String>();
    if s.trim().is_empty() {
        "Unknown".to_owned()
    } else {
        s
    }
}

fn clean_status_detail(detail: Option<&str>) -> Option<String> {
    static WHITESPACE: OnceLock<Regex> = OnceLock::new();
    let whitespace = WHITESPACE.get_or_init(|| Regex::new(r"\s+").unwrap());
    let text = whitespace
        .replace_all(detail.unwrap_or("").trim(), " ")
        .to_string();
    let text = text.chars().take(500).collect::<String>();
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

fn format_regex() -> &'static Regex {
    static FORMAT_RE: OnceLock<Regex> = OnceLock::new();
    FORMAT_RE.get_or_init(|| {
        Regex::new(r"(?i)\b(mp3|m4b|flac|aac|ogg|opus|wav|alac|ape|epub|pdf|mobi|azw3|cbz|cbr)\b")
            .unwrap()
    })
}

fn flatten_value(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                    return flatten_value(Some(&v));
                }
            }
            let stripped = trimmed.trim_matches(|c| c == '{' || c == '}');
            let parts = stripped
                .split(',')
                .map(|chunk| chunk.split_once(':').map(|(_, right)| right).unwrap_or(chunk))
                .map(|chunk| chunk.trim().trim_matches('"').trim_matches('\'').to_owned())
                .filter(|chunk| !chunk.is_empty())
                .collect::<Vec<_>>();
            if parts.is_empty() {
                trimmed.to_owned()
            } else {
                parts.join(", ")
            }
        }
        Some(Value::Array(items)) => items
            .iter()
            .map(value_to_string)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        Some(Value::Object(map)) => map
            .values()
            .map(value_to_string)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        Some(other) => value_to_string(other),
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => flatten_value(Some(v)),
    }
}

fn detect_format(item: &Value) -> String {
    for key in ["format", "filetype", "container", "encoding", "format_name"] {
        if let Some(value) = item.get(key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_owned();
            }
        }
    }
    let name = item
        .get("title")
        .and_then(Value::as_str)
        .or_else(|| item.get("name").and_then(Value::as_str))
        .unwrap_or("");

    let toks = format_regex()
        .captures_iter(name)
        .filter_map(|caps| caps.get(1).map(|m| m.as_str().to_uppercase()))
        .collect::<Vec<_>>();

    let mut uniq = Vec::<String>::new();
    for tok in toks {
        if !uniq.contains(&tok) {
            uniq.push(tok);
        }
    }
    if uniq.is_empty() {
        String::new()
    } else {
        uniq.join("/")
    }
}

fn truthy_value(v: Option<&Value>) -> bool {
    v.map(is_truthy_json).unwrap_or(false)
}

fn json_number_or_string(v: Option<&Value>) -> Option<Value> {
    v.cloned()
}

fn extract_string_id(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => value_to_string(other).trim().to_owned(),
        None => String::new(),
    }
}

fn media_main_category(media_type: &str) -> &'static str {
    if media_type == MEDIA_TYPE_EBOOK {
        MAM_MAIN_CATEGORIES_EBOOK
    } else {
        MAM_MAIN_CATEGORIES_AUDIOBOOK
    }
}

fn transmission_auth(settings: &Settings) -> Option<(String, String)> {
    if settings.transmission_user.is_empty() && settings.transmission_pass.is_empty() {
        None
    } else {
        Some((
            settings.transmission_user.clone(),
            settings.transmission_pass.clone(),
        ))
    }
}

async fn transmission_rpc(
    client: &Client,
    settings: &Settings,
    method: &str,
    arguments: Option<Value>,
) -> AppResult<Value> {
    let payload = json!({
        "method": method,
        "arguments": arguments.unwrap_or_else(|| json!({}))
    });

    let mut request = client.post(&settings.transmission_url).json(&payload);
    if let Some((user, pass)) = transmission_auth(settings) {
        request = request.basic_auth(user, Some(pass));
    }

    let response = request
        .send()
        .await
        .map_err(|e| AppError::bad_gateway(format!("Transmission RPC failed: {e}")))?;

    let status = response.status();
    let response = if status == StatusCode::CONFLICT {
        if let Some(session_id) = response.headers().get("X-Transmission-Session-Id") {
            let mut request = client
                .post(&settings.transmission_url)
                .json(&payload)
                .header("X-Transmission-Session-Id", session_id);
            if let Some((user, pass)) = transmission_auth(settings) {
                request = request.basic_auth(user, Some(pass));
            }
            request
                .send()
                .await
                .map_err(|e| AppError::bad_gateway(format!("Transmission RPC failed: {e}")))?
        } else {
            response
        }
    } else {
        response
    };

    let status = response.status();
    if status != StatusCode::OK {
        let text = response.text().await.unwrap_or_default();
        return Err(AppError::bad_gateway(format!(
            "Transmission RPC failed: {} {}",
            status,
            text.chars().take(160).collect::<String>()
        )));
    }

    let data: Value = response
        .json()
        .await
        .map_err(|e| AppError::bad_gateway(format!("Transmission returned non-JSON: {e}")))?;

    if data.get("result").and_then(Value::as_str) != Some("success") {
        return Err(AppError::bad_gateway(format!(
            "Transmission {method} failed: {}",
            data.get("result")
                .map(value_to_string)
                .unwrap_or_else(|| "unknown".to_owned())
        )));
    }

    Ok(data
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({})))
}

fn transmission_labels(settings: &Settings, mam_id: &str) -> Vec<String> {
    let mut labels = Vec::new();
    if !settings.transmission_label.is_empty() {
        labels.push(settings.transmission_label.clone());
    }
    if !mam_id.is_empty() {
        labels.push(format!("mamid={mam_id}"));
    }
    labels
}

fn torrent_add_arguments(settings: &Settings, mam_id: &str, source_key: &str, source_value: &str) -> Value {
    let mut args = Map::new();
    args.insert(source_key.to_owned(), Value::String(source_value.to_owned()));
    let labels = transmission_labels(settings, mam_id);
    if !labels.is_empty() {
        args.insert(
            "labels".to_owned(),
            Value::Array(labels.into_iter().map(Value::String).collect()),
        );
    }
    Value::Object(args)
}

fn torrent_hash_from_add_result(args: &Value) -> Option<String> {
    args.get("torrent-added")
        .or_else(|| args.get("torrent-duplicate"))
        .and_then(|torrent| torrent.get("hashString"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn insert_history(
    _state: &AppState,
    mam_id: &str,
    title: &str,
    author: &str,
    narrator: &str,
    media_type: &str,
    dl: &str,
    torrent_hash: Option<String>,
) -> AppResult<()> {
    let added_at = utcnow_str();
    let media_type = normalize_media_type(Some(media_type))?;
    let conn = db_conn()?;
    conn.execute(
        r#"
        INSERT INTO history (
            mam_id,
            title,
            author,
            narrator,
            media_type,
            dl,
            torrent_status,
            torrent_hash,
            added_at,
            status_detail,
            status_updated_at
        )
        VALUES (
            :mam_id,
            :title,
            :author,
            :narrator,
            :media_type,
            :dl,
            :torrent_status,
            :torrent_hash,
            :added_at,
            :status_detail,
            :status_updated_at
        )
        "#,
        named_params! {
            ":mam_id": mam_id,
            ":title": title,
            ":author": author,
            ":narrator": narrator,
            ":media_type": media_type,
            ":dl": dl,
            ":torrent_status": "added",
            ":torrent_hash": torrent_hash,
            ":added_at": added_at,
            ":status_detail": Option::<String>::None,
            ":status_updated_at": utcnow_str(),
        },
    )
    .map_err(|e| AppError::internal(format!("Failed to insert history: {e}")))?;
    Ok(())
}

fn update_history_status(
    _state: &AppState,
    history_id: i64,
    status: &str,
    detail: Option<&str>,
    imported_at: Option<&str>,
) -> AppResult<()> {
    let ts = utcnow_str();
    let conn = db_conn()?;
    conn.execute(
        r#"
        UPDATE history
        SET
            torrent_status = :status,
            status_detail = :detail,
            status_updated_at = :status_updated_at,
            imported_at = COALESCE(:imported_at, imported_at)
        WHERE id = :id
        "#,
        named_params! {
            ":id": history_id,
            ":status": status,
            ":detail": clean_status_detail(detail),
            ":status_updated_at": ts,
            ":imported_at": imported_at,
        },
    )
    .map_err(|e| AppError::internal(format!("Failed to update history: {e}")))?;
    Ok(())
}

fn mark_history_imported(_state: &AppState, history_id: Option<i64>, torrent_hash: &str) -> AppResult<()> {
    let ts = utcnow_str();
    let conn = db_conn()?;
    if let Some(history_id) = history_id {
        conn.execute(
            r#"
            UPDATE history
            SET
                torrent_status = 'imported',
                status_detail = NULL,
                status_updated_at = :ts,
                imported_at = :ts
            WHERE id = :id
            "#,
            named_params! { ":ts": ts, ":id": history_id },
        )
        .map_err(|e| AppError::internal(format!("Failed to mark history imported: {e}")))?;
    } else {
        conn.execute(
            r#"
            UPDATE history
            SET
                torrent_status = 'imported',
                status_detail = NULL,
                status_updated_at = :ts,
                imported_at = :ts
            WHERE torrent_hash = :torrent_hash
            "#,
            named_params! { ":ts": ts, ":torrent_hash": torrent_hash },
        )
        .map_err(|e| AppError::internal(format!("Failed to mark history imported: {e}")))?;
    }
    Ok(())
}

fn mark_history_failed(
    _state: &AppState,
    history_id: Option<i64>,
    torrent_hash: &str,
    detail: &str,
) -> AppResult<()> {
    let ts = utcnow_str();
    let conn = db_conn()?;
    if let Some(history_id) = history_id {
        conn.execute(
            r#"
            UPDATE history
            SET
                torrent_status = 'import_failed',
                status_detail = :detail,
                status_updated_at = :ts
            WHERE id = :id
            "#,
            named_params! { ":ts": ts, ":id": history_id, ":detail": clean_status_detail(Some(detail)) },
        )
        .map_err(|e| AppError::internal(format!("Failed to mark history failed: {e}")))?;
    } else {
        conn.execute(
            r#"
            UPDATE history
            SET
                torrent_status = 'import_failed',
                status_detail = :detail,
                status_updated_at = :ts
            WHERE torrent_hash = :torrent_hash
            "#,
            named_params! { ":ts": ts, ":torrent_hash": torrent_hash, ":detail": clean_status_detail(Some(detail)) },
        )
        .map_err(|e| AppError::internal(format!("Failed to mark history failed: {e}")))?;
    }
    Ok(())
}

fn get_history_media_type(_state: &AppState, history_id: Option<i64>) -> AppResult<Option<String>> {
    let Some(history_id) = history_id else {
        return Ok(None);
    };
    let conn = db_conn()?;
    let media_type: Option<String> = conn
        .query_row(
            "SELECT media_type FROM history WHERE id = ?1",
            params![history_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| AppError::internal(format!("Failed to load history media type: {e}")))?;
    match media_type {
        Some(value) => Ok(Some(normalize_media_type(Some(&value))?)),
        None => Ok(None),
    }
}

fn get_auto_import_candidates(_state: &AppState, completed_hashes: &HashSet<String>) -> AppResult<Vec<HistoryItem>> {
    if completed_hashes.is_empty() {
        return Ok(Vec::new());
    }
    let conn = db_conn()?;
    let mut stmt = conn
        .prepare(
            r#"
            SELECT
                id,
                mam_id,
                title,
                author,
                narrator,
                media_type,
                dl,
                torrent_hash,
                added_at,
                imported_at,
                torrent_status,
                status_detail,
                status_updated_at
            FROM history
            WHERE
                torrent_hash IS NOT NULL
                AND trim(torrent_hash) != ''
                AND (
                    torrent_status IS NULL
                    OR torrent_status NOT IN ('imported', 'import_failed', 'importing')
                )
            ORDER BY id ASC
            "#,
        )
        .map_err(|e| AppError::internal(format!("Failed to query history: {e}")))?;

    let rows = stmt
        .query_map([], |row| {
            Ok(HistoryItem {
                id: row.get(0)?,
                mam_id: row.get(1)?,
                title: row.get(2)?,
                author: row.get(3)?,
                narrator: row.get(4)?,
                media_type: row.get(5)?,
                dl: row.get(6)?,
                torrent_hash: row.get(7)?,
                added_at: row.get(8)?,
                imported_at: row.get(9)?,
                torrent_status: row.get(10)?,
                status_detail: row.get(11)?,
                status_updated_at: row.get(12)?,
            })
        })
        .map_err(|e| AppError::internal(format!("Failed to query history: {e}")))?;

    let mut out = Vec::new();
    let mut seen_hashes = HashSet::new();
    for row in rows {
        let row = row.map_err(|e| AppError::internal(format!("Failed to read history row: {e}")))?;
        let torrent_hash = row
            .torrent_hash
            .as_ref()
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        if torrent_hash.is_empty() || !completed_hashes.contains(&torrent_hash) || seen_hashes.contains(&torrent_hash)
        {
            continue;
        }
        seen_hashes.insert(torrent_hash);
        out.push(row);
    }
    Ok(out)
}

fn validate_download_path(p: &str) -> AppResult<String> {
    let p = p.trim();
    if p.is_empty() {
        return Ok(String::new());
    }
    let expected = DOWNLOADS_DIR.trim_end_matches('/');
    let valid = p == expected || p.starts_with(&format!("{expected}/"));
    if valid {
        Ok(p.to_owned())
    } else {
        Err(AppError::bad_request(format!(
            "Transmission reports downloadDir '{p}', but this app expects completed downloads under {DOWNLOADS_DIR}. Mount the same downloads directory at {DOWNLOADS_DIR} in both containers."
        )))
    }
}

fn is_transient_auto_import_error(err: &AppError) -> bool {
    err.status == StatusCode::BAD_GATEWAY && err.detail.starts_with("Transmission")
}

fn copy_one(src: &Path, dst: &Path) -> AppResult<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| AppError::internal(format!("Failed to create destination directory: {e}")))?;
    }
    fs::copy(src, dst).map_err(|e| AppError::internal(format!("Failed to copy file {src:?} -> {dst:?}: {e}")))?;
    Ok(())
}

fn next_available(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let mut i = 2;
    loop {
        let candidate = path.with_file_name(format!(
            "{} ({i})",
            path.file_name().and_then(|v| v.to_str()).unwrap_or("Unknown")
        ));
        if !candidate.exists() {
            return candidate;
        }
        i += 1;
    }
}

async fn list_completed_torrents(state: Arc<AppState>) -> AppResult<Vec<CompletedTorrent>> {
    let settings = state.settings();
    let client = reqwest::Client::new();
    let args = transmission_rpc(
        &client,
        &settings,
        "torrent-get",
        Some(json!({
            "fields": [
                "id",
                "hashString",
                "name",
                "percentDone",
                "downloadDir",
                "totalSize",
                "addedDate",
                "labels",
                "files"
            ]
        })),
    )
    .await?;

    let infos = args
        .get("torrents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut out = Vec::new();
    for t in infos {
        let labels = t.get("labels").and_then(Value::as_array).cloned().unwrap_or_default();
        if !settings.transmission_label.is_empty()
            && !labels.iter().any(|label| label.as_str() == Some(&settings.transmission_label))
        {
            continue;
        }
        if t.get("percentDone").and_then(Value::as_f64).unwrap_or(0.0) < 1.0 {
            continue;
        }
        let Some(hash) = t.get("hashString").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };

        let files = t.get("files").and_then(Value::as_array).cloned().unwrap_or_default();
        let mut roots = HashSet::new();
        for file in &files {
            if let Some(name) = file.get("name").and_then(Value::as_str) {
                let name = name.trim_start_matches('/');
                if let Some((root, _)) = name.split_once('/') {
                    roots.insert(root.to_owned());
                }
            }
        }
        let root = roots.iter().next().cloned().unwrap_or_else(|| {
            t.get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        });
        let single_file = files.len() == 1
            && files
                .get(0)
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str)
                .map(|name| !name.contains('/'))
                .unwrap_or(false);

        out.push(CompletedTorrent {
            hash,
            name: t.get("name").and_then(Value::as_str).map(str::to_owned),
            download_dir: t
                .get("downloadDir")
                .and_then(Value::as_str)
                .map(str::to_owned),
            root,
            single_file,
            size: json_number_or_string(t.get("totalSize")),
            added_on: json_number_or_string(t.get("addedDate")),
        });
    }

    Ok(out)
}

async fn import_torrent_to_library(
    state: Arc<AppState>,
    author: &str,
    title: &str,
    hash: &str,
    media_type: &str,
) -> AppResult<String> {
    let media_type = normalize_media_type(Some(media_type))?;
    let author = sanitize_name(author);
    let title = sanitize_name(title);
    let settings = state.settings();
    let client = Client::new();
    let args = transmission_rpc(
        &client,
        &settings,
        "torrent-get",
        Some(json!({
            "ids": [hash],
            "fields": ["id", "hashString", "name", "downloadDir", "labels", "files"]
        })),
    )
    .await?;

    let torrents = args
        .get("torrents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let Some(info) = torrents.first() else {
        return Err(AppError::not_found("No files found for torrent"));
    };
    let files = info.get("files").and_then(Value::as_array).cloned().unwrap_or_default();
    if files.is_empty() {
        return Err(AppError::not_found("No files found for torrent"));
    }
    let download_dir = info
        .get("downloadDir")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default();
    if download_dir.trim().is_empty() {
        return Err(AppError::not_found("Torrent download directory not found"));
    }

    let source_dir = PathBuf::from(validate_download_path(&download_dir)?);
    let lib_root = if media_type == MEDIA_TYPE_EBOOK {
        PathBuf::from(EBOOKS_DIR)
    } else {
        PathBuf::from(LIBRARY_DIR)
    };
    let author_dir = lib_root.join(&author);
    fs::create_dir_all(&author_dir)
        .map_err(|e| AppError::internal(format!("Failed to create library directory: {e}")))?;
    let dest_dir = next_available(&author_dir.join(&title));
    let names = files
        .iter()
        .filter_map(|f| f.get("name").and_then(Value::as_str))
        .map(|s| s.trim_start_matches('/').to_owned())
        .collect::<Vec<_>>();

    let roots = names
        .iter()
        .filter_map(|name| name.split_once('/').map(|(root, _)| root.to_owned()))
        .collect::<HashSet<_>>();
    let common_root = if roots.len() == 1 {
        roots.iter().next().cloned().unwrap_or_default()
    } else {
        String::new()
    };

    let mut copied = 0usize;
    if names.len() == 1 {
        let src = source_dir.join(&names[0]);
        if src.extension().and_then(|v| v.to_str()).map(|v| v.eq_ignore_ascii_case("cue")).unwrap_or(false) {
            return Err(AppError::bad_request("Only .cue file found; nothing to import"));
        }
        copy_one(&src, &dest_dir.join(src.file_name().unwrap_or_default()))?;
        copied += 1;
    } else {
        for name in names {
            let src = source_dir.join(&name);
            if src
                .extension()
                .and_then(|v| v.to_str())
                .map(|v| v.eq_ignore_ascii_case("cue"))
                .unwrap_or(false)
            {
                continue;
            }
            let rel_name = if !common_root.is_empty() && name.starts_with(&(common_root.clone() + "/")) {
                name[common_root.len() + 1..].to_owned()
            } else {
                name.clone()
            };
            if rel_name.is_empty() {
                continue;
            }
            copy_one(&src, &dest_dir.join(rel_name))?;
            copied += 1;
        }
    }

    if copied == 0 {
        return Err(AppError::bad_request("No importable files found"));
    }

    Ok(dest_dir.to_string_lossy().to_string())
}

fn history_rows(_state: &AppState) -> AppResult<Vec<HistoryItem>> {
    let conn = db_conn()?;
    let mut stmt = conn
        .prepare(
            r#"
            SELECT
                id,
                mam_id,
                title,
                author,
                narrator,
                media_type,
                dl,
                torrent_hash,
                added_at,
                imported_at,
                torrent_status,
                status_detail,
                status_updated_at
            FROM history
            ORDER BY id DESC
            LIMIT 200
            "#,
        )
        .map_err(|e| AppError::internal(format!("Failed to prepare history query: {e}")))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(HistoryItem {
                id: row.get(0)?,
                mam_id: row.get(1)?,
                title: row.get(2)?,
                author: row.get(3)?,
                narrator: row.get(4)?,
                media_type: row.get(5)?,
                dl: row.get(6)?,
                torrent_hash: row.get(7)?,
                added_at: row.get(8)?,
                imported_at: row.get(9)?,
                torrent_status: row.get(10)?,
                status_detail: row.get(11)?,
                status_updated_at: row.get(12)?,
            })
        })
        .map_err(|e| AppError::internal(format!("Failed to query history: {e}")))?;

    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| AppError::internal(format!("Failed to read history row: {e}")))?);
    }
    Ok(out)
}

async fn auto_import_cycle(state: Arc<AppState>) -> AppResult<()> {
    let completed = list_completed_torrents(state.clone()).await?;
    let completed_hashes = completed
        .into_iter()
        .map(|item| item.hash)
        .collect::<HashSet<_>>();
    let candidates = get_auto_import_candidates(&state, &completed_hashes)?;

    for row in candidates {
        let history_id = row.id;
        let torrent_hash = row.torrent_hash.clone().unwrap_or_default();
        let author = row.author.clone().unwrap_or_default();
        let title = row.title.clone().unwrap_or_default();

        let media_type = match normalize_media_type(row.media_type.as_deref()) {
            Ok(value) => value,
            Err(err) => {
                let _ = mark_history_failed(&state, Some(history_id), &torrent_hash, &err.detail);
                continue;
            }
        };

        if author.trim().is_empty() || title.trim().is_empty() {
            let _ = mark_history_failed(
                &state,
                Some(history_id),
                &torrent_hash,
                "History row is missing author/title; use manual import.",
            );
            continue;
        }

        let _ = update_history_status(&state, history_id, "importing", None, None);

        match import_torrent_to_library(state.clone(), &author, &title, &torrent_hash, &media_type).await {
            Ok(_) => {
                let _ = mark_history_imported(&state, Some(history_id), &torrent_hash);
                info!("Auto-imported history row {history_id}");
            }
            Err(err) if is_transient_auto_import_error(&err) => {
                let _ = update_history_status(&state, history_id, "added", None, None);
                warn!("Auto-import skipped for history row {history_id}: {}", err.detail);
            }
            Err(err) => {
                let _ = mark_history_failed(&state, Some(history_id), &torrent_hash, &err.detail);
                warn!("Auto-import failed for history row {history_id}: {}", err.detail);
            }
        }
    }

    Ok(())
}

async fn auto_import_loop(state: Arc<AppState>) {
    let interval = {
        state.settings.read().unwrap().auto_import_poll_interval
    };
    info!("Auto-import poller started with {interval}s interval");

    loop {
        if !state.settings.read().unwrap().auto_import_enabled {
            break;
        }

        if let Err(err) = auto_import_cycle(state.clone()).await {
            warn!("Auto-import cycle skipped: {}", err.detail);
        }

        let sleep_for = state.settings.read().unwrap().auto_import_poll_interval;
        sleep(Duration::from_secs(sleep_for)).await;
    }

    info!("Auto-import poller stopped");
}

async fn stop_auto_import_task(state: Arc<AppState>) {
    let handle = {
        let mut guard = state.auto_import_task.lock().unwrap();
        guard.take()
    };
    if let Some(handle) = handle {
        handle.abort();
        let _ = handle.await;
    }
}

async fn reconcile_auto_import_task(state: Arc<AppState>) {
    let enabled = state.settings.read().unwrap().auto_import_enabled;
    let mut guard = state.auto_import_task.lock().unwrap();

    if enabled {
        let should_spawn = guard.as_ref().map(|handle| handle.is_finished()).unwrap_or(true);
        if should_spawn {
            if let Some(handle) = guard.take() {
                handle.abort();
            }
            let state_clone = state.clone();
            *guard = Some(tokio::spawn(async move {
                auto_import_loop(state_clone).await;
            }));
        }
    } else if let Some(handle) = guard.take() {
        handle.abort();
    }
}

async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({"ok": true, "version": state.app_version}))
}

async fn home(State(state): State<Arc<AppState>>) -> AppResult<Html<String>> {
    let setup_is_enabled = setup_enabled();
    if needs_setup(&state) && setup_is_enabled {
        let html = render_template(&state, "setup.html", setup_context(&state))?;
        return Ok(Html(html));
    }

    let mut ctx = Context::new();
    ctx.insert("app_version", &state.app_version);
    ctx.insert("setup_enabled", &setup_is_enabled);
    Ok(Html(render_template(&state, "index.html", ctx)?))
}

async fn setup_page(State(state): State<Arc<AppState>>) -> AppResult<Html<String>> {
    if !setup_enabled() {
        return Err(AppError::not_found("Not found"));
    }
    Ok(Html(render_template(&state, "setup.html", setup_context(&state))?))
}

async fn api_setup(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SetupPayload>,
) -> AppResult<Json<Value>> {
    if !setup_enabled() {
        return Err(AppError::not_found("Not found"));
    }

    let mut cfg = load_json_config();
    if let Some(value) = body.mam_cookie.as_ref().map(|v| v.trim()).filter(|v| !v.is_empty()) {
        cfg.insert("MAM_COOKIE".to_owned(), Value::String(value.to_owned()));
    }
    if let Some(value) = body
        .transmission_url
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        cfg.insert("TRANSMISSION_URL".to_owned(), Value::String(value.to_owned()));
    }
    if let Some(value) = body
        .transmission_user
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        cfg.insert("TRANSMISSION_USER".to_owned(), Value::String(value.to_owned()));
    }
    if let Some(value) = body.transmission_pass.as_ref().filter(|v| !v.is_empty()) {
        cfg.insert("TRANSMISSION_PASS".to_owned(), Value::String(value.to_owned()));
    }
    if let Some(value) = body
        .transmission_label
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        cfg.insert("TRANSMISSION_LABEL".to_owned(), Value::String(value.to_owned()));
    }
    if let Some(value) = body.auto_import_enabled {
        cfg.insert("AUTO_IMPORT_ENABLED".to_owned(), Value::Bool(value));
    }

    write_json_config(&state.config_path, &cfg)?;
    state.reload_settings();
    reconcile_auto_import_task(state.clone()).await;
    Ok(Json(json!({ "ok": true })))
}

async fn search(State(state): State<Arc<AppState>>, Json(payload): Json<Value>) -> AppResult<Json<Value>> {
    let settings = state.settings();
    if settings.mam_cookie.is_empty() {
        return Err(AppError::internal("MAM_COOKIE not set on server"));
    }

    let media_type = normalize_media_type(payload.get("media_type").and_then(Value::as_str))?;
    let mut tor = payload
        .get("tor")
        .cloned()
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}));
    let tor_obj = tor.as_object_mut().unwrap();
    tor_obj.entry("text".to_owned()).or_insert_with(|| Value::String(String::new()));
    if media_type == MEDIA_TYPE_EBOOK {
        tor_obj.insert(
            "srchIn".to_owned(),
            Value::Array(vec![
                Value::String("title".to_owned()),
                Value::String("author".to_owned()),
            ]),
        );
    } else {
        tor_obj.insert(
            "srchIn".to_owned(),
            Value::Array(vec![
                Value::String("title".to_owned()),
                Value::String("author".to_owned()),
                Value::String("narrator".to_owned()),
            ]),
        );
    }
    tor_obj.insert("searchType".to_owned(), Value::String("all".to_owned()));
    tor_obj.insert("sortType".to_owned(), Value::String("seedersDesc".to_owned()));
    tor_obj.entry("startNumber".to_owned()).or_insert_with(|| Value::String("0".to_owned()));
    tor_obj.insert(
        "main_cat".to_owned(),
        Value::Array(vec![Value::String(media_main_category(&media_type).to_owned())]),
    );

    let perpage = payload
        .get("perpage")
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        })
        .unwrap_or(25);

    let body = json!({ "tor": tor, "perpage": perpage });
    let client = Client::new();
    let response = client
        .post(format!("{}/tor/js/loadSearchJSONbasic.php", settings.mam_base))
        .header(header::COOKIE, settings.mam_cookie.clone())
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::ACCEPT, "application/json, */*")
        .header(header::USER_AGENT, "Mozilla/5.0")
        .header(header::ORIGIN, "https://www.myanonamouse.net")
        .header(header::REFERER, "https://www.myanonamouse.net/")
        .query(&[("dlLink", "1")])
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::bad_gateway(format!("MAM request failed: {e}")))?;

    let status = response.status();
    if status != StatusCode::OK {
        let text = response.text().await.unwrap_or_default();
        return Err(AppError::bad_gateway(format!(
            "MAM HTTP {}: {}",
            status,
            text.chars().take(300).collect::<String>()
        )));
    }

    let raw: Value = response
        .json()
        .await
        .map_err(|e| AppError::bad_gateway(format!("MAM returned non-JSON. Body parse failed: {e}")))?;

    let mut results = Vec::new();
    for item in raw.get("data").and_then(Value::as_array).cloned().unwrap_or_default() {
        let is_freeleech = truthy_value(item.get("free")) || truthy_value(item.get("fl_vip"));
        let is_vip = truthy_value(item.get("vip")) || truthy_value(item.get("fl_vip"));
        results.push(SearchResult {
            id: extract_string_id(item.get("id").or_else(|| item.get("tid"))),
            title: item
                .get("title")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            author_info: flatten_value(item.get("author_info")),
            narrator_info: flatten_value(item.get("narrator_info")),
            format: detect_format(&item),
            size: json_number_or_string(item.get("size")),
            seeders: json_number_or_string(item.get("seeders")),
            leechers: json_number_or_string(item.get("leechers")),
            catname: json_number_or_string(item.get("catname")),
            added: json_number_or_string(item.get("added")),
            dl: json_number_or_string(item.get("dl")),
            media_type: media_type.clone(),
            is_freeleech,
            is_vip,
        });
    }

    Ok(Json(json!({
        "results": results,
        "total": raw.get("total"),
        "total_found": raw.get("total_found")
    })))
}

async fn add_to_transmission(State(state): State<Arc<AppState>>, Json(body): Json<AddBody>) -> AppResult<Json<Value>> {
    let settings = state.settings();
    let mam_id = extract_string_id(body.id.as_ref());
    let title = body.title.unwrap_or_default().trim().to_owned();
    let author = body.author.unwrap_or_default().trim().to_owned();
    let narrator = body.narrator.unwrap_or_default().trim().to_owned();
    let media_type = normalize_media_type(body.media_type.as_deref())?;
    let dl = body.dl.unwrap_or_default().trim().to_owned();

    if mam_id.is_empty() && dl.is_empty() {
        return Err(AppError::bad_request("Missing MAM id and dl; need at least one"));
    }

    let direct_url = if dl.is_empty() {
        None
    } else {
        Some(format!("{}/tor/download.php/{}", settings.mam_base, dl))
    };
    let id_candidates = if mam_id.is_empty() {
        Vec::new()
    } else {
        vec![
            format!("{}/tor/download.php?id={}", settings.mam_base, mam_id),
            format!("{}/tor/download.php?tid={}", settings.mam_base, mam_id),
        ]
    };

    let client = Client::new();

    if let Some(url) = direct_url {
        match transmission_rpc(
            &client,
            &settings,
            "torrent-add",
            Some(torrent_add_arguments(&settings, &mam_id, "filename", &url)),
        )
        .await
        {
            Ok(args) => {
                let torrent_hash = torrent_hash_from_add_result(&args);
                insert_history(
                    &state,
                    &mam_id,
                    &title,
                    &author,
                    &narrator,
                    &media_type,
                    &dl,
                    torrent_hash,
                )?;
                return Ok(Json(json!({ "ok": true })));
            }
            Err(err) => {
                if id_candidates.is_empty() {
                    return Err(err);
                }
            }
        }
    }

    let mam_headers = [
        (header::COOKIE, settings.mam_cookie.clone()),
        (header::USER_AGENT, "Mozilla/5.0".to_owned()),
        (header::ACCEPT, "application/x-bittorrent, */*".to_owned()),
        (header::REFERER, "https://www.myanonamouse.net/".to_owned()),
        (header::ORIGIN, "https://www.myanonamouse.net".to_owned()),
    ];

    let mut torrent_bytes = None;
    for url in id_candidates {
        let mut request = client.get(url);
        for (key, value) in &mam_headers {
            request = request.header(key.clone(), value.clone());
        }
        let response = request
            .send()
            .await
            .map_err(|e| AppError::bad_gateway(format!("MAM request failed: {e}")))?;
        if response.status() == StatusCode::OK {
            let bytes = response
                .bytes()
                .await
                .map_err(|e| AppError::bad_gateway(format!("MAM torrent body failed: {e}")))?;
            if !bytes.is_empty() {
                torrent_bytes = Some(bytes.to_vec());
                break;
            }
        }
    }

    let Some(torrent_bytes) = torrent_bytes else {
        return Err(AppError::bad_gateway(
            "Could not fetch .torrent from MAM (no dl hash and cookie fetch failed).",
        ));
    };

    let metainfo = base64::engine::general_purpose::STANDARD.encode(torrent_bytes);
    let args = transmission_rpc(
        &client,
        &settings,
        "torrent-add",
        Some(torrent_add_arguments(&settings, &mam_id, "metainfo", &metainfo)),
    )
    .await?;
    let torrent_hash = torrent_hash_from_add_result(&args);
    insert_history(
        &state,
        &mam_id,
        &title,
        &author,
        &narrator,
        &media_type,
        &dl,
        torrent_hash,
    )?;

    Ok(Json(json!({ "ok": true })))
}

async fn history(State(state): State<Arc<AppState>>) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "items": history_rows(&state)? })))
}

async fn delete_history_item(
    State(_state): State<Arc<AppState>>,
    AxumPath(row_id): AxumPath<i64>,
) -> AppResult<Json<Value>> {
    let conn = db_conn()?;
    conn.execute("DELETE FROM history WHERE id = ?1", params![row_id])
        .map_err(|e| AppError::internal(format!("Failed to delete history row: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

async fn transmission_torrents(State(state): State<Arc<AppState>>) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "items": list_completed_torrents(state).await? })))
}

async fn import_item(State(state): State<Arc<AppState>>, Json(body): Json<ImportBody>) -> AppResult<Json<Value>> {
    if let Some(history_id) = body.history_id {
        update_history_status(&state, history_id, "importing", None, None)?;
    }

    let media_type = match get_history_media_type(&state, body.history_id)? {
        Some(value) => value,
        None => normalize_media_type(body.media_type.as_deref())?,
    };

    let result = import_torrent_to_library(state.clone(), &body.author, &body.title, &body.hash, &media_type).await;
    match result {
        Ok(dest) => {
            mark_history_imported(&state, body.history_id, &body.hash)?;
            Ok(Json(json!({ "ok": true, "dest": dest })))
        }
        Err(err) => {
            if let Some(history_id) = body.history_id {
                let _ = mark_history_failed(&state, Some(history_id), &body.hash, &err.detail);
            }
            Err(err)
        }
    }
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .init();

    let state = AppState::load()?;
    reconcile_auto_import_task(state.clone()).await;

    let app = Router::new()
        .route("/health", get(health))
        .route("/", get(home))
        .route("/setup", get(setup_page))
        .route("/api/setup", post(api_setup))
        .route("/search", post(search))
        .route("/add", post(add_to_transmission))
        .route("/history", get(history))
        .route("/history/:row_id", delete(delete_history_item))
        .route("/transmission/torrents", get(transmission_torrents))
        .route("/import", post(import_item))
        .nest_service("/static", ServeDir::new("app/static"))
        .with_state(state.clone());

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    let listener = TcpListener::bind(addr).await?;
    info!("Listening on http://{addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            stop_auto_import_task(state.clone()).await;
        })
        .await?;
    Ok(())
}
