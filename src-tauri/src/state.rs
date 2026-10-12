use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use innertube::InnerTube;
use tauri::async_runtime::Mutex;
use tokio::sync::Mutex as TokioMutex;

use crate::audio::AudioCache;
use crate::db::Db;
use crate::proxy::SharedTable;

/// A resolved googlevideo URL plus the User-Agent that must accompany it.
///
/// The UA is not decoration. googlevideo meters reads per InnerTube client and
/// checks the media request's User-Agent against the client that issued the URL,
/// so a URL resolved through VISIONOS has to be fetched as VISIONOS or it lands
/// back in the throttled bucket and the remux is silently truncated.
#[derive(Clone, Debug)]
pub struct Upstream {
    pub url: String,
    pub user_agent: String,
}

pub struct AppState {
    pub inner_tube: Arc<Mutex<Option<InnerTube>>>,
    pub db: Arc<OnceLock<Arc<TokioMutex<Db>>>>,
    pub proxy_table: SharedTable,
    pub proxy_addr: std::net::SocketAddr,
    /// videoId → instant of last failed `video_info` call.
    pub blacklist: Arc<TokioMutex<HashMap<String, Instant>>>,
    /// Instant the most recent IP-level throttle was observed. YouTube throttles
    /// by source IP after enough anonymous InnerTube requests in a window, and a
    /// fresh visitor_data from the same IP stays throttled until the window passes —
    /// so when this fires we stop issuing *any* InnerTube traffic until it clears.
    pub throttle_until: Arc<TokioMutex<Option<Instant>>>,
    /// Consecutive bot-check refusals since the last success. Drives the
    /// escalating cooldown, and is reset by any successful resolve so a stale
    /// high count cannot keep penalising a long session.
    pub bot_check_strikes: Arc<TokioMutex<u32>>,
    /// Offscreen webview used to answer InnerTube's bot check anonymously.
    ///
    /// A `OnceLock` rather than a plain field because it needs an `AppHandle`,
    /// which does not exist until `setup` runs; `AppState::new` happens first.
    pub bridge: Arc<std::sync::OnceLock<crate::player_bridge::PlayerBridge<tauri::Wry>>>,
    /// Loopback slot a player bridge report is delivered into.
    ///
    /// Lives in the state rather than the bridge because the proxy that receives
    /// the report starts before the bridge window exists.
    pub bridge_inbox: crate::player_bridge::BridgeInbox,
    /// Progress of the server-side audio extraction, keyed by video id.
    pub audio: AudioCache,
    /// Progress of the server-side HD remux, keyed by video id.
    pub mux: crate::mux::MuxCache,
    /// videoId → raw upstream URL of the best progressive rendition, plus the UA
    /// that issued it. Kept here (never sent to the webview) so `audio_source`
    /// and the remux can start without spending another InnerTube request, which
    /// we are throttled on. The UA is needed because the audio now streams
    /// through the relay like the video does.
    pub progressive_upstream: Arc<TokioMutex<HashMap<String, Upstream>>>,
    /// A video id -> audio-only rendition that can be played as-is.
    ///
    /// Populated only when the winning client had no progressive rendition to
    /// extract from - VISIONOS never does. The audio is already a standalone
    /// track in that case, so ffmpeg has nothing to do and would only add a
    /// failure mode.
    pub audio_direct: Arc<TokioMutex<HashMap<String, Upstream>>>,
    /// videoId → raw upstream URL of the highest adaptive video-only rendition.
    /// Same reasoning as above: kept Rust-side for `video_mux_source`.
    pub adaptive_upstream: Arc<TokioMutex<HashMap<String, Upstream>>>,
    /// `videoId#itag` → raw upstream URL of that specific adaptive rendition, so
    /// picking a resolution in the player actually remuxes *that* one.
    pub adaptive_by_itag: Arc<TokioMutex<HashMap<String, Upstream>>>,
    /// videoId → length in seconds, used to detect a remux whose source download
    /// was cut short. ffmpeg exits `0` on a truncated input, so without this we
    /// would cache and serve a video that ends early with no error.
    pub video_duration: Arc<TokioMutex<HashMap<String, u32>>>,
    /// videoId → a resolution and the instant it was fetched.
    ///
    /// One `video_info_full` costs two `player` round-trips plus a QuickJS
    /// decipher pass, and every quality switch asks for the same data again, so
    /// a repeat lookup is served from here instead. Entries expire well inside
    /// googlevideo's URL lifetime, and are dropped outright when the InnerTube
    /// session is reset or a throttle fires, so a cached URL can never outlive
    /// the session that authorised it.
    pub resolved: Arc<TokioMutex<HashMap<String, (Instant, innertube::dto::VideoInfoFull)>>>,
    /// videoId → in-flight `video_info` future, so concurrent callers (hover
    /// pre-resolve racing the click, or a quality switch mid-poll) await the
    /// same request instead of firing duplicate `player` round-trips we are
    /// throttled on. The slot stays `None` until the fetch finishes, then holds
    /// the outcome for whoever waits on it.
    pub resolve_in_flight:
        Arc<TokioMutex<HashMap<String, Arc<TokioMutex<Option<Result<innertube::dto::VideoInfoFull, String>>>>>>>,
    /// videoId → the video's Watch Next list. One `/next` fetch per video per
    /// hour; repeat visits to the same video must not spend another request.
    pub related: Arc<TokioMutex<HashMap<String, (Instant, innertube::dto::RelatedVideosDto)>>>,
    /// videoId → in-flight Watch Next future, so a repeat navigation coalesces
    /// onto an in-flight fetch instead of firing a second `/next`.
    pub related_in_flight:
        Arc<TokioMutex<HashMap<String, Arc<TokioMutex<Option<Result<innertube::dto::RelatedVideosDto, String>>>>>>>,
    /// channelId → that channel's latest uploads. The subscriptions feed asks
    /// for several channels at once, so each channel's first page is cached
    /// (and coalesced) here rather than re-fetched on every visit.
    pub channel_videos:
        Arc<TokioMutex<HashMap<String, (Instant, innertube::dto::ChannelVideosDto)>>>,
    /// channelId → in-flight channel-uploads future, coalescing repeat asks so
    /// opening the feed twice cannot fire two `/browse` calls for one channel.
    pub channel_videos_in_flight:
        Arc<TokioMutex<HashMap<String, Arc<TokioMutex<Option<Result<innertube::dto::ChannelVideosDto, String>>>>>>>,
    /// Live feed-browsing sessions, keyed by the opaque cursor id the UI was
    /// handed. Each holds the per-channel uploads gathered so far so a scroll
    /// does not have to re-merge the whole subscription list every batch.
    pub feed_sessions: Arc<TokioMutex<HashMap<String, crate::commands::FeedSession>>>,
    /// videoId → cached SponsorBlock skip segments. Kept for a day so repeat
    /// visits do not re-ask; a stale entry is revalidated against its ETag.
    pub sponsor_segments: Arc<TokioMutex<HashMap<String, crate::sponsorblock::CachedSegments>>>,
    /// videoId → currently fetching (a fresh fetch happens at most once per
    /// video per visit, and only when segments are actually wanted).
    pub sponsor_in_flight: Arc<TokioMutex<HashSet<String>>>,
    /// (video id, language) → WebVTT body, served by the proxy's `/captions`
    /// route for the player's native `<track>`. Warm once per language per
    /// visit by `captions_fetch`; never touches YouTube again.
    pub captions: crate::proxy::SharedCaptions,
}

