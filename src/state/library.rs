//! Backing the library structure up, and putting it back.
//!
//! Redis holds what no audio file records: which videos the library knows, the
//! order they play in, and the playlists they belong to. The audio itself can
//! always be fetched again — `restore_tracks_if_missing` rebuilds the track
//! metadata from the cache directory alone — but the order and the playlists
//! exist in exactly one place, and a wiped Redis loses them for good. An export
//! is the copy that survives that.
//!
//! An import is deliberately additive. Playlists are matched by name and
//! appended to rather than replaced, and the restored order keeps whatever this
//! server has that the document never mentioned. Ids naming videos that are not
//! here are kept as placeholders: the listing skips them, and downloading one
//! later drops it straight into the gap it was holding.

use super::model::{ImportOutcome, LibraryExport, PlaylistExport, WriteOutcome};
use super::url::is_video_id;
use super::{AppState, now_f64, playlist_key};
use std::collections::{HashMap, HashSet};

/// Document format version. Bumped only for a change an older server could not
/// read correctly; an import refuses anything else rather than guessing.
pub const LIBRARY_EXPORT_VERSION: u32 = 1;

impl AppState {
    /// Everything an import needs to rebuild this library's structure.
    pub async fn export_library(&self) -> LibraryExport {
        let (tracks, playlists, playback_mode, active_playlist) = tokio::join!(
            self.list_tracks(),
            self.playlists(),
            self.playback_mode(),
            self.active_playlist(),
        );

        // Every playlist's membership in one round-trip, as playlists_json
        // reads their counts.
        let mut pipe = redis::pipe();
        for playlist in &playlists {
            pipe.lrange(playlist_key(&playlist.id), 0, -1);
        }
        let mut conn = self.redis.clone();
        let memberships: Vec<Vec<String>> = pipe.query_async(&mut conn).await.unwrap_or_else(|e| {
            tracing::warn!("Redis error reading playlist tracks: {e}");
            vec![Vec::new(); playlists.len()]
        });
        let exported = playlists
            .into_iter()
            .zip(memberships)
            .map(|(playlist, track_ids)| PlaylistExport {
                playlist,
                track_ids,
            })
            .collect();

        LibraryExport {
            version: LIBRARY_EXPORT_VERSION,
            exported_at: now_f64(),
            playback_mode,
            active_playlist,
            tracks,
            playlists: exported,
        }
    }

    /// Apply an exported document to this server. The caller has already
    /// checked the version.
    pub async fn import_library(&self, doc: &LibraryExport) -> ImportOutcome {
        let playlist_ids = self.restore_playlists(&doc.playlists).await;

        let order: Vec<String> = doc
            .tracks
            .iter()
            .map(|t| t.id.clone())
            .filter(|id| is_video_id(id))
            .collect();
        let restored = self.restore_order(&order).await;

        // The scope is named by an ID only the exporting server knew, so it is
        // re-resolved through the playlists just matched. One that resolves to
        // nothing reverts to the whole library rather than to a stale ID.
        let scope = doc
            .active_playlist
            .as_ref()
            .and_then(|id| playlist_ids.get(id));
        self.set_active_playlist(scope.map(String::as_str)).await;
        self.broadcast_active_playlist().await;

        if self.set_playback_mode(&doc.playback_mode).await {
            self.broadcast_playback_mode(&doc.playback_mode);
        }

        // One HMGET for the whole list: what is already here needs no download.
        let registered = self.fetch_tracks_for(order.iter()).await;
        let missing_ids = order
            .iter()
            .filter(|id| !registered.contains_key(id.as_str()))
            .cloned()
            .collect();

        self.broadcast_tracks();
        self.broadcast_playlists().await;

        ImportOutcome {
            playlists: playlist_ids.len(),
            tracks: restored,
            missing_ids,
        }
    }

    /// Recreate the exported playlists here, matching by name so re-importing
    /// into a server that already has them appends rather than duplicating —
    /// the rule a YouTube playlist import already follows.
    ///
    /// Returns the mapping from exported playlist ID to local one, which is
    /// what resolves the exported selection scope.
    async fn restore_playlists(&self, exported: &[PlaylistExport]) -> HashMap<String, String> {
        let mut by_name: HashMap<String, String> = self
            .playlists()
            .await
            .into_iter()
            .map(|p| (p.name, p.id))
            .collect();

        let mut mapping = HashMap::new();
        for entry in exported {
            let name = &entry.playlist.name;
            let local_id = match by_name.get(name) {
                Some(id) => id.clone(),
                None => {
                    let Some(created) = self.create_playlist(name).await else {
                        // A name this server rejects (empty, too long) is the
                        // one playlist that cannot be restored; the rest still
                        // can, so the import goes on without it.
                        tracing::warn!("Library import: cannot create the playlist '{name}'");
                        continue;
                    };
                    by_name.insert(created.name, created.id.clone());
                    created.id
                }
            };

            let track_ids: Vec<&str> = entry
                .track_ids
                .iter()
                .filter(|id| is_video_id(id))
                .map(String::as_str)
                .collect();
            let (added, outcome) = self.add_playlist_tracks(&local_id, &track_ids).await;
            match outcome {
                WriteOutcome::Written => {}
                // Deleted from under the import. The rest of the document is
                // still worth restoring, so only this playlist is given up on.
                WriteOutcome::Gone => {
                    tracing::warn!(
                        "Library import: '{name}' was deleted while it was being filled"
                    );
                }
                WriteOutcome::Failed => tracing::warn!(
                    "Library import: added {added} of {} track(s) to '{name}'",
                    track_ids.len()
                ),
            }
            mapping.insert(entry.playlist.id.clone(), local_id);
        }
        mapping
    }

    /// Replace the library order with the imported one, keeping anything this
    /// server has that the document does not mention. Returns how many ids the
    /// restored order names.
    async fn restore_order(&self, imported: &[String]) -> usize {
        self.rewrite_track_order(|current| merged_order(imported, current))
            .await
            .unwrap_or(0)
    }
}

/// The imported order, followed by the ids this server already had that it does
/// not mention. Repeats are dropped: the document decides where an id goes, and
/// a list holding one twice would have `reorder_track` move an arbitrary copy.
fn merged_order(imported: &[String], current: Vec<String>) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut order: Vec<String> = Vec::with_capacity(imported.len() + current.len());
    for id in imported.iter().chain(current.iter()) {
        if seen.insert(id.as_str()) {
            order.push(id.clone());
        }
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn an_imported_order_leads_and_keeps_what_it_never_mentioned() {
        let order = merged_order(&ids(&["a", "b", "c"]), ids(&["z", "b", "y"]));
        // The document decides the order of what it names; local-only ids
        // follow in the order they already had
        assert_eq!(order, ids(&["a", "b", "c", "z", "y"]));
    }

    #[test]
    fn no_id_is_listed_twice() {
        // A malformed document repeating an id must not leave two copies in the
        // list, which would make a reorder move an arbitrary one of them
        let order = merged_order(&ids(&["a", "a", "b"]), ids(&["b", "b"]));
        assert_eq!(order, ids(&["a", "b"]));
    }
}
