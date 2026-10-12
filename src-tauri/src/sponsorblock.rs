//! SponsorBlock (https://sponsor.ajay.app) integration, read-only.
//!
//! The app never writes to SponsorBlock — no votes, no submissions. It only
//! asks the public skip-segments endpoint which parts of a video to skip. The
//! API expects one `category` query parameter *per* category: both the plural
//! `categories` form and a comma-joined single value are rejected with a 400.

use std::time::{Duration, Instant};

use serde::Deserialize;

use innertube::dto::SponsorSegmentDto;

/// Categories the skip feature can honour. SponsorBlock's full category list
/// also contains segments whose only action is `chapter` or `poi_highlight`
/// (`chapter`, `poi_highlight`) and one that must never be skipped
/// (`exclusive_access`); none of those are offered, so a request can never ask
/// for a category the app would not know what to do with.
pub const CATEGORIES: &[&str] = &[
    "sponsor",
    "selfpromo",
    "interaction",
    "intro",
    "outro",
    "preview",
    "hook",
    "filler",
    "music_offtopic",
];

/// Upper bound on the whole request. Skipping revalidates against a 24 h TTL,
/// so the fetch matters little; the important thing is that a hanging upstream
/// never stalls playback setup.
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);

/// Cache entry for one video. The ETag is kept so a stale entry can be
/// revalidated with `If-None-Match` instead of a full re-download.
#[derive(Clone)]
pub struct CachedSegments {
    pub segments: Vec<SponsorSegmentDto>,
    pub fetched_at: Instant,
    pub etag: Option<String>,
}

/// What a revalidation round produced.
pub enum Fetch {
    /// 200: fresh payload (possibly empty), with its new ETag.
    Updated {
        segments: Vec<SponsorSegmentDto>,
        etag: Option<String>,
    },
    /// 304: the cached entry is still current; keep it, refresh its age.
    Unchanged,
    /// 404: the server has no segments for this id. Treated as a payload of
    /// zero segments (per the spec, not an error), and cached so we do not
    /// re-ask within the TTL.
    Empty,
}

/// One HTTP round against the public API. Returns an error only for transport
/// problems and non-404, non-304, non-200 statuses; the caller decides how a
/// failed round degrades.
pub async fn fetch(
    video_id: &str,
    categories: &[&str],
    etag: Option<&str>,
) -> Result<Fetch, String> {
    let mut params: Vec<(&str, &str)> = vec![("videoID", video_id)];
    params.extend(categories.iter().map(|c| ("category", *c)));
    let mut req = crate::proxy::upstream_client()
        .get("https://sponsor.ajay.app/api/skipSegments")
        .query(&params)
        .timeout(HTTP_TIMEOUT);
    if let Some(e) = etag {
        req = req.header("If-None-Match", e);
    }
    let resp = req.send().await.map_err(|e| e.to_string())?;
    match resp.status().as_u16() {
        200 => {
            let etag = resp
                .headers()
                .get("etag")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let body = resp.bytes().await.map_err(|e| e.to_string())?;
            // The wire shape differs from the webview DTO: times are a
            // two-element `segment` array and the id key is uppercase `UUID`.
            let raw: Vec<RawSegment> = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
            let segments = raw
                .into_iter()
                .map(|s| SponsorSegmentDto {
                    category: s.category,
                    start_seconds: s.segment[0],
                    end_seconds: s.segment[1],
                    uuid: s.uuid,
                })
                .collect();
            Ok(Fetch::Updated { segments, etag })
        }
        304 => Ok(Fetch::Unchanged),
        404 => Ok(Fetch::Empty),
        s => Err(format!("SponsorBlock returned HTTP {s}")),
    }
}

/// The API's on-the-wire segment shape and the field it carries that the
/// public DTO does not.
#[derive(Deserialize)]
struct RawSegment {
    category: String,
    segment: [f64; 2],
    #[serde(rename = "UUID")]
    uuid: String,
}