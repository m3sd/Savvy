//! Loopback HTTP proxy that hands googlevideo stream URLs to the webview.
//!
//! The webview's `<video>` can't directly hit `googlevideo.com`: the request
//! would carry an `Origin` header from `http://localhost:5183` (or
//! `tauri://localhost` in production), which Google rejects. Our proxy
//! hides that — Rust fetches the upstream with reqwest (no Origin header),
//! then re-streams the bytes back to the player with permissive CORS.

use std::collections::{HashMap, VecDeque};
use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::body::{Bytes, Frame};
use hyper::header::HeaderValue;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rand::RngCore;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

/// Cap on live tokens. One resolve mints a token per rendition it hands back
/// (twenty or more), so this is a handful of minutes of browsing rather than a
/// handful of videos — the bound is here to stop a day-long session growing
/// without limit, not to be reached in normal use.
const MAX_TOKENS: usize = 2048;

/// Read size when streaming a local cache file back to the webview.
const FILE_CHUNK: usize = 128 * 1024;

/// What a token points at: a remote googlevideo URL, or a file we produced
/// locally. The extracted-audio cache and the finished remux are the second
/// case — both are ordinary seekable files, so they can honour `Range` without
/// any googlevideo involved. `live` marks a file that ffmpeg is still writing,
/// which is streamed with no `Content-Length` until it is renamed into place.
#[derive(Clone)]
pub enum Target {
    Upstream(String),
    Local { path: PathBuf, mime: String },
    /// A partial file that is still growing.
    Growing { path: PathBuf, mime: String },
    /// Re-serve an upstream URL as a plain sequential byte stream, for ffmpeg.
    ///
    /// `user_agent` is not cosmetic: googlevideo meters reads per InnerTube
    /// client and checks the media request's User-Agent against the client that
    /// issued the URL. Relay with the wrong one and the CDN throttles the
    /// rendition exactly as it did before.
    Relay { url: String, user_agent: String },
}

/// In-memory table mapping opaque tokens to their target.
///
/// Eviction is least-recently-used, never wholesale: the video on screen holds
/// a token that was minted when it was opened, and emptying the table on
/// overflow (the previous behaviour) 404'd whatever was playing at that moment.
/// A retry only worked because the resolve cache had rotated in the meantime.
#[derive(Default)]
pub struct StreamTable {
    map: HashMap<String, Target>,
    /// Tokens ordered least-recently-read first; a lookup moves its token here.
    lru: VecDeque<String>,
}

impl StreamTable {
    pub fn insert(&mut self, url: String) -> String {
        self.insert_target(Target::Upstream(url))
    }

    /// Mint a token for a locally produced file.
    pub fn insert_local(&mut self, path: PathBuf, mime: String) -> String {
        self.insert_target(Target::Local { path, mime })
    }

    /// Mint a token for a file that ffmpeg is still writing.
    pub fn insert_growing(&mut self, path: PathBuf, mime: String) -> String {
        self.insert_target(Target::Growing { path, mime })
    }

    /// Mint a token that re-serves `url` as a sequential stream. Only the
    /// remux uses this — never the webview.
    pub fn insert_relay(&mut self, url: String, user_agent: String) -> String {
        self.insert_target(Target::Relay { url, user_agent })
    }

    fn insert_target(&mut self, target: Target) -> String {
        let mut buf = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut buf);
        let token = hex(&buf);
        self.map.insert(token.clone(), target);
        self.lru.push_back(token.clone());
        self.evict();
        token
    }

    /// Drop the least-recently-read tokens until the table is back under
    /// [`MAX_TOKENS`].
    fn evict(&mut self) {
        let mut dropped = 0usize;
        while self.map.len() > MAX_TOKENS {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            if self.map.remove(&oldest).is_some() {
                dropped += 1;
            }
        }
        if dropped > 0 {
            tracing::info!(
                dropped,
                remaining = self.map.len(),
                "stream table over cap; evicted the least-recently-read tokens"
            );
        }
    }

    /// Look up a token and record that it is in use.
    ///
    /// The touch matters more than the cap: the rendition being streamed right
    /// now is read every second or so, while the tokens minted for the quality
    /// picker are read only if that quality is chosen. Recording the read keeps
    /// the playing video ahead of the pile of tokens a session accumulates.
    pub fn lookup(&mut self, token: &str) -> Option<&Target> {
        if !self.map.contains_key(token) {
            return None;
        }
        if let Some(pos) = self.lru.iter().position(|t| t == token) {
            if let Some(most_recent) = self.lru.remove(pos) {
                self.lru.push_back(most_recent);
            }
        }
        self.map.get(token)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

pub type SharedTable = Arc<Mutex<StreamTable>>;

/// (video id, language code) → the WebVTT body of that caption track.
///
/// Populated by the `captions_fetch` command and served by the `/captions` route
/// so the player's native `<track>` never talks to YouTube directly — the same
/// token/UA discipline the media streams follow.
pub type SharedCaptions = Arc<Mutex<HashMap<(String, String), String>>>;

pub fn new_table() -> SharedTable {
    Arc::new(Mutex::new(StreamTable::default()))
}

/// Result of binding the proxy.
pub struct ProxyHandle {
    pub addr: SocketAddr,
    pub table: SharedTable,
}

/// Bind to a random localhost port and spawn the server task.
///
/// No bridge: reports are dropped. Used by tests and by anything that never
/// creates a bridge window.
pub async fn spawn() -> anyhow::Result<ProxyHandle> {
    spawn_with_captions(
        Arc::new(|_| {
            tracing::debug!("dropping a player bridge report: no bridge is registered");
        }),
        Arc::new(Mutex::new(HashMap::new())),
    )
    .await
}

/// Bind to a random localhost port and route player bridge reports to `bridge`.
///
/// Caption VTTs are served from an empty store: only the app-side spawn passes
/// a real one in.
pub async fn spawn_with_bridge(bridge: BridgeSink) -> anyhow::Result<ProxyHandle> {
    spawn_with_captions(bridge, Arc::new(Mutex::new(HashMap::new()))).await
}

/// Bind to a random localhost port, route player bridge reports to `bridge`,
/// and serve caption VTTs from `captions`.
pub async fn spawn_with_captions(
    bridge: BridgeSink,
    captions: SharedCaptions,
) -> anyhow::Result<ProxyHandle> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0))).await?;
    let addr = listener.local_addr()?;
    let table = new_table();
    let t = table.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let io = TokioIo::new(stream);
                    let table = t.clone();
                    let bridge = bridge.clone();
                    let captions = captions.clone();
                    tokio::spawn(async move {
                        let svc = service_fn(move |req| {
                            let table = table.clone();
                            let bridge = bridge.clone();
                            let captions = captions.clone();
                            async move { handle(req, table, bridge, captions).await }
                        });
                        if let Err(e) = http1::Builder::new().serve_connection(io, svc).await {
                            tracing::debug!("proxy conn error: {e}");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!("proxy accept error: {e}");
                    break;
                }
            }
        }
    });
    Ok(ProxyHandle { addr, table })
}

