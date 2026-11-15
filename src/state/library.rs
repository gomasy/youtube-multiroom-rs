//! Exporting the library structure — tracks, their order, the playlists — and
//! importing it back. Only Redis holds it, so an export is the copy that
//! survives a wipe.
//!
//! An import is additive: playlists are matched by name and appended to, and
//! ids naming videos that are not here stay in the order as placeholders.

use super::model::{ImportOutcome, LibraryExport, PlaylistExport, WriteOutcome};
use super::url::is_video_id;
use super::{AppState, now_f64};
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

        let memberships = self.playlist_memberships(&playlists).await;
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

        // The exported scope ID is re-resolved through the playlists just
        // matched; one that resolves to nothing reverts to the whole library.
        let scope = doc
            .active_playlist
            .as_ref()
            .and_then(|id| playlist_ids.get(id));
        self.set_active_playlist(scope.map(String::as_str)).await;
        self.broadcast_active_playlist().await;

        if self.set_playback_mode(&doc.playback_mode).await {
            self.broadcast_playback_mode(&doc.playback_mode);
        }

        // What is already here needs no download.
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

    /// Recreate the exported playlists, matching by name so a re-import
    /// appends rather than duplicates. Returns the mapping from exported
    /// playlist ID to local one.
    async fn restore_playlists(&self, exported: &[PlaylistExport]) -> HashMap<String, String> {
        let mut mapping = HashMap::new();
        for entry in exported {
            let name = &entry.playlist.name;
            let local_id = match self.find_or_create_playlist(name).await {
                Ok((playlist, _)) => {
                    // Keep the playlist syncable here.
                    if let Some(source) = &entry.playlist.source {
                        self.link_playlist_source(&playlist.id, source).await;
                    }
                    playlist.id
                }
                Err(e) => {
                    // Skip it; the other playlists can still be restored.
                    tracing::warn!("Library import: cannot create the playlist '{name}': {e:?}");
                    continue;
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
                // Deleted from under the import; only this playlist is given
                // up on.
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
    use super::super::ids;
    use super::*;

    #[test]
    fn an_imported_order_leads_and_keeps_what_it_never_mentioned() {
        let order = merged_order(&ids(&["a", "b", "c"]), ids(&["z", "b", "y"]));
        assert_eq!(order, ids(&["a", "b", "c", "z", "y"]));
    }

    #[test]
    fn no_id_is_listed_twice() {
        let order = merged_order(&ids(&["a", "a", "b"]), ids(&["b", "b"]));
        assert_eq!(order, ids(&["a", "b"]));
    }
}
