//! Anonymous playback via a hidden WebView, for when InnerTube refuses the app.
//!
//! # Why this exists
//!
//! Anonymous `/player` requests eventually start coming back
//! `LOGIN_REQUIRED (Sign in to confirm you're not a bot)`. That is YouTube asking
//! the *client* to prove it is not a bot, and it is answerable without an account:
//! a real browser answers it implicitly, because BotGuard runs inside the browser
//! and its attestation rides along on the browser's own network stack. Savvy's
//! direct HTTP calls have no such thing, so once an IP is flagged every client
//! name in rotation is refused and the whole app stops.
//!
//! LibreTube solves this with a hidden Electron window. The same trick is
//! available here, because Tauri on Windows is already WebView2.
//!
//! # Why this is not a PO Token
//!
//! A PO Token is the other way to satisfy the same challenge, but it has to be
//! minted, bound to a client identity, refreshed on a timer and re-pinned whenever
//! YouTube changes BotGuard. This module gets the same result for free: the
//! request is *issued by the browser*, so it is attested by construction. Nothing
//! to mint and nothing to expire.
//!
//! # What is deliberately not changed
//!
//! Only the one refused request moves. The response is deserialised into the same
//! [`innertube_rs::PlayerResponse`] the native path produces, so player response
//! deciphering, variant selection, the progressive/audio split and the muxer are
//! all unchanged. If this module ever returns a response the native path could
//! also have returned, the rest of the app cannot tell the difference.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use tauri::{
    AppHandle, Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

/// Label of the offscreen window. Capabilities are scoped by label, which is what
/// keeps this window's IPC surface small.
pub const WINDOW_LABEL: &str = "player-bridge";

/// Page the bridge loads. It only has to be a real YouTube page so that BotGuard
/// and the innerTube client config are present; nothing is rendered from it.
const BRIDGE_URL: &str = "https://www.youtube.com/";

/// How long to wait for the page to answer one `/player` request.
const BRIDGE_TIMEOUT: Duration = Duration::from_secs(20);

/// Fallback user agent for a response whose page did not report one.
///
/// Only reachable if the page loaded but `navigator.userAgent` was unavailable,
/// which would mean something is badly wrong with the bridge. The stream requests
/// still have to name *a* browser, so this is the same desktop Chrome string the
/// native path uses.
fn default_bridge_user_agent() -> String {
    innertube::dto::WEB_USER_AGENT.to_string()
}