const CORS_HEADERS: [(&str, &str); 4] = [
    ("Access-Control-Allow-Origin", "*"),
    ("Access-Control-Allow-Methods", "GET, OPTIONS, HEAD"),
    ("Access-Control-Allow-Headers", "Range, Content-Type, Origin"),
    (
        "Access-Control-Expose-Headers",
        "Content-Length, Content-Range, Accept-Ranges",
    ),
];

/// Loopback path the player bridge page POSTs its `/player` result to.
///
/// The page runs on the `youtube.com` origin, where Tauri's IPC bridge is not
/// reachable, so a plain HTTP POST back to our own loopback server is the only
/// return path that is guaranteed to exist. It is a plain `fetch`, not a
/// navigation, and it is capped to a size a player response cannot exceed.
pub const BRIDGE_PATH: &str = "/bridge/player";

/// Upper bound on an accepted bridge report. A `/player` response with every
/// format is comfortably under this; the cap exists so a bug or a hostile page
/// cannot make the app buffer without limit.
const BRIDGE_MAX_BODY: usize = 4 * 1024 * 1024;

/// How a bridge report reaches the waiting `resolve`.
pub type BridgeSink = Arc<dyn Fn(serde_json::Value) + Send + Sync>;

/// Accept a report from the bridge page and hand it to the pending request.
///
/// Returns 200 on a handoff, 400 for an unreadable body, and 409 when no request
/// is waiting - the last one matters because it tells the page to stop waiting
/// rather than retry a slot nobody is listening on.
async fn serve_bridge(
    req: Request<hyper::body::Incoming>,
    sink: BridgeSink,
) -> Response<ProxyBody> {
    // The report is a JSON POST from the `youtube.com` origin, so the browser
    // preflights it. Answering that preflight with 405 (the generic path for a
    // non-POST) fails the CORS check and the browser drops the POST before its
    // body is ever sent — which is indistinguishable, from Rust, from a bridge
    // that never answers. This only answered POST, and that was the whole
    // reason the bridge reported nothing.
    if req.method() == Method::OPTIONS {
        return with_bridge_cors(empty(StatusCode::NO_CONTENT));
    }
    if req.method() != Method::POST {
        return with_bridge_cors(empty(StatusCode::METHOD_NOT_ALLOWED));
    }

    let body = match http_body_util::BodyExt::collect(req.into_body()).await {
        Ok(b) => b.to_bytes(),
        Err(e) => {
            tracing::warn!("player bridge body read error: {e}");
            return with_bridge_cors(empty(StatusCode::BAD_REQUEST));
        }
    };
    if body.len() > BRIDGE_MAX_BODY {
        tracing::warn!("player bridge report too large: {} bytes", body.len());
        return with_bridge_cors(empty(StatusCode::PAYLOAD_TOO_LARGE));
    }

    let Ok(report) = serde_json::from_slice::<serde_json::Value>(&body) else {
        tracing::warn!("player bridge sent a report that is not JSON");
        return with_bridge_cors(empty(StatusCode::BAD_REQUEST));
    };

    sink(report);
    with_bridge_cors(empty(StatusCode::OK))
}

fn with_bridge_cors(mut r: Response<ProxyBody>) -> Response<ProxyBody> {
    for (k, v) in BRIDGE_CORS_HEADERS {
        r.headers_mut().insert(k, v.parse().unwrap());
    }
    r
}

/// Either a buffered error body or a live passthrough of the upstream body.
type ProxyBody = BoxBody<Bytes, io::Error>;

/// Headers the player bridge's cross-origin `fetch` needs.
///
/// The bridge page runs on `youtube.com`, so its POST is cross-origin and gets a
/// `Content-Type: application/json` preflighted. Without `POST` and `OPTIONS` in
/// `Access-Control-Allow-Methods` the browser rejects it before the body is ever
/// sent - which looks exactly like the bridge never answering. `127.0.0.1` is a
/// secure context, so no `Access-Control-Allow-Credentials` is required and the
/// wildcard origin is valid.
const BRIDGE_CORS_HEADERS: [(&str, &str); 3] = [
    ("Access-Control-Allow-Origin", "*"),
    ("Access-Control-Allow-Methods", "POST, OPTIONS"),
    ("Access-Control-Allow-Headers", "Content-Type"),
];

/// Headers that must survive the hop untouched — dropping or rewriting
/// `Content-Range`/`Content-Length` alongside the status breaks the player's
/// range bookkeeping and stalls seeking.
const FORWARD_HEADERS: [&str; 7] = [
    "content-type",
    "content-length",
    "content-range",
    "accept-ranges",
    "last-modified",
    "etag",
    "location",
];

fn respond(status: StatusCode, body: Bytes) -> Response<ProxyBody> {
    let mut r = Response::new(Full::new(body).map_err(|e: Infallible| match e {}).boxed());
    *r.status_mut() = status;
    r
}

fn empty(status: StatusCode) -> Response<ProxyBody> {
    respond(status, Bytes::new())
}

fn text(status: StatusCode, msg: &str) -> Response<ProxyBody> {
    let mut r = respond(status, Bytes::from(msg.to_string()));
    r.headers_mut()
        .insert("Content-Type", "text/plain".parse().unwrap());
    r
}

fn with_cors(mut r: Response<ProxyBody>) -> Response<ProxyBody> {
    for (k, v) in CORS_HEADERS {
        r.headers_mut().insert(k, v.parse().unwrap());
    }
    r
}

