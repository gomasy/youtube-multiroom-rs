//! Searching YouTube. Results are not registered anywhere; picking one is what
//! triggers a download over the WebSocket.

use super::{AppError, AppResult};
use crate::state::{AudioTrack, run_yt_dlp};
use axum::extract::Query;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
pub struct SearchQuery {
    q: String,
    limit: Option<usize>,
}

/// GET /api/search?q=...&limit=8
///
/// Searches YouTube via yt-dlp ytsearch and returns lightweight metadata in
/// the same shape as /api/tracks. Uses --flat-playlist to skip resolving
/// individual video pages, keeping response time to a few seconds.
pub async fn search_youtube(Query(query): Query<SearchQuery>) -> AppResult<Json<Value>> {
    let q = query.q.trim();
    if q.is_empty() {
        return Err(AppError::bad_request("Search query is empty"));
    }
    let limit = query.limit.unwrap_or(8).clamp(1, 20);

    let target = format!("ytsearch{limit}:{q}");
    let stdout = run_yt_dlp(
        &["--dump-json", "--flat-playlist", &target],
        std::time::Duration::from_secs(30),
    )
    .await
    .map_err(|e| AppError::internal(format!("Search failed: {e}")))?;

    // Output is one JSON object per video per line
    let results: Vec<AudioTrack> = String::from_utf8_lossy(&stdout)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|v| search_entry(&v))
        .collect();

    Ok(Json(json!({ "results": results })))
}

/// Convert a yt-dlp flat-playlist entry into an AudioTrack for search results.
/// Using AudioTrack ensures wire-format compatibility with /api/tracks
/// (file_path is serde(skip) so it's never exposed).
fn search_entry(v: &Value) -> Option<AudioTrack> {
    let id = v["id"].as_str()?;
    Some(AudioTrack {
        id: id.to_string(),
        title: v["title"].as_str().unwrap_or(id).to_string(),
        // Flat entries have inconsistent thumbnail formats; use a known URL pattern
        thumbnail: format!("https://i.ytimg.com/vi/{id}/mqdefault.jpg"),
        duration: v["duration"].as_f64().unwrap_or(0.0) as u64,
        channel: AudioTrack::extract_channel(v),
        is_live: v["live_status"].as_str() == Some("is_live"),
        created_at: 0.0,
        file_path: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::search_entry;
    use serde_json::json;

    #[test]
    fn search_entry_maps_flat_playlist_fields() {
        let v = json!({
            "id": "dQw4w9WgXcQ",
            "title": "Song",
            "duration": 212.0,
            "channel": "Ch",
            "live_status": "not_live",
        });
        let entry = serde_json::to_value(search_entry(&v).unwrap()).unwrap();
        assert_eq!(entry["id"], "dQw4w9WgXcQ");
        assert_eq!(entry["title"], "Song");
        assert_eq!(entry["duration"], 212);
        assert_eq!(entry["channel"], "Ch");
        assert_eq!(entry["is_live"], false);
        assert_eq!(
            entry["thumbnail"],
            "https://i.ytimg.com/vi/dQw4w9WgXcQ/mqdefault.jpg"
        );
        // Internal file_path must not appear in wire format
        assert!(entry.get("file_path").is_none());
    }

    #[test]
    fn search_entry_fills_missing_fields() {
        // No duration (e.g., live); uploader instead of channel
        let v = json!({
            "id": "dQw4w9WgXcQ",
            "uploader": "Up",
            "live_status": "is_live",
        });
        let entry = search_entry(&v).unwrap();
        assert_eq!(entry.title, "dQw4w9WgXcQ");
        assert_eq!(entry.duration, 0);
        assert_eq!(entry.channel, "Up");
        assert!(entry.is_live);

        // Entries without id are discarded
        assert!(search_entry(&json!({ "title": "x" })).is_none());
    }
}