/// Injected into the bridge window.
///
/// Kept as a plain string rather than a separate asset so it is obvious at the
/// call site that this code executes inside `youtube.com`.
///
/// Reporting deliberately does **not** use Tauri's IPC. The page runs on the
/// remote `youtube.com` origin, where that bridge was never reachable, so every
/// report silently vanished and the resolve only ever timed out. A `fetch` POST
/// back to the app's own loopback port works from any origin.
const BRIDGE_SCRIPT: &str = r#"
(function () {
  if (window.__savvyPlayer) { return; }

  var REPORT_URL = '__REPORT_URL__';

  // Stage markers travel over the same channel as the result. When the bridge
  // misbehaves the stage that arrived last is the only evidence of how far the
  // page got, and it costs nothing to send.
  function post(payload) {
    return fetch(REPORT_URL, {
      method: 'POST',
      mode: 'cors',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload)
    }).then(function (r) { return r.ok; });
  }

  function fail(videoId, stage, message) {
    post({ videoId: videoId, stage: stage, error: String(message) });
  }

  // The innerTube context and key are minted by the page itself. Reading them
  // rather than hard-coding them keeps the request identical to the one this page
  // would make, which is the entire point of the exercise.
  function innerTubeConfig() {
    var cfg = window.ytcfg;
    if (!cfg) { return null; }
    var key = cfg.get('INNERTUBE_API_KEY');
    var context = cfg.get('INNERTUBE_CONTEXT');
    if (!key || !context) { return null; }
    return { key: key, context: context };
  }

  // The object value of `var ytInitialPlayerResponse = {...}` between the marker
  // and the end of its script tag. Brace counting is safe enough here because a
  // closing brace decides the boundary and stray braces inside strings are rare;
  // a parse failure simply falls through to the /player POST.
  function playerResponseFromHtml(html) {
    var marker = 'ytInitialPlayerResponse';
    var idx = html.indexOf(marker);
    if (idx < 0) { return null; }
    var scriptEnd = html.indexOf('</script>', idx);
    if (scriptEnd < 0) { scriptEnd = html.length; }
    var chunk = html.slice(idx, scriptEnd);
    var start = chunk.indexOf('{');
    if (start < 0) { return null; }
    var depth = 0;
    var i = start;
    for (; i < chunk.length; i++) {
      if (chunk.charAt(i) === '{') { depth++; }
      else if (chunk.charAt(i) === '}') {
        depth--;
        if (depth === 0) { break; }
      }
    }
    if (depth !== 0) { return null; }
    try { return JSON.parse(chunk.slice(start, i + 1)); }
    catch (e) { return null; }
  }

  // The server-rendered watch page carries `ytInitialPlayerResponse` before any
  // API round trip. Unlike the anonymous /player POST - which YouTube answers
  // with LOGIN_REQUIRED while a session is still building trust - reading the
  // render needs no BotGuard attestation. It is the same bytes a working browser
  // tab gets, so it is the more reliable source whenever it contains the video.
  function pagePlayerResponse(videoId) {
    return fetch('/watch?v=' + encodeURIComponent(videoId), {
      method: 'GET',
      credentials: 'include',
      headers: { 'Accept': 'text/html' }
    })
      .then(function (r) { return r.text(); })
      .then(playerResponseFromHtml)
      .catch(function () { return null; });
  }

  // A refusal from the page render is still a refusal: do not declare a video
  // playable merely because an error object happened to parse.
  function looksPlayable(json) {
    if (!json) { return false; }
    var st = json.playabilityStatus;
    if (!st || !st.status) { return false; }
    if (st.status === 'LOGIN_REQUIRED' || st.status === 'ERROR' ||
        st.status === 'UNPLAYABLE') { return false; }
    return true;
  }

  // Announce ourselves the moment the script runs, before anything is asked of
  // us. Without this the two ways the bridge can be broken - script never
  // injected, versus script injected but never invoked - look identical from
  // Rust, because a failing eval reports nothing at all.
  post({ videoId: '', stage: 'boot', userAgent: navigator.userAgent })
    .catch(function () {});

  window.__savvyPlayer = function (videoId) {
    post({ videoId: videoId, stage: 'dispatched',
           userAgent: navigator.userAgent });

    // Prefer the page render (no BotGuard needed). Fall back to the API POST
    // exactly as before, so a read failure can never strand a resolve.
    pagePlayerResponse(videoId).then(function (pageJson) {
      if (looksPlayable(pageJson)) {
        post({ videoId: videoId, stage: 'done', source: 'page', json: pageJson,
               userAgent: navigator.userAgent });
        return;
      }
      post({ videoId: videoId, stage: 'page-attempt',
             pageStatus: pageJson && pageJson.playabilityStatus
               ? pageJson.playabilityStatus.status
               : (pageJson ? 'no-playability' : 'unparsed'),
             userAgent: navigator.userAgent }).catch(function () {});

      var attempt = 0;
      function tryFetch() {
        var cfg = innerTubeConfig();
        if (!cfg) {
          // BotGuard / ytcfg not ready yet. A handful of retries covers page load;
          // after that give up so the Rust side can time out rather than spin.
          if (attempt++ < 100) {
            setTimeout(tryFetch, 100);
            return;
          }
          fail(videoId, 'no-config', 'innertube config never appeared on the page');
          return;
        }

        fetch('/youtubei/v1/player?prettyPrint=false&key=' + cfg.key, {
          method: 'POST',
          credentials: 'include',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({
            videoId: videoId,
            context: cfg.context,
            contentCheckOk: true,
            racyCheckOk: true
          })
        })
          .then(function (r) { return r.json(); })
          .then(function (json) {
            post({ videoId: videoId, stage: 'done', source: 'post', json: json,
                   userAgent: navigator.userAgent });
          })
          .catch(function (e) {
            fail(videoId, 'fetch-failed', 'page-context /player fetch failed: ' + e);
          });
      }
      tryFetch();
    });
  };
})();
"#;

