//! Tauri commands exposed to the webview.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use innertube::dto::{
    CaptionTrackDto, ChannelVideosDto, SearchResultsDto, StreamUrlsDto, VideoInfoDto,
};
use innertube::InnerTube;
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::sync::Mutex as TokioMutex;

use crate::audio::AudioStatus;
use crate::db::{CategoryRow, HistoryRow, PlaylistItemRow, PlaylistRow, SavedVideoRow, SubscriptionRow};
use crate::mux::MuxStatus;
use crate::newpipe::{self, ChannelRef};
use crate::state::{AppState, Upstream};

/// Container for the remuxed HD file. Codecs are deliberately omitted: the
/// track is whatever the source rendition carried, and the browser sniffs the
/// real type from the bytes.
const MUX_MIME: &str = "video/mp4";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub version: &'static str,
    pub innertube_ready: bool,
    pub proxy_addr: Option<String>,
}

/// A selectable rendition, carrying a proxy token rather than the upstream
/// googlevideo URL.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamVariantDto {
    pub itag: u32,
    pub quality_label: Option<String>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub bitrate: u64,
    pub is_hdr: bool,
    /// Content type including codecs, e.g. `video/mp4; codecs="avc1.640028"`.
    pub mime: String,
    /// Total size in bytes when known, so the player can skip renditions that
    /// extend past YouTube's ranged-read limit.
    pub content_length: Option<u64>,
    pub token: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoResolveResponse {
    pub info: VideoInfoDto,
    /// Proxied stream URLs (empty if not yet resolved).
    pub streams: StreamUrlsDto,
    /// Progressive renditions, best first, for the quality picker.
    pub variants: Vec<StreamVariantDto>,
    /// Video-only renditions, best first. Combined with the extracted audio by
    /// the server-side remux, these exceed the 720p progressive cap.
    pub video_variants: Vec<StreamVariantDto>,
    /// Best audio-only rendition, or `None` if the video has no audio track.
    pub audio_variant: Option<StreamVariantDto>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MuxSourceDto {
    /// `pending` | `ready` | `failed`.
    pub state: String,
    /// Proxied URL for the remuxed file. Present as soon as ffmpeg has written
    /// its first fragment, so the player can start before the remux finishes.
    pub url: Option<String>,
    /// Bytes written so far, for a progress indicator.
    pub bytes: Option<u64>,
    pub error: Option<String>,
}


#[tauri::command]
pub async fn bootstrap(state: State<'_, AppState>) -> Result<Bootstrap, String> {
    let mut guard = state.inner_tube.lock().await;
    if guard.is_none() {
        // Reuses the persisted `visitorData` so every launch looks like the same
        // returning client rather than a fresh anonymous one.
        match InnerTube::new_with_dir(Some(&AppState::data_dir())).await {
            Ok(it) => *guard = Some(it),
            Err(e) => tracing::warn!("innertube init failed: {e}"),
        }
    }
    Ok(Bootstrap {
        version: env!("CARGO_PKG_VERSION"),
        innertube_ready: guard.is_some(),
        proxy_addr: Some(format!("http://{}", state.proxy_addr)),
    })
}

/// Reset the InnerTube session. YouTube flags anonymous clients after enough
/// requests in a window — a fresh session has a new visitor data + cookies
/// and bypasses the throttle. Returns the new state.
#[tauri::command]
pub async fn innertube_reset(state: State<'_, AppState>) -> Result<Bootstrap, String> {
    // A new session means new visitor_data, and the cached googlevideo URLs were
    // authorised by the old one. Drop them before anything can read them.
    clear_resolve_cache(&state).await;
    // The cooldown is the app's *own* gate, not YouTube's. Leaving it armed made
    // "Reset identity" a no-op: the next resolve was refused by the still-running
    // backoff before it could spend the new session, so the reset looked like it
    // did nothing. Clear it — and the per-video blacklist — so the fresh identity
    // is actually tried.
    *state.throttle_until.lock().await = None;
    *state.bot_check_strikes.lock().await = 0;
    state.blacklist.lock().await.clear();
    let mut guard = state.inner_tube.lock().await;
    *guard = None;
    // The whole point of a reset is a *different* identity, so the persisted
    // `visitorData` has to go first. Left in place it would be reloaded and the
    // reset would silently reproduce the session it was meant to replace.
    match innertube::InnerTube::forget_visitor_data(&AppState::data_dir()) {
        Ok(true) => tracing::info!("dropped persisted visitorData for reset"),
        Ok(false) => tracing::debug!("no persisted visitorData to drop"),
        Err(e) => tracing::warn!("could not drop persisted visitorData: {e}"),
    }
    match InnerTube::new_with_dir(Some(&AppState::data_dir())).await {
        Ok(it) => *guard = Some(it),
        Err(e) => {
            tracing::warn!("innertube reset failed: {e}");
            return Err(e.to_string());
        }
    }
    Ok(Bootstrap {
        version: env!("CARGO_PKG_VERSION"),
        innertube_ready: guard.is_some(),
        proxy_addr: Some(format!("http://{}", state.proxy_addr)),
    })
}

#[tauri::command]
pub async fn ping() -> &'static str {
    "pong"
}