/// URL the player bridge page POSTs its `/player` result to.
pub fn bridge_report_url(addr: std::net::SocketAddr) -> String {
format!("http://{addr}{}", crate::proxy::BRIDGE_PATH)
}

impl AppState {
    /// Build a fresh app state. Spawns the stream proxy so we have its
    /// table + listening address before any Tauri command runs.
    pub async fn new() -> Self {
        // The inbox is created before the proxy so the proxy already has somewhere
        // to deliver reports; the window that fills it only appears later, once
        // Tauri has an `AppHandle`.
        let bridge_inbox = crate::player_bridge::BridgeInbox::new();
        let captions: crate::proxy::SharedCaptions = Arc::new(TokioMutex::new(HashMap::new()));
        let handle = crate::proxy::spawn_with_captions(bridge_inbox.sink(), captions.clone())
            .await
            .expect("bind loopback proxy");
        tracing::info!("stream proxy listening on http://{}", handle.addr);
        Self {
            inner_tube: Arc::new(Mutex::new(None)),
            db: Arc::new(OnceLock::new()),
            proxy_table: handle.table,
            proxy_addr: handle.addr,
            blacklist: Arc::new(TokioMutex::new(HashMap::new())),
            throttle_until: Arc::new(TokioMutex::new(None)),
        bot_check_strikes: Arc::new(TokioMutex::new(0)),
        bridge: Arc::new(OnceLock::new()),
            bridge_inbox,
            audio: AudioCache::new(Self::audio_dir()),
            mux: crate::mux::MuxCache::new(Self::mux_dir()),
            progressive_upstream: Arc::new(TokioMutex::new(HashMap::new())),
            audio_direct: Arc::new(TokioMutex::new(HashMap::new())),
            adaptive_upstream: Arc::new(TokioMutex::new(HashMap::new())),
            adaptive_by_itag: Arc::new(TokioMutex::new(HashMap::new())),
            video_duration: Arc::new(TokioMutex::new(HashMap::new())),
            resolved: Arc::new(TokioMutex::new(HashMap::new())),
            resolve_in_flight: Arc::new(TokioMutex::new(HashMap::new())),
            related: Arc::new(TokioMutex::new(HashMap::new())),
            related_in_flight: Arc::new(TokioMutex::new(HashMap::new())),
            channel_videos: Arc::new(TokioMutex::new(HashMap::new())),
            channel_videos_in_flight: Arc::new(TokioMutex::new(HashMap::new())),
            feed_sessions: Arc::new(TokioMutex::new(HashMap::new())),
            sponsor_segments: Arc::new(TokioMutex::new(HashMap::new())),
            sponsor_in_flight: Arc::new(TokioMutex::new(HashSet::new())),
            captions,
        }
    }

