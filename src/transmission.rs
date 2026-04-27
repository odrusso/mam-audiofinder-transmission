use std::{collections::HashSet, sync::Arc};

use reqwest::Client;
use serde_json::{json, Value};

use crate::app_state::{
    AppError, AppResult, AppState, CompletedTorrent, Settings,
};

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

pub(crate) async fn transmission_rpc(
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
    let response = if status == reqwest::StatusCode::CONFLICT {
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
    if status != reqwest::StatusCode::OK {
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
                .map(|v| v.to_string())
                .unwrap_or_else(|| "unknown".to_owned())
        )));
    }

    Ok(data
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({})))
}

pub(crate) fn transmission_labels(settings: &Settings, mam_id: &str) -> Vec<String> {
    let mut labels = Vec::new();
    if !settings.transmission_label.is_empty() {
        labels.push(settings.transmission_label.clone());
    }
    if !mam_id.is_empty() {
        labels.push(format!("mamid={mam_id}"));
    }
    labels
}

pub(crate) fn torrent_add_arguments(
    settings: &Settings,
    mam_id: &str,
    source_key: &str,
    source_value: &str,
) -> Value {
    let mut args = serde_json::Map::new();
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

pub(crate) fn torrent_hash_from_add_result(args: &Value) -> Option<String> {
    args.get("torrent-added")
        .or_else(|| args.get("torrent-duplicate"))
        .and_then(|torrent| torrent.get("hashString"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub(crate) async fn list_completed_torrents(
    state: Arc<AppState>,
) -> AppResult<Vec<CompletedTorrent>> {
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
            size: t.get("totalSize").cloned(),
            added_on: t.get("addedDate").cloned(),
        });
    }

    Ok(out)
}