/// One client for every upstream response.
///
/// Each media element opens its own connection, and the player issues the audio
/// track's request a couple hundred milliseconds after the video's. Building a
/// client per request threw that gap away: a fresh pool means a fresh TLS
/// handshake before the first byte, on a path where the client is already
/// waiting on it. Reusing one also keeps the connections pooled between seeks.
pub(crate) fn upstream_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new)
}

/// Loopback path a caption VTT is served from: `/captions/{video_id}/{lang}.vtt`.
///
/// Same idea as `/stream`: the webview never touches YouTube's timedtext, it
/// only reads the VTT we stored during `captions_fetch`.
pub const CAPTIONS_PATH: &str = "/captions/";

async fn handle(
    req: Request<hyper::body::Incoming>,
    table: SharedTable,
    sink: BridgeSink,
    captions: SharedCaptions,
) -> Result<Response<ProxyBody>, Infallible> {
    let path = req.uri().path().to_string();

    if path == BRIDGE_PATH {
        return Ok(serve_bridge(req, sink).await);
    }

    if req.method() == Method::OPTIONS {
        return Ok(with_cors(empty(StatusCode::NO_CONTENT)));
    }

    if let Some(rest) = path.strip_prefix(CAPTIONS_PATH) {
        return Ok(serve_captions(&captions, &rest).await);
    }

    let Some(token) = path.strip_prefix("/stream/") else {
        return Ok(with_cors(empty(StatusCode::NOT_FOUND)));
    };

    let target = {
        let mut g = table.lock().await;
        g.lookup(token).cloned()
    };
    let Some(target) = target else {
        // The one failure that used to be invisible: the player sees a 404 and
        // shows a dead video, and the log said nothing. A stale token is now
        // identifiable at a glance (it is either evicted or from a restart).
        tracing::info!(%token, "no such stream token; answering 404");
        return Ok(with_cors(empty(StatusCode::NOT_FOUND)));
    };

    // Forward the method and the client's Range header verbatim. Google's CDN
    // answers ranged reads with 206 + Content-Range, and the media element
    // depends on seeing that exact pairing.
    let method = reqwest::Method::from_bytes(req.method().as_str().as_bytes())
        .unwrap_or(reqwest::Method::GET);
    let range = req
        .headers()
        .get("range")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);

    match target {
        Target::Local { path, mime } => {
            return Ok(serve_local(&path, &mime, range.as_deref()).await)
        }
        Target::Growing { path, mime } => {
            return Ok(serve_growing(&path, &mime, range.as_deref()).await)
        }
        Target::Relay { url, user_agent } => {
            return Ok(serve_relay(&url, &user_agent, range.as_deref()).await)
        }
        Target::Upstream(url) => {
            let mut upstream = upstream_client().request(method, &url);
            if let Some(range) = &range {
                upstream = upstream.header("Range", range);
            }

            let upstream_resp = match upstream.send().await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("proxy upstream error: {e}");
                    return Ok(with_cors(text(
                        StatusCode::BAD_GATEWAY,
                        &format!("upstream: {e}"),
                    )));
                }
            };

            // Pass the real status through rather than collapsing to 200/302.
            let status = StatusCode::from_u16(upstream_resp.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY);

            // Adaptive renditions 403 a plain GET, so a missing Range here is
            // the difference between a working stream and a dead one.
            tracing::info!(
                range = range.as_deref().unwrap_or("<none>"),
                ?status,
                "proxy upstream"
            );

            let forwarded: Vec<(&'static str, HeaderValue)> = FORWARD_HEADERS
                .iter()
                .filter_map(|h| upstream_resp.headers().get(*h).map(|v| (*h, v.clone())))
                .collect();

            // Stream the body through instead of buffering a whole video in memory;
            // the player can then start decoding as soon as the first chunk lands.
            let stream = upstream_resp.bytes_stream().map(|chunk| {
                chunk
                    .map(Frame::data)
                    .map_err(|e| io::Error::other(e.to_string()))
            });

            let mut resp = Response::new(BodyExt::boxed(StreamBody::new(stream)));
            *resp.status_mut() = status;
            for (k, v) in forwarded {
                resp.headers_mut().insert(k, v);
            }

            Ok(with_cors(resp))
        }
    }
}

/// Serve a caption VTT that `captions_fetch` cached for `(video_id, lang)`.
///
/// The key is minted server-side from the player response's track list, so the
/// only way in is a well-formed `/captions/{id}/{lang}.vtt` with a warm cache;
/// anything else is a 404. `text/vtt` plus the usual CORS because the webview
/// sits on a different origin (`localhost:5183` against our `127.0.0.1`).
async fn serve_captions(store: &SharedCaptions, rest: &str) -> Response<ProxyBody> {
    let mut parts = rest.splitn(2, '/');
    let (Some(video_id), Some(lang)) = (parts.next(), parts.next()) else {
        return with_cors(empty(StatusCode::NOT_FOUND));
    };
    let lang = lang.strip_suffix(".vtt").unwrap_or(lang);
    if video_id.is_empty() || lang.is_empty() || !valid_caption_part(video_id) || !valid_caption_part(lang)
    {
        return with_cors(empty(StatusCode::NOT_FOUND));
    }

    let body = store
        .lock()
        .await
        .get(&(video_id.to_string(), lang.to_string()))
        .cloned();

    match body {
        Some(vtt) => {
            let mut r = text(StatusCode::OK, &vtt);
            r.headers_mut()
                .insert("Content-Type", "text/vtt; charset=utf-8".parse().unwrap());
            // The webview caches the VTT for the track's lifetime anyway; a
            // no-cache keeps a language switch from serving a stale body.
            r.headers_mut()
                .insert("Cache-Control", "no-cache".parse().unwrap());
            with_cors(r)
        }
        None => with_cors(text(
            StatusCode::NOT_FOUND,
            "captions for this language were never fetched",
        )),
    }
}