    pub fn data_dir() -> PathBuf {
        let base = std::env::var_os("SAVVY_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("APPDATA")
                    .map(PathBuf::from)
                    .map(|p| p.join("Savvy"))
                    .unwrap_or_else(|| PathBuf::from("."))
            });
        std::fs::create_dir_all(&base).ok();
        base
    }

    /// Where extracted audio tracks are cached between runs.
    pub fn audio_dir() -> PathBuf {
        Self::data_dir().join("audio")
    }

    /// Where remuxed HD playback files are cached between runs.
    pub fn mux_dir() -> PathBuf {
        Self::data_dir().join("mux")
    }

    pub fn db_path() -> PathBuf {
        Self::data_dir().join("savvy.db")
    }

    /// Delete everything the current session recreated — the remuxed HD files
    /// and extracted audio tracks (hundreds of MB each) — so closing the app
    /// returns the disk. Best-effort on purpose: files ffmpeg still holds open
    /// at shutdown are skipped here and swept again at the next startup.
    pub fn clear_transient_caches() {
        for dir in [Self::mux_dir(), Self::audio_dir()] {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }

    pub fn db(&self) -> Arc<TokioMutex<Db>> {
        self.db
            .get_or_init(|| {
                let path = Self::db_path();
                Arc::new(TokioMutex::new(Db::open(&path).expect("open db")))
            })
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `clear_transient_caches` reads the data dir from the environment, which
    /// other tests could race on; serialise any env-mutating test.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn transient_caches_are_emptied_but_the_db_is_kept() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("savvy-state-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("SAVVY_DATA_DIR", &dir);

        std::fs::create_dir_all(AppState::mux_dir()).unwrap();
        std::fs::create_dir_all(AppState::audio_dir()).unwrap();
        std::fs::write(AppState::mux_dir().join("v1.mp4"), "x").unwrap();
        std::fs::write(AppState::mux_dir().join("v2.mp4.part"), "y").unwrap();
        std::fs::write(AppState::audio_dir().join("v1.m4a"), "z").unwrap();
        std::fs::write(AppState::db_path(), "db").unwrap();

        AppState::clear_transient_caches();

        assert!(!AppState::mux_dir().join("v1.mp4").exists(), "completed remux gone");
        assert!(!AppState::mux_dir().join("v2.mp4.part").exists(), "stale .part gone");
        assert!(!AppState::audio_dir().join("v1.m4a").exists(), "audio gone");
        assert!(AppState::db_path().exists(), "history/db must survive a session purge");

        std::env::remove_var("SAVVY_DATA_DIR");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
