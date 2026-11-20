//! `MapSource::Http`'s fetcher (plan Task 11): `GET {base}/v1/map/tiles/{level}/{x}/{z}` with
//! `If-None-Match`, answers decoded and handed back over a channel the fetch loop polls. Natively one
//! `map-http` thread (a current-thread runtime, every request a task of it) that also owns the disk
//! cache (`disk.rs`): a cached tile is answered at once and its etag sent as `If-None-Match`, a fresh
//! tile is written to it. In the browser each request is a `spawn_local` future and the browser's own
//! HTTP cache does the disk's work (the route sends `ETag` and `Cache-Control`).

use crossbeam::channel::{Receiver, Sender, unbounded};
use game::map_tile::{MapTile, MapTileKey};

use super::store::Answer;

/// One answer of the route: `Ok(None)` = 304 (the etag sent is current), `Ok(Some((etag, bytes)))` = 200.
async fn fetch(
    client: &reqwest::Client,
    base: &str,
    key: MapTileKey,
    have: Option<u64>,
) -> Result<Option<(u64, Vec<u8>)>, String> {
    let url = format!(
        "{}/v1/map/tiles/{}/{}/{}",
        base.trim_end_matches('/'),
        key.level,
        key.x,
        key.z
    );
    let mut req = client.get(&url);
    if let Some(etag) = have {
        req = req.header("If-None-Match", format!("\"{etag:016x}\""));
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    match resp.status().as_u16() {
        304 => Ok(None),
        200 => {
            let etag = resp
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| {
                    u64::from_str_radix(v.trim().trim_start_matches("W/").trim_matches('"'), 16)
                        .ok()
                })
                .ok_or("no ETag")?;
            let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
            Ok(Some((etag, bytes.to_vec())))
        }
        code => Err(format!("{url}: HTTP {code}")),
    }
}

fn decoded(etag: u64, bytes: &[u8]) -> Answer {
    match MapTile::decode(bytes) {
        Ok(tile) => Answer::Tile { etag, tile },
        Err(e) => Answer::Failed(e.to_string()),
    }
}

fn answer(result: Result<Option<(u64, Vec<u8>)>, String>) -> Answer {
    match result {
        Ok(None) => Answer::Unchanged,
        Ok(Some((etag, bytes))) => decoded(etag, &bytes),
        Err(e) => Answer::Failed(e),
    }
}

pub(super) struct HttpFetcher {
    #[cfg(target_arch = "wasm32")]
    base: String,
    replies: Receiver<(MapTileKey, Answer)>,
    #[cfg(not(target_arch = "wasm32"))]
    jobs: tokio::sync::mpsc::UnboundedSender<(MapTileKey, Option<u64>)>,
    #[cfg(target_arch = "wasm32")]
    client: reqwest::Client,
    #[cfg(target_arch = "wasm32")]
    reply: Sender<(MapTileKey, Answer)>,
}

impl HttpFetcher {
    /// A fetcher of `base`'s tiles; natively with the disk cache of world `world`.
    pub(super) fn start(base: String, world: u64) -> HttpFetcher {
        let (reply, replies) = unbounded();
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (jobs, rx) = tokio::sync::mpsc::unbounded_channel();
            let disk = crate::player_name::data_dir().and_then(|dir| {
                super::disk::DiskCache::open(
                    &dir.join("map"),
                    &format!("{world:016x}"),
                    super::disk::MAP_DISK_CACHE_BYTES,
                )
                .map_err(|e| bevy::log::warn!("[map] no disk cache: {e}"))
                .ok()
            });
            let thread_base = base.clone();
            let spawned = std::thread::Builder::new()
                .name("map-http".into())
                .spawn(move || serve(thread_base, rx, reply, disk));

            if let Err(e) = spawned {
                bevy::log::error!("[map] no map-http thread: {e}");
            }
            let _ = base;
            HttpFetcher { replies, jobs }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = world;
            HttpFetcher {
                base,
                replies,
                client: reqwest::Client::new(),
                reply,
            }
        }
    }

    /// Ask for `key` (`have`: the etag shown); the answer comes back through [`Self::answers`].
    pub(super) fn request(&self, key: MapTileKey, have: Option<u64>) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = self.jobs.send((key, have));
        }
        #[cfg(target_arch = "wasm32")]
        {
            let (client, base, reply) =
                (self.client.clone(), self.base.clone(), self.reply.clone());
            wasm_bindgen_futures::spawn_local(async move {
                let _ = reply.send((key, answer(fetch(&client, &base, key, have).await)));
            });
        }
    }

    /// Every answer that arrived.
    pub(super) fn answers(&self) -> Vec<(MapTileKey, Answer)> {
        self.replies.try_iter().collect()
    }
}

/// The `map-http` thread: each request a task of one current-thread runtime; the disk cache first.
#[cfg(not(target_arch = "wasm32"))]
fn serve(
    base: String,
    mut jobs: tokio::sync::mpsc::UnboundedReceiver<(MapTileKey, Option<u64>)>,
    reply: Sender<(MapTileKey, Answer)>,
    disk: Option<super::disk::DiskCache>,
) {
    use std::sync::{Arc, Mutex};
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => return bevy::log::error!("[map] no runtime for map-http: {e}"),
    };
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_default();
    let disk = disk.map(|d| Arc::new(Mutex::new(d)));
    runtime.block_on(async move {
        while let Some((key, have)) = jobs.recv().await {
            let at = (key.level, key.x, key.z);
            // A tile on disk is shown at once and revalidated by its etag.
            let cached = if have.is_none() {
                disk.as_ref().and_then(|d| d.lock().ok()?.get(at))
            } else {
                None
            };
            let have = have.or(cached.as_ref().map(|(etag, _)| *etag));
            if let Some((etag, bytes)) = &cached {
                let _ = reply.send((key, decoded(*etag, bytes)));
            }
            let (client, base, reply, disk) =
                (client.clone(), base.clone(), reply.clone(), disk.clone());
            tokio::spawn(async move {
                let result = fetch(&client, &base, key, have).await;
                if let (Ok(Some((etag, bytes))), Some(disk)) = (&result, &disk) {
                    if let Ok(mut d) = disk.lock() {
                        if let Err(e) = d.put(at, *etag, bytes) {
                            bevy::log::debug!("[map] disk cache write: {e}");
                        }
                    }
                }
                // A revalidated disk tile that is current needs no second answer.
                let answer = match (result, cached.is_some()) {
                    (Ok(None), true) => return,
                    (r, _) => answer(r),
                };
                let _ = reply.send((key, answer));
            });
        }
    });
}