/// Path safety for `/captions` parts: video ids and language codes are
/// `[A-Za-z0-9_-]`. Anything wider (a slash, a dot, control characters) is a
/// 404 rather than a lookup key.
fn valid_caption_part(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Serve a locally produced file (the extracted audio track) with the same
/// `Range` semantics a real static server would, so the webview can seek.
async fn serve_local(
    path: &std::path::Path,
    mime: &str,
    range: Option<&str>,
) -> Response<ProxyBody> {
    let meta = match tokio::fs::metadata(path).await {
        Ok(m) => m,
        Err(e) => {
            return with_cors(text(
                StatusCode::NOT_FOUND,
                &format!("local media missing: {e}"),
            ));
        }
    };
    let total = meta.len();

    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) => {
            return with_cors(text(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("open local media: {e}"),
            ));
        }
    };

    // An absent/!bytes= Range means "the whole file", per RFC 9110.
    let (start, end, partial) = match range.and_then(parse_range) {
        Some((s, e)) if s < total => (s, e.unwrap_or(total - 1).min(total - 1), true),
        // Unsatisfiable: a stale player asking past EOF after a cache clear.
        Some(_) => {
            let mut r = text(StatusCode::RANGE_NOT_SATISFIABLE, "range not satisfiable");
            r.headers_mut().insert("Content-Range", format!("bytes */{total}").parse().unwrap());
            return with_cors(r);
        }
        None => (0, total.saturating_sub(1), false),
    };

    let len = end.saturating_sub(start) + 1;
    let mut file = file;
    if let Err(e) = file.seek(std::io::SeekFrom::Start(start)).await {
        return with_cors(text(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("seek local media: {e}"),
        ));
    }

    let stream = futures_util::stream::unfold(
        (file, len),
        |(mut file, remaining)| async move {
            if remaining == 0 {
                return None;
            }
            let mut buf = vec![0u8; FILE_CHUNK.min(remaining as usize)];
            match file.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    let next = remaining.saturating_sub(n as u64);
                    Some((Ok(Frame::data(Bytes::from(buf))), (file, next)))
                }
                Err(e) => Some((Err(io::Error::other(e.to_string())), (file, 0))),
            }
        },
    );

    let mut resp = Response::new(BodyExt::boxed(StreamBody::new(stream)));
    *resp.status_mut() = if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let h = resp.headers_mut();
    h.insert("content-type", mime.parse().unwrap_or_else(|_| "audio/mp4".parse().unwrap()));
    h.insert("accept-ranges", "bytes".parse().unwrap());
    h.insert("content-length", len.into());
    if partial {
        h.insert(
            "content-range",
            format!("bytes {start}-{end}/{total}").parse().unwrap(),
        );
    }
    with_cors(resp)
}

/// How long a live response will wait for ffmpeg to produce bytes or to exit.
/// Comfortably above [`crate::mux::MUX_TIMEOUT`] so the cache owns the failure.
const GROWING_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(1900);

/// How often to re-check a file that has no bytes yet.
const GROWING_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// Stream a file that ffmpeg is still writing.
///
/// The remux writes a fragmented MP4, so the file is playable from its first
/// fragment and only grows. We therefore reply without a `Content-Length`
/// (hyper uses chunked transfer) and keep pumping bytes as they land, ending
/// only when the `.part` file disappears — which is how the cache signals
/// completion, by renaming it into place. That gives the player a normal
/// progressive stream that starts in a second or two and catches up live.
async fn serve_growing(
    path: &Path,
    mime: &str,
    _range: Option<&str>,
) -> Response<ProxyBody> {
    // `range` is deliberately ignored — always stream the whole thing from byte
    // 0. We answer 200 (see the header note below), and a 200 whose body begins
    // at a non-zero offset would desync the player against a partial request.
    // A player that has not seen `Accept-Ranges` will not ask for one anyway;
    // seeking is handled by the finished local file once the mux reports `ready`.
    let start = 0u64;

    // Give ffmpeg a moment to create the file so a dead remux surfaces as a
    // 404 rather than a stream that silently never produces bytes.
    let deadline = tokio::time::Instant::now() + GROWING_MAX_WAIT;
    loop {
        if path.exists() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return with_cors(text(
                StatusCode::NOT_FOUND,
                "muxed file never appeared",
            ));
        }
        tokio::time::sleep(GROWING_POLL).await;
    }

    let path = path.to_path_buf();
    let stream = futures_util::stream::unfold(
        LiveState {
            file: None,
            path,
            start,
            deadline,
        },
        |mut st| async move {
            loop {
                if st.file.is_none() {
                    match tokio::fs::File::open(&st.path).await {
                        Ok(mut f) => {
                            // The player may have asked for a byte offset; seek
                            // before the first read.
                            let _ = f.seek(std::io::SeekFrom::Start(st.start)).await;
                            st.file = Some(f);
                        }
                        Err(_) => {
                            // Renamed away before we opened it: the cache holds
                            // the finished file now, so end cleanly.
                            if tokio::time::Instant::now() >= st.deadline {
                                return None;
                            }
                            tokio::time::sleep(GROWING_POLL).await;
                            continue;
                        }
                    }
                }

                let f = st.file.as_mut().expect("file opened above");
                let mut buf = vec![0u8; FILE_CHUNK];
                match f.read(&mut buf).await {
                    Ok(0) => {
                        // End of what exists so far. If the `.part` is still
                        // there, ffmpeg is mid-write and more is coming.
                        if st.path.exists() {
                            if tokio::time::Instant::now() >= st.deadline {
                                return None;
                            }
                            tokio::time::sleep(GROWING_POLL).await;
                            continue;
                        }
                        return None;
                    }
                    Ok(n) => {
                        buf.truncate(n);
                        return Some((Ok(Frame::data(Bytes::from(buf))), st));
                    }
                    Err(e) => {
                        let err = io::Error::other(e.to_string());
                        st.file = None;
                        return Some((Err(err), st));
                    }
                }
            }
        },
    );

    // Always a 200, even when the player sent a `Range`: see the note on the
    // headers below. A 206 here would reintroduce the `Content-Range` with an
    // unknown total that triggers whole-file buffering.
    let mut resp = Response::new(BodyExt::boxed(StreamBody::new(stream)));
    // Deliberately an open-ended, NON-seekable 200: no `Content-Length`, no
    // `Accept-Ranges`, no `Content-Range`. The total size is not known until
    // ffmpeg renames the file, and advertising ranges with an unknown total
    // (`Content-Range: bytes 0-*/*`) makes Chromium treat the response as a
    // complete, indexable resource: it then fills the whole file into its
    // buffer before advancing `currentTime`, so playback only starts once the
    // remux is done. Serving it as a plain progressive stream (like a live
    // edge) makes the player start on the first fragment and keep going.
    // Seeking is not available while growing, but the moment the mux reports
    // `ready` the UI swaps to the finished local file, which is seekable.
    // Any incoming `Range` is ignored and the body is always sent from byte 0.
    let h = resp.headers_mut();
    h.insert(
        "content-type",
        mime.parse()
            .unwrap_or_else(|_| "video/mp4".parse().unwrap()),
    );
    with_cors(resp)
}

