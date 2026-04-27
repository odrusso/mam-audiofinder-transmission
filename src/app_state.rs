use std::{
    collections::HashSet,
    fs,
    path::Path,
    sync::{Arc, Mutex, RwLock},
};

use chrono::Utc;
use libc::umask;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tera::{Context, Tera};
use tokio::task::JoinHandle;

pub(crate) const CONFIG_PATH_DEFAULT: &str = "/data/config.json";
pub(crate) const DOWNLOADS_DIR: &str = "/downloads";
pub(crate) const LIBRARY_DIR: &str = "/library";
pub(crate) const EBOOKS_DIR: &str = "/ebooks";
pub(crate) const DEFAULT_AUTO_IMPORT_POLL_INTERVAL: u64 = 30;
pub(crate) const MEDIA_TYPE_AUDIOBOOK: &str = "audiobook";
pub(crate) const MEDIA_TYPE_EBOOK: &str = "ebook";
pub(crate) const HISTORY_DB_PATH: &str = "/data/history.db";

pub(crate) const MAM_MAIN_CATEGORIES_AUDIOBOOK: &str = "13";
pub(crate) const MAM_MAIN_CATEGORIES_EBOOK: &str = "14";

#[derive(Clone)]
pub(crate) struct Settings {
    pub(crate) mam_base: String,
    pub(crate) mam_cookie: String,
    pub(crate) transmission_url: String,
    pub(crate) transmission_user: String,
    pub(crate) transmission_pass: String,
    pub(crate) transmission_label: String,
    pub(crate) auto_import_enabled: bool,
    pub(crate) auto_import_poll_interval: u64,
    pub(crate) umask: Option<String>,
}

impl Settings {
    pub(crate) fn load() -> Self {
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

    pub(crate) fn apply_umask(&self) {
        if let Some(raw) = &self.umask {
            if let Ok(mask) = u32::from_str_radix(raw.trim(), 8) {
                unsafe {
                    umask(mask as _);
                }
            }
        }
    }

    pub(crate) fn setup_context(&self, app_version: &str) -> Context {
        let mut ctx = Context::new();
        ctx.insert("app_version", app_version);
        ctx.insert("transmission_url", &self.transmission_url);
        ctx.insert("transmission_user", &self.transmission_user);
        ctx.insert("transmission_label", &self.transmission_label);
        ctx.insert("auto_import_enabled", &self.auto_import_enabled);
        ctx
    }
}

pub(crate) struct AppState {
    pub(crate) app_version: String,
    pub(crate) config_path: String,
    pub(crate) settings: RwLock<Settings>,
    pub(crate) templates: Tera,
    pub(crate) auto_import_task: Mutex<Option<JoinHandle<()>>>,
}

impl AppState {
    pub(crate) fn load() -> anyhow::Result<Arc<Self>> {
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

    pub(crate) fn settings(&self) -> Settings {
        self.settings.read().unwrap().clone()
    }

    pub(crate) fn reload_settings(&self) {
        let settings = Settings::load();
        settings.apply_umask();
        *self.settings.write().unwrap() = settings;
    }
}

#[derive(Debug)]
pub(crate) struct AppError {
    pub(crate) status: axum::http::StatusCode,
    pub(crate) detail: String,
}

impl AppError {
    pub(crate) fn new(status: axum::http::StatusCode, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
        }
    }

    pub(crate) fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(axum::http::StatusCode::BAD_REQUEST, detail)
    }

    pub(crate) fn not_found(detail: impl Into<String>) -> Self {
        Self::new(axum::http::StatusCode::NOT_FOUND, detail)
    }

    pub(crate) fn bad_gateway(detail: impl Into<String>) -> Self {
        Self::new(axum::http::StatusCode::BAD_GATEWAY, detail)
    }

    pub(crate) fn internal(detail: impl Into<String>) -> Self {
        Self::new(axum::http::StatusCode::INTERNAL_SERVER_ERROR, detail)
    }
}

impl axum::response::IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        (self.status, axum::Json(serde_json::json!({ "detail": self.detail }))).into_response()
    }
}