/// What the bridge page reports back.
///
/// `stage` is progress-only telemetry; only the terminal stages carry `json` or
/// `error`. It is what makes a silent failure legible in the log.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeReport {
    pub video_id: String,
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub json: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<String>,
    /// Where the answer came from: the server-rendered watch page (`page`) or the
    /// page-context `/player` POST (`post`).
    #[serde(default)]
    pub source: Option<String>,
    /// Playability status of the server-rendered watch page, or `unparsed` /
    /// `no-playability` when it could not be read. Only sent on `page-attempt`.
    #[serde(default)]
    pub page_status: Option<String>,
}

impl BridgeReport {
    /// A report that carries an actual answer, rather than progress.
    fn is_terminal(&self) -> bool {
        self.json.is_some() || self.error.is_some()
    }
}

struct Pending {
    video_id: String,
    sender: tokio::sync::oneshot::Sender<Result<BridgeAnswer, String>>,
}

/// A player response plus the identity that fetched it.
///
/// The user agent travels with the response because it is the one the CDN sees,
/// and hard-coding a Chrome version made the stream requests disagree with the
/// `/player` request that produced their URLs.
#[derive(Clone)]
pub struct BridgeAnswer {
    pub raw: String,
    pub user_agent: String,
    pub source: String,
}

/// The loopback slot a bridge report is handed to.
///
/// Deliberately free of any `AppHandle`. The stream proxy - which receives the
/// report - is spawned while the app state is being built, long before Tauri has
/// handed out an `AppHandle` for the bridge window to be created from. Holding
/// the slot here breaks that ordering problem: whoever is waiting registers a
/// sender, and the proxy just writes reports into whichever slot is filled.
#[derive(Clone, Default)]
pub struct BridgeInbox {
    pending: Arc<Mutex<Option<Pending>>>,
}

impl BridgeInbox {
    pub fn new() -> Self {
        Self::default()
    }

    /// Callback handed to the loopback server.
    ///
    /// Cheap and infallible: an unreadable or stale report is logged and dropped
    /// rather than turned into an HTTP error the page would have to interpret.
    pub fn sink(&self) -> crate::proxy::BridgeSink {
        let inbox = self.clone();
        Arc::new(move |value: serde_json::Value| {
            let raw = value.to_string();
            let report: BridgeReport = match serde_json::from_value(value) {
                Ok(r) => r,
                Err(e) => {
                    let head = &raw[..raw.len().min(400)];
                    tracing::warn!("player bridge report did not parse: {e} :: {head}");
                    return;
                }
            };
            inbox.deliver(report);
        })
    }

