use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use reqwest::Client;
use rusqlite::{named_params, params, OptionalExtension};
use serde_json::Value;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::{
    app_state::{
        clean_status_detail, db_conn, normalize_media_type, sanitize_name, AppError, AppResult,
        AppState, HistoryItem, MEDIA_TYPE_EBOOK, DOWNLOADS_DIR, EBOOKS_DIR, LIBRARY_DIR,
    },
    transmission::{list_completed_torrents, transmission_rpc},
};

pub(crate) fn validate_download_path(p: &str) -> AppResult<String> {
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

fn copy_one(src: &Path, dst: &Path) -> AppResult<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| AppError::internal(format!("Failed to create destination directory: {e}")))?;
    }
    fs::copy(src, dst)
        .map_err(|e| AppError::internal(format!("Failed to copy file {src:?} -> {dst:?}: {e}")))?;
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

fn is_transient_auto_import_error(err: &AppError) -> bool {
    err.status == axum::http::StatusCode::BAD_GATEWAY && err.detail.starts_with("Transmission")
}

pub(crate) fn insert_history(
    _state: &AppState,
    mam_id: &str,
    title: &str,
    author: &str,
    narrator: &str,
    media_type: &str,
    dl: &str,
    torrent_hash: Option<String>,
) -> AppResult<()> {
    let added_at = crate::app_state::utcnow_str();
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
            ":status_updated_at": crate::app_state::utcnow_str(),
        },
    )
    .map_err(|e| AppError::internal(format!("Failed to insert history: {e}")))?;
    Ok(())
}

pub(crate) fn update_history_status(
    _state: &AppState,
    history_id: i64,
    status: &str,
    detail: Option<&str>,
    imported_at: Option<&str>,
) -> AppResult<()> {
    let ts = crate::app_state::utcnow_str();
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

pub(crate) fn mark_history_imported(
    _state: &AppState,
    history_id: Option<i64>,
    torrent_hash: &str,
) -> AppResult<()> {
    let ts = crate::app_state::utcnow_str();
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

pub(crate) fn mark_history_failed(
    _state: &AppState,
    history_id: Option<i64>,
    torrent_hash: &str,
    detail: &str,
) -> AppResult<()> {
    let ts = crate::app_state::utcnow_str();
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

pub(crate) fn get_history_media_type(
    _state: &AppState,
    history_id: Option<i64>,
) -> AppResult<Option<String>> {
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

pub(crate) fn get_auto_import_candidates(
    _state: &AppState,
    completed_hashes: &HashSet<String>,
) -> AppResult<Vec<HistoryItem>> {
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
        if torrent_hash.is_empty()
            || !completed_hashes.contains(&torrent_hash)
            || seen_hashes.contains(&torrent_hash)
        {
            continue;
        }
        seen_hashes.insert(torrent_hash);
        out.push(row);
    }
    Ok(out)
}

pub(crate) fn history_rows(_state: &AppState) -> AppResult<Vec<HistoryItem>> {
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

pub(crate) async fn import_torrent_to_library(
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
        Some(serde_json::json!({
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
        if src
            .extension()
            .and_then(|v| v.to_str())
            .map(|v| v.eq_ignore_ascii_case("cue"))
            .unwrap_or(false)
        {
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

pub(crate) async fn auto_import_cycle(state: Arc<AppState>) -> AppResult<()> {
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

pub(crate) async fn auto_import_loop(state: Arc<AppState>) {
    let interval = state.settings.read().unwrap().auto_import_poll_interval;
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

pub(crate) async fn stop_auto_import_task(state: Arc<AppState>) {
    let handle = {
        let mut guard = state.auto_import_task.lock().unwrap();
        guard.take()
    };
    if let Some(handle) = handle {
        handle.abort();
        let _ = handle.await;
    }
}

pub(crate) async fn reconcile_auto_import_task(state: Arc<AppState>) {
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
