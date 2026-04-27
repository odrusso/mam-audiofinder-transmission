use std::sync::Arc;

use axum::{
    extract::{Path as AxumPath, State},
    response::Html,
    Json,
};
use base64::Engine;
use reqwest::{header, Client};
use serde_json::{json, Value};

use crate::{
    app_state::{
        needs_setup, render_template, setup_context, setup_enabled, AppError, AppResult, AppState,
        AddBody, ImportBody, SetupPayload, MEDIA_TYPE_EBOOK,
        MAM_MAIN_CATEGORIES_AUDIOBOOK, MAM_MAIN_CATEGORIES_EBOOK,
    },
    import::{
        history_rows, import_torrent_to_library, insert_history, mark_history_failed,
        mark_history_imported, reconcile_auto_import_task,
    },
    transmission::{list_completed_torrents, torrent_add_arguments, torrent_hash_from_add_result, transmission_rpc},
};

use regex::Regex;
use std::sync::OnceLock;

fn media_main_category(media_type: &str) -> &'static str {
    if media_type == MEDIA_TYPE_EBOOK {
        MAM_MAIN_CATEGORIES_EBOOK
    } else {
        MAM_MAIN_CATEGORIES_AUDIOBOOK
    }
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

    static FORMAT_RE: OnceLock<Regex> = OnceLock::new();
    let re = FORMAT_RE.get_or_init(|| {
        Regex::new(r"(?i)\b(mp3|m4b|flac|aac|ogg|opus|wav|alac|ape|epub|pdf|mobi|azw3|cbz|cbr)\b")
            .unwrap()
    });
    let toks = re
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
    match v {
        Some(Value::Bool(x)) => *x,
        Some(Value::Number(n)) => n.as_i64().map(|v| v != 0).unwrap_or(false),
        Some(Value::String(s)) => matches!(s.trim().to_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        _ => false,
    }
}

fn extract_string_id(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => value_to_string(other).trim().to_owned(),
        None => String::new(),
    }
}

pub(crate) async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({"ok": true, "version": state.app_version}))
}

pub(crate) async fn home(State(state): State<Arc<AppState>>) -> AppResult<Html<String>> {
    let setup_is_enabled = setup_enabled();
    if needs_setup(&state) && setup_is_enabled {
        let html = render_template(&state, "setup.html", setup_context(&state))?;
        return Ok(Html(html));
    }

    let mut ctx = tera::Context::new();
    ctx.insert("app_version", &state.app_version);
    ctx.insert("setup_enabled", &setup_is_enabled);
    Ok(Html(render_template(&state, "index.html", ctx)?))
}

pub(crate) async fn setup_page(State(state): State<Arc<AppState>>) -> AppResult<Html<String>> {
    if !setup_enabled() {
        return Err(AppError::not_found("Not found"));
    }
    Ok(Html(render_template(&state, "setup.html", setup_context(&state))?))
}

pub(crate) async fn api_setup(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SetupPayload>,
) -> AppResult<Json<Value>> {
    if !setup_enabled() {
        return Err(AppError::not_found("Not found"));
    }

    let mut cfg = crate::app_state::load_json_config();
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

    crate::app_state::write_json_config(&state.config_path, &cfg)?;
    state.reload_settings();
    reconcile_auto_import_task(state.clone()).await;
    Ok(Json(json!({ "ok": true })))
}

pub(crate) async fn search(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<Value>,
) -> AppResult<Json<Value>> {
    let settings = state.settings();
    if settings.mam_cookie.is_empty() {
        return Err(AppError::internal("MAM_COOKIE not set on server"));
    }

    let media_type = crate::app_state::normalize_media_type(payload.get("media_type").and_then(Value::as_str))?;
    let mut tor = payload
        .get("tor")
        .cloned()
        .filter(|v| v.is_object())
        .unwrap_or_else(|| json!({}));
    let tor_obj = tor.as_object_mut().unwrap();
    tor_obj
        .entry("text".to_owned())
        .or_insert_with(|| Value::String(String::new()));
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
    tor_obj
        .entry("startNumber".to_owned())
        .or_insert_with(|| Value::String("0".to_owned()));
    tor_obj.insert(
        "main_cat".to_owned(),
        Value::Array(vec![Value::String(media_main_category(&media_type).to_owned())]),
    );

    let perpage = payload
        .get("perpage")
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok())))
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
    if status != reqwest::StatusCode::OK {
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
        results.push(crate::app_state::SearchResult {
            id: extract_string_id(item.get("id").or_else(|| item.get("tid"))),
            title: item
                .get("title")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            author_info: flatten_value(item.get("author_info")),
            narrator_info: flatten_value(item.get("narrator_info")),
            format: detect_format(&item),
            size: item.get("size").cloned(),
            seeders: item.get("seeders").cloned(),
            leechers: item.get("leechers").cloned(),
            catname: item.get("catname").cloned(),
            added: item.get("added").cloned(),
            dl: item.get("dl").cloned(),
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

pub(crate) async fn add_to_transmission(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AddBody>,
) -> AppResult<Json<Value>> {
    let settings = state.settings();
    let mam_id = extract_string_id(body.id.as_ref());
    let title = body.title.unwrap_or_default().trim().to_owned();
    let author = body.author.unwrap_or_default().trim().to_owned();
    let narrator = body.narrator.unwrap_or_default().trim().to_owned();
    let media_type = crate::app_state::normalize_media_type(body.media_type.as_deref())?;
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
        if response.status() == reqwest::StatusCode::OK {
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

pub(crate) async fn history(State(state): State<Arc<AppState>>) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "items": history_rows(&state)? })))
}

pub(crate) async fn delete_history_item(
    State(_state): State<Arc<AppState>>,
    AxumPath(row_id): AxumPath<i64>,
) -> AppResult<Json<Value>> {
    let conn = crate::app_state::db_conn()?;
    conn.execute("DELETE FROM history WHERE id = ?1", rusqlite::params![row_id])
        .map_err(|e| AppError::internal(format!("Failed to delete history row: {e}")))?;
    Ok(Json(json!({ "ok": true })))
}

pub(crate) async fn transmission_torrents(
    State(state): State<Arc<AppState>>,
) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "items": list_completed_torrents(state).await? })))
}

pub(crate) async fn import_item(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ImportBody>,
) -> AppResult<Json<Value>> {
    if let Some(history_id) = body.history_id {
        crate::import::update_history_status(&state, history_id, "importing", None, None)?;
    }

    let media_type = match crate::import::get_history_media_type(&state, body.history_id)? {
        Some(value) => value,
        None => crate::app_state::normalize_media_type(body.media_type.as_deref())?,
    };

    let result =
        import_torrent_to_library(state.clone(), &body.author, &body.title, &body.hash, &media_type).await;
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
