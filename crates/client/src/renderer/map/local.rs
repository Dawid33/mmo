//! `MapSource::Local`'s fetcher (native single-player): [`MAP_LOCAL_WORKERS`] threads of its own call
//! the blocking `TileSource::tile` (a store read and a 20–45 ms build) and `refresh_versions`, answering
//! over a channel the fetch loop polls — never on Bevy's compute pool, whose few threads mesh the near
//! terrain.

use std::sync::Arc;

use crossbeam::channel::{Receiver, Sender, unbounded};
use game::map_tile::{MapTile, MapTileKey};

use super::store::Answer;

/// Threads building tiles for a local source.
pub const MAP_LOCAL_WORKERS: usize = 2;

enum Job {
    Tile(MapTileKey, Option<u64>),
    Refresh,
}

pub(super) struct LocalFetcher {
    jobs: Sender<Job>,
    replies: Receiver<(MapTileKey, Answer)>,
}

impl LocalFetcher {
    pub(super) fn start(source: Arc<map_tiles::TileSource>) -> LocalFetcher {
        let (jobs, rx) = unbounded::<Job>();
        let (reply, replies) = unbounded();
        for i in 0..MAP_LOCAL_WORKERS {
            let (rx, reply, source) = (rx.clone(), reply.clone(), source.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("map-local-{i}"))
                .spawn(move || {
                    for job in rx {
                        match job {
                            Job::Tile(key, have) => {
                                let answer = match source.tile(key, have) {
                                    Ok(map_tiles::Fetched::Unchanged) => Answer::Unchanged,
                                    Ok(map_tiles::Fetched::Tile { etag, bytes }) => {
                                        match MapTile::decode(&bytes) {
                                            Ok(tile) => Answer::Tile { etag, tile },
                                            Err(e) => Answer::Failed(e.to_string()),
                                        }
                                    }
                                    Err(e) => Answer::Failed(e.to_string()),
                                };
                                if reply.send((key, answer)).is_err() {
                                    return;
                                }
                            }
                            Job::Refresh => {
                                if let Err(e) = source.refresh_versions() {
                                    bevy::log::warn!("[map] re-reading terrain versions: {e}");
                                }
                            }
                        }
                    }
                });
            if let Err(e) = spawned {
                bevy::log::error!("[map] map-local-{i}: {e}");
            }
        }
        LocalFetcher { jobs, replies }
    }

    pub(super) fn request(&self, key: MapTileKey, have: Option<u64>) {
        let _ = self.jobs.send(Job::Tile(key, have));
    }

    pub(super) fn refresh(&self) {
        let _ = self.jobs.send(Job::Refresh);
    }

    pub(super) fn answers(&self) -> Vec<(MapTileKey, Answer)> {
        self.replies.try_iter().collect()
    }
}