    /// Resolve a waiting request, or log why the report could not be placed.
    fn deliver(&self, report: BridgeReport) {
        // The page reports its progress too, so a request that is still in flight
        // can be distinguished from one that was never made.
        let stage = report.stage.as_deref().unwrap_or("unspecified");
        tracing::info!(
            video_id = %report.video_id,
            stage,
            has_json = report.json.is_some(),
            page_status = report.page_status.as_deref().unwrap_or("<none>"),
            "player bridge report"
        );

        let Ok(mut slot) = self.pending.lock() else {
            tracing::warn!("player bridge slot lock poisoned");
            return;
        };

        match slot.as_ref() {
            Some(p) if p.video_id != report.video_id => {
                // Late reply to a superseded request.
                tracing::debug!(
                    got = %report.video_id,
                    waiting_for = %p.video_id,
                    "player bridge report is for a stale request"
                );
            }
            None => {
                tracing::debug!("player bridge report arrived with nothing waiting");
            }
            Some(_) => {
                if !report.is_terminal() {
                    return;
                }
                let user_agent = report.user_agent.clone();
                let pending = slot.take().expect("checked above");
                let answer = match (report.json, report.error) {
                    (Some(json), _) => {
                        let raw = serde_json::to_string(&json).unwrap_or_default();
                        let source = report.source.as_deref().unwrap_or("post");
                        tracing::info!(
                            video_id = %report.video_id,
                            stage,
                            source,
                            bytes = raw.len(),
                            user_agent = user_agent.as_deref().unwrap_or("<none>"),
                            "player bridge delivered a player response"
                        );
                        Ok(BridgeAnswer {
                            raw,
                            user_agent: user_agent.unwrap_or_else(default_bridge_user_agent),
                            source: source.to_string(),
                        })
                    }
                    (None, Some(err)) => {
                        tracing::warn!(video_id = %report.video_id, error = %err,
                                       "player bridge reported a failure");
                        Err(format!("player bridge: {err}"))
                    }
                    (None, None) => {
                        Err("player bridge sent an empty report".to_string())
                    }
                };
                let _ = pending.sender.send(answer);
            }
        }
    }

    /// Reserve the slot for a request, superseding any request still in flight.
    fn begin(
        &self,
        video_id: &str,
    ) -> tokio::sync::oneshot::Receiver<Result<BridgeAnswer, String>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if let Ok(mut slot) = self.pending.lock() {
            if let Some(stale) = slot.take() {
                // A caller gave up while this was in flight. Fail it explicitly
                // rather than leaving its await hanging forever.
                let _ = stale
                    .sender
                    .send(Err("superseded by a newer bridge request".to_string()));
            }
            *slot = Some(Pending {
                video_id: video_id.to_string(),
                sender: tx,
            });
        }
        rx
    }

    /// Drop the reservation for `video_id` if it is still the current one.
    fn clear(&self, video_id: &str) {
        if let Ok(mut slot) = self.pending.lock() {
            if slot.as_ref().is_some_and(|p| p.video_id == video_id) {
                *slot = None;
            }
        }
    }
}

/// Shared state for the offscreen window and any request in flight.
///
/// Behind an `Arc` because clones must observe the *same* cache and load state. The
/// pending slot is not here: it lives in [`BridgeInbox`] so the loopback server
/// can reach it without an `AppHandle`.
struct BridgeShared<R: Runtime> {
    app: AppHandle<R>,
    /// Where the page reports its answer.
    inbox: BridgeInbox,
    /// Loopback URL the page POSTs to, injected into the script.
    report_url: String,
    /// Set once the bridge document has finished loading. `eval` before this point
    /// is silently dropped, so dispatch waits on it.
loaded: Arc<AtomicBool>,
    /// Set once the static trampoline document has loaded. The bridge waits for
    /// this before navigating, because the injected script only exists on that
    /// document.
    at_trampoline: Arc<AtomicBool>,
    /// Set once the navigation to YouTube has been issued, so it happens exactly
    /// once per window.
    navigated: Arc<AtomicBool>,
    /// video id -> last usable player response, so a retry after a timeout does
    /// not have to pay for a second page load. Refusals are never stored.
    cache: Mutex<HashMap<String, BridgeAnswer>>,
}

pub struct PlayerBridge<R: Runtime>(Arc<BridgeShared<R>>);

