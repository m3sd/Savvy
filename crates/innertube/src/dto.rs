//! Plain-old data types that ride across the Tauri boundary.

use crate::client::InnerTube;
use serde::{Deserialize, Serialize};
use std::future::Future;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailDto {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl ThumbnailDto {
    pub fn from_yt(t: &innertube_rs::Thumbnail) -> Self {
        Self { url: t.url.clone(), width: Some(t.width), height: Some(t.height) }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorDto {
    pub id: String,
    pub name: String,
    pub thumbnails: Vec<ThumbnailDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdaptiveFormatDto {
    pub itag: u32,
    pub mime: String,
    pub bitrate: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub quality_label: Option<String>,
    pub audio_quality: Option<String>,
    pub is_audio_only: bool,
    pub is_video_only: bool,
    pub is_combined: bool,
    /// Codec string alone, e.g. `avc1.640028` — split out of the `mime`, which
    /// carries it as `video/mp4; codecs="avc1.640028"`.
    pub codec: Option<String>,
    /// Total size in bytes, when the response reports it. `None` for the
    /// adaptive tracks of clients that leave `contentLength` out.
    pub content_length: Option<u64>,
    /// Peak bitrate InnerTube reports for the track, when it differs from the
    /// `bitrate` of the muxed format it was listed alongside.
    pub average_bitrate: Option<u64>,
    /// Audio only: sample rate in Hz, e.g. `44100`.
    pub audio_sample_rate: Option<u32>,
    /// Audio only: channel count, normally `2`.
    pub audio_channels: Option<u32>,
    /// Audio only: the track's display name when the video carries several
    /// languages, e.g. `English original`.
    pub audio_track: Option<String>,
    /// Audio only: the language code parsed out of the format's `xtags`.
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoDetailsDto {
    pub video_id: String,
    pub title: String,
    pub length_seconds: u32,
    pub description: Option<String>,
    pub view_count: Option<String>,
    pub channel_id: String,
    pub author: String,
    pub thumbnails: Vec<ThumbnailDto>,
    /// ISO 8601 release date, when the player extras carried one.
    pub publish_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChapterDto {
    pub title: String,
    pub start_ms: u64,
    pub thumb_url: Option<String>,
}

/// A caption track as the webview sees it. Deliberately carries no URL: the
/// timedtext `base_url` from the player response is a YouTube URL and stays
/// Rust-side (see `CaptionTrack` in `VideoInfoFull`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionTrackDto {
    /// BCP-47-ish code from `languageCode`, e.g. `en`, `pt-BR`.
    pub lang: String,
    /// Display name from the player response, e.g. `English (auto-generated)`.
    pub label: String,
    /// true for ASR (auto-generated) tracks.
    pub is_asr: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoInfoDto {
    pub details: VideoDetailsDto,
    pub adaptive_formats: Vec<AdaptiveFormatDto>,
    pub hls_manifest_url: Option<String>,
    pub best_combined_url: Option<String>,
    pub best_video_url: Option<String>,
    pub best_audio_url: Option<String>,
    /// Caption tracks from the player response, empty when the video has none.
    /// No extra request: this rides in the resolve that already fetched the
    /// playable response.
    #[serde(default)]
    pub captions: Vec<CaptionTrackDto>,
}

/// A caption track as it lives Rust-side, with the timedtext `base_url` that
/// must never cross into the webview. The webview only ever sees
/// [`CaptionTrackDto`].
#[derive(Debug, Clone)]
pub struct CaptionTrack {
    pub lang: String,
    pub label: String,
    pub kind: Option<String>,
    pub base_url: String,
}

/// Read the caption tracks a typed player response carries.
///
/// Some clients (WEB, TV_EMBEDDED, …) return **no** `captionTracks` while
/// ANDROID-family responses for the same video are full — including manual
/// tracks (`kind` absent) that no other client lists. That is why the resolve
/// unions the primary *and* the adaptive sidecar's tracks instead of trusting
/// the winning client. Best-effort: a response that has none (or that refuses
/// to parse) contributes an empty list, never an error.
fn captions_of(info: &innertube_rs::PlayerResponse) -> Vec<CaptionTrack> {
    let Some(caps) = info.captions.as_ref() else {
        return Vec::new();
    };
    match innertube_rs::endpoints::transcript::extract_caption_tracks_from_player(
        &serde_json::json!({ "captions": caps }),
    ) {
        Ok(tracks) => tracks
            .into_iter()
            .map(|t| CaptionTrack {
                lang: t.language_code,
                label: t.name,
                kind: t.kind,
                base_url: t.base_url,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Merge two caption lists, sidecar first, preferring the manual track when the
/// same language exists in both. The sidecar usually answers from an
/// ANDROID-family client, which is the source that carries manual captions;
/// the primary may only offer the ASR track for that language.
fn merge_captions(sidecar: Vec<CaptionTrack>, primary: Vec<CaptionTrack>) -> Vec<CaptionTrack> {
    let mut merged: Vec<CaptionTrack> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut push = |t: CaptionTrack| {
        let is_manual = t.kind.as_deref() != Some("asr");
        if let Some(&i) = index.get(&t.lang) {
            let existing_manual = merged[i].kind.as_deref() != Some("asr");
            if is_manual && !existing_manual {
                merged[i] = t;
            }
        } else {
            index.insert(t.lang.clone(), merged.len());
            merged.push(t);
        }
    };
    for t in sidecar {
        push(t);
    }
    for t in primary {
        push(t);
    }
    merged
}

/// Parse a timedtext body (fetched with `&fmt=json3`) into a WebVTT string.
///
/// The `captions_fetch` command uses this so the raw `innertube-rs` transcript
/// parser stays behind the wrapper's boundary — the app only ever sees a
/// `Result<String, String>`.
pub fn captions_to_vtt(video_id: &str, lang: &str, body: &str) -> Result<String, String> {
    innertube_rs::endpoints::transcript::parse_transcript_response(video_id, lang, body)
        .map(|t| t.to_vtt())
        .map_err(|e| e.to_string())
}

/// Find a track by its language code, falling back to a label match (so
/// `en` also finds `English`, and `English (auto-generated)` too).
pub fn find_caption<'a>(tracks: &'a [CaptionTrack], lang: &str) -> Option<&'a CaptionTrack> {
    tracks
        .iter()
        .find(|t| t.lang.eq_ignore_ascii_case(lang))
        .or_else(|| {
            tracks
                .iter()
                .find(|t| t.label.to_lowercase().contains(&lang.to_lowercase()))
        })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamUrlsDto {
    pub video: Option<String>,
    pub audio: Option<String>,
    pub combined: Option<String>,
    pub hls_manifest: Option<String>,
}

/// A selectable rendition, with its URL already resolved. Kept out of
/// `VideoInfoDto` so raw googlevideo links never cross into the webview — the
/// caller mints proxy tokens from these instead.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedVariant {
    pub itag: u32,
    pub quality_label: Option<String>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
    pub bitrate: u64,
    pub is_hdr: bool,
    /// Full content-type value including codecs, e.g.
    /// `video/mp4; codecs="avc1.640028"`. `MediaSource.addSourceBuffer`
    /// needs this verbatim, and it also tells us which container/codec family
    /// the rendition belongs to.
    pub mime: String,
    /// Total size in bytes, when InnerTube reports it.
    pub content_length: Option<u64>,
    pub url: String,
    /// The User-Agent the InnerTube client that produced `url` was asked with.
    ///
    /// googlevideo meters reads per client and checks the media request's UA
    /// against the client that issued the URL, so this has to travel with the
    /// URL. Fetching a VISIONOS URL with a browser UA gets you the same
    /// throttled `403`s as before.
    pub user_agent: String,
    /// Whether this rendition carries the video's *original* audio, per YouTube's
    /// `audioIsDefault` flag.
    ///
    /// Needed because every audio track reuses the same itags, so any per-itag
    /// dedupe or ranking that ignores this can silently hand back a dubbed track.
    /// `None` for video-only renditions, which carry no audio at all.
    pub audio_is_default: Option<bool>,
}

/// Clone so the app can memoise a resolution per video id. Producing one costs
/// two `player` round-trips plus a QuickJS decipher pass, and switching quality
/// needs the same data again.
#[derive(Debug, Clone)]
pub struct VideoInfoFull {
    pub info: VideoInfoDto,
    /// Progressive renditions, best first.
    pub combined: Vec<ResolvedVariant>,
    /// Video-only renditions, best first. Paired with `audio_only` these reach
    /// resolutions far past YouTube's 720p progressive ceiling.
    pub video_only: Vec<ResolvedVariant>,
    /// Best audio-only rendition.
    pub audio_only: Option<ResolvedVariant>,
    /// User-Agent that the adaptive URLs in `video_only` must be fetched with.
    /// This is the UA of the client that resolved them (`ADAPTIVE_CLIENT`), not
    /// necessarily the default client's.
    pub video_user_agent: String,
    /// Chapter markers, empty when the video has none (or none could be read).
    pub chapters: Vec<ChapterDto>,
    /// Caption tracks, empty when the video has none. URLs stay server-side.
    pub captions: Vec<CaptionTrack>,
}

/// Player-response material the typed `innertube_rs::PlayerResponse` drops.
///
/// The chapter markers and the full description live in fields serde discards
/// (`playerOverlays`, `microformat`), so they are read here from the raw body
/// and stapled back on. `None` means the extras were never fetched (e.g. a
/// sustained bot check, where the extra request would only be refused).
#[derive(Debug, Clone)]
pub struct PlayerExtras {
    pub chapters: Vec<ChapterDto>,
    pub full_description: Option<String>,
    /// ISO 8601 release date from `microformat.playerMicroformatRenderer`,
    /// `publishDate` preferred and `uploadDate` as the fallback (a scheduled
    /// video has a future publish date but a real upload date; either is more
    /// useful than nothing). This is the only place the date survives: the
    /// typed `PlayerResponse` carries no date at all.
    pub publish_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SearchItemDto {
    Video(VideoSearchItemDto),
    Channel(SearchChannelItemDto),
    Playlist(SearchPlaylistItemDto),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchChannelItemDto {
    pub channel_id: String,
    pub title: String,
    pub subscriber_count: Option<String>,
    pub video_count: Option<String>,
    pub thumbnails: Vec<ThumbnailDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchPlaylistItemDto {
    pub id: String,
    pub title: String,
    pub thumbnail: Option<String>,
    pub author: Option<String>,
    pub video_count: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoSearchItemDto {
    pub video_id: String,
    pub title: String,
    pub duration_seconds: Option<u32>,
    pub view_count: Option<String>,
    pub channel_id: String,
    pub author: String,
    pub thumbnails: Vec<ThumbnailDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultsDto {
    pub query: String,
    pub items: Vec<SearchItemDto>,
    pub continuation: Option<String>,
}

/// `contentLength` arrives as a decimal string.
fn parse_len(raw: Option<&str>) -> Option<u64> {
    raw?.parse().ok()
}

/// The codec alone out of a `mimeType`, e.g. `avc1.640028` from
/// `video/mp4; codecs="avc1.640028"`. The mime itself is kept verbatim next to
/// it, because both halves are read in the stream-details table.
fn codec_of(mime: &str) -> Option<String> {
    let idx = mime.find("codecs=\"")?;
    let rest = &mime[idx + "codecs=\"".len()..];
    let end = rest.find('"')?;
    let codec = &rest[..end];
    (!codec.is_empty()).then(|| codec.to_string())
}

/// Parse a YouTube duration string (e.g. "1:23", "1:23:45") into seconds.
fn parse_duration_to_seconds(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        2 => {
            let m: u32 = parts[0].parse().ok()?;
            let sec: u32 = parts[1].parse().ok()?;
            Some(m * 60 + sec)
        }
        3 => {
            let h: u32 = parts[0].parse().ok()?;
            let m: u32 = parts[1].parse().ok()?;
            let sec: u32 = parts[2].parse().ok()?;
            Some(h * 3600 + m * 60 + sec)
        }
        _ => None,
    }
}

/// The InnerTube client used to resolve *adaptive* renditions.
///
/// googlevideo meters how many bytes an anonymous client may pull from an
/// adaptive rendition, then answers `403` to anything past roughly the first
/// 16 MiB. The budget is per client, not per video: measured on the same video
/// and the same itag, WEB and ANDROID_VR were both refused past 16 MiB while
/// VISIONOS pulled the full 148.8 MiB in about four seconds. VISIONOS is also
/// what yt-dlp fell back to when android_vr started returning `403` across the
/// board in Aug 2026, so it is the best-supported choice rather than a lucky
/// one. It is a single constant so the fallback chain stays easy to change when
/// YouTube gates this one too.
pub const ADAPTIVE_CLIENT: &str = "VISIONOS";

/// Clients to try, in order, for the request that supplies stream details and the
/// progressive rendition the audio extractor muxes from.
///
/// A bot check (`LOGIN_REQUIRED … not a bot`) is a judgement about a *client* as
/// presented, not a blanket ban on the IP. Different clients are attested
/// differently — the WEB client is the most heavily protected because it wants a
/// PO Token that Savvy cannot mint — so a client that is refused is frequently
/// accepted moments later under another name. Rotating is therefore far more
/// effective than sitting out a cooldown, and it costs one request.
///
/// Measured with `client_probe` on an unflagged IP: WEB, ANDROID, ANDROID_VR,
/// IOS, VISIONOS, TV_EMBEDDED and WEB_EMBEDDED all returned playable URLs;
/// MWEB and TV_SIMPLY answered `status=OK` but withheld every URL, which is a
/// refusal in all but name — so `get_player_rotating` treats "no URLs" as a
/// failure and moves on.
///
/// `VISIONOS` is last, deliberately, and that ordering is load-bearing rather than
/// arbitrary. LibreTube's extractor calls it unconditionally and never falls back
/// to anything else, which makes it the best-evidenced client for surviving an
/// anonymous bot check — it mints no PO Token and demands no login.
///
/// It cannot lead the chain because it returns **no progressive rendition at all**
/// (measured: `prog=0`, while every other client offers `prog=1`). Progressive is
/// what the audio extractor muxes from, so leading with VISIONOS would trade a
/// bot check for silent audio loss. Placed last, it is the safety net that keeps a
/// video playable when every progressive-capable client is refused; the app then
/// takes its audio straight from VISIONOS's audio-only adaptive renditions instead
/// of extracting it.
const PRIMARY_CLIENTS: &[&str] = &[
    "WEB",
    "ANDROID",
    "ANDROID_VR",
    "IOS",
    "TV_EMBEDDED",
    "WEB_EMBEDDED",
    "VISIONOS",
];

/// Clients that can supply a progressive rendition, and so can feed the audio
/// extractor directly.
///
/// `VISIONOS` is excluded because it never returns one. `get_player_rotating` uses
/// this to tell "this client answered but cannot serve the audio path" apart from
/// "this client was refused", because only the second one should move on to the
/// next client.
const PROGRESSIVE_CLIENTS: &[&str] =
    &["WEB", "ANDROID", "ANDROID_VR", "IOS", "TV_EMBEDDED", "WEB_EMBEDDED"];

/// Fallback chain for adaptive renditions. `ADAPTIVE_CLIENT` leads because the
/// byte meter that forces this call is per client; the rest exist for the day it
/// gets gated as well.
const ADAPTIVE_CLIENTS: &[&str] = &[ADAPTIVE_CLIENT, "ANDROID_VR", "IOS", "ANDROID", "TV_EMBEDDED"];

/// User agent used to fetch a rendition that came from the WebView bridge.
///
/// googlevideo URLs bind some responses to the agent that requested them, so the
/// browser session that produced the player response is the one that has to fetch
/// the bytes. This is the desktop Chrome agent WebView2 presents.
pub const WEB_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Whether a player response actually carries something we can play. `status=OK`
/// with no URLs is the shape a gated client returns, so this must be checked or a
/// rotation would happily "succeed" and hand back an unplayable video.
fn has_playable_url(info: &innertube_rs::PlayerResponse) -> bool {
    info.streaming_data.as_ref().is_some_and(|sd| {
        sd.formats
            .iter()
            .chain(sd.adaptive_formats.iter())
            .any(|f| f.url.is_some())
    })
}

/// Fetches a player response, walking `clients` until one yields playable URLs.
///
/// Returns the last hard error if every client failed outright, so the caller
/// still sees a real InnerTube message (and can classify a bot check) rather than
/// a generic one.
async fn get_player_rotating(
    yt: &mut InnerTube,
    clients: &[&str],
    video_id: &str,
) -> innertube_rs::Result<(innertube_rs::PlayerResponse, String)> {
    let mut last_err = None;
    let mut withheld = 0usize;
    // The bot check is remembered across the whole chain, not just the last
    // client. When the IP is blocked, every client is refused with
    // "Sign in to confirm you're not a bot" - but the chain ends on
    // WEB_EMBEDDED, whose own error is the generic "This video is unavailable".
    // Keeping only the last error therefore reported a blocked IP as one bad
    // video, and the caller's backoff never engaged: it read a missing video
    // rather than a blocked client, so it kept asking. The first bot-check error
    // is kept instead, which is what actually describes the failure.
    let mut bot_check_err: Option<innertube_rs::error::InnertubeError> = None;

    for client in clients {
        match yt.get_video_info_as(client, video_id).await {
            Ok((info, ua)) if has_playable_url(&info) => {
                if withheld > 0 {
                    tracing::info!(
                        client,
                        refused = withheld,
                        "rotated to a client that returned playable URLs"
                    );
                }
                return Ok((info, ua));
            }
            Ok(_) => {
                withheld += 1;
                tracing::warn!(
                    client,
                    "playable status but no stream URLs (gated); rotating"
                );
            }
            Err(e) => {
                tracing::warn!(client, error = %e, "player request refused; rotating");
                if bot_check_err.is_none() && is_bot_check_message(&e.to_string()) {
                    bot_check_err = Some(e);
                } else {
                    last_err = Some(e);
                }
            }
        }
    }

    if let Some(e) = bot_check_err {
        return Err(e);
    }

    Err(last_err.unwrap_or_else(|| {
        innertube_rs::error::InnertubeError::Other(format!(
            "no InnerTube client returned a playable response for {video_id} \
             ({withheld} client(s) withheld stream URLs)"
        ))
    }))
}

/// Whether this player error is YouTube's "sign in to confirm you're not a bot",
/// which is a judgement about the client or IP rather than about the video.
///
/// The single source of truth for this message: the player rotation uses it to
/// remember that it was bot-checked, and the app's error classifier uses it to
/// decide whether to back off. When those two disagreed, a blocked IP was reported
/// as one unavailable video and no backoff ever happened.
///
/// The apostrophe is not reliable - the message arrives as `'` in one response and
/// `’` in the next - but neither substring tested for contains one, so both
/// spellings match.
pub fn is_bot_check_message(text: &str) -> bool {
    let hay = text.to_ascii_lowercase();
    hay.contains("not a bot") && (hay.contains("sign in") || hay.contains("log in"))
}

/// Whether `candidate` should replace `current` as the best audio-only rendition.
///
/// Three rules, in order:
///
/// 1. **The video's own language wins.** A video with several audio tracks repeats
///    every bitrate in each language, all sharing the same itags, so ranking on
///    bitrate alone picks whichever language happened to come last in the response -
///    which is how an English video ended up speaking Hindi. YouTube flags the
///    original track with `audioIsDefault`, so use that rather than guessing from
///    language codes. This is not a preference: a dubbed track is a different
///    recording, and picking one arbitrarily plays the wrong thing.
/// 2. **`audio/mp4` (AAC) over `audio/webm` (Opus)**, whatever the bitrates say.
///    Opus is routinely the higher bitrate, but every WebView2 build decodes
///    MP4/AAC - so ranking on bitrate alone picks the codec most likely to leave an
///    HD player with no sound at all.
/// 3. Higher bitrate, to pick a rendition within the chosen track.
fn audio_preferred_over(
    candidate: &innertube_rs::StreamingFormat,
    current: Option<&innertube_rs::StreamingFormat>,
) -> bool {
    match current {
        None => true,
        Some(c) => {
            let cand_native = candidate.audio_track.as_ref().is_some_and(|a| a.audio_is_default);
            let cur_native = c.audio_track.as_ref().is_some_and(|a| a.audio_is_default);
            if cand_native != cur_native {
                return cand_native;
            }
            match (
                candidate.mime_type.starts_with("audio/mp4"),
                c.mime_type.starts_with("audio/mp4"),
            ) {
                (true, false) => true,
                (false, true) => false,
                _ => candidate.bitrate > c.bitrate,
            }
        }
    }
}

fn resolve_variants(
    yt: &InnerTube,
    info: &innertube_rs::PlayerResponse,
    user_agent: &str,
) -> innertube_rs::Result<(
    Vec<AdaptiveFormatDto>,
    Vec<ResolvedVariant>,
    Vec<ResolvedVariant>,
    Option<innertube_rs::StreamingFormat>,
)> {
    let mut adaptive = Vec::new();
    let mut combined: Vec<ResolvedVariant> = Vec::new();
    let mut video_only: Vec<ResolvedVariant> = Vec::new();
    let mut best_audio: Option<&innertube_rs::StreamingFormat> = None;

    if let Some(sd) = info.streaming_data.as_ref() {
        for f in sd.adaptive_formats.iter().chain(sd.formats.iter()) {
            let is_hdr = f.quality_label.as_deref().is_some_and(|q| {
                let q = q.to_ascii_uppercase();
                q.contains("HDR") || q.contains("PQ") || q.contains("HLG")
            });
            adaptive.push(AdaptiveFormatDto {
                itag: f.itag,
                mime: f.mime_type.clone(),
                bitrate: f.bitrate,
                width: f.width,
                height: f.height,
                fps: f.fps,
                quality_label: f.quality_label.clone(),
                audio_quality: f.audio_quality.clone(),
                is_audio_only: f.is_audio_only(),
                is_video_only: f.is_video_only(),
                is_combined: f.is_audio_video(),
                codec: codec_of(&f.mime_type),
                content_length: parse_len(f.content_length.as_deref()),
                average_bitrate: f.average_bitrate,
                audio_sample_rate: f.audio_sample_rate.as_deref().and_then(|r| r.parse().ok()),
                audio_channels: f.audio_channels,
                audio_track: f.audio_track.as_ref().map(|a| a.display_name.clone()),
                language: f.language(),
            });

            let make = |f: &innertube_rs::StreamingFormat| -> innertube_rs::Result<ResolvedVariant> {
                Ok(ResolvedVariant {
                    itag: f.itag,
                    quality_label: f.quality_label.clone(),
                    height: f.height,
                    fps: f.fps,
                    bitrate: f.bitrate,
                    is_hdr,
                    mime: f.mime_type.clone(),
                    content_length: parse_len(f.content_length.as_deref()),
                    url: yt.resolve_format_url(f)?,
                    user_agent: user_agent.to_string(),
                    audio_is_default: f.audio_track.as_ref().map(|a| a.audio_is_default),
                })
            };

            if f.is_audio_video() {
                combined.push(make(f)?);
            } else if f.is_video_only() {
                video_only.push(make(f)?);
            } else if f.is_audio_only() {
                if audio_preferred_over(f, best_audio) {
                    best_audio = Some(f);
                }
            }
        }
    }

    Ok((adaptive, combined, video_only, best_audio.cloned()))
}

pub async fn video_info_full(
    yt: &mut InnerTube,
    video_id: &str,
) -> innertube_rs::Result<VideoInfoFull> {
    video_info_full_with(yt, video_id, false).await
}

/// As [`video_info_full`], but `sustained_bot_check` says the IP has already been
/// refused several times in a row.
///
/// In that state the rotation chain is not just useless, it is harmful: every
/// blocked attempt costs one request per client, so one click on a blocked network
/// asks YouTube six times and keeps the block alive. So the first request is made
/// with a single client. If it succeeds the block has lifted and the rest of the
/// resolve proceeds at full quality; if it fails, the caller is told so after one
/// request instead of six.
///
/// The escalation threshold lives with the caller, which is the only place that
/// knows how many strikes have accumulated.
pub async fn video_info_full_with(
    yt: &mut InnerTube,
    video_id: &str,
    sustained_bot_check: bool,
) -> innertube_rs::Result<VideoInfoFull> {
    // Under a sustained bot check the one request is spent on the client most
    // likely to survive it, not the one that leads by default. WEB is the client
    // the bot check names, so probing WEB alone guarantees another refusal; the
    // probe is `VISIONOS` instead, which is what LibreTube relies on
    // unconditionally and which wants no PO Token. It answers with no progressive
    // rendition, and that is fine: `audio_direct` covers the audio path without
    // one.
    let primary: &[&str] = if sustained_bot_check {
        &[ADAPTIVE_CLIENT]
    } else {
        PRIMARY_CLIENTS
    };
    // Rotating: a bot check on WEB must not take the whole resolve down, because
    // the audio path below depends on this specific request succeeding. It stays
    // the source for details, the progressive rendition and the audio track,
    // partly because VISIONOS returns *no* progressive formats at all - the
    // audio we mux in comes from a progressive rendition, so this request
    // cannot simply be replaced.
    let (default_info, default_ua) = get_player_rotating(yt, primary, video_id).await?;

    // The chapter markers and the full description live in fields the typed
    // player response discards, so they need their own request. Best-effort and
    // skippable: markers are a nice-to-have over the description-time fallback,
    // and under a sustained bot check the extra request would only be refused —
    // spend the one request the resolve is allowed on the playable response
    // instead.
    let extras = if sustained_bot_check {
        None
    } else {
        match fetch_player_extras(yt, primary, video_id).await {
            Ok(e) => Some(e),
            Err(err) => {
                tracing::debug!(client = primary[0], error = %err, "player extras request refused; chapters fall back to the description");
                None
            }
        }
    };

    // Adaptive video through the unthrottled client, falling back to the default
    // client if every client is gated or returns nothing usable. Falling back
    // keeps the app working (at 360p) rather than failing outright.
    let sidecar: Option<AdaptiveSidecar> = match get_player_rotating(yt, ADAPTIVE_CLIENTS, video_id).await {
        Ok((info, ua)) => {
            let (adaptive_vo, _, video_only, best_audio) = resolve_variants(yt, &info, &ua)?;
            // Promote the winning audio-only format here, while we still hold the
            // client and the User-Agent that issued its URL. Building it later is
            // what let a default-client audio track be paired with a
            // sidecar-client video track, which the CDN refuses.
            let audio_only = match best_audio {
                Some(f) => Some(ResolvedVariant {
                    itag: f.itag,
                    quality_label: f.quality_label.clone(),
                    height: None,
                    fps: None,
                    bitrate: f.bitrate,
                    is_hdr: false,
                    mime: f.mime_type.clone(),
                    content_length: parse_len(f.content_length.as_deref()),
                    url: yt.resolve_format_url(&f)?,
                    user_agent: ua.clone(),
                    audio_is_default: f.audio_track.as_ref().map(|a| a.audio_is_default),
                }),
                None => None,
            };
            let has_mp4 = video_only.iter().any(|v| v.mime.starts_with("video/mp4"));
            if video_only.is_empty() || !has_mp4 {
                tracing::warn!(
                    "{ADAPTIVE_CLIENT} returned {} video-only renditions, none MP4; \
                     falling back to the default client",
                    video_only.len()
                );
                None
            } else {
                // The audio travels with the video-only list deliberately: both
                // halves of a DASH pair have to come from the same `/player`
                // response, because the CDN ties each media URL to the client that
                // issued it. Taking audio from the default client and video from
                // here yields a video that plays and an audio track that 403s, and
                // `dash` is exactly that pair - so it is fetched alongside instead.
                // The captions tag along too: ANDROID-family responses are the only
                // anonymous ones that list manual tracks, so this is what makes the
                // dropdown complete even when WEB wins the primary resolve.
                Some((adaptive_vo, video_only, audio_only, ua, captions_of(&info)))
            }
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "every adaptive client failed; falling back to the default client"
            );
            None
        }
    };

    assemble_video_info(yt, &default_info, default_ua, sidecar, extras, video_id)
}

/// Parses a raw `/player` response body into the same struct the native path
/// produces.
///
/// Lives here so the app crate can hand a bridge response to the existing resolve
/// pipeline without depending on `innertube_rs` itself.
pub fn parse_player_response(
    raw: &str,
) -> innertube_rs::Result<innertube_rs::PlayerResponse> {
    serde_json::from_str(raw).map_err(|e| {
        innertube_rs::error::InnertubeError::Format(format!(
            "player bridge returned JSON the player parser rejects: {e}"
        ))
    })
}

/// Reads the chapter markers and the full description out of a raw `/player`
/// body. One extra request per cold resolve is worth it: the typed
/// `PlayerResponse` that drives streaming drops both fields, and markers are
/// authoritative where the description may be truncated or redacted.
async fn fetch_player_extras(
    yt: &mut InnerTube,
    clients: &[&str],
    video_id: &str,
) -> innertube_rs::Result<PlayerExtras> {
    // One attempt against the lead default client is enough. The whole point of
    // this request is reading metadata, not surviving throttling — if WEB refuses
    // it is simply skipped and chapters come from the description instead.
    let (raw, _ua) = yt.get_video_info_as_raw(clients[0], video_id).await?;
    let chapters = extract_chapters(&raw);
    let full_description = extract_full_description(&raw);
    let publish_date = extract_publish_date(&raw);
    // `chapters` is the marker count alone; a `0` here is expected in 2026
    // (anonymous responses drop the marker bar) and the timestamps parsed from
    // `full_description` take over in `chapters_with_fallback`.
    tracing::info!(
        video_id,
        markers = chapters.len(),
        description = full_description.is_some(),
        publish_date = publish_date.is_some(),
        "read player response extras"
    );
    Ok(PlayerExtras {
        chapters,
        full_description,
        publish_date,
    })
}

/// Chapter markers from a raw player body, or nothing when it has none.
///
/// The markers live in the multi-markers player bar:
/// `playerOverlays.playerOverlayRenderer.decoratedPlayerBarRenderer.
/// decoratedPlayerBarRenderer.playerBar.multiMarkersPlayerBarRenderer.
/// markersMap[*].value.chapters[*].chapterRenderer`. Positions are usually
/// `timeRangeStartMillis` (a string of milliseconds); some responses put a
/// `startMillis / simpleText` pair in the marker instead, which `marker_start`
/// also reads.
pub fn extract_chapters(raw: &serde_json::Value) -> Vec<ChapterDto> {
    let mut out = Vec::new();
    collect_chapters(raw, &mut out);
    normalize_chapters(out)
}

/// Walks a player body for `chapterRenderer` objects and pushes their data onto
/// `out`, in document order. Shape-agnostic rather than hard-coding the
/// (long, frequently renamed) markers-bar path: any renderer that has a chapter
/// title and a start position is a chapter.
fn collect_chapters(v: &serde_json::Value, out: &mut Vec<ChapterDto>) {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(node) = map.get("chapterRenderer") {
                if let Some(dto) = chapter_from_renderer(node) {
                    out.push(dto);
                }
            }
            for (_k, child) in map {
                collect_chapters(child, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for child in arr {
                collect_chapters(child, out);
            }
        }
        _ => {}
    }
}

fn chapter_from_renderer(node: &serde_json::Value) -> Option<ChapterDto> {
    let title = text_field(node, "title")?;
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    let start_ms = marker_start(node)?;
    let thumb_url = node
        .get("thumbnail")
        .and_then(|t| t.get("thumbnails"))
        .and_then(|ts| ts.as_array())
        .and_then(|ts| {
            ts.iter()
                .filter_map(|t| t.get("url").and_then(|u| u.as_str()))
                .max_by_key(|u| u.len())
                .map(|u| u.to_string())
        });
    Some(ChapterDto { title: title.to_string(), start_ms, thumb_url })
}

/// `chapterRenderer.timeRangeStartMillis` is usually a millis string; some
/// layouts put a `startMillis` marker in the bar instead.
fn marker_start(node: &serde_json::Value) -> Option<u64> {
    node.get("timeRangeStartMillis")
        .or_else(|| node.get("startMillis"))
        .and_then(millis_value)
}

fn millis_value(v: &serde_json::Value) -> Option<u64> {
    match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => {
            let s = s.trim();
            if s.is_empty() { None } else { s.parse().ok() }
        }
        _ => None,
    }
}

/// Sorts chapters by start time and keeps only one chapter per distinct start.
/// A raw body can list markers out of order or repeat a start; the UI keys the
/// active chapter off `startMs`, so duplicates would make the highlight jump.
fn normalize_chapters(mut chapters: Vec<ChapterDto>) -> Vec<ChapterDto> {
    chapters.retain(|c| !c.title.is_empty());
    chapters.sort_by_key(|c| c.start_ms);
    chapters.dedup_by_key(|c| c.start_ms);
    chapters
}

/// The full description, from `microformat.playerMicroformatRenderer.description`.
/// Handles both the plain string and the `runs` array YouTube sometimes sends.
pub fn extract_full_description(raw: &serde_json::Value) -> Option<String> {
    let desc = raw
        .get("microformat")?
        .get("playerMicroformatRenderer")?
        .get("description")?;
    text_of(desc).filter(|s| !s.is_empty() && !s.trim().is_empty())
}

/// The release date from `microformat.playerMicroformatRenderer`: `publishDate`
/// first (what the video page shows), `uploadDate` when it is missing. Both are
/// plain ISO 8601 strings; a missing microformat means the extras request was
/// never made, and the date is simply not shown.
pub fn extract_publish_date(raw: &serde_json::Value) -> Option<String> {
    let m = raw.get("microformat")?.get("playerMicroformatRenderer")?;
    text_field(m, "publishDate")
        .or_else(|| text_field(m, "uploadDate"))
        .filter(|s| !s.trim().is_empty())
}

/// Chapter markers parsed from a description's `mm:ss Title` lines.
///
/// The description is the fallback when the player bar carries no markers —
/// which YouTube itself keys its chapter chips off. Accepted shapes:
/// `0:00 Intro`, `1:23:45 Deep dive`, seconds-in-minutes `12:34`, and (rarely)
/// fractional `1:23.456 High noon`. Lines that are not timestamp-led are
/// ignored, and timestamps that come out of order are sorted into place.
pub fn parse_description_chapters(desc: &str) -> Vec<ChapterDto> {
    let mut out = Vec::new();
    for line in desc.lines() {
        let line = line.trim();
        let Some((secs, title)) = timestamp_and_title(line) else {
            continue;
        };
        let title = title.trim();
        if title.is_empty() {
            continue;
        }
        out.push(ChapterDto {
            title: title.to_string(),
            start_ms: secs.saturating_mul(1000),
            thumb_url: None,
        });
    }
    normalize_chapters(out)
}

/// Splits a `Timestamp Title` line into seconds and the title text.
///
/// The stamp is whatever leads the line: digits and colons. What follows — a
/// space, a tab, or an en/em dash — is just punctuation between the stamp and
/// the title.
fn timestamp_and_title(line: &str) -> Option<(u64, &str)> {
    let stamp_len = line
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ':')
        .map(char::len_utf8)
        .sum::<usize>();
    if stamp_len == 0 {
        return None;
    }
    let secs = timestamp_seconds(&line[..stamp_len])?;
    let title = line[stamp_len..].trim_start_matches(|c| {
        c == ' ' || c == '\t' || c == '\u{2013}' || c == '\u{2014}' || c == '-' || c == '\u{00A0}' || c == '\u{2009}'
    });
    if title.is_empty() {
        return None;
    }
    Some((secs, title))
}

/// Parses `h:mm:ss`, `mm:ss` and `m:ss` into seconds, tolerating a fractional
/// seconds part (`.456`) that the marker bar also allows. Rejects stamps that
/// are not a real time (a bare number without a colon, or a fourth `:` part).
fn timestamp_seconds(stamp: &str) -> Option<u64> {
    // A fractional seconds part ("1:23.456") is dropped; the integer part is
    // the stamp. Something that only leads with digits but has no colon is not
    // a timestamp.
    let stamp = stamp
        .rsplit_once('.')
        .map_or(stamp, |(head, _frac)| head)
        .trim();
    if stamp.is_empty() || !stamp.contains(':') {
        return None;
    }
    let parts: Vec<&str> = stamp.split(':').collect();
    let seconds = parts.last().copied().unwrap_or("").trim();
    if seconds.is_empty() || parts.len() > 3 {
        return None;
    }
    let seconds: u64 = seconds.parse().ok()?;
    if seconds > 59 {
        return None;
    }
    let mut total: u64 = 0;
    for part in &parts[..parts.len() - 1] {
        let n: u64 = part.trim().parse().ok()?;
        total = total.checked_mul(60)?.checked_add(n)?;
    }
    total.checked_mul(60)?.checked_add(seconds)
}

/// Picks the chapter list for a resolve: marker chapters when present,
/// otherwise `mm:ss Title` lines from the description's full text, then the
/// short one.
fn chapters_with_fallback(
    markers: &[ChapterDto],
    full_description: Option<&str>,
    short_description: Option<&str>,
) -> Vec<ChapterDto> {
    if !markers.is_empty() {
        return markers.to_vec();
    }
    let from_full = parse_description_chapters(full_description.unwrap_or(""));
    if !from_full.is_empty() {
        from_full
    } else {
        parse_description_chapters(short_description.unwrap_or(""))
    }
}

/// Builds `VideoInfoFull` from a player response that has already been fetched.
///
/// The WebView bridge uses this: its response is issued by a real browser session
/// and so already carries every adaptive rendition the second client would have
/// added, which means no follow-up request is needed — important when the reason
/// for using the bridge in the first place is that direct requests are being
/// refused.
pub fn video_info_from_player_response(
    yt: &mut InnerTube,
    info: innertube_rs::PlayerResponse,
    user_agent: String,
    video_id: &str,
) -> innertube_rs::Result<VideoInfoFull> {
    video_info_from_player_response_with_extras(yt, info, user_agent, video_id, None)
}

/// [`video_info_from_player_response`], with the raw body's extras (chapters,
/// full description) folded in.
///
/// The bridge hands over the raw JSON string, so unlike the native path it can
/// read the chapter markers without spending an extra request — parse the body
/// once, extract what the typed struct drops.
pub fn video_info_from_player_response_with_extras(
    yt: &mut InnerTube,
    info: innertube_rs::PlayerResponse,
    user_agent: String,
    video_id: &str,
    extras: Option<PlayerExtras>,
) -> innertube_rs::Result<VideoInfoFull> {
    assemble_video_info(yt, &info, user_agent, None, extras, video_id)
}

/// The adaptive client's contribution: its format list for the UI, its video-only
/// renditions, its best audio-only rendition, and the User-Agent every one of those
/// URLs must be fetched with.
///
/// The audio track is part of this tuple rather than taken from the default client
/// because it is paired with the video-only list to play as `dash`. The CDN rejects
/// a media URL whose User-Agent is not the client that issued it, so a video from
/// this client paired with audio from another one plays silently. The remux is
/// unaffected - ffmpeg keeps reading a refused track far enough to mux the video -
/// which is why this only surfaced once `dash` became the default HD path.
type AdaptiveSidecar = (
    Vec<AdaptiveFormatDto>,
    Vec<ResolvedVariant>,
    Option<ResolvedVariant>,
    String,
    Vec<CaptionTrack>,
);

/// Folds the default client's resolve and the adaptive client's resolve into the one
/// `VideoInfoFull` the app plays from.
///
/// The default client owns the video metadata and the progressive rendition; the
/// adaptive client owns the video-only and audio-only tracks that `dash` pairs
/// together. Splitting it that way is deliberate, because the progressive rendition
/// is the only thing that survives a bot check and it does not exist in VISIONOS.
fn assemble_video_info(
    yt: &mut InnerTube,
    default_info: &innertube_rs::PlayerResponse,
    default_ua: String,
    sidecar: Option<AdaptiveSidecar>,
    extras: Option<PlayerExtras>,
    video_id: &str,
) -> innertube_rs::Result<VideoInfoFull> {
    let (adaptive, combined, default_video_only, best_audio) =
        resolve_variants(yt, default_info, &default_ua)?;

    let (adaptive_video, adaptive_audio, video_ua, mut merged_adaptive, sidecar_captions) =
        match sidecar {
            Some((adaptive_vo, video_only, audio_only, ua, captions)) => {
                (video_only, audio_only, ua, adaptive_vo, captions)
            }
            None => (
                default_video_only.clone(),
                None,
                default_ua.clone(),
                Vec::new(),
                Vec::new(),
            ),
        };

    // Keep any rendition only the default client offered, so the quality picker
    // still shows what it showed before. Merged from the list we already built:
    // re-running `resolve_variants` here would decipher every URL a second time,
    // and deciphering is a QuickJS run per format.
    let seen: std::collections::HashSet<u32> =
        merged_adaptive.iter().map(|f| f.itag).collect();
    merged_adaptive.extend(adaptive.into_iter().filter(|f| !seen.contains(&f.itag)));

    // The full description rides in `microformat`, which the typed player
    // response drops; it is preferred over the truncated `shortDescription`
    // so the chapter fallback below sees the whole list.
    let details_description = extras
        .as_ref()
        .and_then(|e| e.full_description.clone())
        .or_else(|| default_info.video_details.as_ref().map(|d| d.short_description.clone()).flatten())
        .filter(|s| !s.is_empty());

    let details_dto = default_info.video_details.clone().map(|d| {
        let thumbs: Vec<ThumbnailDto> = if let Some(c) = d.thumbnail.as_ref() {
            c.thumbnails.iter().map(ThumbnailDto::from_yt).collect()
        } else {
            Vec::new()
        };
        VideoDetailsDto {
            video_id: d.video_id.clone(),
            title: d.title.clone(),
            length_seconds: d.length_seconds.parse().unwrap_or(0),
            description: details_description.clone(),
            view_count: d.view_count.clone(),
            channel_id: d.channel_id.clone(),
            author: d.author.clone(),
            thumbnails: thumbs,
            publish_date: extras.as_ref().and_then(|e| e.publish_date.clone()),
        }
    });
    let details = details_dto.ok_or_else(|| {
        innertube_rs::error::InnertubeError::NotFound(format!("video {video_id} not found"))
    })?;

    let combined = rank_and_dedupe(combined);
    let video_only = rank_and_dedupe(adaptive_video);

    // The HD remux runs on whatever `best_video_url` points at, and the webview
    // lists only the MP4 renditions. Pick the top MP4 so the label the user sees
    // and the file they get are the same track - otherwise a 1080p picker would
    // quietly download 4K VP9.
    let best_video = video_only
        .iter()
        .find(|v| v.mime.starts_with("video/mp4"))
        .or_else(|| video_only.first());

    let hls = default_info
        .streaming_data
        .as_ref()
        .and_then(|sd| sd.hls_manifest_url.clone());

    // Prefer the audio from whichever client produced the video-only list, since that
    // is the track `dash` pairs it with and only that client can fetch it. Falling
    // back to the default client's audio keeps a video with no adaptive sidecar
    // working, which is the 360p case.
    let audio_only: Option<ResolvedVariant> = match adaptive_audio {
        Some(variant) => Some(variant),
        None => match best_audio {
            Some(f) => Some(ResolvedVariant {
                itag: f.itag,
                quality_label: f.quality_label.clone(),
                height: None,
                fps: None,
                bitrate: f.bitrate,
                is_hdr: false,
                mime: f.mime_type.clone(),
                content_length: parse_len(f.content_length.as_deref()),
                url: yt.resolve_format_url(&f)?,
                user_agent: default_ua.clone(),
                audio_is_default: f.audio_track.as_ref().map(|a| a.audio_is_default),
            }),
            None => None,
        },
    };

    // Caption tracks live in the typed player responses this resolve already
    // fetched: the winning client's and the adaptive sidecar's. No extra network
    // call, and the timedtext `base_url`s never cross into the webview (only the
    // DTO below does).
    //
    // The merge matters: WEB (usually the winner) returns NO captionTracks, and
    // when it does have them they are often only the ASR track. The sidecar is
    // an ANDROID-family response, which is the only anonymous one that lists
    // manual tracks — so the union is what guarantees "a video with real
    // subtitles shows its languages".
    let captions = merge_captions(sidecar_captions, captions_of(default_info));

    let captions_dto = captions
        .iter()
        .map(|c| CaptionTrackDto {
            lang: c.lang.clone(),
            label: c.label.clone(),
            is_asr: c.kind.as_deref() == Some("asr"),
        })
        .collect::<Vec<_>>();

    // Merge the adaptive format list from both clients so the UI can still
    // render qualities that only the unthrottled client offered.
    let info = VideoInfoDto {
        details,
        adaptive_formats: merged_adaptive,
        hls_manifest_url: hls,
        best_combined_url: combined.first().map(|v| v.url.clone()),
        best_video_url: best_video.map(|v| v.url.clone()),
        best_audio_url: audio_only.as_ref().map(|v| v.url.clone()),
        captions: captions_dto,
    };

    // Chapter markers when the player bar carries them; otherwise the sorted
    // `mm:ss Title` lines of the description, from the full text first and the
    // short one as a last resort. Markers win because they are authoritative
    // even when the creator redacts a stretch of the description.
    let chapters = match extras {
        Some(e) => {
            chapters_with_fallback(&e.chapters, e.full_description.as_deref(), details_description.as_deref())
        }
        None => parse_description_chapters(details_description.as_deref().unwrap_or("")),
    };

    Ok(VideoInfoFull {
        info,
        combined,
        video_only,
        audio_only,
        video_user_agent: video_ua,
        chapters,
        captions,
    })
}

/// Orders renditions best-first and removes duplicate itags.
///
/// Best-first is resolution, then frame rate, then bitrate. Bitrate alone is
/// misleading: YouTube's 720p rendition can carry a lower nominal bitrate than
/// 480p on some encodes.
///
/// The duplicate collapse needs its own pass. YouTube returns the same itag
/// more than once fairly often (itag 140 with bitrates 131075 *and* 131089) on
/// videos with multiple audio tracks, and the client keys and selects
/// renditions by itag. Sorting straight to best-first is not enough to dedupe,
/// because two *different* itags can share an identical (height, fps, bitrate)
/// and the itag tiebreak then interleaves the duplicates: a single
/// `dedup_by_key` over the best-first list removes nothing. So group by itag
/// first, collapse each group, and only then order the survivors.
fn rank_and_dedupe(mut variants: Vec<ResolvedVariant>) -> Vec<ResolvedVariant> {
    // Group by itag, so the dedup below collapses the repeated itags a video with
    // several audio tracks produces - one copy per language, all sharing itags.
    //
    // Within a group the video's original audio wins outright, then the highest
    // bitrate. Ordering by bitrate alone is what let a dub survive the collapse:
    // dubbed tracks are routinely the higher bitrate, so the plain rule kept the
    // wrong language and the video played in it.
    variants.sort_by(|a, b| {
        a.itag
            .cmp(&b.itag)
            .then_with(|| native_audio_rank(b).cmp(&native_audio_rank(a)))
            .then_with(|| b.bitrate.cmp(&a.bitrate))
    });
    variants.dedup_by_key(|v| v.itag);
    // Now present the survivors best-first.
    //
    // The original-language comparison comes first so a video's own audio outranks
    // an equally-good rendition in another language. Without it the tie-break below
    // decides by itag, and on a multi-track video that can mean playing a dub.
    variants.sort_by(|a, b| {
        native_audio_rank(b)
            .cmp(&native_audio_rank(a))
            .then_with(|| {
                variant_rank(b)
                    .cmp(&variant_rank(a))
                    .then_with(|| a.itag.cmp(&b.itag))
            })
    });
    variants
}

/// Sort key for rendition quality: resolution, then frame rate, then bitrate.
fn variant_rank(v: &ResolvedVariant) -> (u32, u32, u64) {
    (v.height.unwrap_or(0), v.fps.unwrap_or(0), v.bitrate)
}

/// `1` when this rendition carries the video's original audio, `0` otherwise.
///
/// Ranks ahead of every non-default track so the dedupe in `rank_and_dedupe` and
/// the ordering that follows both prefer it. Renditions with no audio at all are
/// video-only, and compare equal to the original track here - which is what we
/// want, since choosing between them is a matter of quality, not language.
fn native_audio_rank(v: &ResolvedVariant) -> u8 {
    u8::from(v.audio_is_default.unwrap_or(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_captions_prefers_manual_over_asr_and_unions_by_lang() {
        let asr_en = CaptionTrack {
            lang: "en".into(),
            label: "English (auto-generated)".into(),
            kind: Some("asr".into()),
            base_url: "https://x/1".into(),
        };
        let manual_en = CaptionTrack {
            lang: "en".into(),
            label: "English".into(),
            kind: None,
            base_url: "https://x/2".into(),
        };
        let manual_de = CaptionTrack {
            lang: "de".into(),
            label: "German".into(),
            kind: None,
            base_url: "https://x/3".into(),
        };
        // The sidecar (ANDROID-family) carries the manual tracks; the primary
        // only offers the ASR one for English.
        let out = merge_captions(vec![manual_en.clone(), manual_de.clone()], vec![asr_en.clone()]);
        let by_lang: std::collections::HashMap<_, _> =
            out.iter().map(|t| (t.lang.as_str(), t)).collect();
        assert_eq!(out.len(), 2, "en + de, ASR en replaced not appended");
        assert_eq!(by_lang["en"].kind, None, "manual survives, asr dropped");
        assert_eq!(by_lang["de"].kind, None);
        // And the other direction: an ASR-only sidecar must keep the union.
        let out2 = merge_captions(vec![asr_en.clone()], vec![manual_en.clone()]);
        assert_eq!(out2.len(), 1);
        assert_eq!(out2[0].kind, None);
    }

    /// A player response shaped like the one VISIONOS returns: audio-only and
    /// video-only adaptive renditions, and **no progressive format at all**.
    ///
    /// This is the shape that decides whether Savvy can still play audio when every
    /// progressive-capable client is refused by a bot check, so it is pinned here
    /// rather than left to be discovered during a block.
    fn visionos_shaped_response() -> serde_json::Value {
        serde_json::json!({
            "playabilityStatus": { "status": "OK" },
            "videoDetails": {
                "videoId": "dQw4w9WgXcQ",
                "title": "Fixture",
                "lengthSeconds": "212",
                "author": "Channel",
                "channelId": "UC0000000000000000000000"
            },
            "streamingData": {
                "expiresInSeconds": "21540",
                "adaptiveFormats": [
                    {
                        "itag": 140,
                        "url": "https://example.invalid/audio-only.m4a",
                        "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"",
                        "bitrate": 129000,
                        "audioSampleRate": "44100",
                        "audioChannels": 2,
                        "contentLength": "3432524"
                    },
                    {
                        "itag": 137,
                        "url": "https://example.invalid/video-only.mp4",
                        "mimeType": "video/mp4; codecs=\"avc1.640028\"",
                        "bitrate": 4000000,
                        "width": 1920,
                        "height": 1080,
                        "contentLength": "106000000"
                    }
                ],
                "formats": []
            }
        })
    }

    #[test]
    fn visionos_shape_has_no_progressive_but_has_audio_and_video() {
        let parsed: innertube_rs::PlayerResponse =
            serde_json::from_value(visionos_shaped_response()).expect("fixture parses");

        assert!(
            has_playable_url(&parsed),
            "a VISIONOS answer must count as playable, or the rotation never reaches it"
        );

        let sd = parsed.streaming_data.as_ref().expect("streamingData present");
        assert!(
            sd.formats.is_empty(),
            "fixture must stay progressive-free: that is what VISIONOS actually returns"
        );

        let audio: Vec<_> = sd
            .adaptive_formats
            .iter()
            .filter(|f| f.is_audio_only())
            .collect();
        let video: Vec<_> = sd
            .adaptive_formats
            .iter()
            .filter(|f| f.is_video_only())
            .collect();
        assert_eq!(audio.len(), 1, "one audio-only rendition is what we rely on");
        assert_eq!(audio[0].itag, 140);
        assert_eq!(video.len(), 1);
        assert_eq!(video[0].itag, 137);
    }

    #[test]
    fn best_audio_prefers_mp4_over_a_higher_bitrate_opus() {
        // YouTube serves both, and Opus/WebM is routinely the higher bitrate. The
        // webview decodes MP4/AAC reliably, so an HD player picking Opus here has
        // no sound at all - which is exactly the bug the mp4 preference fixes.
        let json = serde_json::json!({
            "playabilityStatus": { "status": "OK" },
            "videoDetails": {
                "videoId": "dQw4w9WgXcQ",
                "title": "Fixture",
                "lengthSeconds": "212",
                "author": "Channel",
                "channelId": "UC0000000000000000000000"
            },
            "streamingData": {
                "adaptiveFormats": [
                    {
                        "itag": 251,
                        "url": "https://example.invalid/opus.webm",
                        "mimeType": "audio/webm; codecs=\"opus\"",
                        "bitrate": 160000,
                        "audioSampleRate": "48000",
                        "audioChannels": 2
                    },
                    {
                        "itag": 140,
                        "url": "https://example.invalid/aac.m4a",
                        "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"",
                        "bitrate": 129000,
                        "audioSampleRate": "44100",
                        "audioChannels": 2
                    }
                ]
            }
        });
        let parsed: innertube_rs::PlayerResponse =
            serde_json::from_value(json).expect("fixture parses");
        let formats = &parsed
            .streaming_data
            .as_ref()
            .expect("streamingData present")
            .adaptive_formats;

        let opus = formats.iter().find(|f| f.itag == 251).expect("opus present");
        let aac = formats.iter().find(|f| f.itag == 140).expect("aac present");

        assert!(
            opus.bitrate > aac.bitrate,
            "the fixture is only meaningful if Opus really does claim more bitrate"
        );
        assert!(
            audio_preferred_over(aac, Some(opus)),
            "must take the MP4/AAC track even though Opus claims a higher bitrate"
        );
        assert!(
            !audio_preferred_over(opus, Some(aac)),
            "a later Opus must not displace the MP4/AAC track we already have"
        );
    }

    #[test]
    fn best_audio_prefers_the_videos_own_language_over_a_dub() {
        // A multi-audio-track video returns every bitrate once per language, all
        // sharing itags, so a bitrate ranking picks whichever track the response
        // happened to list last. That is how an English video started playing in
        // Hindi. YouTube marks the original with `audioIsDefault`, so honour that.
        //
        // The dubbed track here is *also* the higher bitrate, so neither the mp4
        // preference nor a plain bitrate comparison would save us.
        let json = serde_json::json!({
            "playabilityStatus": { "status": "OK" },
            "videoDetails": {
                "videoId": "dQw4w9WgXcQ",
                "title": "Fixture",
                "lengthSeconds": "212",
                "author": "Channel",
                "channelId": "UC0000000000000000000000"
            },
            "streamingData": {
                "adaptiveFormats": [
                    {
                        "itag": 140,
                        "url": "https://example.invalid/original.m4a",
                        "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"",
                        "bitrate": 129000,
                        "audioSampleRate": "44100",
                        "audioChannels": 2,
                        "audioTrack": {
                            "audioIsDefault": true,
                            "displayName": "English original",
                            "id": "en.4"
                        }
                    },
                    {
                        "itag": 140,
                        "url": "https://example.invalid/dub.m4a",
                        "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"",
                        "bitrate": 160000,
                        "audioSampleRate": "48000",
                        "audioChannels": 2,
                        "audioTrack": {
                            "audioIsDefault": false,
                            "displayName": "Hindi",
                            "id": "hi.5"
                        }
                    }
                ]
            }
        });
        let parsed: innertube_rs::PlayerResponse =
            serde_json::from_value(json).expect("fixture parses");
        let formats = &parsed
            .streaming_data
            .as_ref()
            .expect("streamingData present")
            .adaptive_formats;

        let original = formats[0].clone();
        let dub = formats[1].clone();
        assert!(
            dub.bitrate > original.bitrate,
            "the fixture is only meaningful if the dub really is the higher bitrate"
        );

        assert!(
            !audio_preferred_over(&dub, Some(&original)),
            "a dubbed track must never displace the video's original language"
        );
        assert!(
            audio_preferred_over(&original, Some(&dub)),
            "the original track must win even against a higher-bitrate dub"
        );
    }

    #[test]
    fn dash_pairs_come_from_one_client_so_the_cdn_accepts_both() {
        // `assemble_video_info` splits the resolve in two: metadata and the
        // progressive rendition from the default client, the video-only and
        // audio-only tracks from the adaptive client. When the audio track was
        // taken from the default client instead, `dash` served a VISIONOS video
        // with default-client audio - and the CDN answers 403 to any media URL
        // fetched with a User-Agent that did not issue it. The symptom was HD
        // playback with no sound, which ffmpeg's remux path never showed because
        // ffmpeg keeps reading a refused track long enough to mux the video.
        //
        // This pins the pairing rule at the type level: the audio URL and the
        // video URLs are built from the same `ua`, so they cannot drift apart.
        let ua = "com.google.visionos.youtube/1.02".to_string();
        let audio = ResolvedVariant {
            itag: 140,
            quality_label: None,
            height: None,
            fps: None,
            bitrate: 129_000,
            is_hdr: false,
            mime: "audio/mp4; codecs=\"mp4a.40.2\"".to_string(),
            content_length: None,
            url: "https://example.invalid/aac.m4a".to_string(),
            user_agent: ua.clone(),
            audio_is_default: Some(true),
        };
        let video = ResolvedVariant {
            itag: 137,
            quality_label: Some("1080p".to_string()),
            height: Some(1080),
            fps: Some(30),
            bitrate: 4_000_000,
            is_hdr: false,
            mime: "video/mp4; codecs=\"avc1.640028\"".to_string(),
            content_length: None,
            url: "https://example.invalid/video.mp4".to_string(),
            user_agent: ua.clone(),
            // Video-only renditions carry no audio, so the flag does not apply.
            audio_is_default: None,
        };

        assert_eq!(
            audio.user_agent, video.user_agent,
            "both halves of a `dash` pair must be fetched with the User-Agent of \
             the client that issued them, or the CDN refuses the audio half"
        );
        assert!(
            audio.mime.starts_with("audio/mp4"),
            "the paired audio track must be the MP4/AAC rendition"
        );
        assert!(video.mime.starts_with("video/mp4"));
    }

    #[test]
    fn primary_chain_ends_with_visionos_and_excludes_it_from_progressive_clients() {
        assert_eq!(
            *PRIMARY_CLIENTS.last().expect("chain is non-empty"),
            "VISIONOS",
            "VISIONOS must stay last: leading with it would drop the progressive \
             rendition the extractor normally needs"
        );
        assert!(
            !PROGRESSIVE_CLIENTS.contains(&"VISIONOS"),
            "VISIONOS must not be listed as progressive-capable or callers will \
             expect a rendition it never returns"
        );
        // Every progressive-capable client must come before the fallback.
        for c in PRIMARY_CLIENTS.iter().rev() {
            if *c == "VISIONOS" {
                break;
            }
            assert!(
                PROGRESSIVE_CLIENTS.contains(c),
                "{c} is in the primary chain but cannot supply a progressive rendition"
            );
        }
    }

    /// One grid item in the exact shape a live channel Videos page returned: a
    /// single `metadataRows` entry whose two parts are the view count and the
    /// upload age. `innertube-rs` reads these out of `metadataRows[1]`, so it
    /// sees neither, and reports the view count as the channel name.
    fn lockup_item(id: &str, views: &str, age_label: &str, age_compact: &str) -> serde_json::Value {
        serde_json::json!({
            "contentId": id,
            "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
            "contentImage": {
                "thumbnailViewModel": {
                    "image": {
                        "sources": [
                            { "url": "https://example.invalid/small.jpg", "width": 360, "height": 202 },
                            { "url": "https://example.invalid/large.jpg", "width": 720, "height": 404 }
                        ]
                    },
                    "overlays": [{
                        "thumbnailBottomOverlayViewModel": {
                            "badges": [{ "thumbnailBadgeViewModel": { "text": "12:34" } }]
                        }
                    }]
                }
            },
            "metadata": {
                "lockupMetadataViewModel": {
                    "title": { "content": format!("Video {id}") },
                    "metadata": {
                        "contentMetadataViewModel": {
                            "metadataRows": [{
                                "metadataParts": [
                                    { "text": { "content": views } },
                                    {
                                        "accessibilityLabel": age_label,
                                        "text": { "content": age_compact }
                                    }
                                ]
                            }]
                        }
                    },
                    "menuButton": {
                        "buttonViewModel": {
                            "onTap": {
                                "innertubeCommand": {
                                    "playlistEditEndpoint": {
                                        "actions": [{ "videoId": id }]
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })
    }

    fn wrap(items: Vec<serde_json::Value>) -> serde_json::Value {
        serde_json::json!({
            "onResponseReceivedActions": [{
                "appendContinuationItemsAction": {
                    "continuationItems": items
                }
            }]
        })
    }

    #[test]
    fn reads_publish_date_and_views_from_the_single_row_grid() {
        let raw = wrap(vec![lockup_item("abc123", "1.1M", "2 days ago", "2d ago")]);
        let dto = channel_videos_from_raw(&raw, "UCtest");
        assert_eq!(dto.videos.len(), 1);
        let v = &dto.videos[0];
        assert_eq!(v.video_id, "abc123");
        assert_eq!(v.title, "Video abc123");
        // The spelled-out label is preferred: it does not shift units by locale.
        assert_eq!(v.published_text.as_deref(), Some("2 days ago"));
        assert_eq!(v.view_count.as_deref(), Some("1.1M"));
        assert_eq!(v.duration_seconds, Some(754));
        assert_eq!(v.thumbnails.len(), 2);
        assert_eq!(v.channel_id, "UCtest");
    }

    /// The menu on a grid item repeats the video id inside playlist-edit
    /// endpoints. Walking naively would collect the same video several times and
    /// the feed would show it repeatedly.
    #[test]
    fn does_not_collect_menu_duplicates() {
        let raw = wrap(vec![lockup_item("dup1", "5K", "3 days ago", "3d ago")]);
        let dto = channel_videos_from_raw(&raw, "UCtest");
        assert_eq!(dto.videos.len(), 1);
        assert_eq!(dto.videos[0].video_id, "dup1");
    }

    #[test]
    fn keeps_grid_items_in_page_order() {
        let raw = wrap(vec![
            lockup_item("first", "1K", "1 hour ago", "1h ago"),
            lockup_item("second", "2K", "2 days ago", "2d ago"),
            lockup_item("third", "3K", "3 weeks ago", "3w ago"),
        ]);
        let dto = channel_videos_from_raw(&raw, "UCtest");
        let ids: Vec<&str> = dto.videos.iter().map(|v| v.video_id.as_str()).collect();
        assert_eq!(ids, ["first", "second", "third"]);
    }

    #[test]
    fn finds_the_next_page_token() {
        let raw = serde_json::json!({
            "onResponseReceivedActions": [{
                "appendContinuationItemsAction": {
                    "continuationItems": [
                        { "continuationItemRenderer": {
                            "continuationEndpoint": {
                                "continuationCommand": { "token": "NEXT_PAGE_TOKEN" }
                            }
                        }}
                    ]
                }
            }]
        });
        let dto = channel_videos_from_raw(&raw, "UCtest");
        assert_eq!(dto.continuation.as_deref(), Some("NEXT_PAGE_TOKEN"));
        assert!(dto.videos.is_empty());
    }

    /// The older rich-item renderer, so a layout change in either direction does
    /// not silently empty the feed.
    #[test]
    fn still_reads_the_legacy_video_renderer() {
        let raw = serde_json::json!({
            "contents": [{
                "richItemRenderer": {
                    "content": {
                        "videoRenderer": {
                            "videoId": "legacy1",
                            "title": { "runs": [{ "text": "Legacy video" }] },
                            "publishedTimeText": { "simpleText": "5 days ago" },
                            "viewCountText": { "simpleText": "9,876 views" },
                            "lengthText": { "simpleText": "1:02:03" },
                            "thumbnail": { "thumbnails": [
                                { "url": "https://example.invalid/legacy.jpg", "width": 480, "height": 360 }
                            ] }
                        }
                    }
                }
            }]
        });
        let dto = channel_videos_from_raw(&raw, "UCtest");
        assert_eq!(dto.videos.len(), 1);
        let v = &dto.videos[0];
        assert_eq!(v.video_id, "legacy1");
        assert_eq!(v.published_text.as_deref(), Some("5 days ago"));
        assert_eq!(v.view_count.as_deref(), Some("9,876 views"));
        assert_eq!(v.duration_seconds, Some(3723));
        assert_eq!(v.thumbnails.len(), 1);
    }

    #[test]
    fn a_page_with_no_items_yields_an_empty_list_rather_than_panicking() {
        let dto = channel_videos_from_raw(&serde_json::json!({}), "UCtest");
        assert!(dto.videos.is_empty());
        assert!(dto.continuation.is_none());
    }

    fn video_item(id: &str, title: &str) -> innertube_rs::SearchResultItem {
        innertube_rs::SearchResultItem::Video(innertube_rs::SearchVideoItem {
            video_id: id.to_string(),
            title: title.to_string(),
            author: "author".to_string(),
            channel_id: "UCauthor".to_string(),
            duration: None,
            view_count: None,
            published_time: None,
            thumbnails: Vec::new(),
        })
    }

    fn channel_item(id: &str, title: &str) -> innertube_rs::SearchResultItem {
        innertube_rs::SearchResultItem::Channel(innertube_rs::SearchChannelItem {
            channel_id: id.to_string(),
            title: title.to_string(),
            subscriber_count: None,
            video_count: None,
            thumbnails: Vec::new(),
        })
    }

    /// A channel for the *channel-forced* half, which is now a DTO list read from
    /// the raw response rather than parser output.
    fn channel_dto(id: &str, title: &str) -> SearchChannelItemDto {
        SearchChannelItemDto {
            channel_id: id.to_string(),
            title: title.to_string(),
            subscriber_count: None,
            video_count: None,
            thumbnails: Vec::new(),
        }
    }

    fn channel_ids(items: &[SearchItemDto]) -> Vec<&str> {
        items
            .iter()
            .filter_map(|i| match i {
                SearchItemDto::Channel(c) => Some(c.channel_id.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The bug being fixed: the unfiltered search answers a loose channel query
    /// with videos only, so the channel is absent from the response entirely. It
    /// must still surface, from the channel-forced half.
    #[test]
    fn a_channel_absent_from_the_unfiltered_results_still_appears() {
        let channels = [channel_dto("UCjohndoes", "JohnDoesChannel")];
        let general = [video_item("v1", "Some video")];
        let merged = merge_search_items(&channels, &general, &[]);
        assert_eq!(channel_ids(&merged), ["UCjohndoes"]);
    }

    #[test]
    fn channels_are_hoisted_above_videos() {
        let channels = [channel_dto("UCa", "A")];
        let general = [video_item("v1", "one"), video_item("v2", "two")];
        let merged = merge_search_items(&channels, &general, &[]);
        assert!(
            matches!(merged.first(), Some(SearchItemDto::Channel(_))),
            "expected a channel first, got {:?}",
            merged.first()
        );
        assert_eq!(merged.len(), 3);
    }

    /// A channel in both halves must appear once. Without this, searching a name
    /// shows the same channel twice at the top of its own results.
    #[test]
    fn a_channel_present_in_both_halves_is_not_duplicated() {
        let channels = [channel_dto("UCdup", "Dup")];
        let general = [channel_item("UCdup", "Dup"), video_item("v1", "one")];
        let merged = merge_search_items(&channels, &general, &[]);
        assert_eq!(channel_ids(&merged), ["UCdup"]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn the_channel_section_is_capped() {
        let channels: Vec<_> = (0..10)
            .map(|i| channel_dto(&format!("UC{i}"), &format!("Channel {i}")))
            .collect();
        let general = [video_item("v1", "one")];
        let merged = merge_search_items(&channels, &general, &[]);
        assert_eq!(
            merged
                .iter()
                .take_while(|i| matches!(i, SearchItemDto::Channel(_)))
                .count(),
            CHANNEL_SECTION_LIMIT
        );
        // The video must survive even though the channels filled the front.
        assert!(merged.iter().any(|i| matches!(i, SearchItemDto::Video(_))));
    }

    /// YouTube has been seen to repeat a channel id inside one shelf; the set
    /// has to collapse those.
    #[test]
    fn duplicate_ids_inside_one_shelf_collapse() {
        let channels = [
            channel_dto("UCsame", "Same"),
            channel_dto("UCsame", "Same"),
        ];
        let merged = merge_search_items(&channels, &[], &[]);
        assert_eq!(channel_ids(&merged), ["UCsame"]);
    }

    /// A channel search with no videos must still produce results rather than an
    /// empty page that reads as "no results for".
    #[test]
    fn a_channel_only_result_set_is_not_empty() {
        let channels = [channel_dto("UCsolo", "Solo")];
        let merged = merge_search_items(&channels, &[], &[]);
        assert_eq!(merged.len(), 1);
    }

    /// Both halves failing must not be fatal, and must not panic on empty slices.
    #[test]
    fn empty_inputs_produce_no_items() {
        assert!(merge_search_items(&[], &[], &[]).is_empty());
    }

    fn playlist_item(id: &str, title: &str) -> SearchItemDto {
        SearchItemDto::Playlist(SearchPlaylistItemDto {
            id: id.to_string(),
            title: title.to_string(),
            thumbnail: None,
            author: None,
            video_count: None,
        })
    }

    /// A playlist must sit where YouTube put it, not after the last video. Its
    /// ordinal counts the videos that preceded it in the raw response.
    #[test]
    fn playlists_are_interleaved_at_their_video_ordinal() {
        let general = [
            video_item("v1", "One"),
            video_item("v2", "Two"),
            video_item("v3", "Three"),
        ];
        let playlists = [(1usize, playlist_item("PLx", "X"))];
        let merged = merge_search_items(&[], &general, &playlists);
        let kinds: Vec<&str> = merged
            .iter()
            .map(|i| match i {
                SearchItemDto::Video(_) => "video",
                SearchItemDto::Channel(_) => "channel",
                SearchItemDto::Playlist(_) => "playlist",
            })
            .collect();
        assert_eq!(kinds, ["video", "playlist", "video", "video"]);
    }

    /// A playlist at ordinal 0 (above every returned video) stays ahead of them,
    /// and one whose ordinal overshoots still surfaces rather than vanishing.
    #[test]
    fn playlist_ordinals_at_the_edges_still_land() {
        let general = [video_item("v1", "One"), video_item("v2", "Two")];
        let playlists = [
            (0usize, playlist_item("PLtop", "Top")),
            (9usize, playlist_item("PLend", "End")),
        ];
        let merged = merge_search_items(&[], &general, &playlists);
        let ids: Vec<String> = merged
            .iter()
            .map(|i| match i {
                SearchItemDto::Playlist(p) => p.id.clone(),
                SearchItemDto::Video(v) => v.video_id.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(ids, ["PLtop", "v1", "v2", "PLend"]);
    }

    // -- search_channels_from_raw ---------------------------------------------
    //
    // `innertube_rs` parses zero channels out of a search response that plainly
    // contains them, which is why this extractor exists. These tests pin the
    // response shape it depends on.

    fn raw_search_with(channels: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "contents": {
                "twoColumnSearchResultsRenderer": {
                    "primaryContents": {
                        "sectionListRenderer": {
                            "contents": [{
                                "itemSectionRenderer": { "contents": channels }
                            }]
                        }
                    }
                }
            }
        })
    }

    fn channel_renderer(id: &str, title: &str) -> serde_json::Value {
        serde_json::json!({
            "channelRenderer": {
                "channelId": id,
                "title": { "simpleText": title },
                "subscriberCountText": { "simpleText": "@handle" },
                "videoCountText": { "simpleText": "30.9K subscribers" },
                "thumbnail": {
                    "thumbnails": [
                        { "url": "//yt3.example/a88.jpg", "width": 88, "height": 88 },
                        { "url": "//yt3.example/a176.jpg", "width": 176, "height": 176 },
                    ]
                },
            }
        })
    }

    /// The core bug: the node is present in the response and must come out.
    #[test]
    fn channel_renderers_are_found_wherever_they_are_nested() {
        let raw = raw_search_with(serde_json::json!([
            channel_renderer("UCa", "Jamie Gilgen Furniture"),
        ]));
        let out = search_channels_from_raw(&raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].channel_id, "UCa");
        assert_eq!(out[0].title, "Jamie Gilgen Furniture");
    }

    /// YouTube's `channelRenderer` names these backwards: `subscriberCountText`
    /// is the handle, `videoCountText` is the subscriber count. Reading them the
    /// obvious way shows "@gilgen_furniture — 30.9K subscribers".
    #[test]
    fn the_subscriber_count_comes_from_video_count_text() {
        let raw = raw_search_with(serde_json::json!([channel_renderer("UCa", "Name")]));
        let out = search_channels_from_raw(&raw);
        assert_eq!(out[0].subscriber_count.as_deref(), Some("30.9K subscribers"));
        assert!(
            !out[0].subscriber_count.as_deref().unwrap_or_default().starts_with('@'),
            "subscriber_count must not hold the handle"
        );
    }

    #[test]
    fn channel_avatars_are_collected() {
        let raw = raw_search_with(serde_json::json!([channel_renderer("UCa", "Name")]));
        let out = search_channels_from_raw(&raw);
        assert_eq!(out[0].thumbnails.len(), 2);
        assert_eq!(out[0].thumbnails[0].url, "//yt3.example/a88.jpg");
    }

    #[test]
    fn a_channel_shelf_repeating_one_id_yields_it_once() {
        let raw = raw_search_with(serde_json::json!([
            channel_renderer("UCsame", "Same"),
            channel_renderer("UCsame", "Same"),
        ]));
        assert_eq!(search_channels_from_raw(&raw).len(), 1);
    }

    /// A channel result that cannot be opened is worse than no result.
    #[test]
    fn a_channel_without_an_id_or_title_is_dropped() {
        let raw = raw_search_with(serde_json::json!([
            channel_renderer("UCok", "Fine"),
            serde_json::json!({ "channelRenderer": { "title": { "simpleText": "No id" } } }),
            serde_json::json!({ "channelRenderer": { "channelId": "UCnoid" } }),
        ]));
        let out = search_channels_from_raw(&raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].channel_id, "UCok");
    }

    /// Titles also arrive as `runs`, not just `simpleText`.
    #[test]
    fn a_run_split_title_is_rejoined() {
        let raw = raw_search_with(serde_json::json!([{
            "channelRenderer": {
                "channelId": "UCa",
                "title": { "runs": [{ "text": "Jamie Gilgen " }, { "text": "Furniture" }] },
            }
        }]));
        let out = search_channels_from_raw(&raw);
        assert_eq!(out[0].title, "Jamie Gilgen Furniture");
    }

    /// A response with no channel shelf must yield nothing, not panic.
    #[test]
    fn a_video_only_response_yields_no_channels() {
        let raw = raw_search_with(serde_json::json!([
            serde_json::json!({ "videoRenderer": { "videoId": "v1" } }),
        ]));
        assert!(search_channels_from_raw(&raw).is_empty());
        assert!(search_channels_from_raw(&serde_json::json!({})).is_empty());
    }

    // -- search_playlists_from_raw ---------------------------------------------
    //
    // `innertube_rs` reads search playlists from `playlistRenderer` only, but
    // modern search serves `lockupViewModel` — its parser yields empty ids and
    // "Untitled Playlist". These tests pin the lockup shape this extractor reads.

    /// A `lockupViewModel` playlist exactly as `playlist_from_lockup` expects it.
    fn lockup_playlist(id: &str, title: &str) -> serde_json::Value {
        serde_json::json!({
            "lockupViewModel": {
                "contentId": id,
                "contentType": "LOCKUP_CONTENT_TYPE_PLAYLIST",
                "metadata": {
                    "lockupMetadataViewModel": {
                        "title": { "content": title },
                        "metadata": {
                            "contentMetadataViewModel": {
                                "metadataRows": [{
                                    "metadataParts": [
                                        { "text": { "content": "Satish Dhawale" } },
                                        { "text": { "content": "Playlist" } }
                                    ]
                                }]
                            }
                        }
                    }
                },
                "contentImage": {
                    "collectionThumbnailViewModel": {
                        "primaryThumbnail": {
                            "thumbnailViewModel": {
                                "image": {
                                    "sources": [
                                        { "url": "//i.ytimg.example/pl1.jpg", "width": 336, "height": 188 },
                                        { "url": "//i.ytimg.example/pl2.jpg", "width": 672, "height": 376 }
                                    ]
                                },
                                "overlays": [{
                                    "thumbnailOverlayBadgeViewModel": {
                                        "thumbnailBadges": [{
                                            "thumbnailBadgeViewModel": { "text": { "content": "31 videos" } }
                                        }]
                                    }
                                }]
                            }
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn lockup_playlists_are_found_in_the_response() {
        let raw = raw_search_with(serde_json::json![lockup_playlist("PLid", "SQL Course")]);
        let out = search_playlists_from_raw(&raw);
        assert_eq!(out.len(), 1);
        let SearchItemDto::Playlist(p) = &out[0] else {
            panic!("expected a playlist item");
        };
        assert_eq!(p.id, "PLid");
        assert_eq!(p.title, "SQL Course");
    }

    #[test]
    fn lockup_playlist_detail_fields_are_read() {
        let raw = raw_search_with(serde_json::json![lockup_playlist("PLid", "SQL Course")]);
        let out = search_playlists_from_raw(&raw);
        let SearchItemDto::Playlist(p) = &out[0] else {
            panic!("expected a playlist item");
        };
        assert_eq!(p.author.as_deref(), Some("Satish Dhawale"));
        assert_eq!(p.video_count.as_deref(), Some("31 videos"));
        assert_eq!(p.thumbnail.as_deref(), Some("//i.ytimg.example/pl2.jpg"));
    }

    #[test]
    fn lockup_playlists_without_id_or_title_are_dropped() {
        let raw = raw_search_with(serde_json::json!([
            lockup_playlist("PLok", "Fine"),
            serde_json::json!({
                "lockupViewModel": { "contentType": "LOCKUP_CONTENT_TYPE_PLAYLIST", "title": { "content": "No id" } }
            }),
            serde_json::json!({
                "lockupViewModel": { "contentType": "LOCKUP_CONTENT_TYPE_PLAYLIST", "contentId": "PLnoid" }
            }),
        ]));
        let out = search_playlists_from_raw(&raw);
        assert_eq!(out.len(), 1);
        let SearchItemDto::Playlist(p) = &out[0] else {
            panic!("expected a playlist item");
        };
        assert_eq!(p.id, "PLok");
    }

    /// Non-playlist lockups (videos, shorts, channels) must not leak through.
    #[test]
    fn non_playlist_lockups_are_ignored() {
        let raw = raw_search_with(serde_json::json!([
            serde_json::json!({
                "lockupViewModel": {
                    "contentId": "VIDEO1",
                    "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
                    "metadata": { "lockupMetadataViewModel": { "title": { "content": "A video" } } }
                }
            })
        ]));
        assert!(search_playlists_from_raw(&raw).is_empty());
    }

    #[test]
    fn legacy_playlist_renderers_are_still_read() {
        let raw = raw_search_with(serde_json::json!([{
            "gridPlaylistRenderer": {
                "playlistId": "PLlegacy",
                "title": { "simpleText": "Legacy list" },
                "videoCountText": { "simpleText": "12 videos" },
                "shortBylineText": { "runs": [{ "text": "Author Name" }] },
                "thumbnail": {
                    "thumbnails": [
                        { "url": "//i.ytimg.example/l1.jpg", "width": 336, "height": 188 }
                    ]
                }
            }
        }]));
        let out = search_playlists_from_raw(&raw);
        assert_eq!(out.len(), 1);
        let SearchItemDto::Playlist(p) = &out[0] else {
            panic!("expected a playlist item");
        };
        assert_eq!(p.id, "PLlegacy");
        assert_eq!(p.title, "Legacy list");
        assert_eq!(p.video_count.as_deref(), Some("12 videos"));
        assert_eq!(p.author.as_deref(), Some("Author Name"));
    }

    /// Duplicate shelf entries collapse to one playlist.
    #[test]
    fn a_repeated_lockup_yields_one_playlist() {
        let raw = raw_search_with(serde_json::json!([
            lockup_playlist("PLdup", "Repeated"),
            lockup_playlist("PLdup", "Repeated"),
        ]));
        assert_eq!(search_playlists_from_raw(&raw).len(), 1);
    }

    #[test]
    fn a_video_only_response_yields_no_playlists() {
        let raw = raw_search_with(serde_json::json!([
            serde_json::json!({ "videoRenderer": { "videoId": "v1" } }),
        ]));
        assert!(search_playlists_from_raw(&raw).is_empty());
    }

    /// Search playlists must carry the number of videos that preceded them, so
    /// the merge can drop them back where YouTube put them.
    #[test]
    fn playlists_carry_their_video_ordinal() {
        let raw = raw_search_with(serde_json::json!([
            { "videoRenderer": { "videoId": "v1" } },
            { "videoRenderer": { "videoId": "v2" } },
            lockup_playlist("PLmid", "Mid"),
            { "videoRenderer": { "videoId": "v3" } },
            lockup_playlist("PLend", "End"),
        ]));
        let out = playlists_with_position(&raw);
        let positions: Vec<usize> = out.iter().map(|(p, _)| *p).collect();
        let ids: Vec<&str> = out.iter().map(|(_, it)| it.id.as_str()).collect();
        assert_eq!(positions, [2, 3]);
        assert_eq!(ids, ["PLmid", "PLend"]);
    }

    // -- playlist_detail_from_raw ----------------------------------------------

    fn playlist_video_lockup(
        video_id: &str,
        title: &str,
        channel: &str,
        channel_id: &str,
        views: &str,
        published: &str,
        duration: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "lockupViewModel": {
                "contentId": video_id,
                "contentType": "LOCKUP_CONTENT_TYPE_VIDEO",
                "contentImage": {
                    "thumbnailViewModel": {
                        "image": { "sources": [
                            { "url": "//i.ytimg.example/v1.jpg", "width": 336, "height": 188 },
                            { "url": "//i.ytimg.example/v2.jpg", "width": 672, "height": 376 }
                        ] },
                        "overlays": [{
                            "thumbnailBottomOverlayViewModel": {
                                "badges": [{
                                    "thumbnailBadgeViewModel": { "text": duration }
                                }]
                            }
                        }]
                    }
                },
                "metadata": {
                    "lockupMetadataViewModel": {
                        "title": { "content": title },
                        "image": {
                            "decoratedAvatarViewModel": {
                                "rendererContext": { "commandContext": { "onTap": {
                                    "innertubeCommand": { "browseEndpoint": { "browseId": channel_id } }
                                } } }
                            }
                        },
                        "metadata": {
                            "contentMetadataViewModel": {
                                "metadataRows": [
                                    { "metadataParts": [ {
                                        "text": {
                                            "content": channel,
                                            "commandRuns": [ { "onTap": { "innertubeCommand": {
                                                "browseEndpoint": { "browseId": channel_id }
                                            } } } ]
                                        }
                                    } ] },
                                    { "metadataParts": [
                                        { "accessibilityLabel": "961 thousand views", "text": { "content": views } },
                                        { "accessibilityLabel": "1 year ago", "text": { "content": published } }
                                    ] }
                                ]
                            }
                        }
                    }
                }
            }
        })
    }

    fn playlist_raw_with(video: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "metadata": {
                "playlistMetadataRenderer": { "title": "My List", "description": "Desc" }
            },
            "sidebar": {
                "playlistSidebarRenderer": { "items": [
                    { "playlistSidebarPrimaryInfoRenderer": { "stats": [
                        { "runs": [ { "text": "2" }, { "text": " videos" } ] },
                        { "simpleText": "1,000 views" }
                    ] } },
                    { "videoOwnerRenderer": { "title": { "runs": [ { "text": "Chan" } ] } } }
                ] }
            },
            "contents": { "wrap": [ video, extra ] }
        })
    }

    #[test]
    fn playlist_detail_reads_header_and_video_metadata() {
        let raw = playlist_raw_with(
            playlist_video_lockup(
                "KvT6", "SQL Day 1", "Satish Dhawale", "UCRDhr", "961K", "1y ago", "16:55",
            ),
            // A related-playlist lockup must not be mistaken for a playlist row.
            lockup_playlist("PLrelated", "Related"),
        );
        let dto = playlist_detail_from_raw(&raw, "PLmine");

        assert_eq!(dto.id, "PLmine");
        assert_eq!(dto.title, "My List");
        assert_eq!(dto.author.as_deref(), Some("Chan"));
        assert_eq!(dto.description.as_deref(), Some("Desc"));
        assert_eq!(dto.video_count, Some(2));

        assert_eq!(dto.videos.len(), 1);
        let v = &dto.videos[0];
        assert_eq!(v.video_id, "KvT6");
        assert_eq!(v.title, "SQL Day 1");
        assert_eq!(dto.videos[0].author, "Satish Dhawale");
        assert_eq!(v.channel_id.as_deref(), Some("UCRDhr"));
        assert_eq!(v.duration_seconds, Some(16 * 60 + 55));
        assert_eq!(dto.videos[0].view_count.as_deref(), Some("961K"));
        assert_eq!(dto.videos[0].published.as_deref(), Some("1 year ago"));
        assert_eq!(dto.videos[0].index, Some(1));
        assert_eq!(dto.thumbnail.as_deref(), Some("//i.ytimg.example/v2.jpg"));
    }

    #[test]
    fn classifies_ages_apart_from_view_counts() {
        assert!(looks_like_age("2 days ago"));
        assert!(looks_like_age("Streamed 3 weeks ago"));
        assert!(looks_like_age("Yesterday"));
        assert!(!looks_like_age("1.1M"));
        assert!(looks_like_views("1.1M views"));
        assert!(looks_like_views("1.1M"));
        assert!(!looks_like_views("2 days ago"));
    }

    fn v(itag: u32, height: u32, fps: u32, bitrate: u64) -> ResolvedVariant {
        ResolvedVariant {
            itag,
            quality_label: Some(format!("{height}p")),
            height: Some(height),
            fps: Some(fps),
            bitrate,
            is_hdr: false,
            mime: "video/mp4; codecs=\"avc1.640028\"".to_string(),
            content_length: None,
            url: format!("https://example.invalid/{itag}"),
            user_agent: "test-ua".to_string(),
            // Video-only renditions: no audio track to speak of.
            audio_is_default: None,
        }
    }

    /// A muxed rendition in a named language, as a multi-audio-track video returns.
    fn muxed(itag: u32, height: u32, bitrate: u64, original: bool) -> ResolvedVariant {
        ResolvedVariant {
            itag,
            quality_label: Some(format!("{height}p")),
            height: Some(height),
            fps: Some(30),
            bitrate,
            is_hdr: false,
            mime: "video/mp4; codecs=\"avc1.42001E, mp4a.40.2\"".to_string(),
            content_length: None,
            url: format!("https://example.invalid/{itag}-{}", if original { "en" } else { "hi" }),
            user_agent: "test-ua".to_string(),
            audio_is_default: Some(original),
        }
    }

    #[test]
    fn dedupe_keeps_the_original_audio_track_over_a_higher_bitrate_dub() {
        // Every audio track reuses the same itags, so `rank_and_dedupe` collapses
        // one copy per language. Ordering that group by bitrate alone kept the dub,
        // because dubbed tracks are routinely the higher bitrate - which is how an
        // English video ended up playing in Hindi.
        let out = rank_and_dedupe(vec![
            muxed(18, 360, 500_000, false),
            muxed(18, 360, 900_000, true),
        ]);

        assert_eq!(out.len(), 1, "the two copies of itag 18 must collapse");
        assert_eq!(
            out[0].audio_is_default,
            Some(true),
            "the surviving rendition must be the video's original language"
        );
    }

    #[test]
    fn ordering_puts_the_original_language_ahead_of_a_better_dub() {
        // Distinct itags, so nothing collapses here - this pins the ordering. The
        // original track is the *lower* quality rendition (360p against 720p) and
        // still has to come first, because presenting a dub in place of the video's
        // own audio is worse than presenting it at a lower resolution.
        let out = rank_and_dedupe(vec![
            muxed(22, 720, 2_000_000, false),
            muxed(18, 360, 1_000_000, true),
        ]);

        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0].audio_is_default,
            Some(true),
            "the original-language rendition must be presented first even at 360p"
        );
    }

    #[test]
    fn ranks_by_resolution_then_frame_rate_then_bitrate() {
        let out = rank_and_dedupe(vec![
            v(134, 360, 30, 700_000),
            v(137, 1080, 30, 4_000_000),
            v(136, 720, 60, 2_000_000),
            v(135, 480, 30, 9_000_000),
        ]);
        let itags: Vec<u32> = out.iter().map(|x| x.itag).collect();
        assert_eq!(itags, vec![137, 136, 135, 134]);
    }

    /// Regression: duplicate itags reached the client and would collide in the
    /// UI's keyed quality list.
    #[test]
    fn drops_duplicate_itags_keeping_highest_bitrate() {
        let out = rank_and_dedupe(vec![
            v(140, 0, 0, 50_470),
            v(140, 0, 0, 131_075),
            v(139, 0, 0, 50_466),
            v(139, 0, 0, 50_470),
            v(137, 1080, 30, 4_791_052),
        ]);
        let itags: Vec<u32> = out.iter().map(|x| x.itag).collect();
        assert_eq!(itags, vec![137, 140, 139]);
        let itag140 = out.iter().find(|x| x.itag == 140).unwrap();
        assert_eq!(itag140.bitrate, 131_075);
    }

    #[test]
    fn empty_input_stays_empty() {
        assert!(rank_and_dedupe(Vec::new()).is_empty());
    }

    /// Regression: a plain `dedup_by_key` over the best-first list silently
    /// removed nothing here, because 139 and 140 interleave once equal
    /// (height, fps, bitrate) pairs are broken by the itag tiebreak.
    #[test]
    fn interleaved_duplicate_itags_are_collapsed() {
        let out = rank_and_dedupe(vec![
            v(140, 0, 0, 131_075),
            v(139, 0, 0, 50_470),
            v(139, 0, 0, 50_466),
            v(140, 0, 0, 131_089),
        ]);
        let itags: Vec<u32> = out.iter().map(|x| x.itag).collect();
        assert_eq!(itags, vec![140, 139]);
        let mut seen = std::collections::HashSet::new();
        assert!(out.iter().all(|x| seen.insert(x.itag)), "itag repeated");
    }

    fn markers_body() -> serde_json::Value {
        serde_json::json!({
            "playabilityStatus": { "status": "OK" },
            "playerOverlays": {
                "playerOverlayRenderer": {
                    "decoratedPlayerBarRenderer": {
                        "decoratedPlayerBarRenderer": {
                            "playerBar": {
                                "multiMarkersPlayerBarRenderer": {
                                    "markersMap": [
                                        {
                                            "key": "time_bar_chapters",
                                            "value": {
                                                "chapters": [
                                                    {
                                                        "chapterRenderer": {
                                                            "title": { "simpleText": "Intro" },
                                                            "timeRangeStartMillis": "0",
                                                            "thumbnail": {
                                                                "thumbnails": [
                                                                    { "url": "https://i.ytimg.com/vi/x/intro.jpg", "width": 160, "height": 90 }
                                                                ]
                                                            }
                                                        }
                                                    },
                                                    {
                                                        "chapterRenderer": {
                                                            "title": { "simpleText": "Deep dive" },
                                                            "timeRangeStartMillis": "54000",
                                                            "thumbnail": {
                                                                "thumbnails": [
                                                                    { "url": "https://i.ytimg.com/vi/x/deep.jpg" }
                                                                ]
                                                            }
                                                        }
                                                    }
                                                ]
                                            }
                                        }
                                    ]
                                }
                            }
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn chapter_markers_are_read_from_the_player_bar() {
        let chapters = extract_chapters(&markers_body());
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].title, "Intro");
        assert_eq!(chapters[0].start_ms, 0);
        assert_eq!(
            chapters[0].thumb_url.as_deref(),
            Some("https://i.ytimg.com/vi/x/intro.jpg")
        );
        assert_eq!(chapters[1].title, "Deep dive");
        assert_eq!(chapters[1].start_ms, 54_000);
    }

    #[test]
    fn markers_are_sorted_and_duplicate_starts_collapsed() {
        let mut body = markers_body();
        // Repeat the 0:00 start and shove the deep-dive chapter first.
        let map = body.pointer_mut(
            "/playerOverlays/playerOverlayRenderer/decoratedPlayerBarRenderer/decoratedPlayerBarRenderer/playerBar/multiMarkersPlayerBarRenderer/markersMap/0/value/chapters",
        ).expect("fixture path");
        map.as_array_mut().expect("chapters array").swap(0, 1);
        map.as_array_mut().expect("chapters array")
            .push(serde_json::json!({ "chapterRenderer": {
                "title": { "simpleText": "Intro again" },
                "timeRangeStartMillis": "0"
            } }));

        let chapters = extract_chapters(&body);
        assert_eq!(chapters.len(), 2, "0:00 dupes collapse to one chapter");
        assert!(chapters.windows(2).all(|w| w[0].start_ms <= w[1].start_ms));
    }

    #[test]
    fn markers_without_a_start_are_dropped() {
        let mut body = markers_body();
        let map = body.pointer_mut(
            "/playerOverlays/playerOverlayRenderer/decoratedPlayerBarRenderer/decoratedPlayerBarRenderer/playerBar/multiMarkersPlayerBarRenderer/markersMap/0/value/chapters",
        ).expect("fixture path");
        map.as_array_mut().expect("chapters array")
            .push(serde_json::json!({ "chapterRenderer": { "title": { "simpleText": "No stamp" } } }));

        let chapters = extract_chapters(&body);
        assert!(chapters.iter().all(|c| c.title != "No stamp"));
    }

    #[test]
    fn description_timestamps_become_chapters() {
        let desc = concat!(
            "0:00 Intro\n",
            "1:23:45 Deep dive\n",
            "12:34 Middle\n",
            "not a timestamp line\n",
            "0:00 - Intro again\n",
        );
        let chapters = parse_description_chapters(desc);
        let starts: Vec<u64> = chapters.iter().map(|c| c.start_ms).collect();
        // 0:00 from the first line, 12:34 = 754s, 1:23:45 = 5025s; both 0:00
        // lines collapse, and the duplicate-started "Intro again" is dropped.
        assert_eq!(starts, vec![0, 754_000, 5_025_000]);
    }

    #[test]
    fn fractional_and_dashed_timestamps_parse() {
        let chapters = parse_description_chapters(
            "1:23.456 High noon\n1:24 - High noon\n",
        );
        let starts: Vec<u64> = chapters.iter().map(|c| c.start_ms).collect();
        assert_eq!(starts, vec![83_000, 84_000]);
    }

    #[test]
    fn non_time_lines_are_ignored() {
        let chapters = parse_description_chapters("Text intro\n0:00 Start\njust words");
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].title, "Start");
    }

    #[test]
    fn full_description_is_read_from_microformat() {
        let raw = serde_json::json!({
            "microformat": {
                "playerMicroformatRenderer": {
                    "description": { "runs": [ { "text": "Part 1: " }, { "text": "setup" } ] }
                }
            }
        });
        assert_eq!(
            extract_full_description(&raw).as_deref(),
            Some("Part 1: setup")
        );
        assert_eq!(extract_full_description(&serde_json::json!({})), None);
    }

    #[test]
    fn markers_win_over_description_fallback() {
        let markers = extract_chapters(&markers_body());
        let desc = "0:00 Outro\n1:00 Wrong titles\n";
        let got = chapters_with_fallback(&markers, Some(desc), None);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].title, "Intro", "markers beat description text");
    }

    #[test]
    fn fallback_uses_full_then_short_description() {
        let empty: Vec<ChapterDto> = Vec::new();
        let got =
            chapters_with_fallback(&empty, Some("0:00 Full\n"), Some("0:00 Short"));
        assert_eq!(got[0].title, "Full");
        let got = chapters_with_fallback(&empty, None, Some("0:00 Short"));
        assert_eq!(got[0].title, "Short");
        assert!(chapters_with_fallback(&empty, None, None).is_empty());
    }
}

pub async fn video_info(
    yt: &mut InnerTube,
    video_id: &str,
) -> innertube_rs::Result<VideoInfoDto> {
    video_info_full(yt, video_id).await.map(|f| f.info)
}


/// Searches everything at once: the unfiltered result set, plus channels that
/// only appear once the search is *forced* to be about channels.
///
/// YouTube answers an unfiltered `/search` with a channel shelf only when the
/// query matches a channel closely. A loose query — `John Doe` for a channel
/// called `JohnDoesChannel` — comes back as videos only, so the channel is
/// genuinely absent from the response rather than being missed by the parser.
/// Forcing `SearchTypeFilter::Channel` gets it back. Previously the page hid
/// that behind a mode toggle, so a channel search looked broken.
///
/// Both requests run concurrently and cost the same round-trip latency, so the
/// merged view is free relative to searching twice.
///
/// Channels are hoisted to the top (capped at [`CHANNEL_SECTION_LIMIT`]) because
/// that is the case that was broken; leaving them at the bottom would reproduce
/// the original complaint, just with a longer scroll.
const CHANNEL_SECTION_LIMIT: usize = 4;

/// Runs `fut` on a dedicated thread with a large stack.
///
/// `innertube_rs`'s search parser recurses through the response far more deeply
/// than the raw tree walk used for channels and playlists: some live queries
/// (e.g. "sql full course") overflow the default ~2MiB worker stack inside it and
/// abort the whole process. The parser can't be fixed from here, so the call is
/// isolated on a thread that will not run out of stack. `spawn_blocking` keeps
/// the join off the async worker; the nested current-thread runtime drives the
/// request itself.
async fn run_on_big_stack<T, F>(fut: F) -> innertube_rs::Result<T>
where
    T: Send + 'static,
    F: Future<Output = innertube_rs::Result<T>> + Send + 'static,
{
    let joined = tokio::task::spawn_blocking(move || -> innertube_rs::Result<T> {
        let handle = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .name("search-parse".to_string())
            .spawn(move || -> innertube_rs::Result<T> {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("big-stack search runtime");
                rt.block_on(fut)
            })
            .map_err(|e| {
                innertube_rs::error::InnertubeError::Other(format!("spawn search thread: {e}"))
            })?;
        handle
            .join()
            .map_err(|_| innertube_rs::error::InnertubeError::Other("search worker panicked".into()))?
    });

    match joined.await {
        Ok(result) => result,
        Err(e) => Err(innertube_rs::error::InnertubeError::Other(format!(
            "search worker task failed: {e}"
        ))),
    }
}

/// The general (video) half of [`search_all`], run through [`run_on_big_stack`].
async fn big_stack_search(
    yt: &InnerTube,
    query: &str,
) -> innertube_rs::Result<innertube_rs::SearchResults> {
    let yt = yt.clone();
    let query = query.to_string();
    run_on_big_stack(async move { yt.search_with_filters(&query, None, None).await }).await
}

pub async fn search_all(yt: &InnerTube, query: &str) -> innertube_rs::Result<SearchResultsDto> {
// The channel half is read from the raw response, not through
    // `search_with_filters`: that parser returns no channels at all, which would
    // make this merge a no-op and leave "all" search video-only.
    //
    // A failure in the channel half must not fail the whole search: the video
    // results are still perfectly good, and the user asked for "all".
    let (general, channels, playlists) = tokio::join!(
        // See `run_on_big_stack`: this half uses innertube-rs's recursive search
        // parser, which has overflowed the 2MiB worker stack on real responses.
        big_stack_search(yt, query),
        search_channels(yt, query),
        search_playlists(yt, query),
    );
    let general = general?;

    let channel_items = match channels {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("channel half of search_all failed ({e}); showing general results only");
            Vec::new()
        }
    };
    let playlist_items = match playlists {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("playlist half of search_all failed ({e}); showing results without playlists");
            Vec::new()
        }
    };

    Ok(SearchResultsDto {
        query: general.query,
        items: merge_search_items(&channel_items, &general.items, &playlist_items),
        continuation: general.continuation_token,
    })
}

/// Pure merge of a channel-forced result set and an unfiltered one.
///
/// Split out from [`search_all`] so the ordering and de-duplication rules can be
/// tested without a network call — these are the rules that make channel search
/// work at all, and they are easy to regress silently.
fn merge_search_items(
    channel_items: &[SearchChannelItemDto],
    general_items: &[innertube_rs::SearchResultItem],
    playlist_items: &[(usize, SearchItemDto)],
) -> Vec<SearchItemDto> {
    let mut seen_channels: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut items: Vec<SearchItemDto> = Vec::new();

    // Channels first, capped. A channel that appears in both halves must appear
    // once, and the hoisted copy is the one that wins so ordering stays stable.
    for c in channel_items {
        if seen_channels.insert(c.channel_id.clone()) {
            items.push(SearchItemDto::Channel(c.clone()));
            if items.len() >= CHANNEL_SECTION_LIMIT {
                break;
            }
        }
    }

    for item in general_items {
        match item {
            innertube_rs::SearchResultItem::Video(v) => {
                items.push(SearchItemDto::Video(VideoSearchItemDto {
                    video_id: v.video_id.clone(),
                    title: v.title.clone(),
                    duration_seconds: v.duration.as_deref().and_then(parse_duration_to_seconds),
                    view_count: v.view_count.clone(),
                    channel_id: v.channel_id.clone(),
                    author: v.author.clone(),
                    thumbnails: v.thumbnails.iter().map(ThumbnailDto::from_yt).collect(),
                }));
            }
            innertube_rs::SearchResultItem::Channel(c) => {
                if seen_channels.insert(c.channel_id.clone()) {
                    items.push(channel_to_dto(c));
                }
            }
            innertube_rs::SearchResultItem::Playlist(_) => {
                // innertube-rs cannot read modern lockup-shaped playlist results
                // (empty id, "Untitled Playlist" fallback). Playlists are
                // re-extracted from the raw response and interleaved below.
            }
        }
    }

    interleave_playlists(&items, playlist_items)
}

/// Drops each positioned playlist in after the video it followed in YouTube's
/// own response, rather than appending every playlist after the last video.
///
/// The position is a video ordinal (how many videos preceded the playlist in
/// the raw document), so a playlist with position `N` lands after the Nth video
/// item. Channels are hoisted to the front and ignored here.
fn interleave_playlists(
    items: &[SearchItemDto],
    playlists: &[(usize, SearchItemDto)],
) -> Vec<SearchItemDto> {
    if playlists.is_empty() {
        return items.to_vec();
    }

    let mut out: Vec<SearchItemDto> = Vec::with_capacity(items.len() + playlists.len());
    let mut next = 0usize;
    let mut videos_seen = 0usize;

    // Playlists that precede every video come first.
    while next < playlists.len() && playlists[next].0 <= videos_seen {
        out.push(playlists[next].1.clone());
        next += 1;
    }

    for item in items {
        out.push(item.clone());
        if matches!(item, SearchItemDto::Video(v) if !v.video_id.is_empty()) {
            videos_seen += 1;
            while next < playlists.len() && playlists[next].0 <= videos_seen {
                out.push(playlists[next].1.clone());
                next += 1;
            }
        }
    }

    // Any playlist whose ordinal overshot the video count still has to surface.
    while next < playlists.len() {
        out.push(playlists[next].1.clone());
        next += 1;
    }

    out
}

fn channel_to_dto(c: &innertube_rs::SearchChannelItem) -> SearchItemDto {
    SearchItemDto::Channel(SearchChannelItemDto {
        channel_id: c.channel_id.clone(),
        title: c.title.clone(),
        subscriber_count: c.subscriber_count.clone(),
        video_count: c.video_count.clone(),
        thumbnails: c.thumbnails.iter().map(ThumbnailDto::from_yt).collect(),
    })
}

/// Raw `/search` response, for diagnostics.
///
/// `innertube-rs`'s parser is the suspect whenever channel results go missing: it
/// decides what a response *means*, so a shape it does not recognise comes back
/// as "no results" rather than as an error. Going to the raw JSON is the only way
/// to tell "YouTube sent nothing" apart from "the parser dropped it".
pub async fn search_raw(
    yt: &innertube_rs::Innertube,
    query: &str,
    channels_only: bool,
) -> innertube_rs::Result<serde_json::Value> {
    let params = if channels_only {
        let filters = innertube_rs::SearchFilters {
            search_type: Some(innertube_rs::SearchTypeFilter::Channel),
            ..Default::default()
        };
        Some(innertube_rs::utils::proto::encode_search_filters(&filters)?)
    } else {
        None
    };
    let mut payload = serde_json::json!({ "query": query });
    if let Some(p) = params {
        payload["params"] = serde_json::Value::String(p);
    }
    yt.call("/search", payload).await
}

/// Raw playlist `/browse` response (`browseId: VL<id>`), for diagnostics and for
/// reading fields `innertube_rs`'s `PlaylistView` drops (per-video duration,
/// views, published time).
pub async fn playlist_raw(
    yt: &innertube_rs::Innertube,
    playlist_id: &str,
) -> innertube_rs::Result<serde_json::Value> {
    let clean = if playlist_id.starts_with("VL") {
        playlist_id.to_string()
    } else {
        format!("VL{playlist_id}")
    };
    yt.call("/browse", serde_json::json!({ "browseId": clean }))
        .await
}

pub async fn search(yt: &InnerTube, query: &str) -> innertube_rs::Result<SearchResultsDto> {
    // `run_on_big_stack` for the same reason as `search_all`: the innertube-rs
    // parser overflows on some responses.
    let r = {
        let yt = yt.clone();
        let query = query.to_string();
        run_on_big_stack(async move { yt.search(&query, None).await }).await?
    };
    let mut items: Vec<SearchItemDto> = Vec::new();
    for item in r.items {
        match item {
            innertube_rs::SearchResultItem::Video(v) => {
                items.push(SearchItemDto::Video(VideoSearchItemDto {
                    video_id: v.video_id.clone(),
                    title: v.title.clone(),
                    duration_seconds: v.duration.as_deref().and_then(parse_duration_to_seconds),
                    view_count: v.view_count.clone(),
                    channel_id: v.channel_id.clone(),
                    author: v.author.clone(),
                    thumbnails: v.thumbnails.iter().map(ThumbnailDto::from_yt).collect(),
                }));
            }
            innertube_rs::SearchResultItem::Channel(c) => {
                items.push(SearchItemDto::Channel(SearchChannelItemDto {
                    channel_id: c.channel_id.clone(),
                    title: c.title.clone(),
                    subscriber_count: c.subscriber_count.clone(),
                    video_count: c.video_count.clone(),
                    thumbnails: c.thumbnails.iter().map(ThumbnailDto::from_yt).collect(),
                }));
            }
            innertube_rs::SearchResultItem::Playlist(_) => {
                // See `search_playlists`: innertube-rs emits empty ids and
                // "Untitled Playlist" for modern lockup-shaped results.
            }
        }
    }
    // Playlists come from the raw response (same query) and are interleaved at
    // the position YouTube gave them instead of being appended.
    let items = match search_playlists(yt, query).await {
        Ok(pl) => interleave_playlists(&items, &pl),
        Err(e) => {
            tracing::warn!("playlist half of search failed ({e}); showing results without playlists");
            items
        }
    };
    Ok(SearchResultsDto {
        query: query.to_string(),
        items,
        continuation: r.continuation_token,
    })
}

/// Channel-only search, backed by the same endpoint with `SearchTypeFilter::Channel`.
pub async fn search_channels(
    yt: &InnerTube,
    query: &str,
) -> innertube_rs::Result<Vec<SearchChannelItemDto>> {
    let raw = search_raw(yt.raw(), query, true).await?;
    Ok(search_channels_from_raw(&raw))
}

/// Extracts channels from a raw `/search` response.
///
/// This bypasses `innertube_rs`'s parser on purpose. That parser only recognises
/// channels in some other node shape, so for an ordinary search it silently
/// returns *zero* channels: YouTube sends the matches, and they are dropped on
/// the way out. A missing result and a dropped result look identical from the
/// outside, so the raw response is parsed directly.
///
/// Two more quirks of `channelRenderer`, both of which cost a wrong field if
/// assumed the other way round:
/// - `subscriberCountText` holds the **handle** (`@gilgen_furniture`), not a
///   subscriber count.
/// - `videoCountText` holds the **subscriber count** ("30.9K subscribers").
pub fn search_channels_from_raw(raw: &serde_json::Value) -> Vec<SearchChannelItemDto> {
    let mut found = Vec::new();
    collect_channel_renderers(raw, &mut found);

    let mut out: Vec<SearchChannelItemDto> = found
        .into_iter()
        .filter_map(|c| {
            let channel_id = text_field(&c, "channelId");
            let title = text_field(&c, "title");
            // Both are needed to open the channel, and a nameless result is not
            // worth showing.
            let (channel_id, title) = match (channel_id, title) {
                (Some(id), Some(t)) if !id.is_empty() && !t.is_empty() => (id, t),
                _ => return None,
            };
            // Note the swap: YouTube's `subscriberCountText` is the handle, and
            // `videoCountText` is the subscriber count. `subscriber_count` keeps
            // the handle only so the existing frontend field stays populated.
            let handle = text_field(&c, "subscriberCountText");
            let subscribers = text_field(&c, "videoCountText");
            Some(SearchChannelItemDto {
                channel_id,
                title,
                subscriber_count: subscribers.or(handle),
                video_count: None,
                thumbnails: grid_thumbnails(&c),
            })
        })
        .collect();

    // The same channel can appear in more than one shelf of the same response.
    let mut seen = std::collections::HashSet::new();
    out.retain(|c| seen.insert(c.channel_id.clone()));
    out
}

fn collect_channel_renderers(node: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
    match node {
        serde_json::Value::Object(map) => {
            if let Some(c) = map.get("channelRenderer") {
                out.push(c.clone());
            }
            for v in map.values() {
                collect_channel_renderers(v, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_channel_renderers(v, out);
            }
        }
        _ => {}
    }
}

// ---- Playlist results in search (raw parse) -----------------------------------

/// Extract playlists from a raw `/search` response.
///
/// `innertube_rs` handles the legacy `playlistRenderer` shape, but modern search
/// serves playlists as `lockupViewModel` (`contentType: ...PLAYLIST`), and its
/// `PlaylistNode::from_value` reads fields that do not exist on that node: the id
/// comes back empty and the title becomes "Untitled Playlist". Playlists are
/// therefore read from the raw response directly, like channels — unlike
/// channels they cannot be recovered from a second, filtered request, so the
/// unfiltered response is walked here.
pub fn search_playlists_from_raw(raw: &serde_json::Value) -> Vec<SearchItemDto> {
    playlists_with_position(raw)
        .into_iter()
        .map(|(_, item)| SearchItemDto::Playlist(item))
        .collect()
}

pub async fn search_playlists(
    yt: &InnerTube,
    query: &str,
) -> innertube_rs::Result<Vec<(usize, SearchItemDto)>> {
    let raw = search_raw(yt.raw(), query, false).await?;
    Ok(playlists_with_position(&raw)
        .into_iter()
        .map(|(pos, item)| (pos, SearchItemDto::Playlist(item)))
        .collect())
}

/// Every playlist-shaped node, in document order, paired with the number of
/// `videoRenderer` nodes that preceded it. That ordinal is what lets search
/// merge seat a playlist where YouTube put it rather than after every video.
fn playlists_with_position(
    raw: &serde_json::Value,
) -> Vec<(usize, SearchPlaylistItemDto)> {
    let mut out = Vec::new();
    let mut videos = 0usize;
    let mut seen = std::collections::HashSet::new();
    walk_playlists_with_position(raw, &mut videos, &mut seen, &mut out);
    out
}

fn walk_playlists_with_position(
    node: &serde_json::Value,
    videos: &mut usize,
    seen: &mut std::collections::HashSet<String>,
    out: &mut Vec<(usize, SearchPlaylistItemDto)>,
) {
    match node {
        serde_json::Value::Object(map) => {
            // A `videoRenderer` is one video result; count it and stop. Its
            // navigation endpoints reference other videos but never nest another
            // `videoRenderer`, so there is nothing below worth descending into.
            if map.contains_key("videoRenderer") {
                *videos += 1;
                return;
            }

            let playlist = map
                .get("lockupViewModel")
                .filter(|lvm| {
                    lvm.get("contentType")
                        .and_then(|c| c.as_str())
                        .map(|ct| ct.contains("PLAYLIST"))
                        .unwrap_or(false)
                })
                .or_else(|| map.get("playlistRenderer"))
                .or_else(|| map.get("gridPlaylistRenderer"));
            if let Some(pl) = playlist {
                if let Some(dto) = playlist_from_node(pl) {
                    if seen.insert(dto.id.clone()) {
                        out.push((*videos, dto));
                    }
                }
                return;
            }

            for v in map.values() {
                walk_playlists_with_position(v, videos, seen, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                walk_playlists_with_position(v, videos, seen, out);
            }
        }
        _ => {}
    }
}

fn playlist_from_node(node: &serde_json::Value) -> Option<SearchPlaylistItemDto> {
    if node.get("contentId").is_some() || node.get("contentType").is_some() {
        playlist_from_lockup(node)
    } else {
        playlist_from_renderer(node)
    }
}

/// Modern `lockupViewModel` playlist (live-verified shape).
fn playlist_from_lockup(lvm: &serde_json::Value) -> Option<SearchPlaylistItemDto> {
    let id = lvm.get("contentId").and_then(|v| v.as_str())?;
    let title = lvm
        .pointer("/metadata/lockupMetadataViewModel/title/content")
        .and_then(|t| t.as_str())?;
    if id.is_empty() || title.is_empty() {
        return None;
    }

    // The thumbnail ladder is this shape only (a grid item keeps it under
    // `collectionThumbnailViewModel.image`); `grid_thumbnails` covers both.
    let thumbnail = grid_thumbnails(lvm).pop().map(|t| t.url);

    // Byline row: parts[0] is the channel, parts[1] the literal "Playlist"
    // label. Take the first part that is not that label.
    let author = lvm
        .pointer(
            "/metadata/lockupMetadataViewModel/metadata/contentMetadataViewModel/metadataRows/0/metadataParts",
        )
        .and_then(|p| p.as_array())
        .and_then(|parts| {
            parts.iter().find_map(|part| {
                let text = part.pointer("/text/content").and_then(|t| t.as_str())?;
                if text.eq_ignore_ascii_case("playlist") {
                    None
                } else {
                    Some(text.to_string())
                }
            })
        });

    let video_count = lockup_video_count(lvm);

    Some(SearchPlaylistItemDto {
        id: id.to_string(),
        title: title.to_string(),
        thumbnail,
        author,
        video_count,
    })
}

/// Legacy `playlistRenderer` / `gridPlaylistRenderer`.
fn playlist_from_renderer(pl: &serde_json::Value) -> Option<SearchPlaylistItemDto> {
    let id = pl.get("playlistId").and_then(|v| v.as_str())?;
    let title = pl.get("title").and_then(text_of)?;
    if id.is_empty() || title.is_empty() {
        return None;
    }

    let thumbnail = grid_thumbnails(pl).pop().map(|t| t.url);
    let author = pl
        .get("shortBylineText")
        .or_else(|| pl.get("longBylineText"))
        .and_then(text_of);
    let video_count = pl
        .get("videoCountText")
        .or_else(|| pl.get("numVideosText"))
        .or_else(|| pl.get("videoCountShortText"))
        .and_then(text_of);

    Some(SearchPlaylistItemDto {
        id: id.to_string(),
        title: title.to_string(),
        thumbnail,
        author,
        video_count,
    })
}

/// "31 videos" lives on the thumbnail's overlay badge; fall back to any
/// metadata part that reads like a count.
fn lockup_video_count(lvm: &serde_json::Value) -> Option<String> {
    if let Some(overlays) = lvm
        .pointer("/contentImage/collectionThumbnailViewModel/primaryThumbnail/thumbnailViewModel/overlays")
        .and_then(|o| o.as_array())
    {
        for overlay in overlays {
            if let Some(badges) = overlay
                .pointer("/thumbnailOverlayBadgeViewModel/thumbnailBadges")
                .and_then(|b| b.as_array())
            {
                for badge in badges {
                    if let Some(text) = badge
                        .pointer("/thumbnailBadgeViewModel/text")
                        .and_then(text_of)
                    {
                        if text.chars().any(|c| c.is_ascii_digit()) {
                            return Some(text);
                        }
                    }
                }
            }
        }
    }

    let rows = lvm
        .pointer("/metadata/lockupMetadataViewModel/metadata/contentMetadataViewModel/metadataRows")
        .and_then(|r| r.as_array())
        .map(|r| r.to_vec())
        .unwrap_or_default();
    for row in rows {
        let Some(parts) = row.get("metadataParts").and_then(|p| p.as_array()) else {
            continue;
        };
        for part in parts {
            let Some(text) = part.pointer("/text/content").and_then(|t| t.as_str()) else {
                continue;
            };
            let lower = text.to_ascii_lowercase();
            if lower.contains("video") && text.chars().any(|c| c.is_ascii_digit()) {
                return Some(text.to_string());
            }
        }
    }
    None
}

// ---- Related videos (Watch Next) --------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubePlaylistVideoDto {
    pub video_id: String,
    pub title: String,
    pub author: String,
    /// Uploader channel id, for "Go to channel" on the playlist page.
    pub channel_id: Option<String>,
    pub thumbnail: Option<String>,
    pub duration_seconds: Option<u32>,
    /// Compact view count ("961K") or its spelled-out form, whichever the page
    /// carried.
    pub view_count: Option<String>,
    /// Upload age as text ("1 year ago"); the page can only sort this by
    /// parsing it, there is no absolute date on a playlist row.
    pub published: Option<String>,
    /// 1-based position in the playlist as returned.
    pub index: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct YoutubePlaylistDto {
    pub id: String,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub video_count: Option<u32>,
    pub thumbnail: Option<String>,
    pub videos: Vec<YoutubePlaylistVideoDto>,
}

/// A YouTube playlist's metadata and first page of videos, for the
/// `/playlist/<id>` destination that search results link to.
///
/// Read from the raw `/browse` response rather than `innertube_rs::get_playlist`:
/// that parser returns no per-video duration, views or published time for the
/// modern lockup layout, which is exactly what the page wants to show and sort
/// on.
pub async fn youtube_playlist(
    yt: &InnerTube,
    playlist_id: &str,
) -> innertube_rs::Result<YoutubePlaylistDto> {
    let raw = playlist_raw(yt.raw(), playlist_id).await?;
    Ok(playlist_detail_from_raw(&raw, playlist_id))
}

/// Turns a raw playlist `/browse` response into the page DTO.
///
/// Split out from [`youtube_playlist`] so the shape this reads can be unit
/// tested without a network call.
pub fn playlist_detail_from_raw(
    raw: &serde_json::Value,
    playlist_id: &str,
) -> YoutubePlaylistDto {
    let meta = find_first(raw, "playlistMetadataRenderer");
    let title = meta
        .and_then(|m| m.get("title"))
        .and_then(text_of)
        .or_else(|| {
            find_first(raw, "microformatDataRenderer")
                .and_then(|m| m.get("title"))
                .and_then(text_of)
        })
        .unwrap_or_default();
    let description = meta
        .and_then(|m| m.get("description"))
        .and_then(text_of)
        .filter(|d| !d.is_empty());

    let author = find_first(raw, "videoOwnerRenderer")
        .and_then(|o| o.get("title"))
        .and_then(text_of)
        .filter(|a| !a.is_empty());

    // The videos stat reads "31 videos" (as runs), the first numeric stat.
    let video_count = find_first(raw, "playlistSidebarPrimaryInfoRenderer")
        .and_then(|s| s.get("stats"))
        .and_then(|s| s.as_array())
        .and_then(|stats| {
            stats.iter().find_map(|s| {
                let text = text_of(s)?;
                text.to_ascii_lowercase()
                    .contains("video")
                    .then(|| first_number(&text))
                    .flatten()
            })
        });

    let mut videos: Vec<YoutubePlaylistVideoDto> = collect_grid_items(raw)
        .into_iter()
        .filter(|node| {
            // Keep the playlist's own rows only; related-playlist lockups also
            // carry an id and a title.
            node.get("contentType")
                .and_then(|c| c.as_str())
                .map(|ct| ct.contains("VIDEO"))
                .unwrap_or(true)
        })
        .filter_map(playlist_video_from_node)
        .collect();
    for (i, v) in videos.iter_mut().enumerate() {
        v.index = Some((i + 1) as u32);
    }

    // Headers often carry no image; the first video's thumbnail reads better
    // than an empty box.
    let thumbnail = videos.iter().find_map(|v| v.thumbnail.clone());

    YoutubePlaylistDto {
        id: playlist_id.to_string(),
        title,
        author,
        description,
        video_count,
        thumbnail,
        videos,
    }
}

fn playlist_video_from_node(node: &serde_json::Value) -> Option<YoutubePlaylistVideoDto> {
    let video_id = node
        .get("contentId")
        .or_else(|| node.get("videoId"))
        .and_then(|v| v.as_str())?
        .to_string();
    let title = node
        .pointer("/metadata/lockupMetadataViewModel/title/content")
        .and_then(|t| t.as_str())
        .map(String::from)
        .or_else(|| node.pointer("/title").and_then(text_of))
        .unwrap_or_default();
    let (author, channel_id) = lockup_channel(node);
    let (view_count, published) = grid_metadata(node);

    Some(YoutubePlaylistVideoDto {
        video_id,
        title,
        author: author.unwrap_or_default(),
        channel_id,
        thumbnail: grid_thumbnails(node).pop().map(|t| t.url),
        duration_seconds: grid_duration(node),
        view_count,
        published,
        index: None,
    })
}

/// The uploader's name and channel id from a video lockup.
///
/// The byline row links the name to the channel with a `browseEndpoint`; the
/// id is also repeated on the avatar, used as a fallback when the byline is
/// absent.
fn lockup_channel(node: &serde_json::Value) -> (Option<String>, Option<String>) {
    if let Some(rows) = node
        .pointer("/metadata/lockupMetadataViewModel/metadata/contentMetadataViewModel/metadataRows")
        .and_then(|r| r.as_array())
    {
        for row in rows {
            let Some(parts) = row.get("metadataParts").and_then(|p| p.as_array()) else {
                continue;
            };
            for part in parts {
                let Some(id) = channel_id_in_part(part) else {
                    continue;
                };
                let name = part
                    .pointer("/text/content")
                    .and_then(|c| c.as_str())
                    .map(String::from);
                return (name, Some(id));
            }
        }
    }

    let id = node
        .pointer("/metadata/lockupMetadataViewModel/image/decoratedAvatarViewModel/rendererContext/commandContext/onTap/innertubeCommand/browseEndpoint/browseId")
        .and_then(|c| c.as_str())
        .map(String::from);
    (None, id)
}

/// A metadata part is a channel link when its text run browses to a `UC…` id.
fn channel_id_in_part(part: &serde_json::Value) -> Option<String> {
    let id = part
        .pointer("/text/commandRuns/0/onTap/innertubeCommand/browseEndpoint/browseId")
        .and_then(|b| b.as_str())?;
    id.starts_with("UC").then(|| id.to_string())
}

/// First object carrying `key` anywhere in the tree (document order).
fn find_first<'a>(node: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    match node {
        serde_json::Value::Object(map) => {
            if let Some(v) = map.get(key) {
                return Some(v);
            }
            map.values().find_map(|v| find_first(v, key))
        }
        serde_json::Value::Array(arr) => arr.iter().find_map(|v| find_first(v, key)),
        _ => None,
    }
}

/// The leading integer in text like "31 videos" or "1,281,294 views".
fn first_number(text: &str) -> Option<u32> {
    let digits: String = text
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedVideoDto {
    pub video_id: String,
    pub title: String,
    pub author: String,
    /// `author_id` on the /next payload; absent for some legacy items.
    pub channel_id: Option<String>,
    pub duration_seconds: Option<u32>,
    pub view_count: Option<String>,
    pub thumbnails: Vec<ThumbnailDto>,
    pub is_live: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedVideosDto {
    pub videos: Vec<RelatedVideoDto>,
}

pub async fn related_videos(
    yt: &InnerTube,
    video_id: &str,
) -> innertube_rs::Result<RelatedVideosDto> {
    let items = yt.get_related_videos(video_id).await?;
    Ok(RelatedVideosDto {
        videos: items
            .into_iter()
            .map(|v| RelatedVideoDto {
                video_id: v.video_id,
                title: v.title,
                author: v.author,
                channel_id: v.author_id,
                duration_seconds: v.duration_seconds.map(|s| s as u32),
                view_count: v.view_count_text,
                thumbnails: v.thumbnails.iter().map(ThumbnailDto::from_yt).collect(),
                is_live: v.is_live,
            })
            .collect(),
    })
}

pub async fn suggestions(yt: &InnerTube, query: &str) -> innertube_rs::Result<Vec<String>> {
    let r = yt.get_search_suggestions(query, false).await?;
    Ok(r.suggestions)
}

// ---- Channel DTOs -----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelDto {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub thumbnails: Vec<ThumbnailDto>,
    pub subscriber_count: Option<String>,
    pub videos_count: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelVideoItem {
    pub video_id: String,
    pub title: String,
    pub thumbnails: Vec<ThumbnailDto>,
    pub duration_seconds: Option<u32>,
    pub published_text: Option<String>,
    pub view_count: Option<String>,
    pub channel_id: String,
    /// innertube-rs's per-video channel item carries no author, so this is
    /// usually `None`; callers that need a name use `ChannelDto`.
    pub author: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelVideosDto {
    pub channel_id: String,
    pub channel_name: String,
    pub videos: Vec<ChannelVideoItem>,
    pub continuation: Option<String>,
}

fn parse_duration_to_seconds_from_str(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        2 => {
            let m: u32 = parts[0].parse().ok()?;
            let sec: u32 = parts[1].parse().ok()?;
            Some(m * 60 + sec)
        }
        3 => {
            let h: u32 = parts[0].parse().ok()?;
            let m: u32 = parts[1].parse().ok()?;
            let sec: u32 = parts[2].parse().ok()?;
            Some(h * 3600 + m * 60 + sec)
        }
        _ => None,
    }
}

/// One avatar/banner URL becomes a single-entry thumbnail list, which is all the
/// upstream `ChannelAbout`/`ChannelVideoItem` models give us (a plain URL, not a
/// size ladder).
fn one_thumb(url: Option<String>) -> Vec<ThumbnailDto> {
    url.into_iter()
        .map(|u| ThumbnailDto {
            url: u,
            width: None,
            height: None,
        })
        .collect()
}

/// Reads the text out of a metadata leaf, which YouTube writes as either
/// `{"content": "..."}` or the older `{"simpleText": "..."}`.
fn leaf_text(node: &serde_json::Value) -> Option<String> {
    if let Some(s) = node
        .get("content")
        .or_else(|| node.get("simpleText"))
        .and_then(|t| t.as_str())
    {
        return Some(s.to_string());
    }
    // Titles are also delivered pre-split across `runs` — a channel name can be
    // one run ("Jamie Gilgen ") plus another ("Furniture"). Taking only the first
    // would truncate the name, and taking none at all drops the result.
    if let Some(runs) = node.get("runs").and_then(|r| r.as_array()) {
        let joined = runs
            .iter()
            .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
            .collect::<String>();
        if !joined.is_empty() {
            return Some(joined);
        }
    }
    None
}

/// Same, but for fields that are sometimes a bare string and sometimes a text
/// object — duration badges arrive as `"text": "12:34"`, titles as an object.
fn text_of(node: &serde_json::Value) -> Option<String> {
    match node {
        serde_json::Value::String(s) => Some(s.clone()),
        other => leaf_text(other),
    }
}

/// [`text_of`] for a named field that may be missing.
fn text_field(node: &serde_json::Value, key: &str) -> Option<String> {
    node.get(key).and_then(text_of)
}

// ─── Channel Videos grid, parsed from raw JSON ───────────────────────────────
//
// innertube-rs cannot be used here. Its lockup parser reads the view count and
// publish time out of `metadataRows[1]` (`parser/nodes/video.rs`), assuming the
// grid lays out one row for the channel and a second for views + age. A live
// Videos page returns a *single* row carrying both parts, so that lookup returns
// nothing: `view_count` and `published_time` are `None` for every item, and the
// single row is read as the author instead. A subscriptions feed built on those
// fields has no upload dates at all.
//
// So the grid is walked here instead. The walk is deliberately shape-agnostic —
// it looks for anything that identifies a video and reads every metadata part
// present, rather than hard-coding an index that YouTube is free to move.

/// Collects every video-shaped node on the page, in document order.
///
/// Two renderers carry channel uploads: the current `lockupViewModel` (keyed by
/// `contentId`) and the older `richItemRenderer`/`videoRenderer` (keyed by
/// `videoId`). Both are handled so the feed survives a layout change in either
/// direction. Nodes already visited are skipped, because `lockupViewModel`
/// repeats its own `contentId` deep inside navigation endpoints.
fn collect_grid_items(raw: &serde_json::Value) -> Vec<&serde_json::Value> {
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    walk_grid(raw, &mut out, &mut seen);
    out
}

fn walk_grid<'a>(
    v: &'a serde_json::Value,
    out: &mut Vec<&'a serde_json::Value>,
    seen: &mut std::collections::HashSet<&'a str>,
) {
    let serde_json::Value::Object(map) = v else {
        if let serde_json::Value::Array(a) = v {
            for item in a {
                walk_grid(item, out, seen);
            }
        }
        return;
    };

    // A grid item needs an id plus a title to be worth keeping; that excludes
    // bare `videoId` references inside menus and navigation endpoints.
    let id = map
        .get("contentId")
        .or_else(|| map.get("videoId"))
        .and_then(|v| v.as_str());
    if let Some(id) = id {
        let has_title = map
            .get("metadata")
            .and_then(|m| m.pointer("/lockupMetadataViewModel/title/content"))
            .is_some()
            || map.get("title").is_some();
        if has_title && !seen.contains(id) {
            seen.insert(id);
            out.push(v);
            // The item's own metadata is all we need; do not descend into the
            // menu's playlist-edit endpoints and collect phantom duplicates.
            return;
        }
    }

    for val in map.values() {
        walk_grid(val, out, seen);
    }
}

/// Pulls views and upload age out of whatever metadata rows the page provides.
///
/// Every `metadataPart` is inspected and classified by content rather than by
/// position, because the number of rows and parts per row varies by client and
/// has changed more than once. The `accessibilityLabel` is preferred over the
/// compact `content` string because it is spelled out ("2 days ago" rather than
/// "2d ago") and does not shift units by locale.
fn grid_metadata(node: &serde_json::Value) -> (Option<String>, Option<String>) {
    let rows = node
        .pointer("/metadata/lockupMetadataViewModel/metadata/contentMetadataViewModel/metadataRows")
        .and_then(|r| r.as_array())
        .map(|rows| rows.to_vec())
        .unwrap_or_default();

    let mut views = None;
    let mut published = None;
    for row in &rows {
        let Some(parts) = row.get("metadataParts").and_then(|p| p.as_array()) else {
            continue;
        };
        for part in parts {
            let label = part
                .get("accessibilityLabel")
                .and_then(|l| l.as_str())
                .map(|s| s.to_string());
            let text = part
                .get("text")
                .and_then(text_of)
                .or_else(|| label.clone());
            let Some(text) = text else { continue };

            let is_age = label
                .as_deref()
                .map(|l| looks_like_age(l))
                .unwrap_or_else(|| looks_like_age(&text));
            if is_age {
                // First age wins: later parts are "Premiered"/"Streamed" repeats.
                if published.is_none() {
                    published = Some(label.unwrap_or(text));
                }
            } else if views.is_none() && looks_like_views(&text) {
                views = Some(text);
            }
        }
    }

    // The legacy renderer keeps both fields outside the metadata rows.
    if published.is_none() {
        published = node
            .pointer("/publishedTimeText")
            .and_then(text_of)
            .or_else(|| {
                node.pointer("/publishedTimeText/simpleText")
                    .and_then(|s| s.as_str())
                    .map(String::from)
            });
    }
    if views.is_none() {
        views = node
            .pointer("/viewCountText")
            .and_then(text_of)
            .or_else(|| node.pointer("/shortViewCountText").and_then(text_of));
    }

    (views, published)
}

/// "2 days ago", "Streamed 3 weeks ago", "Yesterday" — an age, not a count.
fn looks_like_age(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    lower.contains(" ago")
        || lower == "yesterday"
        || lower == "now"
        || lower == "just now"
        || lower == "streamed"
        || lower == "premiered"
}

/// "1.1M views", "9,876 views", "No views" — a view count, not an age.
fn looks_like_views(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    if lower.contains("view") || lower.contains("watching") || lower.contains("playing") {
        return true;
    }
    // Compact grid counts carry no unit word at all — "1.1M", "12K", "9,876" —
    // so a bare numeric token is a count too.
    let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    !compact.is_empty()
        && compact.chars().all(|c| {
            // Scale letters are matched lowercase because `compact` was lowered.
            c.is_ascii_digit() || matches!(c, ',' | '.' | 'k' | 'm' | 'b' | '·')
        })
}

/// The duration badge on a grid thumbnail ("12:34", "1:02:03", "LIVE"), or the
/// legacy renderer's `lengthText`.
fn grid_duration(node: &serde_json::Value) -> Option<u32> {
    let text = node
        .pointer(
            "/contentImage/thumbnailViewModel/overlays/0/thumbnailBottomOverlayViewModel/badges/0/thumbnailBadgeViewModel/text",
        )
        .or_else(|| {
            node.pointer(
                "/contentImage/collectionThumbnailViewModel/overlays/0/thumbnailBottomOverlayViewModel/badges/0/thumbnailBadgeViewModel/text",
            )
        })
        .and_then(text_of)
        .or_else(|| node.pointer("/lengthText").and_then(text_of))?;
    parse_duration_to_seconds_from_str(&text)
}

/// The thumbnail ladder off a grid item, which innertube-rs flattens to one URL.
fn grid_thumbnails(node: &serde_json::Value) -> Vec<ThumbnailDto> {
    let sources = node
        .pointer("/contentImage/thumbnailViewModel/image/sources")
        .or_else(|| {
            node.pointer("/contentImage/collectionThumbnailViewModel/image/sources")
        })
        .or_else(|| {
            node.pointer(
                "/contentImage/collectionThumbnailViewModel/primaryThumbnail/thumbnailViewModel/image/sources",
            )
        })
        .or_else(|| node.pointer("/thumbnail/thumbnails"))
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();

    let mut out: Vec<ThumbnailDto> = sources
        .iter()
        .filter_map(|s| {
            Some(ThumbnailDto {
                url: s.get("url")?.as_str()?.to_string(),
                width: s.get("width").and_then(|w| w.as_u64()).map(|w| w as u32),
                height: s.get("height").and_then(|h| h.as_u64()).map(|h| h as u32),
            })
        })
        .collect();
    if out.is_empty() {
        if let Some(url) = node
            .pointer("/thumbnail/thumbnails/0/url")
            .and_then(|s| s.as_str())
        {
            out.push(ThumbnailDto {
                url: url.to_string(),
                width: None,
                height: None,
            });
        }
    }
    out
}

/// Finds the token for the next page of the grid.
fn find_continuation(raw: &serde_json::Value) -> Option<String> {
    fn walk(v: &serde_json::Value, out: &mut Option<String>) {
        if out.is_some() {
            return;
        }
        match v {
            serde_json::Value::Object(m) => {
                if let Some(token) = m
                    .get("continuationCommand")
                    .and_then(|c| c.get("token"))
                    .and_then(|t| t.as_str())
                {
                    *out = Some(token.to_string());
                    return;
                }
                for val in m.values() {
                    walk(val, out);
                }
            }
            serde_json::Value::Array(a) => {
                for item in a {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }
    let mut out = None;
    walk(raw, &mut out);
    out
}

/// Builds a [`ChannelVideosDto`] from a raw `/browse` response.
pub fn channel_videos_from_raw(
    raw: &serde_json::Value,
    channel_id: &str,
) -> ChannelVideosDto {
    let videos = collect_grid_items(raw)
        .into_iter()
        .filter_map(|node| {
            let video_id = node
                .get("contentId")
                .or_else(|| node.get("videoId"))
                .and_then(|v| v.as_str())?
                .to_string();
            let title = node
                .pointer("/metadata/lockupMetadataViewModel/title/content")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
                .or_else(|| node.pointer("/title/content").and_then(text_of))
                .or_else(|| node.pointer("/title").and_then(text_of))
                .unwrap_or_else(|| "Untitled".to_string());
            let (view_count, published_text) = grid_metadata(node);
            Some(ChannelVideoItem {
                video_id,
                title,
                thumbnails: grid_thumbnails(node),
                duration_seconds: grid_duration(node),
                published_text,
                view_count,
                channel_id: channel_id.to_string(),
                author: None,
            })
        })
        .collect();

    ChannelVideosDto {
        channel_id: channel_id.to_string(),
        channel_name: String::new(),
        videos,
        continuation: find_continuation(raw),
    }
}

/// Uses `get_channel_about`, not `get_channel`: the latter returns
/// `ChannelArtistView`, which is innertube-rs's *music artist* model (genres,
/// top_tracks, spotify_url) and carries none of the channel profile fields.
/// Takes the *upstream* client, not the [`crate::InnerTube`] wrapper: the
/// wrapper's channel methods already return these DTOs, so calling through it
/// here would be circular.
pub async fn get_channel(
    yt: &innertube_rs::Innertube,
    channel_id: &str,
) -> innertube_rs::Result<ChannelDto> {
    let channel = yt.get_channel_about(channel_id).await?;
    // Prefer the id the *response* resolved to. `channel_id` is whatever the
    // caller passed, which may be a `@handle` or a legacy `c/`/`user/` path;
    // building links from that would produce channels that cannot be reopened.
    let id = if channel.channel_id.is_empty() {
        channel_id.to_string()
    } else {
        channel.channel_id.clone()
    };
    Ok(ChannelDto {
        id,
        name: channel.title,
        description: channel.description,
        thumbnails: one_thumb(channel.avatar),
        subscriber_count: channel.subscriber_count,
        videos_count: channel.video_count,
    })
}

pub async fn get_channel_videos_inner(
    yt: &innertube_rs::Innertube,
    channel_id: &str,
) -> innertube_rs::Result<ChannelVideosDto> {
    get_channel_videos_page(yt, channel_id, None).await
}

/// One page of a channel's Videos tab. `continuation` is the token from the
/// previous page's [`ChannelVideosDto::continuation`]; passing it walks deeper
/// into the channel's back catalogue without re-requesting page one.
///
/// This issues the `/browse` call itself rather than going through
/// `innertube_rs::get_channel_videos`, because that parser cannot see the upload
/// date on this grid — see [`channel_videos_from_raw`]. The `params` value is
/// the Videos tab selector, copied from upstream.
pub async fn get_channel_videos_page(
    yt: &innertube_rs::Innertube,
    channel_id: &str,
    continuation: Option<&str>,
) -> innertube_rs::Result<ChannelVideosDto> {
    let payload = match continuation {
        Some(token) => serde_json::json!({ "continuation": token }),
        None => serde_json::json!({
            "browseId": channel_id,
            // Videos tab, uploads sorted newest-first.
            "params": "EgZ2aWRlb3PyBgQKAjoA",
        }),
    };
    let raw = yt.call("/browse", payload).await?;
    Ok(channel_videos_from_raw(&raw, channel_id))
}

/// One SponsorBlock segment as handed to the webview.
///
/// The public API's wire shape differs from this (it carries the times in a
/// two-element `segment` array, under a `UUID` key) — parsing happens in
/// `savvy_app::sponsorblock`, which converts to this DTO. Only the fields the
/// app's skip feature needs are kept; the API also sends `actionType`,
/// `videoDuration`, `votes` and a `description`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SponsorSegmentDto {
    pub category: String,
    pub start_seconds: f64,
    pub end_seconds: f64,
    /// The API emits this key in uppercase.
    #[serde(rename = "UUID")]
    pub uuid: String,
}