/// Size of one upstream read while relaying. googlevideo refuses ranges at or
/// past 16 MiB in a single request (verified: 8 MiB returns `206`, 32 MiB
/// returns `403`), so stay well under that.
const RELAY_CHUNK: u64 = 4 * 1024 * 1024;

/// First upstream read of a response is this small, so the consumer can start
/// working on the head of the file immediately instead of waiting out a full
/// `RELAY_CHUNK` before it sees anything.
const RELAY_FIRST_CHUNK: u64 = 512 * 1024;

/// Re-serve an upstream URL as an ordinary sequential byte stream.
///
/// googlevideo rejects the two things a normal HTTP client does when it opens a
/// file: a plain GET, and an open-ended `Range: bytes=0-`. Both answer `403`
/// (measured), which is why ffmpeg could not open an adaptive input at all — it
/// probes with exactly those. Only *bounded* ranges are honoured.
///
/// So we hide the restriction from the client: honour its `Range` for the seek
/// offset, then satisfy every read with a bounded upstream request and hand the
/// bytes over. The consumer sees a normal file it can probe and seek in, and we
/// keep issuing the only kind of request the CDN actually serves.
async fn serve_relay(url: &str, user_agent: &str, range: Option<&str>) -> Response<ProxyBody> {
    let (start, client_end) = match range.and_then(parse_range) {
        Some((s, e)) => (s, e),
        None => (0, None),
    };

    // One client for the whole response so the UA is stable and connections are
    // pooled. reqwest sends no User-Agent of its own, and the CDN treats a
    // missing one as an unrecognised client.
    let cdn = match reqwest::Client::builder().user_agent(user_agent).build() {
        Ok(c) => c,
        Err(e) => return text(StatusCode::BAD_GATEWAY, &format!("relay client: {e}")),
    };

    // Resolve the real length before the response is built so it can carry
    // honest Content-Length/Content-Range headers. ffmpeg sizes its reads from
    // these and issues parallel range requests when it seeks; without them it
    // splices the stream together wrongly and emits corrupt samples.
    let total = match relay_length(&cdn, url, start).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("relay metadata for {url}: {e}");
            return text(StatusCode::BAD_GATEWAY, &format!("relay unavailable: {e}"));
        }
    };
    let last = total.saturating_sub(1);
    let start = start.min(last);
    let end = client_end.unwrap_or(last).min(last);
    if end < start {
        return text(StatusCode::RANGE_NOT_SATISFIABLE, "range not satisfiable");
    }
    let length = end - start + 1;

    let stream = futures_util::stream::unfold(
        RelayState {
            url: url.to_string(),
            pos: start,
            end,
            done: false,
            small_first: true,
            first_read_done: None,
            created: std::time::Instant::now(),
            cdn: cdn.clone(),
        },
        |mut st| async move {
            if st.done || st.pos > st.end {
                return None;
            }
            // Small first read, full chunks after. ffmpeg is waiting on this
            // response to start muxing, so handing it the first bytes quickly
            // matters more than the steady-state chunk size; the extra
            // round-trips cost nothing once the download is running.
            let want = if st.small_first {
                st.small_first = false;
                RELAY_FIRST_CHUNK
            } else {
                RELAY_CHUNK
            };
            let want_end = st.end.min(st.pos.saturating_add(want - 1));
            let spec = format!("bytes={}-{}", st.pos, want_end);

            // A 403 here means the client's byte budget is temporarily spent,
            // not that the range is unavailable. The budget refills, so wait it
            // out instead of cutting the stream: giving up here is what used to
            // leave ffmpeg with a truncated file that it reported as success.
            let mut waited = 0u64;
            let body = loop {
                match st.cdn.get(&st.url).header("Range", &spec).send().await {
                    Ok(r) if r.status().is_success() => match r.bytes().await {
                        Ok(b) if !b.is_empty() => break b,
                        Ok(_) => {
                            tracing::warn!("relay body {spec}: empty");
                            return None;
                        }
                        Err(e) => {
                            tracing::warn!("relay body {spec}: {e}");
                            return None;
                        }
                    },
                    Ok(r) => {
                        let status = r.status();
                        if !is_throttle(status) {
                            tracing::warn!("relay upstream {spec}: HTTP {status}");
                            return None;
                        }
                        if waited >= RELAY_THROTTLE_BUDGET {
                            tracing::warn!(
                                "relay gave up on {spec}: still throttled after {waited}s"
                            );
                            return None;
                        }
                        let nap = RELAY_THROTTLE_BACKOFF.min(RELAY_THROTTLE_BUDGET - waited);
                        waited += nap;
                        tracing::warn!("relay upstream {spec}: HTTP {status}; retrying in {nap}s");
                        tokio::time::sleep(std::time::Duration::from_secs(nap)).await;
                    }
                    Err(e) => {
                        tracing::warn!("relay fetch {spec}: {e}");
                        return None;
                    }
                }
            };

            st.pos += body.len() as u64;
            if st.first_read_done.is_none() {
                st.first_read_done = Some(std::time::Instant::now());
                let latency = st.created.elapsed();
                tracing::info!(
                    created_to_first_read_ms = latency.as_millis() as u64,
                    bytes = body.len(),
                    "relay first read"
                );
            }
            Some((Ok(Frame::data(body)), st))
        },
    );

    let mut resp = Response::new(BodyExt::boxed(StreamBody::new(stream)));
    resp.headers_mut()
        .insert("content-type", "video/mp4".parse().unwrap());
    resp.headers_mut()
        .insert("accept-ranges", "bytes".parse().unwrap());
    resp.headers_mut()
        .insert("content-length", length.to_string().parse().unwrap());
    if range.is_some() {
        *resp.status_mut() = StatusCode::PARTIAL_CONTENT;
        if let Ok(v) = format!("bytes {start}-{end}/{total}").parse() {
            resp.headers_mut().insert("content-range", v);
        }
    }
    with_cors(resp)
}

