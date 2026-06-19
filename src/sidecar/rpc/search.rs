use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::sidecar::protocol::{
    err_response, ok_response, parse_params, JsonRpcRequest, JsonRpcResponse,
};
use crate::sidecar::rpc::indexing::adapters::helix::HelixTextStore;

#[derive(Debug, Deserialize)]
struct SearchQueryParams {
    q: String,
}

fn value_as_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(ToString::to_string)
}

fn infer_thumbnails_dir() -> PathBuf {
    if let Ok(custom_dir) = env::var("THUMBNAILS_DIR") {
        return PathBuf::from(custom_dir);
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("videos")
        .join("output_indexer")
        .join("thumbnail_cache")
}

fn infer_extracted_thumbnails_dir() -> PathBuf {
    if let Ok(custom_dir) = env::var("EXTRACTED_THUMBNAILS_DIR") {
        return PathBuf::from(custom_dir);
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("videos")
        .join("output_indexer")
        .join("thumbnails")
}

fn find_extracted_thumbnail(video_path: &str) -> Option<PathBuf> {
    let stem = Path::new(video_path)
        .file_stem()?
        .to_string_lossy()
        .to_string();
    if stem.is_empty() {
        return None;
    }
    let extracted_dir = infer_extracted_thumbnails_dir();
    for name in ["middle.jpg", "start.jpg", "end.jpg"] {
        let direct = extracted_dir.join(&stem).join(name);
        if direct.exists() {
            return Some(direct);
        }
    }
    let prefix = format!("{}_chunk_", stem);
    let mut candidates = fs::read_dir(&extracted_dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let file_type = entry.file_type().ok()?;
            if !file_type.is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&prefix) {
                Some((name, entry.path()))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| a.0.cmp(&b.0));
    for (_, dir) in candidates {
        for name in ["middle.jpg", "start.jpg", "end.jpg"] {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

fn resolve_thumbnail_path(content_hash: &str, video_path: &str) -> Option<String> {
    if content_hash.is_empty() {
        return None;
    }
    let cache_dir = infer_thumbnails_dir();
    let cached = cache_dir.join(format!("{}.jpg", content_hash));
    if cached.exists() {
        return Some(cached.to_string_lossy().replace('\\', "/"));
    }
    let source = find_extracted_thumbnail(video_path)?;
    fs::create_dir_all(&cache_dir).ok()?;
    fs::copy(source, &cached).ok()?;
    Some(cached.to_string_lossy().replace('\\', "/"))
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for &byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~' | b'/' | b':')
        {
            encoded.push(byte as char);
        } else {
            encoded.push_str(&format!("%{:02X}", byte));
        }
    }
    encoded
}

fn is_empty_vector_index_error(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("no entry point found for hnsw index")
        || lowered.contains("empty input provided to reranker")
        || (lowered.contains("graph_error") && lowered.contains("vector error"))
        || (lowered.contains("graph_error") && lowered.contains("reranker error"))
}

fn is_transient_embedding_error(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("embeddingerror")
        || lowered.contains("embedding error")
        || lowered.contains("error while embedding text")
        || lowered.contains("failed to send request to openai")
        || lowered.contains("error sending request for url")
}

fn normalize_search_result(label: &str, result: Result<Value, String>) -> Result<Value, String> {
    match result {
        Ok(value) => Ok(value),
        Err(message) => {
            if is_empty_vector_index_error(&message) {
                eprintln!(
                    "[sidecar:search] {} search returned empty-index response; treating as no results: {}",
                    label, message
                );
                Ok(Value::Array(Vec::new()))
            } else if is_transient_embedding_error(&message) {
                eprintln!(
                    "[sidecar:search] {} search embedding backend failed; treating as no results: {}",
                    label, message
                );
                Ok(Value::Array(Vec::new()))
            } else {
                Err(format!("{} search failed: {}", label, message))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Distance threshold.
// HelixDB returns $distance (lower = more similar, 0.0 = identical).
// Voyage cosine distances range roughly 0.0–2.0.
// Default 0.30 for image_caption units — captions are rich text so a tighter
// threshold is appropriate. Override via SEARCH_DISTANCE_THRESHOLD env var.
// ---------------------------------------------------------------------------
fn distance_threshold() -> f64 {
    env::var("SEARCH_DISTANCE_THRESHOLD")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(0.30)
}

async fn rust_helix_search_query(query: &str) -> Result<Value, String> {
    let store = HelixTextStore::from_env()?;
    let vector_f32 = store.embed_search_query(query).await?;

    let backend_timeout_ms = env::var("SIDECAR_SEARCH_BACKEND_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(12_000);
    let backend_timeout = Duration::from_millis(backend_timeout_ms);

    let raw = tokio::time::timeout(
        backend_timeout,
        store.search_asset_embeddings(vector_f32),
    )
    .await;

    let response = match raw {
        Ok(inner) => normalize_search_result("asset", inner)?,
        Err(_) => {
            eprintln!("[sidecar:search] asset search timed out; treating as no results");
            Value::Array(Vec::new())
        }
    };

    // -----------------------------------------------------------------------
    // Response shape from HelixDB (after search_asset_embeddings now returns
    // embeddings only — no assets array):
    //   embeddings.properties[] — AssetEmbedding nodes {
    //       $id, $distance, unit_kind, unit_key, content, vector
    //   }
    //
    // Strategy:
    //   1. Iterate embeddings, skip file_path units (too short/generic).
    //   2. Keep embeddings whose $distance <= threshold.
    //   3. Sort by distance ascending.
    //   4. Fetch the connected Asset for each matched embedding via
    //      store.get_assets_for_embedding_ids() — preserves 1:1 mapping.
    //   5. Deduplicate by content_hash, keeping the first (best distance).
    // -----------------------------------------------------------------------

    let embeddings_raw = response
        .get("embeddings")
        .map(|v| v.get("properties").unwrap_or(v))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let threshold = distance_threshold();

    // Filter and collect (distance, embedding_$id) for non-noise units.
    let mut relevant: Vec<(f64, i64, String)> = Vec::new();
    for emb in &embeddings_raw {
        let Some(obj) = emb.as_object() else { continue };
        let unit_kind = value_as_string(obj.get("unit_kind")).unwrap_or_default();
        // Skip indexing noise units — these are internal bookkeeping, not real content.
        if unit_kind == "file_path" || unit_kind == "video_index_state" {
            continue;
        }
        let dist = obj.get("$distance").and_then(Value::as_f64).unwrap_or(f64::MAX);
        let id = obj.get("$id").and_then(Value::as_i64).unwrap_or(-1);
        let content = value_as_string(obj.get("content")).unwrap_or_default();
        eprintln!(
            "[sidecar:search] embedding $id={} distance={:.4} unit_kind={} content_preview={}",
            id, dist, unit_kind,
            &content[..content.len().min(80)]
        );
        if dist <= threshold && id >= 0 {
            relevant.push((dist, id, unit_kind.clone()));
        } else {
            eprintln!(
                "[sidecar:search] FILTERED OUT $id={} distance={:.4} unit_kind={}",
                id, dist, unit_kind
            );
        }
    }

    // Sort ascending — best match first.
    relevant.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    eprintln!(
        "[sidecar:search] {}/{} caption embeddings passed distance threshold {:.2}",
        relevant.len(),
        embeddings_raw.iter().filter(|e| {
            e.as_object()
                .and_then(|o| o.get("unit_kind"))
                .and_then(Value::as_str)
                .map(|u| u != "file_path" && u != "video_index_state")
                .unwrap_or(false)
        }).count(),
        threshold
    );

    if relevant.is_empty() {
        return Ok(json!({ "query": query, "results": [] }));
    }

    // Fetch assets for matched embedding IDs (1:1, preserves order).
    let ids: Vec<i64> = relevant.iter().map(|(_, id, _)| *id).collect();
    let assets_value = store.get_assets_for_embedding_ids(ids).await?;
    let assets_arr = assets_value.as_array().cloned().unwrap_or_default();

    // Deduplicate by content_hash, keeping first occurrence (lowest distance).
    let mut seen: HashSet<String> = HashSet::new();
    let mut results: Vec<Value> = Vec::new();

    for ((dist, id, unit_kind), asset) in relevant.iter().zip(assets_arr.iter()) {
        if asset.is_null() { continue; }
        let Some(map) = asset.as_object() else { continue };
        let Some(hash) = value_as_string(map.get("content_hash")) else { continue };
        if !seen.insert(hash.clone()) { continue; }
        let Some(path) = value_as_string(map.get("path")) else { continue };
        let kind = value_as_string(map.get("kind")).unwrap_or_else(|| "file".to_string());

        // Find the matching embedding's content text
        let content_preview = embeddings_raw.iter()
            .find(|e| e.get("$id").and_then(Value::as_i64) == Some(*id))
            .and_then(|e| e.get("content").and_then(Value::as_str))
            .map(|s| s.chars().take(300).collect::<String>());

        let mut result = json!({
            "label": kind,
            "path": path,
            "score": (1.0 - dist).max(0.0),
            "match_kind": unit_kind,
            "content": content_preview,
        });

        if kind == "file" {
            if let Ok(text) = fs::read_to_string(&path) {
                result["content"] = Value::String(text);
            }
        }

        if kind == "video" {
            if let Some(thumbnail_path) = resolve_thumbnail_path(&hash, &path) {
                result["thumbnail_url"] = Value::String(format!(
                    "localimg://preview?path={}",
                    percent_encode(&thumbnail_path)
                ));
            }
        }

        results.push(result);
    }

    results.sort_by(|a, b| {
        let sa = a.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        let sb = b.get("score").and_then(Value::as_f64).unwrap_or(0.0);
        sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(json!({
        "query": query,
        "results": results,
    }))
}

pub fn handle_query(request: &JsonRpcRequest) -> JsonRpcResponse {
    let parsed: SearchQueryParams = match parse_params(request) {
        Ok(parsed) => parsed,
        Err(error_response) => return error_response,
    };

    let started = Instant::now();

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(error) => {
            return err_response(
                request.id.clone(),
                -32603,
                "Search query failed",
                Some(json!({ "reason": format!("failed to init runtime: {}", error) })),
            )
        }
    };

    match runtime.block_on(rust_helix_search_query(&parsed.q)) {
        Ok(result) => {
            let count = result
                .get("results")
                .and_then(Value::as_array)
                .map(|items| items.len())
                .unwrap_or(0);
            eprintln!(
                "[sidecar:search] completed in {}ms with {} results",
                started.elapsed().as_millis(),
                count
            );
            ok_response(request.id.clone(), result)
        }
        Err(message) => {
            eprintln!(
                "[sidecar:search] failed in {}ms: {}",
                started.elapsed().as_millis(),
                message
            );
            err_response(
                request.id.clone(),
                -32603,
                "Search query failed",
                Some(json!({ "reason": message })),
            )
        }
    }
}