impl<R: Runtime> Clone for PlayerBridge<R> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<R: Runtime> PlayerBridge<R> {
    pub fn new(app: AppHandle<R>, inbox: BridgeInbox, report_url: String) -> Self {
        Self(Arc::new(BridgeShared {
            app,
            inbox,
            report_url,
            loaded: Arc::new(AtomicBool::new(false)),
            at_trampoline: Arc::new(AtomicBool::new(false)),
            navigated: Arc::new(AtomicBool::new(false)),
            cache: Mutex::new(HashMap::new()),
        }))
    }

/// Fetches a player response for `video_id` from the hidden webview.
    ///
    /// Returns the raw JSON plus the user agent the page used, which the caller
    /// deserialises into the same [`innertube_rs::PlayerResponse`] the native path
    /// yields.
    pub async fn player_response(&self, video_id: &str) -> Result<BridgeAnswer, String> {
        // Reuse a recent answer. The bridge is the scarce resource here, and a
        // player response for one video is interchangeable with the next request
        // for that same video.
        if let Some(hit) = self
            .0
            .cache
            .lock()
            .map_err(|_| "player bridge cache lock poisoned".to_string())?
            .get(video_id)
            .cloned()
        {
            tracing::debug!(video_id, "player bridge cache hit");
            return Ok(hit);
        }

let rx = self.0.inbox.begin(video_id);

        if let Err(e) = self.dispatch(video_id).await {
            self.0.inbox.clear(video_id);
            return Err(e);
        }

        let answer = match tokio::time::timeout(BRIDGE_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("player bridge dropped the request".to_string()),
            Err(_) => Err(format!(
                "player bridge did not answer within {}s",
                BRIDGE_TIMEOUT.as_secs()
            )),
        };

        self.0.inbox.clear(video_id);

if let Ok(hit) = &answer {
            // Only cache a real answer. Caching a refusal would pin the video to
            // "unavailable" for the rest of the session, long after the block
            // that caused it has gone.
            if hit.raw.contains("\"status\":\"OK\"") || !hit.raw.contains("\"status\"") {
                if let Ok(mut cache) = self.0.cache.lock() {
                    // Small: one entry per video the user actually opened, and the
                    // session ends with the app anyway.
                    if cache.len() > 32 {
                        cache.clear();
                    }
                    cache.insert(video_id.to_string(), hit.clone());
                }
            } else {
                tracing::debug!(video_id, "not caching an unplayable bridge response");
            }
        }

        answer
    }

/// Creates the window if needed, then waits for it to be usable.
    ///
    /// The trampoline's load is what arms the navigation, and the navigation is
    /// what produces the document the bridge script lives on. Both are polled
    /// rather than pushed: doing work inside `on_page_load` re-enters WebView2
    /// mid-navigation and takes the process down with it.
    async fn ensure_window(&self) -> Result<WebviewWindow<R>, String> {
        let existed = self.0.app.get_webview_window(WINDOW_LABEL).is_some();
        let window = match self.0.app.get_webview_window(WINDOW_LABEL) {
            Some(w) => w,
            None => {
                let window = self.build_window()?;
                // Belt and braces against the creation flash on Windows: `visible`
                // is applied after the HWND exists.
                let _ = window.hide();
                window
            }
        };

        let deadline = std::time::Instant::now() + BRIDGE_TIMEOUT;
        loop {
            // Arm the navigation exactly once, after the script exists.
            if self.0.at_trampoline.load(Ordering::SeqCst)
                && !self.0.navigated.swap(true, Ordering::SeqCst)
            {
                let target = serde_json::to_string(BRIDGE_URL).unwrap_or_default();
                window
                    .eval(&format!("window.location.replace({target})"))
                    .map_err(|e| format!("could not navigate the player bridge: {e}"))?;
                tracing::info!("player bridge navigating to YouTube");
            }

            if self.0.loaded.load(Ordering::SeqCst) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                // A window that was already open and has been sitting on YouTube
                // needs no trampoline, so treat a live window as progress.
                if existed {
                    tracing::warn!("player bridge window existed but never finished loading");
                }
                return Err(format!(
                    "player bridge page never finished loading within {}s",
                    BRIDGE_TIMEOUT.as_secs()
                ));
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }

        // Never let a stray focus or visibility change show this window.
        let _ = window.hide();
        Ok(window)
    }