/// Ask the CDN for a single byte purely to read the real length out of its
/// `Content-Range`. Resolving this *before* the response is built is what lets
/// the relay answer with honest `Content-Length`/`Content-Range` headers, which
/// ffmpeg needs: it sizes its reads from them and issues parallel range requests
/// when it seeks. Without them it splices the stream together wrongly and starts
/// emitting corrupt samples.
async fn relay_length(cdn: &reqwest::Client, url: &str, start: u64) -> Result<u64, String> {
    let spec = format!("bytes={start}-{start}");
    let resp = cdn
        .get(url)
        .header("Range", &spec)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    resp.headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(|cr| cr.rsplit('/').next())
        .and_then(|t| t.trim().parse::<u64>().ok())
        .ok_or_else(|| "upstream sent no total length".to_string())
}

/// Mutable cursor for one relay response.
struct RelayState {
    url: String,
    pos: u64,
    end: u64,
    done: bool,
    /// Cleared after the first read, so only the head of the response is
    /// fetched in small pieces. State rather than a comparison against `pos`,
    /// which would re-arm after any seek back to the start.
    small_first: bool,
    /// When the first body chunk actually arrived. ffmpeg cannot begin probing
    /// until then, so this is the floor on time-to-first-frame.
    first_read_done: Option<std::time::Instant>,
    /// When the stream was created, so the first-read log measures real latency
    /// (relay minted → CDN answered) rather than the tautological "time since
    /// the first read".
    created: std::time::Instant,
    cdn: reqwest::Client,
}