pub(crate) type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SearchResult {
    pub(crate) id: String,
    pub(crate) title: Option<String>,
    pub(crate) author_info: String,
    pub(crate) narrator_info: String,
    pub(crate) format: String,
    pub(crate) size: Option<Value>,
    pub(crate) seeders: Option<Value>,
    pub(crate) leechers: Option<Value>,
    pub(crate) catname: Option<Value>,
    pub(crate) added: Option<Value>,
    pub(crate) dl: Option<Value>,
    pub(crate) media_type: String,
    pub(crate) is_freeleech: bool,
    pub(crate) is_vip: bool,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HistoryItem {
    pub(crate) id: i64,
    pub(crate) mam_id: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) author: Option<String>,
    pub(crate) narrator: Option<String>,
    pub(crate) media_type: Option<String>,
    pub(crate) dl: Option<String>,
    pub(crate) torrent_hash: Option<String>,
    pub(crate) added_at: Option<String>,
    pub(crate) imported_at: Option<String>,
    pub(crate) torrent_status: Option<String>,
    pub(crate) status_detail: Option<String>,
    pub(crate) status_updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CompletedTorrent {
    pub(crate) hash: String,
    pub(crate) name: Option<String>,
    pub(crate) download_dir: Option<String>,
    pub(crate) root: String,
    pub(crate) single_file: bool,
    pub(crate) size: Option<Value>,
    pub(crate) added_on: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SetupPayload {
    pub(crate) mam_cookie: Option<String>,
    pub(crate) transmission_url: Option<String>,
    pub(crate) transmission_user: Option<String>,
    pub(crate) transmission_pass: Option<String>,
    pub(crate) transmission_label: Option<String>,
    pub(crate) auto_import_enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AddBody {
    pub(crate) id: Option<Value>,
    pub(crate) title: Option<String>,
    pub(crate) dl: Option<String>,
    pub(crate) author: Option<String>,
    pub(crate) narrator: Option<String>,
    pub(crate) media_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ImportBody {
    pub(crate) author: String,
    pub(crate) title: String,
    pub(crate) hash: String,
    pub(crate) history_id: Option<i64>,
    pub(crate) media_type: Option<String>,
}

pub(crate) fn env_or_default(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

pub(crate) fn is_truthy_json(value: &Value) -> bool {
    match value {
        Value::Bool(v) => *v,
        Value::Number(n) => n.as_i64().map(|v| v != 0).unwrap_or(false),
        Value::String(s) => is_truthy_str(s),
        _ => false,
    }
}

pub(crate) fn is_truthy_str(value: &str) -> bool {
    matches!(
        value.trim().to_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub(crate) fn parse_positive_int(value: &str, default: u64) -> u64 {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

pub(crate) fn build_mam_cookie(raw: &str) -> String {
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

pub(crate) fn normalize_media_type(value: Option<&str>) -> AppResult<String> {
    let media_type = value.unwrap_or(MEDIA_TYPE_AUDIOBOOK).trim().to_lowercase();
    match media_type.as_str() {
        "audiobook" | "audiobooks" | "audio" => Ok(MEDIA_TYPE_AUDIOBOOK.to_owned()),
        "ebook" | "ebooks" | "e-book" | "e-books" => Ok(MEDIA_TYPE_EBOOK.to_owned()),
        _ => Err(AppError::bad_request("media_type must be audiobook or ebook")),
    }
}

pub(crate) fn load_json_config() -> Map<String, Value> {
    let path = env_or_default("APP_CONFIG_PATH", CONFIG_PATH_DEFAULT);
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default(),
        Err(_) => Map::new(),
    }
}

pub(crate) fn write_json_config(path: &str, cfg: &Map<String, Value>) -> AppResult<()> {
    if let Some(parent) = Path::new(path).parent() {
        fs::create_dir_all(parent)
            .map_err(|e| AppError::internal(format!("Failed to create config directory: {e}")))?;
    }

    let text = serde_json::to_string_pretty(cfg)
        .map_err(|e| AppError::internal(format!("Failed to serialize config: {e}")))?;
    fs::write(path, text).map_err(|e| AppError::internal(format!("Failed to write config: {e}")))
}

pub(crate) fn init_db() -> anyhow::Result<()> {
    if let Some(parent) = Path::new(HISTORY_DB_PATH).parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = rusqlite::Connection::open(HISTORY_DB_PATH)?;
    ensure_history_schema(&conn)?;
    Ok(())
}

pub(crate) fn db_conn() -> AppResult<rusqlite::Connection> {
    if let Some(parent) = Path::new(HISTORY_DB_PATH).parent() {
        fs::create_dir_all(parent)
            .map_err(|e| AppError::internal(format!("Failed to create database directory: {e}")))?;
    }
    let conn = rusqlite::Connection::open(HISTORY_DB_PATH)
        .map_err(|e| AppError::internal(format!("Failed to open database: {e}")))?;
    ensure_history_schema(&conn)
        .map_err(|e| AppError::internal(format!("Failed to prepare database schema: {e}")))?;
    Ok(conn)
}

pub(crate) fn ensure_history_schema(conn: &rusqlite::Connection) -> anyhow::Result<()> {
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

pub(crate) fn utcnow_str() -> String {
    Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

pub(crate) fn setup_enabled() -> bool {
    !is_truthy_str(&env_or_default("DISABLE_SETUP", ""))
}

pub(crate) fn needs_setup(state: &AppState) -> bool {
    state.settings.read().unwrap().mam_cookie.is_empty()
}

pub(crate) fn render_template(state: &AppState, name: &str, context: Context) -> AppResult<String> {
    state
        .templates
        .render(name, &context)
        .map_err(|e| AppError::internal(format!("Template render failed: {e}")))
}

pub(crate) fn setup_context(state: &AppState) -> Context {
    state.settings.read().unwrap().setup_context(&state.app_version)
}

pub(crate) fn sanitize_name(name: &str) -> String {
    static WHITESPACE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
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

pub(crate) fn clean_status_detail(detail: Option<&str>) -> Option<String> {
    static WHITESPACE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
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