    /// Waits for the page to be ready, then pokes it.
    ///
    /// No acknowledgement loop: `eval` returns `Ok` even when the webview has no
    /// document to run in, so a window title proves nothing - it reports the
    /// native caption, which `document.title` never changes. The page's own
    /// `dispatched` stage report over loopback is the real signal, and the caller
    /// already waits for the terminal one.
    async fn dispatch(&self, video_id: &str) -> Result<(), String> {
        let window = self.ensure_window().await?;
        // The page's entry point takes the bare id: `window.__savvyPlayer("abc")`.
        // Wrapping it in `{ videoId }` passed an *object* as the argument, which
        // the page then nested inside its own request body — YouTube answered
        // `INVALID_ARGUMENT (Invalid value (video_id))` and the report failed to
        // parse on the way back. Serialise the string itself, not a wrapper.
        let arg = serde_json::to_string(video_id)
            .map_err(|e| format!("could not encode bridge request: {e}"))?;
        window
            .eval(&format!("window.__savvyPlayer({arg})"))
            .map_err(|e| format!("could not reach the player bridge page: {e}"))
    }

    /// The injected script with this run's loopback report URL substituted in.
///
/// Substituted at window-creation time because the port is only known once the
/// proxy has bound, and a script that guessed it would post nowhere.
fn script(&self) -> String {
    BRIDGE_SCRIPT.replace("__REPORT_URL__", &self.0.report_url)
}

fn build_window(&self) -> Result<WebviewWindow<R>, String> {
        let loaded = self.0.loaded.clone();
        let at_trampoline = self.0.at_trampoline.clone();
        let script = self.script();

        // `initialization_script` runs before the page's own scripts, which is
        // what makes `window.__savvyPlayer` available. A static `about:blank`
        // trampoline is used to create the webview first: injecting a script and
        // navigating in the same call raced, and a lost race meant the page loaded
        // with no bridge script on it at all - the failure that made every eval a
        // silent no-op.
        let window = WebviewWindowBuilder::new(
            &self.0.app,
            WINDOW_LABEL,
            WebviewUrl::App("bridge.html".into()),
        )
        .title("player bridge")
        // Offscreen: never shown, never focused, never in the taskbar. This window
        // exists only to give the page a real browser network stack.
        .decorations(false)
        .visible(false)
        .focused(false)
        .skip_taskbar(true)
        // Windows briefly shows a new window before `visible(false)` is applied.
        // Parking it off-screen removes that flash for good.
        .position(-32_000.0, -32_000.0)
        .inner_size(640.0, 480.0)
        .initialization_script(script.clone())
        .on_page_load(move |_, payload| {
            // Record only. Calling `eval` from inside this callback re-enters
            // WebView2 while it is still running its own navigation, which kills
            // the process outright. The navigation itself is issued later, from
            // `ensure_window`, on a different thread.
            if payload.event() != tauri::webview::PageLoadEvent::Finished {
                return;
            }
            let url = payload.url().as_str().to_string();
            // Identify the page by *what* it is, not by how it is served. In a
            // production build the trampoline is `tauri://localhost/bridge.html`,
            // but under `devUrl` the same asset is served by the dev server as
            // `http://localhost:<port>/bridge.html`. Matching the scheme alone
            // missed the dev URL, so `at_trampoline` never latched, the window
            // never navigated to YouTube, and every bridge attempt stalled until
            // the 20s timeout.
            if url.contains("youtube.com") {
                tracing::info!(url, "player bridge page loaded");
                loaded.store(true, Ordering::SeqCst);
            } else {
                at_trampoline.store(true, Ordering::SeqCst);
            }
        })
        .build()
        .map_err(|e| format!("could not open the player bridge window: {e}"))?;

        tracing::info!("player bridge window opened");
        Ok(window)
    }
}