/// Statuses that mean "this client is over its byte budget", which clears on
/// its own, as opposed to a range that will never be served.
fn is_throttle(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::FORBIDDEN
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

/// Initial nap between retries of a throttled range, and the total time one range
/// may spend waiting before we accept the failure and let the caller fall back
/// to 360p. Kept short: the budget usually refills in seconds, and the player is
/// waiting on these bytes to start.
const RELAY_THROTTLE_BACKOFF: u64 = 2;
const RELAY_THROTTLE_BUDGET: u64 = 20;

/// Mutable cursor for a single live response.
struct LiveState {
    file: Option<tokio::fs::File>,
    path: PathBuf,
    start: u64,
    deadline: tokio::time::Instant,
}

/// Parse `bytes=<start>[-<end>]`. Open-ended (`bytes=0-`) means "to EOF", and
/// an unparseable header is treated as absent so the caller still gets the
/// whole file rather than a 416.
fn parse_range(spec: &str) -> Option<(u64, Option<u64>)> {
    let spec = spec.trim();
    let rest = spec.strip_prefix("bytes=").unwrap_or(spec);
    // Multi-range requests are legal but we only ever issue single ranges.
    let first = rest.split(',').next()?.trim();
    let (s, e) = first.split_once('-')?;
    let start: u64 = s.trim().parse().ok()?;
    let end: Option<u64> = match e.trim() {
        "" => None,
        v => Some(v.parse().ok()?),
    };
    Some((start, end.filter(|x| *x >= start)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Stands in for googlevideo: answers a ranged read with `206` plus
    /// `Content-Range`, and a plain read with `200`.
    async fn fake_cdn() -> SocketAddr {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let payload: Vec<u8> = (0..64u8).collect();
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let payload = payload.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 2048];
                    let Ok(n) = sock.read(&mut buf).await else {
                        return;
                    };
                    let head = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                    let ranged = head.contains("range: bytes=0-9");
                    let slice: &[u8] = if ranged { &payload[..10] } else { &payload };
                    let status = if ranged {
                        "206 Partial Content"
                    } else {
                        "200 OK"
                    };
                    let head = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: audio/mp4\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\n\r\n",
                        slice.len()
                    );
                    if ranged {
                        let head = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: audio/mp4\r\nAccept-Ranges: bytes\r\nContent-Range: bytes 0-9/{}\r\nContent-Length: {}\r\n\r\n",
                            payload.len(),
                            slice.len()
                        );
                        let _ = sock.write_all(head.as_bytes()).await;
                    } else {
                        let _ = sock.write_all(head.as_bytes()).await;
                    }
                    let _ = sock.write_all(slice).await;
                    let _ = sock.flush().await;
                });
            }
        });
        addr
    }

    /// The growing mux stream must look like a live progressive stream, not a
    /// seekable resource. Advertising `Accept-Ranges` (and a `Content-Range`
    /// with an unknown `*` total) makes Chromium index the whole response and
    /// refuse to advance `currentTime` until the remux finishes, which showed up
    /// as an endless pre-roll buffer. Playback should start on the first
    /// fragment instead.
    #[tokio::test]
    async fn growing_stream_is_not_advertised_as_seekable() {
        let dir = std::env::temp_dir().join("proxy-growing-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("v#137.mp4.part");
        // Enough for several FILE_CHUNK reads, so the body really does stream.
        let seed: Vec<u8> = (0..(FILE_CHUNK * 3)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&part, &seed).unwrap();

        let handle = spawn().await.unwrap();
        let token = handle
            .table
            .lock()
            .await
            .insert_growing(part.clone(), "video/mp4".to_string());

        let res = reqwest::Client::new()
            .get(format!("http://{}/stream/{token}", handle.addr))
            .send()
            .await
            .unwrap();

        assert_eq!(res.status(), 200);
        assert_eq!(res.headers().get("content-type").unwrap(), "video/mp4");
        // No range advertisement, and no content-length: the size is not known
        // until ffmpeg renames the file.
        assert!(res.headers().get("accept-ranges").is_none());
        assert!(res.headers().get("content-range").is_none());
        assert!(res.headers().get("content-length").is_none());

        // Even if the player sends a Range, the body must still be served from
        // byte 0 as a plain 200 — a 206 with a `*` total is what triggers the
        // whole-file buffering.
        // NB: only read a single chunk. A growing stream has no end until the
        // `.part` is renamed, so `bytes()` would block for the full
        // `GROWING_MAX_WAIT` — which is correct behaviour, and a trap in tests.
        let mut res = reqwest::Client::new()
            .get(format!("http://{}/stream/{token}", handle.addr))
            .header("Range", "bytes=1000-")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        assert!(res.headers().get("content-range").is_none());
        let first = tokio::time::timeout(std::time::Duration::from_secs(10), res.chunk())
            .await
            .expect("no bytes arrived on the growing stream")
            .expect("stream ended")
            .expect("stream ended");
        assert_eq!(first[0], seed[0], "body must start at byte 0");
        drop(res);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Bytes must reach the client as they are written, not in one lump when
    /// the file is renamed. This is what makes the first seconds playable while
    /// ffmpeg is still remuxing.
    #[tokio::test]
    async fn growing_stream_delivers_bytes_before_the_file_completes() {
        let dir = std::env::temp_dir().join("proxy-growing-live-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let part = dir.join("v#137.mp4.part");
        std::fs::write(&part, vec![7u8; FILE_CHUNK]).unwrap();

        let handle = spawn().await.unwrap();
        let token = handle
            .table
            .lock()
            .await
            .insert_growing(part.clone(), "video/mp4".to_string());

        let mut res = reqwest::Client::new()
            .get(format!("http://{}/stream/{token}", handle.addr))
            .send()
            .await
            .unwrap();

        // Read one chunk while the file is still only one chunk long and still
        // named `.part` (i.e. mid-remux). If this blocks until completion, the
        // pre-roll buffer problem is back.
        let mut got = 0usize;
        let first = res.chunk().await.unwrap().expect("no bytes");
        got += first.len();
        assert!(got > 0, "expected data before the remux finished");

        // Now grow the file and confirm more arrives, i.e. the stream follows
        // the writer instead of closing at the initial EOF.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&part)
            .unwrap();
        use std::io::Write;
        f.write_all(&vec![9u8; FILE_CHUNK]).unwrap();
        f.flush().unwrap();
        drop(f);

        let more = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            res.chunk(),
        )
        .await
        .expect("stream did not follow the growing file")
        .expect("no bytes")
        .expect("stream ended early");
        assert!(!more.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn ranged_read_keeps_206_and_content_range() {
        let upstream = fake_cdn().await;
        let handle = spawn().await.unwrap();
        let token = handle
            .table
            .lock()
            .await
            .insert(format!("http://{upstream}/media"));

        let res = reqwest::Client::new()
            .get(format!("http://{}/stream/{token}", handle.addr))
            .header("Range", "bytes=0-9")
            .send()
            .await
            .unwrap();

        // The old code collapsed every 2xx to 200, which contradicts the
        // Content-Range header it forwarded and stalls the media element.
        assert_eq!(res.status(), 206);
        assert_eq!(res.headers().get("content-type").unwrap(), "audio/mp4");
        assert_eq!(res.headers().get("accept-ranges").unwrap(), "bytes");
        assert_eq!(
            res.headers().get("content-range").unwrap(),
            "bytes 0-9/64"
        );
        assert_eq!(res.headers().get("access-control-allow-origin").unwrap(), "*");

        let body = res.bytes().await.unwrap();
        assert_eq!(body.len(), 10);
        assert_eq!(body[0], 0);
        assert_eq!(body[9], 9);
    }

    #[tokio::test]
    async fn plain_read_passes_200_and_full_body() {
        let upstream = fake_cdn().await;
        let handle = spawn().await.unwrap();
        let token = handle
            .table
            .lock()
            .await
            .insert(format!("http://{upstream}/media"));

        let res = reqwest::Client::new()
            .get(format!("http://{}/stream/{token}", handle.addr))
            .send()
            .await
            .unwrap();

        assert_eq!(res.status(), 200);
        assert!(res.headers().get("content-range").is_none());
        assert_eq!(res.bytes().await.unwrap().len(), 64);
    }

    #[tokio::test]
    async fn unknown_token_is_404() {
        let handle = spawn().await.unwrap();
        let res = reqwest::Client::new()
            .get(format!("http://{}/stream/deadbeefdeadbeef", handle.addr))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 404);
    }

    #[tokio::test]
    async fn dead_upstream_is_502() {
        let handle = spawn().await.unwrap();
        // Bind then immediately drop, so the port is almost certainly closed.
        let dead = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        let token = handle
            .table
            .lock()
            .await
            .insert(format!("http://{dead}/media"));

        let res = reqwest::Client::new()
            .get(format!("http://{}/stream/{token}", handle.addr))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 502);
    }

    #[tokio::test]
    async fn warmed_caption_vtt_is_served_via_captions_route() {
        let store: SharedCaptions = Arc::new(Mutex::new(HashMap::new()));
        store
            .lock()
            .await
            .insert(("abc123".into(), "en".into()), "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\nHello".into());
        let handle = spawn_with_captions(
            Arc::new(|_| {}),
            store,
        )
        .await
        .unwrap();

        let res = reqwest::Client::new()
            .get(format!("http://{}/captions/abc123/en.vtt", handle.addr))
            .send()
            .await
            .unwrap();

        assert_eq!(res.status(), 200);
        assert_eq!(res.headers().get("content-type").unwrap(), "text/vtt; charset=utf-8");
        assert_eq!(res.headers().get("access-control-allow-origin").unwrap(), "*");
        assert!(res.bytes().await.unwrap().starts_with(b"WEBVTT"));
    }

    #[tokio::test]
    async fn unwarmed_or_malformed_caption_keys_are_404() {
        let handle = spawn().await.unwrap();
        let client = reqwest::Client::new();

        for path in [
            "captions/abc123/en.vtt",   // never fetched
            "captions/abc123/en",       // wrong suffix
            "captions//en.vtt",         // empty id
            "captions/abc123/../lib.vtt", // path breakout
            "captions/abc123/en%2Evtt", // encoded dot
        ] {
            let res = client
                .get(format!("http://{}/{}", handle.addr, path))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), 404, "expected 404 for {path:?}");
        }
    }

    #[tokio::test]
    async fn cross_origin_caption_preflight_returns_cors_headers() {
        let store: SharedCaptions = Arc::new(Mutex::new(HashMap::new()));
        store.lock().await.insert(("abc123".into(), "fr".into()), "WEBVTT".into());
        let handle = spawn_with_captions(Arc::new(|_| {}), store).await.unwrap();

        let res = reqwest::Client::new()
            .get(format!("http://{}/captions/abc123/fr.vtt", handle.addr))
            .header("Origin", "http://localhost:5183")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(res.headers().get("access-control-allow-origin").unwrap(), "*");
    }

    /// The bridge reports over a cross-origin JSON POST from `youtube.com`, so
    /// the browser preflights it. A preflight that is refused makes the POST
    /// vanish, which reads as "the bridge never answered".
    #[tokio::test]
    async fn bridge_preflight_and_report_are_accepted() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sink: BridgeSink = Arc::new(move |v| {
            let _ = tx.send(v);
        });
        let handle = spawn_with_bridge(sink).await.unwrap();
        let client = reqwest::Client::new();

        let res = client
            .request(
                reqwest::Method::OPTIONS,
                format!("http://{}{}", handle.addr, BRIDGE_PATH),
            )
            .header("Origin", "https://www.youtube.com")
            .header("Access-Control-Request-Method", "POST")
            .header("Access-Control-Request-Headers", "content-type")
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 204);
        assert_eq!(res.headers().get("access-control-allow-origin").unwrap(), "*");
        assert!(res
            .headers()
            .get("access-control-allow-methods")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("POST"));

        let res = client
            .post(format!("http://{}{}", handle.addr, BRIDGE_PATH))
            .header("Origin", "https://www.youtube.com")
            .header("Content-Type", "application/json")
            .body(r#"{"videoId":"abc","stage":"done","json":{"x":1}}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let got = rx.try_recv().expect("the report reached the sink");
        assert_eq!(got["videoId"], "abc");
    }

    /// Write a temp file and mint a token for it, returning the proxy addr.
    async fn local_media(contents: &[u8]) -> (SocketAddr, String) {
        let path = std::env::temp_dir().join(format!(
            "savvy-proxy-test-{}-{}.m4a",
            std::process::id(),
            contents.len()
        ));
        tokio::fs::write(&path, contents).await.unwrap();
        let handle = spawn().await.unwrap();
        let token = handle
            .table
            .lock()
            .await
            .insert_local(path, "audio/mp4; codecs=\"mp4a.40.2\"".to_string());
        (handle.addr, token)
    }

    #[tokio::test]
    async fn local_file_honours_range_with_206() {
        let payload: Vec<u8> = (0..64u8).collect();
        let (addr, token) = local_media(&payload).await;

        let res = reqwest::Client::new()
            .get(format!("http://{addr}/stream/{token}"))
            .header("Range", "bytes=10-19")
            .send()
            .await
            .unwrap();

        // The webview seeks by range, so this must behave like a static server.
        assert_eq!(res.status(), 206);
        assert_eq!(
            res.headers().get("content-range").unwrap(),
            "bytes 10-19/64"
        );
        assert_eq!(res.headers().get("accept-ranges").unwrap(), "bytes");
        assert_eq!(
            res.headers().get("content-length").unwrap(),
            "10"
        );
        assert_eq!(
            res.headers().get("content-type").unwrap(),
            "audio/mp4; codecs=\"mp4a.40.2\""
        );
        let body = res.bytes().await.unwrap();
        assert_eq!(body.as_ref(), &payload[10..20]);
    }

    #[tokio::test]
    async fn local_file_open_ended_range_runs_to_eof() {
        let payload: Vec<u8> = (0..32u8).collect();
        let (addr, token) = local_media(&payload).await;

        let res = reqwest::Client::new()
            .get(format!("http://{addr}/stream/{token}"))
            .header("Range", "bytes=16-")
            .send()
            .await
            .unwrap();

        assert_eq!(res.status(), 206);
        assert_eq!(
            res.headers().get("content-range").unwrap(),
            "bytes 16-31/32"
        );
        assert_eq!(res.bytes().await.unwrap().as_ref(), &payload[16..]);
    }

    #[tokio::test]
    async fn local_file_without_range_is_200_and_whole_file() {
        let payload: Vec<u8> = (0..48u8).collect();
        let (addr, token) = local_media(&payload).await;

        let res = reqwest::Client::new()
            .get(format!("http://{addr}/stream/{token}"))
            .send()
            .await
            .unwrap();

        assert_eq!(res.status(), 200);
        assert!(res.headers().get("content-range").is_none());
        assert_eq!(res.bytes().await.unwrap().as_ref(), &payload[..]);
    }

    #[test]
    fn parse_range_handles_closed_open_and_garbage() {
        assert_eq!(parse_range("bytes=0-9"), Some((0, Some(9))));
        assert_eq!(parse_range("bytes=100-"), Some((100, None)));
        assert_eq!(parse_range("bytes=5-1"), Some((5, None))); // inverted -> open
        // Multi-range: we only serve the first.
        assert_eq!(parse_range("bytes=0-9,20-29"), Some((0, Some(9))));
        assert_eq!(parse_range("nonsense"), None);
    }

    /// Overflow evicts the least-recently-read token, and reading a token is
    /// enough to keep it. This is the invariant the whole player depends on: a
    /// resolve mints a token per rendition, so reaching the cap is routine, and
    /// the token the video on screen is streaming from must never be the one
    /// dropped — the previous wholesale `clear()` did exactly that.
    #[test]
    fn eviction_drops_the_oldest_but_keeps_the_token_being_read() {
        let mut table = StreamTable::default();
        let playing = table.insert("http://upstream/playing".into());

        // Fill to exactly the cap: `playing` is now the oldest token there is.
        // The first two fillers are kept aside so the assertions can name the
        // ones eviction should and should not touch.
        let mut filler0 = String::new();
        let mut filler1 = String::new();
        for i in 0..MAX_TOKENS - 1 {
            let token = table.insert(format!("http://upstream/{i}"));
            match i {
                0 => filler0 = token,
                1 => filler1 = token,
                _ => {}
            }
        }
        assert_eq!(table.map.len(), MAX_TOKENS, "table should be exactly full");

        // The player reads its token; the other fillers are never requested.
        assert!(table.lookup(&playing).is_some());
        assert!(table.lookup(&filler0).is_some());

        // One more mint pushes it over the cap: the oldest *unread* token goes,
        // not the playing one.
        let newest = table.insert("http://upstream/newest".into());
        assert_eq!(table.map.len(), MAX_TOKENS, "cap must hold after eviction");

        let target = table.lookup(&playing).expect("the playing token survived");
        assert!(
            matches!(target, Target::Upstream(u) if u == "http://upstream/playing"),
            "surviving token must still point at its own upstream"
        );
        assert!(table.lookup(&filler0).is_some(), "a read token stays");
        assert!(table.lookup(&newest).is_some(), "the token just minted stays");
        assert!(
            table.lookup(&filler1).is_none(),
            "the least-recently-read token is the one evicted"
        );
    }
}