#[tauri::command]
pub async fn search(state: State<'_, AppState>, query: String) -> Result<SearchResultsDto, String> {
    let guard = state.inner_tube.lock().await;
    let yt = guard.as_ref().ok_or("innertube not initialised")?;
    innertube::dto::search(yt, &query).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn search_all(
    state: State<'_, AppState>,
    query: String,
) -> Result<SearchResultsDto, String> {
    let guard = state.inner_tube.lock().await;
    let yt = guard.as_ref().ok_or("innertube not initialised")?;
    innertube::dto::search_all(yt, &query).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn search_channels(
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<innertube::dto::SearchChannelItemDto>, String> {
    let guard = state.inner_tube.lock().await;
    let yt = guard.as_ref().ok_or("innertube not initialised")?;
    innertube::dto::search_channels(yt, &query)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn search_suggestions(
    state: State<'_, AppState>,
    query: String,
) -> Result<Vec<String>, String> {
    let guard = state.inner_tube.lock().await;
    let yt = guard.as_ref().ok_or("innertube not initialised")?;
    innertube::dto::suggestions(yt, &query).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn youtube_playlist(
    state: State<'_, AppState>,
    playlist_id: String,
) -> Result<innertube::dto::YoutubePlaylistDto, String> {
    let guard = state.inner_tube.lock().await;
    let yt = guard.as_ref().ok_or("innertube not initialised")?;
    innertube::dto::youtube_playlist(yt, &playlist_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn video_info(
    state: State<'_, AppState>,
    video_id: String,
) -> Result<VideoInfoDto, String> {
    resolve_with_gate(state, video_id)
        .await
        .map(|full| full.info)
}

/// Caption tracks the resolved player response carried. No extra request: the
/// webview asks only when it wants the list, and `captions_fetch` reuses this
/// same cached resolve to fetch the one track the user picked.
#[tauri::command]
pub async fn captions_list(
    state: State<'_, AppState>,
    video_id: String,
) -> Result<Vec<CaptionTrackDto>, String> {
    let full = resolve_with_gate(state.clone(), video_id).await?;
    Ok(full.info.captions)
}

/// Result of a `captions_fetch` call: how to point the native `<track>` at the
/// proxied VTT. The webview only ever sees the loopback URL, language code,
/// and label — never the timedtext `base_url` from the player response.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionsFetchResult {
    pub url: String,
    pub lang: String,
    pub label: String,
}

/// Loopback URL the player's `<track>` element loads the warmed VTT from.
fn captions_vtt_url(
    addr: std::net::SocketAddr,
    video_id: &str,
    lang: &str,
) -> String {
    format!("http://{addr}/captions/{video_id}/{lang}.vtt")
}

/// Warm a caption track into the shared VTT store and return the proxied URL.
///
/// One network fetch per (video, language): the player response's timedtext URL
/// carries a signature, so re-fetching on every language switch would both
/// leak a YouTube URL into the webview *and* burn a request for no gain. The
/// VTT is cached in `state.captions` and served by the proxy's `/captions`
/// route.
#[tauri::command]
pub async fn captions_fetch(
    state: State<'_, AppState>,
    video_id: String,
    lang: String,
) -> Result<CaptionsFetchResult, String> {
    let full = resolve_with_gate(state.clone(), video_id.clone()).await?;
    let track = innertube::dto::find_caption(&full.captions, &lang)
        .ok_or_else(|| format!("no caption track matches {lang:?}"))?
        .clone();

    // Warm languages are free: return the cached URL without touching the
    // network. This is what makes a language switch (and a replay of the same
    // video) instant.
    {
        let store = state.captions.lock().await;
        if store.contains_key(&(video_id.clone(), track.lang.clone())) {
            return Ok(CaptionsFetchResult {
                url: captions_vtt_url(state.proxy_addr, &video_id, &track.lang),
                lang: track.lang,
                label: track.label,
            });
        }
    }

    // Ask for structured json3 cues where the base URL allows; the rest is
    // already parameterised (expiry, signature) by YouTube.
    let upstream = if track.base_url.contains("fmt=") {
        track.base_url.clone()
    } else {
        format!("{}&fmt=json3", track.base_url)
    };

    // reqwest::Client is an Arc handle: grab a clone so the guard on the
    // inner_tube can drop *before* the network await, keeping a slow caption
    // fetch from stalling unrelated resolves.
    let http = {
        let yt = state.inner_tube.lock().await;
        let yt = yt.as_ref().ok_or("innertube not initialised")?;
        yt.raw().session.http_client.clone()
    };

    let resp = http
        .get(&upstream)
        .send()
        .await
        .map_err(|e| format!("caption fetch failed: {e}"))?;
    let status = resp.status();
    let body = resp
        .bytes()
        .await
        .map_err(|e| format!("caption read failed: {e}"))?;
    if !status.is_success() {
        let head = String::from_utf8_lossy(&body)[..body.len().min(160)].to_string();
        tracing::warn!(video_id = %video_id, lang = %track.lang, status = %status, %head, "captions_fetch refused by YouTube");
        return Err(format!(
            "YouTube refused this caption track ({status}); the track may need a fresh resolve"
        ));
    }

    let vtt = innertube::dto::captions_to_vtt(&video_id, &track.lang, &String::from_utf8_lossy(&body))
        .map_err(|e| {
            tracing::warn!(video_id = %video_id, lang = %track.lang, error = %e, "captions_fetch parse failed");
            e
        })?;

    // A track that exists but carries no cues means "no timed segments yet" —
    // surface it as a clear error rather than shipping an empty WebVTT.
    if vtt.trim().is_empty() || vtt.trim() == "WEBVTT" {
        return Err("this caption track has no timed segments".to_string());
    }

    {
        let mut store = state.captions.lock().await;
        store.insert((video_id.clone(), track.lang.clone()), vtt);
    }

    tracing::info!(
        video_id = %video_id, lang = %track.lang,
        url = %captions_vtt_url(state.proxy_addr, &video_id, &track.lang),
        "captions_fetch warmed"
    );

    Ok(CaptionsFetchResult {
        url: captions_vtt_url(state.proxy_addr, &video_id, &track.lang),
        lang: track.lang,
        label: track.label,
    })
}

/// Cooldown after an ordinary quota response. Short, because the next request
/// is often the one that gets through.
const THROTTLE_BACKOFF: Duration = Duration::from_secs(30);
/// First cooldown after a bot check. Longer than a 429 because the block survives
/// a fast retry.
const BOT_CHECK_BACKOFF: Duration = Duration::from_secs(2 * 60);
/// Ceiling for the bot-check backoff. Repeated hits escalate rather than sitting
/// at one value, because each refusal is evidence the block is still in force; the
/// cap stops a long session from refusing every video for an hour.
const BOT_CHECK_BACKOFF_MAX: Duration = Duration::from_secs(15 * 60);

/// After this many consecutive bot checks the network is treated as blocked rather
/// than merely gated, and resolves stop fanning out across clients.
///
/// One strike can mean a single awkward client, and rotation is exactly the right
/// answer then. Two in a row means the IP itself; repeating the whole chain per
/// click is what turns a temporary block into a persistent one.
const SUSTAINED_BOT_CHECK_STRIKES: u32 = 2;

/// How long to wait after `strike` consecutive bot checks, in order.
fn bot_check_backoff(strike: u32) -> Duration {
    if strike == 0 {
        return BOT_CHECK_BACKOFF;
    }
    // Double from the base, capped. Saturating so a large counter cannot wrap.
    BOT_CHECK_BACKOFF
        .saturating_mul(1u32.checked_shl(strike.min(5)).unwrap_or(u32::MAX))
        .min(BOT_CHECK_BACKOFF_MAX)
}

/// Why a `video_info` lookup failed. The distinction matters: only a genuine
/// network/IP throttle may lock *every* video, whereas a per-video problem must
/// not.
#[derive(Debug, PartialEq, Eq)]
enum ResolveFailure {
    /// YouTube is throttling this IP; other videos are affected too.
    Throttled,
    /// This video alone cannot be played (age gate, private, removed, region).
    Video,
}

/// `LOGIN_REQUIRED` covers two unrelated failures that look identical in the
/// enum: the age gate ("Sign in to confirm your age"), which is a property of
/// the video, and the bot check ("Sign in to confirm you're not a bot"), which is
/// a property of *this IP*. The second one is why an app that only watches for
/// the age message plays fine for hours and then fails on nearly every video at
/// once.
///
/// Deliberately keyed on `"not a bot"` and nothing looser. Matching the shared
/// `Sign in to confirm you…` prefix would re-create the original bug in mirror
/// image: "confirm your age" contains "confirm you", so the age gate would arm a
/// global 5-minute block. That phrasing contains an apostrophe, so no apostrophe
/// style has to be matched.
fn is_bot_check(hay_lower: &str) -> bool {
    // Shared with the player rotation, which has to make the same call to decide
    // whether a failed rotation chain was bot-checked. Two copies of this rule
    // already disagreed once, and the result was a blocked IP reported as a
    // single unavailable video.
    innertube::dto::is_bot_check_message(hay_lower)
}

/// Classify a `video_info` error.
///
/// The previous heuristic (`contains("ERROR") || contains("unavailable") ||
/// contains("not playable")`) misread YouTube's
/// `LOGIN_REQUIRED (Sign in to confirm your age)` as a throttle, because that
/// message contains "not playable". One age-restricted video therefore armed a
/// 90-second *global* block, after which the app refused every other video with
/// a bogus "YouTube is rate-limiting this network" message. So match only on
/// signals that genuinely indicate a shared limit, and treat everything else as
/// a per-video problem.
fn classify_resolve_error(e: &str) -> ResolveFailure {
    let hay = e.to_ascii_lowercase();

    // Checked first: a bot check is a shared limit, and it also carries
    // "not playable", which the guard below would otherwise excuse.
    if is_bot_check(&hay) {
        return ResolveFailure::Throttled;
    }

    // Hard signals that the limit is on us, not on the video. 429 in particular
    // is an HTTP status that only the shared/IP quota produces.
    let throttled = ["429", "too many requests", "quotaexceeded", "quota exceeded"]
        .iter()
        .any(|m| hay.contains(m))
        || (hay.contains("throttl") && !hay.contains("login_required"));

    if throttled {
        ResolveFailure::Throttled
    } else {
        ResolveFailure::Video
    }
}

/// Human-facing reason for a per-video failure, so the UI does not claim the
/// network is throttled when the video is simply gated.
fn describe_video_failure(e: &str) -> String {
    let hay = e.to_ascii_lowercase();
    if is_bot_check(&hay) {
        "YouTube is asking this network to prove it is not a bot. This is \
         temporary and affects every video, not just this one. Waiting before \
         retrying usually clears it."
            .to_string()
    } else if hay.contains("login_required") || hay.contains("confirm your age") {
        "This video is age-restricted — it needs a signed-in YouTube session, \
         which Savvy does not use."
            .to_string()
    } else if hay.contains("private") {
        "This video is private.".to_string()
    } else if hay.contains("removed") || hay.contains("terminated") {
        "This video has been removed.".to_string()
    } else if hay.contains("unavailable") {
        "This video is unavailable.".to_string()
    } else {
        e.to_string()
    }
}

/// Shared gated lookup: global throttle check, per-id cooldown, blacklist
/// bookkeeping, then the actual InnerTube call.
async fn resolve_with_gate(
    state: State<'_, AppState>,
    video_id: String,
) -> Result<innertube::dto::VideoInfoFull, String> {
    // Ahead of the throttle gate on purpose: a cache hit spends no InnerTube
    // request and touches no CDN, so there is no reason to refuse it. During a
    // quota window this is what keeps an already-seen video playable instead of
    // locking the app.
    if let Some(info) = cached_resolve(&state, &video_id).await {
        clear_blacklist(&state, &video_id).await;
        return Ok(info);
    }
    // Global IP-throttle gate: only ever armed by a real throttle signal, so a
    // single restricted video cannot lock the whole app.
    if let Some(until) = *state.throttle_until.lock().await {
        if until > Instant::now() {
            return Err(format!(
                "YouTube is rate-limiting this network. Try again in {}s.",
                (until - Instant::now()).as_secs()
            ));
        }
    }

    match fetch_video_info(&state, &video_id).await {
        Ok(info) => {
            clear_blacklist(&state, &video_id).await;
            // Any success means the network is being served again, so the
            // escalation must not carry over into the next unrelated failure.
            *state.bot_check_strikes.lock().await = 0;
            Ok(info)
        }
        Err(e) => {
            match classify_resolve_error(&e) {
                ResolveFailure::Throttled => {
                    // A real IP limit, so back off globally. A bot check gets a
                    // much longer window than a 429: retrying inside 30s just
                    // re-presents the same flagged client and re-arms the check,
                    // which is how one bad moment turns into an outage.
                    blacklist_insert(&state, &video_id).await;
                    // The cached URLs are very likely part of what tripped this,
                    // and we are about to stop serving them anyway.
                    clear_resolve_cache(&state).await;
                    let bot = is_bot_check(&e.to_ascii_lowercase());
                    let backoff = if bot {
                        // Escalate across consecutive refusals, and reset the
                        // counter on any success so a stale high count cannot
                        // keep penalising a long session.
                        let strike = {
                            let mut s = state.bot_check_strikes.lock().await;
                            *s = s.saturating_add(1);
                            *s
                        };
                        bot_check_backoff(strike)
                    } else {
                        *state.bot_check_strikes.lock().await = 0;
                        THROTTLE_BACKOFF
                    };
                    let until = Instant::now() + backoff;
                    *state.throttle_until.lock().await = Some(until);
                    tracing::warn!(video_id, bot, "throttled by YouTube; backoff until {until:?}");
                    // Surface the reason instead of the raw InnerTube string, so
                    // the UI can explain a wait rather than print "video
                    // unavailable" at someone who did nothing wrong.
                    return Err(if bot {
                        format!(
                            "YouTube is asking this network to prove it is not a bot. \
                             Pausing InnerTube requests for {}s; most videos will play \
                             again once it clears.",
                            backoff.as_secs()
                        )
                    } else {
                        format!(
                            "YouTube is rate-limiting this network. Try again in {}s.",
                            backoff.as_secs()
                        )
                    });
                }
                ResolveFailure::Video => {
                    // Per-video only. Do not touch `throttle_until`: other videos
                    // are unaffected and must stay playable.
                    tracing::warn!(video_id, "video unavailable: {e}");
                    return Err(describe_video_failure(&e));
                }
            }
        }
    }
}

/// How long a resolution stays usable. googlevideo URLs live for hours, so this
/// is not about expiry — it is about noticing channel/title edits and keeping the
/// working set small. The InnerTube session reset and the throttle path both
/// drop the cache outright, so a URL is never reused across sessions.
const RESOLVE_TTL: Duration = Duration::from_secs(20 * 60);
/// Most videos kept warm at once. A user bouncing between a handful of videos
/// should always hit; beyond that the oldest entry goes.
const RESOLVE_CACHE_MAX: usize = 16;

/// Drops a cached resolution so the next lookup spends a fresh request. Called
/// whenever the session that authorised the cached URLs is gone.
pub async fn clear_resolve_cache(state: &AppState) {
    let mut g = state.resolved.lock().await;
    if !g.is_empty() {
        tracing::debug!(entries = g.len(), "dropping resolve cache");
        g.clear();
    }
    // Drop in-flight coalescing slots too, or a fetch awaiting them would
    // surface a result authorised by the old, now-voided session.
    let mut inflight = state.resolve_in_flight.lock().await;
    inflight.clear();
}

/// videoId → (when it was fetched, the resolution). Used by the cache tests,
/// which exercise the expiry and eviction rules directly.
#[cfg(test)]
type ResolveCache = HashMap<String, (Instant, innertube::dto::VideoInfoFull)>;

/// Reads a cached value, evicting it if it has aged out. Returns `None` for a
/// miss, an expired entry, or an unknown key.
fn cache_get<T: Clone>(
    cache: &mut HashMap<String, (Instant, T)>,
    key: &str,
    ttl: Duration,
) -> Option<T> {
    let (at, value) = cache.get(key)?;
    if at.elapsed() > ttl {
        cache.remove(key);
        return None;
    }
    Some(value.clone())
}

/// Stores a value, making room by dropping the oldest entry when full. Eviction
/// is by age rather than insertion order, so repeatedly refreshing one key
/// cannot push out the rest.
fn cache_put<T: Clone>(cache: &mut HashMap<String, (Instant, T)>, key: &str, value: &T, max: usize) {
    if cache.len() >= max {
        if let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, (at, _))| *at)
            .map(|(id, _)| id.clone())
        {
            cache.remove(&oldest);
        }
    }
    cache.insert(key.to_string(), (Instant::now(), value.clone()));
}

async fn cached_resolve(state: &AppState, video_id: &str) -> Option<innertube::dto::VideoInfoFull> {
    let mut g = state.resolved.lock().await;
    let hit = cache_get(&mut g, video_id, RESOLVE_TTL);
    if hit.is_some() {
        tracing::debug!(video_id, "resolve cache hit");
    }
    hit
}

async fn store_resolve(
    state: &AppState,
    video_id: &str,
    info: &innertube::dto::VideoInfoFull,
) {
    let mut g = state.resolved.lock().await;
    cache_put(&mut g, video_id, info, RESOLVE_CACHE_MAX);
}

async fn fetch_video_info(
    state: &State<'_, AppState>,
    video_id: &str,
) -> Result<innertube::dto::VideoInfoFull, String> {
    // Guard against bogus IDs that the UI might forward when a route param is missing
    // (SvelteKit stringifies `undefined` to the literal "undefined").
    if !valid_video_id(video_id) {
        return Err(format!("invalid video id: {video_id:?}"));
    }
    // If we recently failed on this id, refuse rather than waste a request.
    if let Some(at) = state.blacklist.lock().await.get(video_id).copied() {
        if at.elapsed() < Duration::from_secs(30) {
            return Err(format!(
                "video_info is in cooldown ({:.0}s left); YouTube is throttling us, try again shortly",
                30u64.saturating_sub(at.elapsed().as_secs())
            ));
        }
    }
    // Before the InnerTube call: a cache hit is a normal outcome, not a hit
    // against the network.
    if let Some(info) = cached_resolve(state, video_id).await {
        return Ok(info);
    }
    // Single-flight: coalesce concurrent callers of the same id (a hover
    // pre-resolve racing the click, or a quality switch mid-poll) into one
    // network request. `tokio::sync::Mutex` never poisons, so a caller dropped
    // mid-fetch simply yields the slot for the next one to redo.
    single_flight(
        &state.resolve_in_flight,
        video_id,
        || fetch_video_info_once(state, video_id),
    )
    .await
}

/// Guards a per-key shared slot so that at most one `make` runs at a time for a
/// given key. The owner holds the slot's guard for the whole future, so a
/// second caller awaiting the same key blocks on the guard and then reuses the
/// stored outcome instead of redoing the work. The outcome is `Clone`d back to
/// both paths.
async fn single_flight<T, E, F, Fut>(
    inflight: &TokioMutex<HashMap<String, Arc<TokioMutex<Option<Result<T, E>>>>>>,
    key: &str,
    make: F,
) -> Result<T, E>
where
    T: Clone,
    E: Clone + std::fmt::Display,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let slot = {
        let mut map = inflight.lock().await;
        map.entry(key.to_string())
            .or_insert_with(|| Arc::new(TokioMutex::new(None)))
            .clone()
    };
    let mut held = slot.lock().await;
    if let Some(outcome) = held.as_ref() {
        return outcome.clone();
    }
    let outcome = make().await;
    *held = Some(outcome.clone());
    outcome
}

/// The actual InnerTube fetch, without caching or coalescing. Callers that
/// raced inside `fetch_video_info` only reach this when their slot is empty.
async fn fetch_video_info_once(
    state: &State<'_, AppState>,
    video_id: &str,
) -> Result<innertube::dto::VideoInfoFull, String> {
    let mut guard = state.inner_tube.lock().await;
    let yt = guard.as_mut().ok_or("innertube not initialised")?;
    // Once the block has survived a few attempts, stop fanning out across clients:
    // one request is enough to learn whether it has lifted, and six only make it
    // worse.
    let sustained = *state.bot_check_strikes.lock().await >= SUSTAINED_BOT_CHECK_STRIKES;

    match innertube::dto::video_info_full_with(yt, video_id, sustained).await {
        Ok(info) => {
            store_resolve(state, video_id, &info).await;
            Ok(info)
        }
        Err(native_err) => {
            let native_err = native_err.to_string();

            // Only a bot check is worth escalating. A missing or age-gated video
            // will be refused identically by a browser, and paying for a webview
            // round trip to learn that would be pure latency.
            if !is_bot_check(&native_err.to_ascii_lowercase()) {
                return Err(native_err);
            }

            tracing::warn!(
                video_id,
                "InnerTube refused every client; retrying through the player bridge"
            );

            let bridge = state
                .bridge
                .get()
                .cloned()
                .ok_or("player bridge not initialised")?;
            let answer = match bridge.player_response(video_id).await {
                Ok(answer) => answer,
                Err(e) => {
                    return Err(format!(
                        "YouTube is refusing this network's direct requests ({native_err}) \
                         and the in-app browser fallback did not answer ({e})."
                    ));
                }
            };

            let response = match innertube::dto::parse_player_response(&answer.raw) {
                Ok(r) => r,
                Err(e) => {
                    return Err(e.to_string());
                }
            };

            // The bridge can answer a block just like the native path can. Log
            // what it actually returned so a stall is diagnosable instead of
            // surfacing as a vague "no valid URL" once the formats are unpacked.
            tracing::info!(
                video_id,
                status = %response.playability_status.status,
                reason = response.playability_status.reason.as_deref().unwrap_or("<none>"),
                body_bytes = answer.raw.len(),
                source = %answer.source,
                has_streaming_data = answer.raw.contains("\"streamingData\""),
                "player bridge response"
            );

            // The raw body also carries the chapter markers and the full
            // description, which the typed parse discards; read them here where
            // no extra request is needed.
            let extras = match serde_json::from_str::<serde_json::Value>(&answer.raw) {
                Ok(value) => innertube::dto::PlayerExtras {
                    chapters: innertube::dto::extract_chapters(&value),
                    full_description: innertube::dto::extract_full_description(&value),
                    publish_date: innertube::dto::extract_publish_date(&value),
                },
                Err(e) => {
                    tracing::warn!(video_id, error = %e, "player bridge body did not re-parse; chapter markers unavailable");
                    innertube::dto::PlayerExtras {
                        chapters: Vec::new(),
                        full_description: None,
                        publish_date: None,
                    }
                }
            };

            // If the browser was refused too, the video is genuinely unavailable
            // rather than the network being blocked. Say that instead of
            // implying a block.
            let status = response.playability_status.status.clone();
            let reason = response
                .playability_status
                .reason
                .clone()
                .unwrap_or_else(|| "no reason given".to_string());
            if status.is_empty() || status == "ERROR" || status == "UNPLAYABLE" {
                return Err(format!("This video is unavailable ({reason})."));
            }
            // A refusal here is still a refusal of this network. Returning the
            // YouTube wording verbatim lets the shared classifier recognise it and
            // count a strike, rather than reporting a perfectly good video as
            // broken.
            if is_bot_check(&reason.to_ascii_lowercase()) {
                return Err(reason);
            }

            // The stream URLs in a browser-issued response belong to the browser
            // that asked for them, so they are fetched as that browser. Using the
            // native user agent here produced signed URLs the CDN then refused.
            let user_agent = answer.user_agent;
            let info = match innertube::dto::video_info_from_player_response_with_extras(
                yt,
                response,
                user_agent,
                video_id,
                Some(extras),
            ) {
                Ok(info) => info,
                Err(e) => return Err(e.to_string()),
            };

            tracing::info!(
                video_id,
                formats = info.combined.len() + info.video_only.len(),
                "player bridge resolved the video"
            );
            // A success clears the strikes: whatever the block was, it is not
            // biting any more.
            *state.bot_check_strikes.lock().await = 0;
            store_resolve(state, video_id, &info).await;
            Ok(info)
        }
    }
}

/// A video id is `[A-Za-z0-9_-]{6,}` — the shape YouTube actually issues. This
/// rejects the empty/`"undefined"` strings SvelteKit produces from a missing
/// route param, and anything that could be used to escape a cache path.
fn valid_video_id(video_id: &str) -> bool {
    video_id.len() >= 6
        && video_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

async fn blacklist_insert(state: &State<'_, AppState>, video_id: &str) {
    let mut g = state.blacklist.lock().await;
    g.retain(|_, at| at.elapsed() < Duration::from_secs(60 * 5));
    g.insert(video_id.to_string(), Instant::now());
}

async fn clear_blacklist(state: &State<'_, AppState>, video_id: &str) {
    state.blacklist.lock().await.remove(video_id);
}

#[tauri::command]
pub async fn video_resolve_streams(
    state: State<'_, AppState>,
    video_id: String,
) -> Result<VideoResolveResponse, String> {
    let proxy = state.proxy_table.clone();
    let upstream_cache = state.progressive_upstream.clone();
    let audio_direct = state.audio_direct.clone();
    let adaptive_cache = state.adaptive_upstream.clone();
    let duration_cache = state.video_duration.clone();
    let by_itag_cache = state.adaptive_by_itag.clone();
    let cache_key = video_id.clone();
    let t_resolve = Instant::now();
    let was_cached = state.resolved.lock().await.contains_key(&video_id);
    let full = resolve_with_gate(state, video_id.clone()).await?;
    tracing::info!(
        video_id = %cache_key,
        ms = t_resolve.elapsed().as_millis() as u64,
        cached = was_cached,
        adaptive = full.video_only.len(),
        "resolved"
    );
    let info = full.info;
    // Remember the raw progressive URL for the audio. It stays in Rust — the
    // remux needs it to stream audio alongside the video, `audio_source` needs
    // it to run ffmpeg, and the webview must never see a googlevideo URL. The
    // combined rendition carries the default client's UA, which the relay has to
    // present or the CDN throttles it.
    if let Some(first) = full.combined.first() {
        // Drop any direct-audio entry from an earlier resolve. This resolve has a
        // progressive rendition, so the extractor owns the audio; leaving the old
        // entry behind would make `audio_source` keep serving it and quietly pin
        // the video to whatever that earlier client offered.
        audio_direct.lock().await.remove(&cache_key);
        upstream_cache.lock().await.insert(
            cache_key.clone(),
            Upstream {
                url: first.url.clone(),
                user_agent: first.user_agent.clone(),
            },
        );
    } else if let Some(audio) = &full.audio_only {
        // No progressive rendition came back, which means the winning client was
        // VISIONOS - LibreTube's client, and the one that survives a bot check
        // most often, but it never returns a muxed stream. Its audio arrives as a
        // standalone adaptive track, so there is nothing to extract: register it
        // for direct playback instead. Without this the audio extractor would be
        // handed no input and the video would play silent.
        tracing::info!(
            video_id,
            itag = audio.itag,
            "no progressive rendition; using the audio-only track directly"
        );
        audio_direct.lock().await.insert(
            cache_key.clone(),
            Upstream {
                url: audio.url.clone(),
                user_agent: audio.user_agent.clone(),
            },
        );
    }
    // Likewise the top adaptive video-only rendition, for the HD remux. The UA
    // travels with the URL: the CDN checks it against the client that issued it.
    if let Some(url) = info.best_video_url.clone() {
        adaptive_cache.lock().await.insert(
            cache_key.clone(),
            Upstream { url, user_agent: full.video_user_agent.clone() },
        );
    }
    // Every adaptive rendition, so the player's quality picker can ask for a
    // specific one instead of only ever getting the top.
    {
        let mut m = by_itag_cache.lock().await;
        for v in &full.video_only {
            if v.mime.starts_with("video/mp4") {
                m.insert(
                    format!("{cache_key}#{}", v.itag),
                    Upstream { url: v.url.clone(), user_agent: v.user_agent.clone() },
                );
            }
        }
    }
    // The remux is checked against this to catch a source download that was cut
    // short, which ffmpeg otherwise reports as success.
    duration_cache
        .lock()
        .await
        .insert(cache_key, info.details.length_seconds);

    let streams = StreamUrlsDto {
        video: mint_opt(&proxy, info.best_video_url.as_ref()).await,
        audio: mint_opt(&proxy, info.best_audio_url.as_ref()).await,
        combined: mint_opt(&proxy, info.best_combined_url.as_ref()).await,
        hls_manifest: mint_opt(&proxy, info.hls_manifest_url.as_ref()).await,
    };
    let mut variants = Vec::with_capacity(full.combined.len());
    for v in &full.combined {
        variants.push(variant_dto(&proxy, v).await);
    }
    let mut video_variants = Vec::with_capacity(full.video_only.len());
    for v in &full.video_only {
        video_variants.push(variant_dto(&proxy, v).await);
    }
    let audio_variant = match &full.audio_only {
        Some(a) => Some(variant_dto(&proxy, a).await),
        None => None,
    };
    Ok(VideoResolveResponse {
        info,
        streams,
        variants,
        video_variants,
        audio_variant,
    })
}

/// Progress of the server-side audio extraction, as consumed by the webview.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioSourceDto {
    /// `pending` | `ready` | `failed`.
    pub state: &'static str,
    /// Proxied URL of the extracted audio track, when ready.
    pub url: Option<String>,
    pub mime: Option<String>,
    pub bytes: Option<u64>,
    pub error: Option<String>,
}

/// Poll (and start) the extraction of a seekable audio track for `video_id`.
///
/// The adaptive path needs a real, range-requestable audio stream, but
/// YouTube's audio-only renditions are hard-capped at their first 1 MiB, so we
/// demux the audio out of the progressive rendition once and cache it as a
/// file. This returns `pending` while ffmpeg works and `ready` afterwards; the
/// UI polls it and falls back to progressive 360p if it fails or never lands.
#[tauri::command]
pub async fn audio_source(
    state: State<'_, AppState>,
    video_id: String,
) -> Result<AudioSourceDto, String> {
    if !valid_video_id(&video_id) {
        return Err(format!("invalid video id: {video_id:?}"));
    }
    let proxy = state.proxy_table.clone();
    let proxy_addr = state.proxy_addr;
    let cache = state.audio.clone();

    // A standalone audio-only rendition is already the finished article, so it is
    // relayed as-is. This is the path that keeps audio working when the winning
    // client had no progressive rendition to extract from, and running ffmpeg on a
    // URL that is already audio-only would only add a way to fail.
    if let Some(upstream) = state.audio_direct.lock().await.get(&video_id).cloned() {
        let token = {
            let mut g = proxy.lock().await;
            g.insert_relay(upstream.url, upstream.user_agent)
        };
        tracing::info!(video_id, "serving the audio-only rendition directly");
        return Ok(AudioSourceDto {
            state: "ready",
            url: Some(format!("http://{proxy_addr}/stream/{token}")),
            mime: Some("audio/mp4".to_string()),
            bytes: None,
            error: None,
        });
    }

    let url = state
        .progressive_upstream
        .lock()
        .await
        .get(&video_id)
        .map(|u| u.url.clone());

    Ok(match cache.ensure(&video_id, url.as_deref()).await {
        AudioStatus::Pending => AudioSourceDto {
            state: "pending",
            url: None,
            mime: None,
            bytes: None,
            error: None,
        },
        AudioStatus::Ready { path, mime, bytes } => {
            let token = {
                let mut g = proxy.lock().await;
                g.insert_local(path, mime.clone())
            };
            AudioSourceDto {
                state: "ready",
                url: Some(format!("http://{proxy_addr}/stream/{token}")),
                mime: Some(mime),
                bytes: Some(bytes),
                error: None,
            }
        }
            AudioStatus::Failed { error } => AudioSourceDto {
                state: "failed",
                url: None,
                mime: None,
                bytes: None,
                error: Some(error),
            },
        })
    }

    /// Start (or poll) the server-side HD remux: the adaptive video-only
    /// rendition combined with the extracted audio track.
    ///
    /// The URL comes back while ffmpeg is still writing, because the output is
    /// a fragmented MP4 that the proxy streams as it grows. So `pending` is a
    /// playable state, not a "please wait" one.
    #[tauri::command]
    pub async fn video_mux_source(
        state: State<'_, AppState>,
        video_id: String,
        itag: Option<u32>,
    ) -> Result<MuxSourceDto, String> {
        if !valid_video_id(&video_id) {
            return Err(format!("invalid video id: {video_id:?}"));
        }
        let proxy = state.proxy_table.clone();
        let proxy_addr = state.proxy_addr;
        let cache = state.mux.clone();
        // Each rendition is cached under its own key, so switching quality
        // remuxes that one rather than reusing the previous file.
        let key = match itag {
            Some(i) => format!("{video_id}#{i}"),
            None => video_id.clone(),
        };
        let video_url = {
            let by_itag = state.adaptive_by_itag.lock().await;
            by_itag.get(&key).cloned()
        };
        // A rendition we have no URL for (not MP4, or the resolver has not run
        // yet) falls back to the default rather than failing outright.
        let video_url = match video_url {
            Some(u) => Some(u),
            None => state.adaptive_upstream.lock().await.get(&video_id).cloned(),
        };

        // The audio track must exist before the remux can start; wait for the
        // extraction the watch page kicked off rather than failing outright.
        //
        // This is the old serial path: it blocks playback on downloading the
        // *entire* progressive rendition and running a second ffmpeg pass, so
        // time-to-first-frame scaled with the audio's size. Prefer streaming the
        // audio through the relay as ffmpeg's second input instead, which starts
        // emitting frames as soon as the first audio packets land, in parallel
        // with the video. The extracted file stays as a fallback for when the
        // progressive URL is unknown.
        let progressive = state
            .progressive_upstream
            .lock()
            .await
            .get(&video_id)
            .cloned();
        // No progressive rendition means the winning client was VISIONOS. Its audio
        // arrives as a standalone adaptive track, which is exactly what ffmpeg's
        // second input wants - so relay that instead and skip the extraction step
        // entirely.
        let audio_input = match progressive.clone() {
            Some(u) => Some(u),
            None => state.audio_direct.lock().await.get(&video_id).cloned(),
        };
        let audio_relay = if let Some(u) = audio_input.as_ref() {
            let token = {
                let mut g = proxy.lock().await;
                g.insert_relay(u.url.clone(), u.user_agent.clone())
            };
            Some(format!("http://{proxy_addr}/stream/{token}"))
        } else {
            None
        };
        // Whether we take the fast parallel path or the old extract-then-mux
        // fallback decides time-to-first-frame, so it is worth a log line.
        let video_relay_seen = audio_relay.is_some();

        let audio_path = if audio_relay.is_some() {
            None
        } else {
            let audio_status = state.audio.status_for(&video_id).await;
            match &audio_status {
                AudioStatus::Ready { path, .. } if path.exists() => Some(path.clone()),
                _ => {
                    let _ = state
                        .audio
                        .ensure(
                            &video_id,
                            // Only ever a progressive URL: `audio_input` may be a
                            // ready-made audio track, and handing that to the
                            // extractor would mean demuxing audio out of audio.
                            progressive
                                .as_ref()
                                .map(|u| u.url.as_str()),
                        )
                        .await;
                    let pending = state.audio.status_for(&video_id).await;
                    if matches!(pending, AudioStatus::Pending) {
                        return Ok(MuxSourceDto {
                            state: "pending".to_string(),
                            url: None,
                            bytes: None,
                            error: None,
                        });
                    }
                    None
                }
            }
        };

        // ffmpeg takes the video and the audio as two loopback relay URLs, so both
        // stream in parallel and the first frame no longer waits on the audio's
        // full download. The relay rewrites every read into a bounded range (the
        // CDN refuses the plain GET and open-ended range ffmpeg probes with) and
        // presents the issuing client's User-Agent, or the CDN throttles the
        // rendition and the remux is silently cut short.
        let video_input = match video_url {
            Some(u) => {
                let token = {
                    let mut g = proxy.lock().await;
                    g.insert_relay(u.url, u.user_agent)
                };
                Some(format!("http://{proxy_addr}/stream/{token}"))
            }
            None => None,
        };
        let audio_input = audio_relay.or_else(|| audio_path.map(|p| p.to_string_lossy().into_owned()));
        // Once a mux is running the controller polls every 500ms, so this is
        // debug-level: it proved the relay path is taken, and now it would just
        // repeat for the duration of the remux.
        tracing::debug!(
            video_id = %video_id,
            key = %key,
            audio = if video_relay_seen { "relay" } else { "extracted" },
            itag = itag.unwrap_or(0),
            "mux inputs ready"
        );

        // Used to detect a download that was cut short, which ffmpeg reports as
        // success.
        let expected_duration = state
            .video_duration
            .lock()
            .await
            .get(&video_id)
            .copied();

        Ok(match cache
            .ensure(
                &key,
                video_input.as_deref(),
                audio_input.as_deref(),
                expected_duration,
            )
            .await
        {
            MuxStatus::Pending { path, bytes } => {
                let token = {
                    let mut g = proxy.lock().await;
                    g.insert_growing(path, MUX_MIME.to_string())
                };
                MuxSourceDto {
                    state: "pending".to_string(),
                    url: Some(format!("http://{proxy_addr}/stream/{token}")),
                    bytes: Some(bytes),
                    error: None,
                }
            }
            MuxStatus::Ready { path, bytes } => {
                let token = {
                    let mut g = proxy.lock().await;
                    g.insert_local(path, MUX_MIME.to_string())
                };
                MuxSourceDto {
                    state: "ready".to_string(),
                    url: Some(format!("http://{proxy_addr}/stream/{token}")),
                    bytes: Some(bytes),
                    error: None,
                }
            }
            MuxStatus::Failed { error } => MuxSourceDto {
                state: "failed".to_string(),
                url: None,
                bytes: None,
                error: Some(error),
            },
        })
    }


async fn variant_dto(
    proxy: &crate::proxy::SharedTable,
    v: &innertube::dto::ResolvedVariant,
) -> StreamVariantDto {
    StreamVariantDto {
        itag: v.itag,
        quality_label: v.quality_label.clone(),
        height: v.height,
        fps: v.fps,
        bitrate: v.bitrate,
        is_hdr: v.is_hdr,
            mime: v.mime.clone(),
            content_length: v.content_length,
            token: mint(proxy, v.url.clone()).await,
        }
    }


/// Must always hand back a proxy *token* — returning the raw upstream URL would
/// produce an unusable `/stream/https://…` path, so we await the lock.
async fn mint(table: &crate::proxy::SharedTable, url: String) -> String {
    let mut g = table.lock().await;
    g.insert(url)
}

async fn mint_opt(
    table: &crate::proxy::SharedTable,
    url: Option<&String>,
) -> Option<String> {
    match url {
        Some(u) => Some(mint(table, u.clone()).await),
        None => None,
    }
}

#[tauri::command]
pub fn history_list(state: State<'_, AppState>, limit: Option<u32>) -> Result<Vec<HistoryRow>, String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.history(limit.unwrap_or(200)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn history_clear(state: State<'_, AppState>) -> Result<(), String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.clear_history().map_err(|e| e.to_string())
}

/// The context menu's "remove from history": drop one row. A video that was
/// never watched is a no-op rather than an error, so the menu can offer this
/// everywhere without first asking whether the row exists.
#[tauri::command]
pub fn history_remove(state: State<'_, AppState>, video_id: String) -> Result<(), String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.remove_history(&video_id).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryAddArgs {
    pub video_id: String,
    pub title: String,
    pub author: String,
    pub thumbnail: String,
    pub duration_seconds: u32,
    #[serde(default)]
    pub channel_id: String,
    pub progress_seconds: f64,
}

#[tauri::command]
pub fn history_add(state: State<'_, AppState>, args: HistoryAddArgs) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp();
    let db = state.db();
    let db = db.blocking_lock();
    db.upsert_history(
        &args.video_id,
        &args.title,
        &args.author,
        &args.thumbnail,
        args.duration_seconds,
        &args.channel_id,
        now,
        args.progress_seconds,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn history_get(state: State<'_, AppState>, video_id: String) -> Result<Option<HistoryRow>, String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.get_history(&video_id).map_err(|e| e.to_string())
}

/// Heartbeat for feature "continue watching": the watch page reports the
/// playhead periodically and on pause/unload, and this records it. Positions
/// inside the first/last 5 seconds are treated as "not started" / "finished"
/// and reset progress to 0 so the video drops out of the resume list while
/// still appearing in history.
#[tauri::command]
pub fn history_progress(
    state: State<'_, AppState>,
    args: HistoryProgressArgs,
) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp();
    let db = state.db();
    let db = db.blocking_lock();
    db.update_progress(&args.video_id, stored_progress(args.position_seconds, args.duration_seconds), now)
        .map_err(|e| e.to_string())
}

/// Positions inside the first/last `EDGE_SECONDS` of a video are treated as
/// "not started" / "finished" and stored as 0, so the video stops offering a
/// resume while remaining in history. Unknown lengths (0) only ever zero at the
/// head.
pub fn stored_progress(position_seconds: f64, duration_seconds: i64) -> f64 {
    const EDGE_SECONDS: f64 = 5.0;
    let position = position_seconds.max(0.0);
    if position < EDGE_SECONDS {
        return 0.0;
    }
    if duration_seconds > 0 && position >= (duration_seconds as f64) - EDGE_SECONDS {
        return 0.0;
    }
    position
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryProgressArgs {
    pub video_id: String,
    /// Seconds into the video the playhead is at when this heartbeat fires.
    pub position_seconds: f64,
    /// Total video length, or 0 if unknown (live streams).
    pub duration_seconds: i64,
}

#[tauri::command]
pub fn playlists_list(state: State<'_, AppState>) -> Result<Vec<PlaylistRow>, String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.playlists().map_err(|e| e.to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistCreateArgs {
    pub id: String,
    pub title: String,
}

#[tauri::command]
pub fn playlist_create(state: State<'_, AppState>, args: PlaylistCreateArgs) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp();
    let db = state.db();
    let db = db.blocking_lock();
    db.playlist_create(&args.id, &args.title, now)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn playlist_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.playlist_delete(&id).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistAddItemArgs {
    pub playlist_id: String,
    pub video_id: String,
    pub title: String,
    pub author: String,
    pub thumbnail: String,
    pub duration_seconds: u32,
    #[serde(default)]
    pub channel_id: String,
}

#[tauri::command]
pub fn playlist_add_item(state: State<'_, AppState>, args: PlaylistAddItemArgs) -> Result<(), String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.playlist_add_item(
        &args.playlist_id,
        &args.video_id,
        &args.title,
        &args.author,
        &args.thumbnail,
        args.duration_seconds,
        &args.channel_id,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn playlist_remove_item(
    state: State<'_, AppState>,
    playlist_id: String,
    video_id: String,
) -> Result<(), String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.playlist_remove_item(&playlist_id, &video_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn playlist_items(
    state: State<'_, AppState>,
    playlist_id: String,
) -> Result<Vec<PlaylistItemRow>, String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.playlist_items(&playlist_id).map_err(|e| e.to_string())
}

// ─── Saved videos ("save to library") ─────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedAddArgs {
    pub video_id: String,
    pub title: String,
    pub author: String,
    pub thumbnail: String,
    pub duration_seconds: u32,
    pub channel_id: String,
}

#[tauri::command]
pub fn saved_add(state: State<'_, AppState>, args: SavedAddArgs) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp();
    let db = state.db();
    let db = db.blocking_lock();
    db.saved_add(
        &args.video_id,
        &args.title,
        &args.author,
        &args.thumbnail,
        args.duration_seconds,
        &args.channel_id,
        now,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn saved_list(state: State<'_, AppState>, limit: Option<u32>) -> Result<Vec<SavedVideoRow>, String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.saved_list(limit.unwrap_or(500)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn saved_ids(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.saved_ids().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn saved_remove(state: State<'_, AppState>, video_id: String) -> Result<(), String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.saved_remove(&video_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn setting_set(state: State<'_, AppState>, key: String, value: String) -> Result<(), String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.setting_set(&key, &value).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn setting_get(state: State<'_, AppState>, key: String) -> Result<Option<String>, String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.setting_get(&key).map_err(|e| e.to_string())
}

/// Every setting, resolved: the stored override where one exists, the default
/// where none does. This is what the webview loads once at startup — one round
/// trip instead of a read per control.
#[tauri::command]
pub fn settings_all(state: State<'_, AppState>) -> Result<HashMap<String, String>, String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.settings_all().map_err(|e| e.to_string())
}

/// Drop an override so the key reverts to its documented default.
#[tauri::command]
pub fn setting_reset(state: State<'_, AppState>, key: String) -> Result<(), String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.setting_reset(&key).map_err(|e| e.to_string())
}

// ─── Subscriptions ────────────────────────────────────────────────────────────

/// Mirrors [`SubscriptionRow`] one-for-one. The DB layer was written first and
/// is the canonical schema here; an earlier draft of this file invented
/// `id`/`channel_name`/`channel_url`/`avatar_url`/`created_at`/`updated_at`,
/// none of which exist in `db.rs`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionDto {
    pub channel_id: String,
    pub title: String,
    pub thumbnail: String,
    pub subscriber_count: Option<i64>,
    /// Unix seconds, assigned server-side so the client cannot backdate a row.
    pub added_at: i64,
    /// Categories this subscription belongs to. Many-to-many, so a sub that is
    /// in two categories appears under both filters.
    pub category_ids: Vec<String>,
}

impl From<SubscriptionRow> for SubscriptionDto {
    fn from(row: SubscriptionRow) -> Self {
        Self {
            channel_id: row.channel_id,
            title: row.title,
            thumbnail: row.thumbnail,
            subscriber_count: row.subscriber_count,
            added_at: row.added_at,
            category_ids: row.category_ids,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddSubscriptionInput {
    pub channel_id: String,
    pub title: String,
    pub thumbnail: String,
    #[serde(default)]
    pub subscriber_count: Option<i64>,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[tauri::command]
pub fn subscription_add(
    state: State<'_, AppState>,
    input: AddSubscriptionInput,
) -> Result<(), String> {
    let db = state.db();
    let db = db.blocking_lock();

    // No pre-check: `Db::subscription_add` is an upsert, so re-subscribing
    // refreshes a stale title/avatar rather than failing or duplicating.
    db.subscription_add(
        &input.channel_id,
        &input.title,
        &input.thumbnail,
        input.subscriber_count,
        now_unix(),
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn subscription_remove(state: State<'_, AppState>, channel_id: String) -> Result<(), String> {
    let db = state.db();
    let db = db.blocking_lock();

    db.subscription_remove(&channel_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn subscription_list(
    state: State<'_, AppState>,
    category_id: Option<String>,
) -> Result<Vec<SubscriptionDto>, String> {
    let db = state.db();
    let db = db.blocking_lock();

    let subs = db.subscriptions(category_id.as_deref()).map_err(|e| e.to_string())?;
    Ok(subs.into_iter().map(Into::into).collect())
}

#[tauri::command]
pub fn subscription_exists(
    state: State<'_, AppState>,
    channel_id: String,
) -> Result<bool, String> {
    let db = state.db();
    let db = db.blocking_lock();

    db.subscription_exists(&channel_id).map_err(|e| e.to_string())
}

/// How many avatars one refresh pass may resolve. Mirrors `IMPORT_LOOKUP_BUDGET`:
/// each is a `/browse`, so a large library is topped up over several visits
/// rather than in one quota-bursting burst. The pass shrinks as rows are filled,
/// because every hit is persisted.
const AVATAR_REFRESH_BUDGET: usize = 25;

/// Fill in avatars for subscriptions that have none (spec item 16).
///
/// Channels imported from a NewPipe export whose file omitted `avatar_url`, or
/// rows written before avatars were stored at all, render as a blank circle.
/// This resolves them one channel at a time — sequential and budget-capped, so a
/// large subscription list cannot burst the anonymous quota — and persists each
/// hit, so any given channel is fetched at most once ever. Cosmetic and
/// best-effort: a failed resolve is skipped, and the current list is returned
/// either way, so the caller can always just re-render from the result.
#[tauri::command]
pub async fn subscription_refresh_avatars(
    state: State<'_, AppState>,
) -> Result<Vec<SubscriptionDto>, String> {
    let db = state.db();

    // Snapshot the rows that still need an avatar, cheapest possible lock.
    let missing: Vec<String> = {
        let db = db.lock().await;
        db.subscriptions(None)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|s| s.thumbnail.trim().is_empty())
            .take(AVATAR_REFRESH_BUDGET)
            .map(|s| s.channel_id)
            .collect()
    };

    if !missing.is_empty() {
        // Never spend a request while an IP-level throttle is in force: the whole
        // point of the backoff is to issue no InnerTube traffic until it lifts.
        let throttled = {
            let g = state.throttle_until.lock().await;
            (*g).map(|until| until > Instant::now()).unwrap_or(false)
        };

        if !throttled {
            for channel_id in missing {
                let avatar = {
                    let guard = state.inner_tube.lock().await;
                    match guard.as_ref() {
                        Some(yt) => yt
                            .get_channel_info(&channel_id)
                            .await
                            .ok()
                            .and_then(|dto| dto.thumbnails.last().map(|t| t.url.clone())),
                        None => None,
                    }
                };
                if let Some(url) = avatar.filter(|u| !u.trim().is_empty()) {
                    let db = db.lock().await;
                    if let Err(e) = db.subscription_set_thumbnail(&channel_id, &url) {
                        tracing::warn!("avatar refresh: could not store {channel_id}: {e}");
                    }
                }
            }
        }
    }

    // Hand back the (now fuller) list so the UI re-renders from one source.
    let db = db.lock().await;
    let subs = db.subscriptions(None).map_err(|e| e.to_string())?;
    Ok(subs.into_iter().map(Into::into).collect())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetCategoriesInput {
    pub channel_id: String,
    /// Replace-all, matching the many-to-many model: the client sends the full
    /// desired set, so there is no add/remove pair to keep in sync.
    pub category_ids: Vec<String>,
}

// ─── Categories ───────────────────────────────────────────────────────────────

// All of these are plain SQLite work on a local file, so they stay *sync* like
// the rest of the DB commands: `blocking_lock` on a `tokio::sync::Mutex` panics
// when called from an async-runtime thread.

#[tauri::command]
pub fn category_list(state: State<'_, AppState>) -> Result<Vec<CategoryRow>, String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.categories().map_err(|e| e.to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryNameInput {
    pub name: String,
}

#[tauri::command]
pub fn category_add(
    state: State<'_, AppState>,
    input: CategoryNameInput,
) -> Result<CategoryRow, String> {
    let db = state.db();
    let db = db.blocking_lock();
    // New categories sort after the existing ones; the UI can reorder later.
    let next_position = db
        .categories()
        .map_err(|e| e.to_string())?
        .iter()
        .map(|c| c.position)
        .max()
        .unwrap_or(0)
        + 1;
    db.category_add(&input.name, next_position)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn category_rename(
    state: State<'_, AppState>,
    id: String,
    input: CategoryNameInput,
) -> Result<CategoryRow, String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.category_rename(&id, &input.name)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn category_remove(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.category_remove(&id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn subscription_set_categories(
    state: State<'_, AppState>,
    input: SetCategoriesInput,
) -> Result<(), String> {
    let db = state.db();
    let db = db.blocking_lock();
    db.set_sub_categories(&input.channel_id, &input.category_ids)
        .map_err(|e| e.to_string())
}

// ─── NewPipe import ───────────────────────────────────────────────────────────

/// Upper bound on `/browse` calls a single import may spend.
///
/// Only vanity URLs and rows the file left nameless need a lookup at all — a
/// plain `/channel/UC…` import costs zero — but an export full of handles must
/// not be able to walk the IP into the same throttle that breaks playback.
const IMPORT_LOOKUP_BUDGET: usize = 25;

/// Cap on the file we are willing to read. NewPipe exports are a few hundred
/// kilobytes at most; anything near this is not a subscription export.
const IMPORT_MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewPipeImport {
    /// Raw file contents. Parsed and validated here rather than in the webview
    /// so the rules live in one place.
    pub subscriptions_json: String,
}

/// Outcome of an import, shaped for the summary line the UI shows.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    /// Channels newly subscribed by this import.
    pub added: usize,
    /// Channels that were already subscribed — this is what makes a re-import
    /// visibly idempotent rather than a silent no-op.
    pub existing: usize,
    /// Rows dropped: other services, unusable URLs, duplicates, unresolved
    /// channels, and group members whose lookup failed.
    pub skipped: usize,
    /// Categories created from NewPipe groups. Groups that already existed as
    /// categories are reused, not counted.
    pub categories_created: usize,
    /// Anything the user should know about: a spent lookup budget, a group whose
    /// channels could not be resolved.
    pub warnings: Vec<String>,
}

/// Import subscriptions from a pasted NewPipe export.
#[tauri::command]
pub async fn subscription_import(
    state: State<'_, AppState>,
    input: NewPipeImport,
) -> Result<ImportResult, String> {
    run_import(&state, &input.subscriptions_json).await
}

/// Import from a file chosen in the native picker.
///
/// Reads and parses the file in the backend so the whole export never has to
/// cross into the webview as a string.
#[tauri::command]
pub async fn subscription_import_file(
    state: State<'_, AppState>,
    path: String,
) -> Result<ImportResult, String> {
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("could not open that file: {e}"))?;
    if !meta.is_file() {
        return Err("that is not a file".into());
    }
    if meta.len() > IMPORT_MAX_BYTES {
        return Err("that file is too large to be a subscription export".into());
    }
    let json = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("could not read that file: {e}"))?;
    run_import(&state, &json).await
}

/// What a lookup taught us about a channel.
struct Resolved {
    id: String,
    name: String,
    thumbnail: String,
}

async fn run_import(state: &AppState, json: &str) -> Result<ImportResult, String> {
    let plan = newpipe::plan(&newpipe::parse(json)?);
    let mut result = ImportResult {
        skipped: plan.skipped,
        ..Default::default()
    };
    if plan.subscriptions.is_empty() {
        return Err("that file has no YouTube subscriptions in it".into());
    }

    // Pass 1: the only part that costs InnerTube quota. Sequential on purpose —
    // the client is serialised anyway, so this is the gentlest possible rate,
    // and every lookup here is one the user can justify.
    let mut info: HashMap<ChannelRef, Option<Resolved>> = HashMap::new();
    let mut budget = IMPORT_LOOKUP_BUDGET;
    let mut budget_spent = false;
    for target in &plan.lookups {
        if budget == 0 {
            budget_spent = true;
            continue;
        }
        budget -= 1;
        let got = {
            let guard = state.inner_tube.lock().await;
            let Some(yt) = guard.as_ref() else {
                return Err("innertube not initialised".into());
            };
            yt.get_channel_info(target.browse_id()).await.ok()
        };
        let resolved = got.map(|dto| Resolved {
            id: dto.id,
            name: dto.name,
            thumbnail: dto
                .thumbnails
                .last()
                .map(|t| t.url.clone())
                .unwrap_or_default(),
        });
        if resolved.is_none() {
            tracing::warn!("import: could not resolve {target:?}");
        }
        info.insert(target.clone(), resolved);
    }
    if budget_spent {
        result.warnings.push(format!(
            "Stopped after {IMPORT_LOOKUP_BUDGET} channel lookups to protect your quota; \
             handle-based and unnamed channels beyond that were skipped. Import again to retry them."
        ));
    }

    // Pass 2: one database lock for the whole write.
    let db = state.db();
    let db = db.lock().await;
    let now = now_unix();

    /// The canonical id for a channel reference, if we ended up knowing one.
    fn known_id(target: &ChannelRef, info: &HashMap<ChannelRef, Option<Resolved>>) -> Option<String> {
        match target {
            ChannelRef::Id(id) => Some(id.clone()),
            ChannelRef::Vanity(_) => info.get(target).and_then(|r| r.as_ref()).map(|r| r.id.clone()),
        }
    }

    let mut written: HashSet<String> = HashSet::new();
    for sub in &plan.subscriptions {
        let (id, name, thumbnail) = match known_id(&sub.channel, &info) {
            Some(id) if !id.trim().is_empty() => {
                // Prefer what the file said; a lookup only fills gaps.
                let looked_up = info.get(&sub.channel).and_then(|r| r.as_ref());
                let name = if sub.name.is_empty() {
                    looked_up.map(|r| r.name.clone()).unwrap_or_default()
                } else {
                    sub.name.clone()
                };
                let thumbnail = if sub.thumbnail.is_empty() {
                    looked_up.map(|r| r.thumbnail.clone()).unwrap_or_default()
                } else {
                    sub.thumbnail.clone()
                };
                (id, name, thumbnail)
            }
            _ => {
                result.skipped += 1;
                continue;
            }
        };
        // Two different URLs can resolve to one channel; write it once.
        if !written.insert(id.clone()) {
            result.skipped += 1;
            continue;
        }
        // A row the file left nameless still needs something to show in the list.
        let title = if name.trim().is_empty() { id.clone() } else { name };

        if db.subscription_exists(&id).map_err(|e| e.to_string())? {
            result.existing += 1;
        } else {
            result.added += 1;
        }
        // Always upsert: a re-import should refresh names and avatars.
        db.subscription_add(&id, &title, &thumbnail, None, now)
            .map_err(|e| e.to_string())?;
    }

    // Groups become categories. Membership is accumulated per channel and
    // written once, because `set_sub_categories` is replace-all: calling it per
    // group would drop every group but the last.
    let mut pending: HashMap<String, Vec<String>> = HashMap::new();
    for group in &plan.groups {
        let (category, created) = match db.category_ensure(&group.name) {
            Ok(pair) => pair,
            Err(e) => {
                result.warnings.push(format!("group {:?}: {e}", group.name));
                continue;
            }
        };
        if created {
            result.categories_created += 1;
        }
        for member in &group.channels {
            let Some(id) = known_id(member, &info) else {
                result.skipped += 1;
                continue;
            };
            let ids = pending.entry(id).or_default();
            if !ids.contains(&category.id) {
                ids.push(category.id.clone());
            }
        }
    }
    for (channel_id, ids) in pending {
        if let Err(e) = db.set_sub_categories(&channel_id, &ids) {
            result
                .warnings
                .push(format!("could not file {channel_id} under its groups: {e}"));
        }
    }

    Ok(result)
}

// ─── Channel ──────────────────────────────────────────────────────────────────

// The channel payloads are returned straight from the `innertube` crate
// (`innertube::dto::ChannelDto` / `ChannelVideosDto`). They are already
// `Serialize` + camelCase and follow the same `id`/`name`/`thumbnails`
// convention as `VideoDetailsDto`, so re-declaring them here only added a
// second, incompatible schema. The deleted structs read
// `channel_id`/`title`/`thumbnail_url`, which no producer ever emitted.

#[tauri::command]
pub async fn channel_get_info(
    state: State<'_, AppState>,
    channel_id: String,
) -> Result<innertube::dto::ChannelDto, String> {
    let guard = state.inner_tube.lock().await;
    let yt = guard.as_ref().ok_or("innertube not initialised")?;
    yt.get_channel_info(&channel_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn channel_get_videos(
    state: State<'_, AppState>,
    channel_id: String,
) -> Result<ChannelVideosDto, String> {
    let guard = state.inner_tube.lock().await;
    let yt = guard.as_ref().ok_or("innertube not initialised")?;
    yt.get_channel_videos(&channel_id)
        .await
        .map_err(|e| e.to_string())
}

// ─── Watch Next (related videos) ─────────────────────────────────────────────

/// How long a Watch Next list stays usable before it may be re-fetched.
const RELATED_TTL: Duration = Duration::from_secs(60 * 60);
const RELATED_CACHE_MAX: usize = 32;

/// Recommended videos for a video the user is watching.
///
/// One `/next` request per video per hour, coalesced through the same
/// single-flight slot pattern as the resolve. Deliberately behind the resolve
/// on the watch page's critical path, so a slow extra request never delays
/// playback.
#[tauri::command]
pub async fn related_videos(
    state: State<'_, AppState>,
    video_id: String,
) -> Result<innertube::dto::RelatedVideosDto, String> {
    // Cache first: a hit spends no InnerTube quota.
    if let Some(v) = {
        let mut g = state.related.lock().await;
        cache_get(&mut g, &video_id, RELATED_TTL)
    } {
        return Ok(v);
    }
    // Respect an active IP-throttle window rather than adding `/next` traffic.
    if let Some(until) = *state.throttle_until.lock().await {
        if until > Instant::now() {
            return Err(format!(
                "YouTube is rate-limiting this network. Try again in {}s.",
                (until - Instant::now()).as_secs()
            ));
        }
    }
    single_flight(&state.related_in_flight, &video_id, || async {
        let guard = state.inner_tube.lock().await;
        let yt = guard.as_ref().ok_or("innertube not initialised")?;
        let dto = innertube::dto::related_videos(yt, &video_id)
            .await
            .map_err(|e| e.to_string())?;
        let mut g = state.related.lock().await;
        cache_put(&mut g, &video_id, &dto, RELATED_CACHE_MAX);
        Ok(dto)
    })
    .await
}

// ─── SponsorBlock ────────────────────────────────────────────────────────────

/// Container handed back from `sponsor_segments`. The segments are wrapped so
/// a later addition to the feature does not change the shape the webview
/// already parses.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SponsorSegmentsDto {
    pub segments: Vec<innertube::dto::SponsorSegmentDto>,
}

/// How long a fetched segment list is trusted before a revisit revalidates it
/// against the ETag instead of re-downloading.
const SPONSOR_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const SPONSOR_CACHE_MAX: usize = 256;

/// SponsorBlock skip segments for a video, filtered to the categories the app
/// can act on. At most one request per video per day (see the ETag
/// revalidation in `sponsorblock.rs`); any failure degrades to an empty list
/// so the feature can never stall or error playback.
#[tauri::command]
pub async fn sponsor_segments(
    state: State<'_, AppState>,
    video_id: String,
    categories: Vec<String>,
) -> Result<SponsorSegmentsDto, String> {
    if !valid_video_id(&video_id) {
        return Ok(SponsorSegmentsDto { segments: vec![] });
    }
    // Only categories this feature can actually skip are ever requested. The
    // API expects one `category` parameter per category; both a plural
    // `categories` form and a comma-joined value are rejected with a 400.
    let mut wanted: Vec<&str> = categories
        .iter()
        .filter(|c| crate::sponsorblock::CATEGORIES.contains(&c.as_str()))
        .map(|s| s.as_str())
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    if wanted.is_empty() {
        return Ok(SponsorSegmentsDto { segments: vec![] });
    }

    // Cache fast path: a hit younger than a day spends nothing.
    {
        let g = state.sponsor_segments.lock().await;
        if let Some(c) = g.get(&video_id) {
            if c.fetched_at.elapsed() < SPONSOR_TTL {
                return Ok(SponsorSegmentsDto { segments: c.segments.clone() });
            }
        }
    }
    // One fetch at a time per video: a repeat tap while the first is still in
    // flight must not fire a second request.
    {
        let mut in_flight = state.sponsor_in_flight.lock().await;
        if !in_flight.insert(video_id.clone()) {
            return Ok(SponsorSegmentsDto { segments: vec![] });
        }
    }
    let outcome = fetch_and_store(&state, &video_id, &wanted).await;
    let _ = state.sponsor_in_flight.lock().await.remove(&video_id);
    match outcome {
        Ok(dto) => {
            tracing::info!(
                video_id,
                count = dto.segments.len(),
                "sponsorblock segments"
            );
            Ok(dto)
        }
        Err(e) => {
            tracing::warn!(video_id, "sponsorblock request failed: {e}");
            Ok(SponsorSegmentsDto { segments: vec![] })
        }
    }
}

/// The video's chapter list, straight from the cached resolve — warm ids spend
/// no network at all (the markers ride inside `VideoInfoFull`), and a cold id
/// pays whatever a resolve already costs. Empty when the video has no chapters
/// or none could be read.
#[tauri::command]
pub async fn video_chapters(
    state: State<'_, AppState>,
    video_id: String,
) -> Result<Vec<innertube::dto::ChapterDto>, String> {
    if !valid_video_id(&video_id) {
        return Err(format!("invalid video id: {video_id:?}"));
    }
    let info = fetch_video_info(&state, &video_id).await?;
    Ok(info.chapters)
}

/// The fetch behind `sponsor_segments`; the store lives here so the in-flight
/// guard above is released exactly once whichever way the round falls.
async fn fetch_and_store(
    state: &AppState,
    video_id: &str,
    categories: &[&str],
) -> Result<SponsorSegmentsDto, String> {
    // A stale entry's ETag lets a revalidation short-circuit on 304 instead of
    // re-downloading an unchanged payload.
    let cached = state.sponsor_segments.lock().await.get(video_id).cloned();
    match crate::sponsorblock::fetch(video_id, categories, cached.as_ref().and_then(|c| c.etag.as_deref()))
        .await?
    {
        crate::sponsorblock::Fetch::Updated { segments, etag } => {
            let dto = SponsorSegmentsDto { segments: segments.clone() };
            sponsor_store(
                state,
                video_id,
                crate::sponsorblock::CachedSegments {
                    segments,
                    fetched_at: Instant::now(),
                    etag,
                },
            )
            .await;
            Ok(dto)
        }
        crate::sponsorblock::Fetch::Unchanged => {
            // A 304 is only served because we sent an ETag, so the entry exists.
            // Refresh its age and hand the cached payload back.
            let dto = if let Some(c) = state.sponsor_segments.lock().await.get_mut(video_id) {
                c.fetched_at = Instant::now();
                Some(SponsorSegmentsDto { segments: c.segments.clone() })
            } else {
                None
            };
            Ok(dto.unwrap_or(SponsorSegmentsDto { segments: vec![] }))
        }
        crate::sponsorblock::Fetch::Empty => {
            // The server knows this video and has nothing for it; remember that
            // so we do not re-ask for a day.
            sponsor_store(
                state,
                video_id,
                crate::sponsorblock::CachedSegments {
                    segments: vec![],
                    fetched_at: Instant::now(),
                    etag: None,
                },
            )
            .await;
            Ok(SponsorSegmentsDto { segments: vec![] })
        }
    }
}

/// Stores a cached segment list, evicting the oldest entry when the cache is
/// full so a long session visits cannot grow it without bound.
async fn sponsor_store(state: &AppState, video_id: &str, entry: crate::sponsorblock::CachedSegments) {
    let mut g = state.sponsor_segments.lock().await;
    if g.len() >= SPONSOR_CACHE_MAX {
        if let Some(oldest) = g
            .iter()
            .min_by_key(|(_, c)| c.fetched_at)
            .map(|(id, _)| id.clone())
        {
            g.remove(&oldest);
        }
    }
    g.insert(video_id.to_string(), entry);
}

// ─── Subscriptions feed ──────────────────────────────────────────────────────

/// How long a channel's latest uploads stay usable before they may be
/// re-fetched. The feed is a browsing surface, so a channel being half an hour
/// stale is fine and saves a `/browse` call we are throttled on.
const FEED_CHANNEL_TTL: Duration = Duration::from_secs(30 * 60);
const FEED_CHANNEL_CACHE_MAX: usize = 256;
/// Videos kept per channel across all deepening rounds. Bounds the merge and
/// the session's memory; the newest are kept, so a channel uploading many times
/// a day contributes its most recent uploads only.
const FEED_MAX_PER_CHANNEL: usize = 120;
/// How old a video may be and still be shown. Without this a channel that has
/// been dormant for a year contributes its whole stale page and pushes live
/// channels out of the first batches.
const FEED_CUTOFF_DAYS: i64 = 90;
/// Videos handed to the webview per batch. The UI appends a further batch when
/// the user reaches the bottom of the list.
const FEED_BATCH: usize = 24;
/// How many continuation pages deep the feed may walk before it stops. Each
/// round is one `/browse` per channel, so this is the quota ceiling on a long
/// scroll session.
const FEED_MAX_DEPTH: u32 = 4;
/// How long an idle feed session is kept before its server-side merged list and
/// per-channel continuation tokens are dropped.
const FEED_SESSION_TTL: Duration = Duration::from_secs(30 * 60);
const FEED_SESSION_MAX: usize = 8;
/// How many channels are fetched at once. Keeps a large subscription list from
/// firing dozens of `/browse` calls simultaneously, which is what gets an
/// anonymous IP throttled.
const FEED_FETCH_CONCURRENCY: usize = 4;
/// Seconds between two uploads assumed when a channel's page carries no
/// parseable date at all. Only used to keep undated items in a sane relative
/// order; never shown to the user.
const FEED_UNDATED_STEP: i64 = 12 * 60 * 60;

/// One upload in the subscriptions feed, with its channel joined on so the UI
/// can render an avatar without a second lookup.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FeedItemDto {
    pub video_id: String,
    pub title: String,
    pub channel_id: String,
    pub channel_name: String,
    pub channel_thumbnail: String,
    pub thumbnails: Vec<innertube::dto::ThumbnailDto>,
    pub duration_seconds: Option<u32>,
    pub view_count: Option<String>,
    /// The upstream relative label ("2 hours ago"), if any.
    pub published_text: Option<String>,
    /// Approximate unix time the upload happened, derived from `published_text`.
    /// Used only to order the merged feed; `None` when unparseable.
    pub published_at: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedDto {
    /// The next slice, in chronological order.
    pub items: Vec<FeedItemDto>,
    /// Opaque token to pass back for the following slice. `None` when the feed
    /// is exhausted.
    pub next_cursor: Option<String>,
    /// Whether asking for more would yield anything.
    pub has_more: bool,
    /// The server had no session for the cursor it was given and started a new
    /// feed from the top. The UI must replace its list, not append to it.
    pub restarted: bool,
}

/// Seconds in one of YouTube's age units, accepting both the compact spelling
/// and the spelled-out one. Both the raw token and its singular form are tried,
/// because trimming a trailing `s` turns the seconds unit `s` into "".
fn unit_secs(unit: &str) -> Option<i64> {
    for u in [unit, unit.trim_end_matches('s')] {
        let secs = match u {
            "s" | "sec" | "second" => 1,
            // "m" is minutes here; months abbreviate to "mo".
            "m" | "min" | "minute" => 60,
            "h" | "hr" | "hour" => 60 * 60,
            "d" | "day" => 24 * 60 * 60,
            "w" | "wk" | "week" => 7 * 24 * 60 * 60,
            "mo" | "mon" | "month" => 30 * 24 * 60 * 60,
            "y" | "yr" | "year" => 365 * 24 * 60 * 60,
            _ => continue,
        };
        return Some(secs);
    }
    None
}

/// Turns YouTube's relative upload label into an approximate age in seconds.
///
/// The channel Videos grid abbreviates: `innertube-rs` surfaces the compact
/// `text.content` form ("2d ago", "30m ago", "1y ago") and discards the sibling
/// `accessibilityLabel`, which is the same value spelled out ("2 days ago").
/// Only the compact form reaches us, and it joins the number to the unit with no
/// space, so both shapes are handled here. Without this every date failed to
/// parse and the feed fell back to guessing ages.
fn relative_age_secs(text: &str) -> Option<i64> {
    let mut lower = text.trim().to_ascii_lowercase();
    for prefix in ["streamed ", "premiered ", "watched ", "released "] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            lower = rest.trim().to_string();
            break;
        }
    }
    // One-word labels that carry no number at all.
    match lower.as_str() {
        "now" | "just now" | "live" => return Some(0),
        "yesterday" => return Some(24 * 60 * 60),
        _ => {}
    }

    let mut parts = lower.split_whitespace();
    let first = parts.next()?;

    let (count, unit): (i64, &str) = if first == "a" || first == "an" {
        (1, parts.next()?)
    } else {
        // Compact labels run the number straight into the unit ("2d"), while the
        // spelled-out form separates them ("2 days").
        let split = first
            .char_indices()
            .find(|(_, c)| !c.is_ascii_digit())
            .map(|(i, _)| i)
            .unwrap_or(first.len());
        if split == 0 {
            return None;
        }
        let (num, tail) = first.split_at(split);
        let unit = if tail.is_empty() { parts.next()? } else { tail };
        (num.parse::<i64>().ok()?, unit)
    };

    Some(count.checked_mul(unit_secs(unit)?)?)
}

/// Estimates an absolute upload time for every video in one channel, given the
/// `published_text` labels YouTube returned (already newest-first).
///
/// The channel's own ordering is the trustworthy signal: the Videos tab is
/// sorted newest-first by the server. Labels like "2d ago" are far too coarse to
/// order a global feed against, so an undated entry is interpolated between the
/// nearest dated neighbours in the same channel. That is exact in ordering terms,
/// because the surrounding entries bound it.
///
/// A channel with no dated entry at all has nothing to interpolate from, so its
/// uploads are laid out below `watermark` — the newest upload anywhere in the
/// feed that we could actually date. Anchoring them to `now` instead would
/// claim that a dormant channel's year-old uploads happened hours ago, and sort
/// them to the very top of the feed.
fn order_channel(published_text: &[Option<String>], now: i64, watermark: i64) -> Vec<i64> {
    let n = published_text.len();
    let mut out = vec![i64::MIN; n];

    // Absolute times we can read straight off the label.
    let dated: Vec<Option<i64>> = published_text
        .iter()
        .map(|t| t.as_deref().and_then(relative_age_secs).map(|age| now - age))
        .collect();

    // Forward pass: an undated entry inherits the one above it, stepped back so
    // it sorts strictly older. The running value advances for undated entries
    // too, so a run of them keeps descending instead of tying.
    let mut running: Option<i64> = None;
    for i in 0..n {
        let at = match dated[i] {
            Some(at) => at,
            None => match running {
                // Nothing dated yet in this channel: fall back to the feed-wide
                // watermark rather than to `now`.
                None => watermark - ((i as i64) + 1) * FEED_UNDATED_STEP,
                Some(prev) => prev - FEED_UNDATED_STEP,
            },
        };
        running = Some(at);
        out[i] = at;
    }

    // Anything above the first dated entry is newer than it, because the tab
    // is sorted newest-first. Re-anchor that leading run just above the first
    // real date instead of leaving it on the nominal decay — otherwise a channel
    // whose only label is a year old would claim to have uploaded this week.
    if let Some(first) = dated.iter().position(|d| d.is_some()) {
        let anchor = dated[first].expect("position found a dated entry");
        for i in 0..first {
            out[i] = anchor + ((first - i) as i64) * FEED_UNDATED_STEP;
        }
    }

    out
}

/// Latest uploads for one channel, served from the feed cache when fresh and
/// coalesced through a single-flight slot otherwise — mirrors `related_videos`.
///
/// Page one is cached per channel because every feed visit needs it. Deeper
/// pages are cached per `(channel, continuation)` in the same map, so walking
/// back through the feed after a scroll-up does not re-spend `/browse` calls.
async fn channel_videos_cached(
    state: &AppState,
    channel_id: &str,
    continuation: Option<&str>,
) -> Result<innertube::dto::ChannelVideosDto, String> {
    let key = match continuation {
        None => channel_id.to_string(),
        Some(token) => format!("{channel_id}\u{1f}{token}"),
    };
    if let Some(v) = {
        let mut g = state.channel_videos.lock().await;
        cache_get(&mut g, &key, FEED_CHANNEL_TTL)
    } {
        return Ok(v);
    }
    single_flight(&state.channel_videos_in_flight, &key, || async {
        let guard = state.inner_tube.lock().await;
        let yt = guard.as_ref().ok_or("innertube not initialised")?;
        let dto = yt
            .get_channel_videos_page(channel_id, continuation)
            .await
            .map_err(|e| e.to_string())?;
        let mut g = state.channel_videos.lock().await;
        cache_put(&mut g, &key, &dto, FEED_CHANNEL_CACHE_MAX);
        Ok(dto)
    })
    .await
}

/// Server-side state for one browsing session over the feed.
///
/// The merged list is expensive to rebuild (it needs every channel's uploads),
/// so the per-channel uploads are kept here and re-merged into slices of
/// [`FEED_BATCH`] as the user scrolls. Deepening means fetching one more
/// continuation page per channel.
pub struct FeedSession {
    /// Per-channel uploads gathered so far, plus each one's next-page token.
    pub channels: HashMap<String, ChannelUploads>,
    /// How many continuation pages deep each channel has been walked.
    pub depth: u32,
    pub last_used: Instant,
}

/// One channel's contribution to a merge: the uploads it has yielded so far,
/// oldest last, plus the token needed to go deeper.
#[derive(Clone)]
pub struct ChannelUploads {
    pub sub: SubscriptionRow,
    pub videos: Vec<innertube::dto::ChannelVideoItem>,
    pub next: Option<String>,
}

impl ChannelUploads {
    /// First page for a channel. The only page eligible for the shared cache:
    /// continuation pages are keyed by their token, which is per-session.
    async fn load_first(state: &AppState, sub: SubscriptionRow) -> ChannelUploads {
        match channel_videos_cached(state, &sub.channel_id, None).await {
            Ok(dto) => ChannelUploads {
                sub,
                videos: dto.videos,
                next: dto.continuation,
            },
            Err(e) => {
                tracing::warn!("feed: channel {} failed: {e}", sub.channel_id);
                ChannelUploads {
                    sub,
                    videos: Vec::new(),
                    next: None,
                }
            }
        }
    }

    /// Appends one continuation page, dropping videos already held and stopping
    /// when the channel runs out of pages or the page adds nothing new.
    fn absorb(&mut self, dto: innertube::dto::ChannelVideosDto) {
        let fresh: Vec<_> = dto
            .videos
            .into_iter()
            .filter(|v| !self.videos.iter().any(|h| h.video_id == v.video_id))
            .collect();
        self.videos.extend(fresh);
        // Keep the newest: the Videos tab is already sorted newest-first.
        if self.videos.len() > FEED_MAX_PER_CHANNEL {
            self.videos.truncate(FEED_MAX_PER_CHANNEL);
        }
        self.next = dto.continuation;
    }
}

/// Merges every channel's uploads into one newest-first list.
///
/// Each channel is ordered by its own server-side ranking, then the whole set
/// is sorted by estimated upload time with the per-channel rank as tiebreak. The
/// rank component is what guarantees the feed interleaves: without it, channels
/// with equally-coarse labels ("3 weeks ago") would still come out in blocks.
/// Videos older than [`FEED_CUTOFF_DAYS`] are dropped.
fn merge_feed(
    per_channel: &[ChannelUploads],
    now: i64,
    cutoff_days: i64,
) -> Vec<FeedItemDto> {
    let cutoff = now - cutoff_days * 24 * 60 * 60;
    let mut ranked: Vec<(i64, u32, FeedItemDto)> = Vec::new();

    // The newest upload in the whole feed we can actually date. Channels with no
    // readable date at all are laid out below this instead of below `now`.
    let watermark = per_channel
        .iter()
        .flat_map(|ch| ch.videos.iter())
        .filter_map(|v| v.published_text.as_deref().and_then(relative_age_secs))
        .map(|age| now - age)
        .max()
        .unwrap_or(now);

    for ch in per_channel {
        let labels: Vec<Option<String>> =
            ch.videos.iter().map(|v| v.published_text.clone()).collect();
        let stamps = order_channel(&labels, now, watermark);

        for (rank, (video, at)) in ch.videos.iter().zip(stamps).enumerate() {
            if at < cutoff {
                continue;
            }
            ranked.push((
                at,
                rank as u32,
                FeedItemDto {
                    video_id: video.video_id.clone(),
                    title: video.title.clone(),
                    channel_id: ch.sub.channel_id.clone(),
                    channel_name: ch.sub.title.clone(),
                    channel_thumbnail: ch.sub.thumbnail.clone(),
                    thumbnails: video.thumbnails.clone(),
                    duration_seconds: video.duration_seconds,
                    view_count: video.view_count.clone(),
                    published_text: video.published_text.clone(),
                    published_at: (at != i64::MIN).then_some(at),
                },
            ));
        }
    }

    // Newest first; rank, then id, keep the order total and stable across runs
    // so paging through the feed cannot repeat or skip a video.
    ranked.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.channel_id.cmp(&b.2.channel_id))
            .then(a.2.video_id.cmp(&b.2.video_id))
    });

    // A video can appear twice if it was uploaded to two subscribed channels, or
    // if a continuation page overlapped the one before it.
    let mut seen = HashSet::new();
    ranked
        .into_iter()
        .filter(|(_, _, item)| seen.insert(item.video_id.clone()))
        .map(|(_, _, item)| item)
        .collect()
}

/// Builds the merged list for a set of channels from whatever has been fetched.
fn rebuild(
    channels: &mut HashMap<String, ChannelUploads>,
    now: i64,
) -> Vec<FeedItemDto> {
    let mut list: Vec<ChannelUploads> = channels.values().cloned().collect();
    // Stable channel order keeps the merge deterministic between rebuilds.
    list.sort_by(|a, b| a.sub.channel_id.cmp(&b.sub.channel_id));
    merge_feed(&list, now, FEED_CUTOFF_DAYS)
}

fn next_session_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    format!(
        "{:x}{:x}",
        COUNTER.fetch_add(1, Ordering::Relaxed),
        now_unix()
    )
}

#[tauri::command]
pub async fn subscription_feed(
    state: State<'_, AppState>,
    cursor: Option<String>,
    limit: Option<u32>,
) -> Result<FeedDto, String> {
    // Respect an active IP-throttle window rather than adding `/browse` traffic.
    if let Some(until) = *state.throttle_until.lock().await {
        if until > Instant::now() {
            return Err(format!(
                "YouTube is rate-limiting this network. Try again in {}s.",
                (until - Instant::now()).as_secs()
            ));
        }
    }

    let batch = limit.unwrap_or(FEED_BATCH as u32).clamp(1, 100) as usize;
    let now = now_unix();

    // The cursor is `sessionId:offset`. An unknown or expired session is not an
    // error: the user gets a fresh feed from the top, and `restarted` tells the
    // UI to replace its list rather than append to a stale one.
    let parsed = cursor.as_deref().and_then(|c| c.split_once(':'));
    let (mut session_id, mut offset) = match parsed {
        Some((id, off)) => (Some(id.to_string()), off.parse::<usize>().unwrap_or(0)),
        None => (None, 0),
    };

    let mut restarted = false;
    let mut channels: HashMap<String, ChannelUploads> = HashMap::new();
    let mut depth = 0u32;

    // Reuse a live session, otherwise start a new one.
    if let Some(id) = session_id.clone() {
        let mut g = state.feed_sessions.lock().await;
        if let Some(s) = g.get_mut(&id) {
            s.last_used = Instant::now();
            for (cid, ch) in &s.channels {
                channels.insert(cid.clone(), ch.clone());
            }
            depth = s.depth;
        } else {
            restarted = true;
            session_id = None;
            offset = 0;
        }
    }

    if session_id.is_none() {
        let subs: Vec<SubscriptionRow> = {
            let db = state.db();
            let guard = db.lock().await;
            // The feed is always the unfiltered list: it merges across every
            // subscription, and the category filter is a UI concern.
            guard.subscriptions(None).map_err(|e| e.to_string())?
        };
        if subs.is_empty() {
            return Ok(FeedDto {
                items: Vec::new(),
                next_cursor: None,
                has_more: false,
                restarted: true,
            });
        }

        use futures_util::StreamExt;
        let state_ref: &AppState = &state;
        let loaded = futures_util::stream::iter(subs.into_iter().map(|sub| async move {
            ChannelUploads::load_first(state_ref, sub).await
        }))
        .buffer_unordered(FEED_FETCH_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

        for ch in loaded {
            channels.insert(ch.sub.channel_id.clone(), ch);
        }
        session_id = Some(next_session_id());
    }

    let sid = session_id.clone().expect("session id assigned above");

    // Off the end of what we have merged: walk every channel one page deeper
    // and re-merge, until something new turns up or the depth ceiling is hit.
    let mut items = rebuild(&mut channels, now);
    while offset >= items.len() && depth < FEED_MAX_DEPTH {
        let before = items.len();
        deepen_once(&state, &mut channels).await;
        depth += 1;
        items = rebuild(&mut channels, now);
        // Every channel has run out of continuations: nothing left to merge.
        if !channels.values().any(|c| c.next.is_some()) {
            break;
        }
        let _ = before;
    }

    let has_more = offset + batch < items.len() || channels.values().any(|c| c.next.is_some());

    // Keep the session for the next scroll batch.
    {
        let mut g = state.feed_sessions.lock().await;
        // Drop idle sessions so a long-lived app does not accumulate merged
        // lists for feeds nobody is looking at any more.
        let ttl = FEED_SESSION_TTL;
        g.retain(|_, s| s.last_used.elapsed() < ttl);
        if g.len() >= FEED_SESSION_MAX {
            if let Some(oldest) = g
                .iter()
                .min_by_key(|(_, s)| s.last_used)
                .map(|(k, _)| k.clone())
            {
                if oldest != sid {
                    g.remove(&oldest);
                }
            }
        }
        g.insert(
            sid.clone(),
            FeedSession {
                channels: channels.clone(),
                depth,
                last_used: Instant::now(),
            },
        );
    }

    let end = (offset + batch).min(items.len());
    let page = items[offset.min(items.len())..end].to_vec();
    let next_offset = end;

    Ok(FeedDto {
        items: page,
        next_cursor: if has_more {
            Some(format!("{sid}:{next_offset}"))
        } else {
            None
        },
        has_more,
        restarted,
    })
}

/// Fetches the next page for every channel that still has a continuation and
/// appends it to what that channel has already yielded.
///
/// The pages are fetched concurrently, but they are applied afterwards: a task
/// holding `&mut` into the channel map across an `await` would make the map
/// unborrowable by the other concurrent tasks.
async fn deepen_once(state: &AppState, channels: &mut HashMap<String, ChannelUploads>) {
    let pending: Vec<(String, String)> = channels
        .iter()
        .filter_map(|(cid, c)| c.next.clone().map(|token| (cid.clone(), token)))
        .collect();
    if pending.is_empty() {
        return;
    }

    use futures_util::StreamExt;
    let state_ref: &AppState = state;
    let fetched = futures_util::stream::iter(pending.into_iter().map(|(cid, token)| {
        let state_ref: &AppState = state_ref;
        async move {
            let page = channel_videos_cached(state_ref, &cid, Some(&token)).await;
            (cid, page)
        }
    }))
    .buffer_unordered(FEED_FETCH_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;

    for (cid, page) in fetched {
        let Some(ch) = channels.get_mut(&cid) else {
            continue;
        };
        match page {
            Ok(dto) => ch.absorb(dto),
            Err(e) => {
                tracing::warn!("feed: {} continuation failed: {e}", cid);
                // Do not retry a page that already failed; that would turn one
                // bad channel into a repeat request on every scroll batch.
                ch.next = None;
            }
        }
    }
}

// ─── External links ───────────────────────────────────────────────────────────

/// Hand a YouTube URL to the system default browser ("open in external
/// browser" in the context menu). The webview cannot leave the app by itself
/// and an opener plugin would be a whole dependency for one call, so this
/// launches the platform's URL handler directly - no shell involved, so a `&`
/// in the query string cannot turn into a command separator.
///
/// The URL is always built by the app from a video or channel id, but it still
/// arrives here as a plain string, so only the host we mint links for is
/// accepted.
#[tauri::command]
pub fn open_external(url: String) -> Result<(), String> {
    const ALLOWED: [&str; 2] = ["https://www.youtube.com/", "https://youtu.be/"];
    if !ALLOWED.iter().any(|prefix| url.starts_with(prefix)) {
        return Err(format!("refusing to open {url:?}"));
    }
    if url.contains(['"', '\n', '\r', '\0']) {
        return Err("refusing to open a URL with control characters".to_string());
    }

    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = std::process::Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", &url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = std::process::Command::new("open");
        c.arg(&url);
        c
    };
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let mut command = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(&url);
        c
    };

    command
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not open a browser: {e}"))
}

// ─── Deprecated Commands ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn video(id: &str, published: Option<&str>) -> innertube::dto::ChannelVideoItem {
        innertube::dto::ChannelVideoItem {
            video_id: id.to_string(),
            title: format!("video {id}"),
            thumbnails: Vec::new(),
            duration_seconds: None,
            published_text: published.map(|s| s.to_string()),
            view_count: None,
            channel_id: String::new(),
            author: None,
        }
    }

    fn sub(id: &str) -> SubscriptionRow {
        SubscriptionRow {
            channel_id: id.to_string(),
            title: format!("channel {id}"),
            thumbnail: String::new(),
            subscriber_count: None,
            added_at: 0,
            category_ids: Vec::new(),
        }
    }

    const NOW: i64 = 1_700_000_000;

    // ─── relative_age_secs ──────────────────────────────────────────────

    #[test]
    fn parses_the_labels_youtube_actually_emits() {
        let day = 24 * 60 * 60;
        assert_eq!(relative_age_secs("3 days ago"), Some(3 * day));
        assert_eq!(relative_age_secs("Streamed 2 days ago"), Some(2 * day));
        assert_eq!(relative_age_secs("Premiered 5 hours ago"), Some(5 * 3600));
        assert_eq!(relative_age_secs("1 year ago"), Some(365 * day));
        assert_eq!(relative_age_secs("a day ago"), Some(day));
        assert_eq!(relative_age_secs("an hour ago"), Some(3600));
        assert_eq!(relative_age_secs("Yesterday"), Some(day));
        assert_eq!(relative_age_secs("now"), Some(0));
    }

    /// The exact pair a live channel Videos page returned: `innertube-rs` keeps
    /// the compact `text.content` value and drops the spelled-out
    /// `accessibilityLabel` sibling, so only the compact form reaches us. When
    /// this returned `None`, every channel fell back to the nominal decay and a
    /// dormant channel's year-old uploads sorted to the very top of the feed.
    #[test]
    fn parses_the_compact_labels_the_channel_grid_actually_returns() {
        let day = 24 * 60 * 60;
        assert_eq!(relative_age_secs("2d ago"), Some(2 * day));
        assert_eq!(relative_age_secs("5h ago"), Some(5 * 3600));
        assert_eq!(relative_age_secs("30m ago"), Some(30 * 60));
        assert_eq!(relative_age_secs("3w ago"), Some(3 * 7 * day));
        assert_eq!(relative_age_secs("2mo ago"), Some(60 * day));
        assert_eq!(relative_age_secs("1y ago"), Some(365 * day));
        assert_eq!(relative_age_secs("45s ago"), Some(45));
        assert_eq!(relative_age_secs("Streamed 2d ago"), Some(2 * day));
    }

    #[test]
    fn unknown_labels_are_none_rather_than_guessed() {
        assert_eq!(relative_age_secs(""), None);
        assert_eq!(relative_age_secs("some time ago"), None);
        assert_eq!(relative_age_secs("12"), None);
    }

    // ─── order_channel ──────────────────────────────────────────────────

    #[test]
    fn undated_entries_are_interpolated_between_dated_neighbours() {
        // The bug this feed had: upstream often omits the date, and a missing
        // date must not sink an upload to the bottom of a merged feed.
        let labels = vec![
            Some("10 days ago".to_string()),
            None,
            None,
            Some("20 days ago".to_string()),
        ];
        let out = order_channel(&labels, NOW, NOW);
        // Monotonically older down the channel, and strictly between the anchors.
        assert!(out[0] > out[1], "newest first");
        assert!(out[1] > out[2]);
        assert!(out[2] > out[3]);
        assert!(out[1] < NOW - 10 * 86_400, "undated sits below the 10-day anchor");
        assert!(out[2] > NOW - 20 * 86_400, "undated sits above the 20-day anchor");
    }

    #[test]
    fn a_channel_with_no_dates_decays_rather_than_claiming_to_be_fresh() {
        let labels = vec![None, None, None];
        let out = order_channel(&labels, NOW, NOW);
        assert!(out[0] > out[1] && out[1] > out[2], "still newest-first");
        assert!(out[0] < NOW, "never in the future");
    }

    /// The bug behind "old videos at the top": a channel whose page carried no
    /// readable date was laid out below `now`, so a dormant channel's year-old
    /// uploads were stamped hours old and sorted above genuinely fresh ones.
    #[test]
    fn an_undated_channel_never_outranks_the_newest_dated_upload() {
        let fresh = NOW - 2 * 86_400; // "2d ago" elsewhere in the feed.
        let undated = ChannelUploads {
            sub: sub("UCundated"),
            videos: vec![video("u1", None), video("u2", None), video("u3", None)],
            next: None,
        };
        let dated = ChannelUploads {
            sub: sub("UCdated"),
            videos: vec![video("d1", Some("2d ago"))],
            next: None,
        };
        let merged = merge_feed(&[undated, dated], NOW, FEED_CUTOFF_DAYS);
        let ids: Vec<&str> = merged.iter().map(|i| i.video_id.as_str()).collect();
        assert_eq!(ids, ["d1", "u1", "u2", "u3"], "undated sorts below the watermark");
        assert_eq!(merged[0].published_at, Some(fresh));
    }

    /// With every channel undated there is no watermark to anchor to, so the feed
    /// falls back to `now` and interleaves by rank rather than blocking.
    #[test]
    fn an_entirely_undated_feed_still_interleaves() {
        let a = ChannelUploads {
            sub: sub("UCaaa"),
            videos: vec![video("a1", None), video("a2", None)],
            next: None,
        };
        let b = ChannelUploads {
            sub: sub("UCbbb"),
            videos: vec![video("b1", None), video("b2", None)],
            next: None,
        };
        let merged = merge_feed(&[a, b], NOW, FEED_CUTOFF_DAYS);
        let ids: Vec<&str> = merged.iter().map(|i| i.video_id.as_str()).collect();
        assert_eq!(ids, ["a1", "b1", "a2", "b2"]);
    }

    #[test]
    fn a_stale_channel_is_anchored_to_its_oldest_dated_entry() {
        // One label, at the very top, dated a year ago: the undated entries
        // below must not come out looking newer than it.
        let labels = vec![Some("1 year ago".to_string()), None, None];
        let out = order_channel(&labels, NOW, NOW);
        let anchor = NOW - 365 * 86_400;
        for at in &out {
            assert!(*at <= anchor, "undated entry must not outrank the anchor");
        }
    }

    #[test]
    fn a_leading_undated_run_is_anchored_above_the_first_real_date() {
        // The tab is newest-first, so entries above "1 year ago" are newer than
        // it. On the nominal decay from `now` they would outrank it and claim
        // to be days old, dragging a dormant channel to the top of the feed.
        let labels = vec![None, None, Some("1 year ago".to_string())];
        let out = order_channel(&labels, NOW, NOW);
        let anchor = NOW - 365 * 86_400;
        assert!(out[2] == anchor);
        assert!(out[1] > out[2], "still newest-first within the run");
        assert!(out[0] > out[1]);
        for at in &out[..2] {
            assert!(*at > anchor, "entries above the anchor must outrank it");
        }
    }

    #[test]
    fn a_channel_with_no_entries_yields_nothing() {
        assert!(order_channel(&[], NOW, NOW).is_empty());
    }

    // ─── merge_feed ─────────────────────────────────────────────────────

    #[test]
    fn the_feed_interleaves_instead_of_grouping_by_channel() {
        // Two channels with entirely undated pages: with a stable sort and no
        // rank tiebreak this regressed to one channel's block after the other.
        let a = ChannelUploads {
            sub: sub("UCaaa"),
            videos: vec![video("a1", None), video("a2", None), video("a3", None)],
            next: None,
        };
        let b = ChannelUploads {
            sub: sub("UCbbb"),
            videos: vec![video("b1", None), video("b2", None), video("b3", None)],
            next: None,
        };
        let merged = merge_feed(&[a, b], NOW, FEED_CUTOFF_DAYS);
        let ids: Vec<&str> = merged.iter().map(|i| i.video_id.as_str()).collect();
        assert_eq!(ids, ["a1", "b1", "a2", "b2", "a3", "b3"]);
    }

    #[test]
    fn the_feed_is_ordered_by_upload_time_across_channels() {
        let a = ChannelUploads {
            sub: sub("UCaaa"),
            videos: vec![video("a1", Some("1 day ago")), video("a2", Some("9 days ago"))],
            next: None,
        };
        let b = ChannelUploads {
            sub: sub("UCbbb"),
            videos: vec![video("b1", Some("2 hours ago")), video("b2", Some("30 days ago"))],
            next: None,
        };
        let merged = merge_feed(&[a, b], NOW, FEED_CUTOFF_DAYS);
        let ids: Vec<&str> = merged.iter().map(|i| i.video_id.as_str()).collect();
        assert_eq!(ids, ["b1", "a1", "a2", "b2"]);
    }

    #[test]
    fn videos_older_than_the_cutoff_are_dropped() {
        let dormant = ChannelUploads {
            sub: sub("UCaaa"),
            videos: vec![video("a1", Some("2 years ago")), video("a2", Some("1 day ago"))],
            next: None,
        };
        let merged = merge_feed(&[dormant], NOW, FEED_CUTOFF_DAYS);
        let ids: Vec<&str> = merged.iter().map(|i| i.video_id.as_str()).collect();
        assert_eq!(ids, ["a2"]);
    }

    #[test]
    fn a_video_from_two_subscribed_channels_appears_once() {
        let a = ChannelUploads {
            sub: sub("UCaaa"),
            videos: vec![video("shared", Some("1 day ago"))],
            next: None,
        };
        let b = ChannelUploads {
            sub: sub("UCbbb"),
            videos: vec![video("shared", Some("1 day ago"))],
            next: None,
        };
        let merged = merge_feed(&[a, b], NOW, FEED_CUTOFF_DAYS);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn the_feed_is_empty_when_every_channel_is_outside_the_cutoff() {
        let stale = ChannelUploads {
            sub: sub("UCaaa"),
            videos: vec![video("a1", Some("3 years ago"))],
            next: None,
        };
        assert!(merge_feed(&[stale], NOW, FEED_CUTOFF_DAYS).is_empty());
    }

    #[test]
    fn merge_is_deterministic_for_a_tie_on_time_and_rank() {
        // Paging through a session re-merges, so two rebuilds of the same input
        // must produce byte-identical output or a video can be skipped or shown
        // twice across the boundary.
        let mk = || {
            vec![
                ChannelUploads {
                    sub: sub("UCaaa"),
                    videos: vec![video("x1", Some("1 day ago")), video("x2", Some("1 day ago"))],
                    next: None,
                },
                ChannelUploads {
                    sub: sub("UCbbb"),
                    videos: vec![video("y1", Some("1 day ago")), video("y2", Some("1 day ago"))],
                    next: None,
                },
            ]
        };
        let first: Vec<String> = merge_feed(&mk(), NOW, FEED_CUTOFF_DAYS)
            .into_iter()
            .map(|i| i.video_id)
            .collect();
        let second: Vec<String> = merge_feed(&mk(), NOW, FEED_CUTOFF_DAYS)
            .into_iter()
            .map(|i| i.video_id)
            .collect();
        assert_eq!(first, second);
    }

    /// The exact message from the log that made one age-restricted video lock
    /// the entire app for 90s. It contains "not playable", which the old
    /// heuristic treated as a throttle.
    const AGE_GATED: &str = "innertube: Content is restricted: Video is not playable: \
                             LOGIN_REQUIRED (Sign in to confirm your age)";

    #[test]
    fn age_restricted_video_is_not_a_throttle() {
        assert_eq!(classify_resolve_error(AGE_GATED), ResolveFailure::Video);
    }

    /// The message from the live failure: because it contains "not playable", the
    /// old heuristic called this a throttle; because it contains
    /// "login_required", the `age`-era guard excused it and called it per-video.
    /// So it was treated as one bad video while it was in fact every video.
    const BOT_CHECK: &str = "innertube: Content is restricted: Video is not playable: \
                             LOGIN_REQUIRED (Sign in to confirm you\u{2019}re not a bot) video_id=abc123";

    #[test]
    fn bot_check_is_a_throttle_because_it_hits_every_video() {
        assert_eq!(classify_resolve_error(BOT_CHECK), ResolveFailure::Throttled);
    }

    #[test]
    fn bot_check_survives_a_straight_apostrophe_too() {
        let straight = "LOGIN_REQUIRED (Sign in to confirm you're not a bot)";
        assert_eq!(classify_resolve_error(straight), ResolveFailure::Throttled);
    }

    /// The distinction the old code collapsed: both are `LOGIN_REQUIRED`, but one
    /// is about the IP and the other about the video.
    #[test]
    fn bot_check_and_age_gate_are_classified_apart() {
        assert_ne!(classify_resolve_error(BOT_CHECK), classify_resolve_error(AGE_GATED));
    }

    /// Reporting the IP block as an age gate sends users looking for a setting
    /// that does not exist and cannot fix it.
    #[test]
    fn bot_check_is_not_blamed_on_the_video() {
        let msg = describe_video_failure(BOT_CHECK);
        assert!(
            msg.contains("not a bot"),
            "expected a bot-check explanation, got: {msg}"
        );
        assert!(
            !msg.contains("age-restricted"),
            "must not call a bot check an age gate: {msg}"
        );
    }

    #[test]
    fn age_gate_still_reports_the_age_message() {
        assert!(describe_video_failure(AGE_GATED).contains("age-restricted"));
    }

    /// Backoff must grow with consecutive refusals or a persistent block is
    /// re-tested every 2 minutes forever; and it must stop growing so a long
    /// session cannot lock itself out for an hour.
    #[test]
    fn bot_check_backoff_escalates_then_caps() {
        let first = bot_check_backoff(0);
        let later = bot_check_backoff(3);
        assert!(
            later > first,
            "expected escalation: {later:?} should exceed {first:?}"
        );
        for strike in 0..32 {
            assert!(
                bot_check_backoff(strike) <= BOT_CHECK_BACKOFF_MAX,
                "strike {strike} exceeded the cap"
            );
            assert!(
                bot_check_backoff(strike) >= BOT_CHECK_BACKOFF,
                "strike {strike} dropped below the base backoff"
            );
        }
        assert_eq!(bot_check_backoff(31), BOT_CHECK_BACKOFF_MAX);
    }

    /// A huge counter must not overflow the doubling into a tiny or zero
    /// duration, which would turn "back off" into "hammer harder".
    #[test]
    fn absurd_strike_counts_do_not_produce_a_tiny_backoff() {
        for strike in [u32::MAX, u32::MAX - 1, 1000, 64] {
            let d = bot_check_backoff(strike);
            assert!(
                d >= BOT_CHECK_BACKOFF && d <= BOT_CHECK_BACKOFF_MAX,
                "strike {strike} gave {d:?}"
            );
        }
    }

    #[test]
    fn an_ordinary_throttle_stays_short() {
        // A 429 usually clears on the next request, so it must not inherit the
        // bot-check escalation.
        assert!(THROTTLE_BACKOFF < BOT_CHECK_BACKOFF);
    }

    #[test]
    fn per_video_failures_never_lock_the_network() {
        for e in [
            AGE_GATED,
            "innertube: This video is unavailable",
            "innertube: Video is private",
            "innertube: This video has been removed by the uploader",
            "innertube: ERROR: Video unavailable",
            "innertube: The uploader has not made this video available",
        ] {
            assert_eq!(
                classify_resolve_error(e),
                ResolveFailure::Video,
                "must not lock every video: {e}"
            );
        }
    }

    #[test]
    fn genuine_rate_limits_are_throttles() {
        for e in [
            "http: status code 429 Too Many Requests",
            "innertube: quotaExceeded",
            "innertube: The request is throttled, please retry later",
        ] {
            assert_eq!(
                classify_resolve_error(e),
                ResolveFailure::Throttled,
                "should back off globally: {e}"
            );
        }
    }

    #[test]
    fn age_gate_message_does_not_claim_rate_limiting() {
        let msg = describe_video_failure(AGE_GATED);
        assert!(msg.contains("age-restricted"), "got: {msg}");
        assert!(!msg.contains("rate-limit"), "got: {msg}");
    }

    fn fixture(id: &str) -> innertube::dto::VideoInfoFull {
        use innertube::dto::*;
        innertube::dto::VideoInfoFull {
            info: VideoInfoDto {
                details: VideoDetailsDto {
                    video_id: id.into(),
                    title: "t".into(),
                    length_seconds: 1,
                    description: None,
                    view_count: None,
                    channel_id: "c".into(),
                    author: "a".into(),
                    thumbnails: vec![],
                    publish_date: None,
                },
                adaptive_formats: vec![],
                hls_manifest_url: None,
                best_combined_url: None,
                best_video_url: None,
                best_audio_url: None,
                captions: vec![],
            },
            combined: vec![],
            video_only: vec![],
            audio_only: None,
            video_user_agent: "ua".into(),
            chapters: vec![],
            captions: vec![],
        }
    }

    #[test]
    fn find_caption_matches_exact_lang_then_label() {
        use innertube::dto::{find_caption, CaptionTrack};
        let tracks = vec![
            CaptionTrack {
                lang: "en-US".into(),
                label: "English (US)".into(),
                kind: Some("asr".into()),
                base_url: "https://x.com".into(),
            },
            CaptionTrack {
                lang: "en".into(),
                label: "English (auto-generated)".into(),
                kind: Some("asr".into()),
                base_url: "https://x.com".into(),
            },
        ];
        // Exact language code wins before any label scan.
        assert_eq!(find_caption(&tracks, "en").unwrap().lang, "en");
        // Label fallback is case-insensitive and takes the first containing
        // track; `ENGLISH` lands on "English (US)" here.
        assert_eq!(find_caption(&tracks, "ENGLISH").unwrap().lang, "en-US");
        assert!(find_caption(&tracks, "de").is_none());
    }

    #[test]
    fn resolve_cache_round_trips() {
        let mut c = ResolveCache::new();
        assert!(cache_get(&mut c, "abc123", RESOLVE_TTL).is_none(), "cold miss");
        cache_put(&mut c, "abc123", &fixture("abc123"), RESOLVE_CACHE_MAX);
        let hit = cache_get(&mut c, "abc123", RESOLVE_TTL).expect("warm hit");
        assert_eq!(hit.info.details.video_id, "abc123");
    }

    #[test]
    fn expired_resolve_is_a_miss_and_is_dropped() {
        let mut c = ResolveCache::new();
        cache_put(&mut c, "abc123", &fixture("abc123"), RESOLVE_CACHE_MAX);
        // A zero TTL is the same as "instantly stale" without sleeping.
        assert!(cache_get(&mut c, "abc123", Duration::ZERO).is_none());
        assert!(c.is_empty(), "expired entry must not linger in the map");
    }

    #[test]
    fn resolve_cache_is_bounded_and_drops_the_oldest() {
        let mut c = ResolveCache::new();
        for i in 0..RESOLVE_CACHE_MAX {
            let id = format!("vid{i:04}");
            cache_put(&mut c, &id, &fixture(&id), RESOLVE_CACHE_MAX);
        }
        assert_eq!(c.len(), RESOLVE_CACHE_MAX);
        // Refreshing one video must not grow the map.
        cache_put(&mut c, "vid0000", &fixture("vid0000"), RESOLVE_CACHE_MAX);
        assert_eq!(c.len(), RESOLVE_CACHE_MAX, "cache must stay bounded");
        // ...and must not evict the *newest* entry either.
        assert!(
            cache_get(&mut c, &format!("vid{:04}", RESOLVE_CACHE_MAX - 1), RESOLVE_TTL).is_some(),
            "newest entry was evicted"
        );
    }

    /// Two callers for the same key must result in exactly one `make`, so a
    /// hover pre-resolve racing the click does not spend a second innerTube
    /// request. A slow `make` body opens the window: if the implementation did
    /// not coalesce, the second caller would start its own slow fetch and the
    /// counter would reach two.
    #[tokio::test]
    async fn single_flight_runs_the_fetch_once() {
        let inflight: TokioMutex<HashMap<String, Arc<TokioMutex<Option<Result<i32, String>>>>>> =
            TokioMutex::new(HashMap::new());
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let make = |runs: Arc<std::sync::atomic::AtomicUsize>| move || {
            let runs = runs.clone();
            async move {
                runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                Ok::<i32, String>(42)
            }
        };
        let vid = "vidX".to_string();
        let (a, b) = tokio::join!(
            single_flight(&inflight, &vid, make(runs.clone())),
            single_flight(&inflight, &vid, make(runs.clone())),
        );
        assert_eq!(a, Ok(42));
        assert_eq!(b, Ok(42));
        assert_eq!(
            runs.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "only one caller must reach the fetch body"
        );
    }

    /// Positions in the first/last 5 seconds are treated as not-started /
    /// finished and reset, so a nearly-finished video stops offering a resume.
    #[test]
    fn progress_heartbeat_zeroes_the_edges() {
        assert_eq!(stored_progress(0.0, 180), 0.0);
        assert_eq!(stored_progress(3.0, 180), 0.0, "head = not started");
        assert_eq!(stored_progress(177.0, 180), 0.0, "tail = finished");
        assert_eq!(stored_progress(179.9, 180), 0.0);
        assert_eq!(stored_progress(30.0, 180), 30.0, "middle stays");
        assert_eq!(stored_progress(60.0, 0), 60.0, "unknown length never zeroes");
        assert_eq!(stored_progress(-5.0, 180), 0.0, "negative clamps to not-started");
    }

    /// Reopening a video runs `history_add` with a zero playhead (the watch
    /// page only knows "start" at that point); that must refresh the row's
    /// metadata but never clobber a playhead a previous heartbeat already
    /// saved — otherwise "continue watching" dies the instant you re-enter.
    #[test]
    fn adding_history_again_preserves_the_saved_playhead() {
        let db = crate::db::Db::open_in_memory().expect("db");
        db.upsert_history("v1", "t", "a", "", 180, "UCh", 1, 30.0).expect("first open");
        db.upsert_history("v1", "t2", "a2", "", 180, "UCh", 2, 0.0).expect("reopen");
        let row = db.get_history("v1").expect("row").expect("exists");
        assert_eq!(row.progress_seconds, 30.0, "reopen must keep the playhead");
        assert_eq!(row.title, "t2", "metadata refresh still applies");
        db.update_progress("v1", 0.0, 3).expect("reset");
        let row = db.get_history("v1").expect("row").expect("exists");
        assert_eq!(row.progress_seconds, 0.0, "explicit reset still works");
    }

    /// The subscriptions feed merges channels by parsing YouTube's relative
    /// upload label; a misparse would silently scramble the ordering.
    #[test]
    fn relative_age_parses_the_labels_youtube_emits() {
        assert_eq!(relative_age_secs("2 hours ago"), Some(2 * 3600));
        assert_eq!(relative_age_secs("1 day ago"), Some(86_400));
        assert_eq!(relative_age_secs("3 weeks ago"), Some(3 * 7 * 86_400));
        assert_eq!(relative_age_secs("Streamed 2 years ago"), Some(2 * 365 * 86_400));
        assert_eq!(relative_age_secs("Premiered 5 hours ago"), Some(5 * 3600));
        // Unrecognised labels must not be mistaken for a real age.
        assert_eq!(relative_age_secs("recently"), None);
        assert_eq!(relative_age_secs(""), None);
    }
}
