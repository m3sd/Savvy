//! Server-side audio extraction for the adaptive (HD) playback path.
//!
//! YouTube's *audio-only* renditions cannot be streamed by us: every one we
//! probed serves exactly the first 1 MiB and answers `403` for any range that
//! starts beyond it — reproduced across 9/10 videos, on the IOS, ANDROID and
//! ANDROID_VR clients, with both `Range` headers and `&range=` query params,
//! fresh and reused sessions, and varied CPN/User-Agent/Origin. HLS is absent
//! for every client, so it is not an alternative either.
//!
//! The *progressive* rendition (itag 18) is unaffected: it is fully seekable,
//! and it carries a perfectly serviceable AAC track. So we demux that audio
//! out once, cache it as a real file, and hand it to the remuxer in [`crate::mux`],
//! which combines it with the adaptive video track. Because the result is an
//! ordinary file, the webview gets normal `Range` semantics and seeking keeps
//! working — no long-lived ffmpeg process and no byte-offset/time mapping to
//! guess at.
//!
//! The cost is startup latency: the progressive rendition must be fetched and
//! remuxed before the audio track can play. Extraction is therefore spawned in
//! the background and polled, and a failure just falls back to progressive
//! 360p playback.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Mutex;

/// Desktop UA used for the googlevideo fetch. The signature on a progressive
/// URL is not bound to the client that minted it, and our probes returned 200
/// across a range of user agents, so a stable desktop string is enough.
const FETCH_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/131.0.0.0 Safari/537.36";

/// Upper bound on one extraction. The progressive rendition is tens of MB, so
/// this is generous, but a bounded wait is what stops a stalled CDN or a
/// wedged ffmpeg from pinning a video in `pending` forever.
const EXTRACT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// ffmpeg options that must precede the `-i`. Output options after them are
/// positional: ffmpeg binds each option to the *next* file argument, so
/// anything describing the output has to come after the input URL.
const FFMPEG_GLOBAL: &[&str] = &["-hide_banner", "-loglevel", "error", "-nostdin", "-y"];

/// Applied to the output: audio only, stream-copied (no re-encode), with the
/// moov atom relocated to the front. The output is a real file, so the
/// faststart rewrite is legal and the player gets its index up front.
const FFMPEG_AUDIO_OUT: &[&str] = &[
    "-vn",
    "-map",
    "0:a:0",
    "-c:a",
    "copy",
    "-movflags",
    "+faststart",
];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum AudioStatus {
    /// Extraction is running.
    Pending,
    /// The cache file is complete and safe to serve.
    Ready { path: PathBuf, mime: String, bytes: u64 },
    /// Extraction failed; the caller should fall back to progressive.
    Failed { error: String },
}

struct Entry {
    status: AudioStatus,
}

#[derive(Clone)]
pub struct AudioCache {
    entries: Arc<Mutex<HashMap<String, Entry>>>,
    dir: PathBuf,
}

impl AudioCache {
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            dir,
        }
    }

    fn final_path(&self, video_id: &str) -> PathBuf {
        // Video ids are `[A-Za-z0-9_-]`, but never let a route param become a
        // path traversal when building a cache key.
        let safe: String = video_id
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        self.dir.join(format!("{safe}.m4a"))
    }

    /// Report the current status *without* starting anything.
    ///
    /// [`Self::ensure`] spawns ffmpeg as a side effect, which is wrong for a
    /// caller that only wants to know whether the track is ready yet.
    pub async fn status_for(&self, video_id: &str) -> AudioStatus {
        let path = self.final_path(video_id);
        let cached = self.entries.lock().await.get(video_id).map(|e| e.status.clone());
        match cached {
            Some(AudioStatus::Ready { path: p, mime, bytes }) if p.exists() => {
                AudioStatus::Ready { path: p, mime, bytes }
            }
            Some(status) => status,
            None if path.exists() => {
                // Left over from a previous run.
                let bytes = tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);
                AudioStatus::Ready {
                    path,
                    mime: "audio/mp4; codecs=\"mp4a.40.2\"".to_string(),
                    bytes,
                }
            }
            None => AudioStatus::Pending,
        }
    }

    /// Report the current status, kicking off extraction if it has not run yet.
    /// Cheap to call repeatedly — the UI polls it while the video loads.
    pub async fn ensure(&self, video_id: &str, upstream_url: Option<&str>) -> AudioStatus {
        let path = self.final_path(video_id);

        {
            let mut g = self.entries.lock().await;
            if let Some(entry) = g.get(video_id) {
                match &entry.status {
                    // Re-validate: the file can vanish if the cache dir is
                    // cleaned underneath us, in which case we extract again.
                    AudioStatus::Ready { path: p, .. } if p.exists() => {
                        return entry.status.clone();
                    }
                    // Terminal: returning early here matters. Falling through
                    // would let every UI poll respawn ffmpeg, hammering both the
                    // CDN and the local process table.
                    AudioStatus::Failed { .. } => return entry.status.clone(),
                    AudioStatus::Pending => return entry.status.clone(),
                    AudioStatus::Ready { .. } => {}
                }
            }
            g.insert(
                video_id.to_string(),
                Entry {
                    status: AudioStatus::Pending,
                },
            );
        }

        let Some(url) = upstream_url.map(str::to_string) else {
            // No progressive rendition means no extractable audio. Record that
            // as terminal so the UI falls back instead of polling forever.
            let error = "no progressive rendition available for audio extraction".to_string();
            fail_in(&self.entries, video_id, error.clone()).await;
            return AudioStatus::Failed { error };
        };

        let entries = self.entries.clone();
        let video_id_owned = video_id.to_string();
        let tmp = path.with_extension("m4a.part");

        tokio::spawn(async move {
            match extract(&url, &tmp).await {
                Ok(mime) => {
                    // Publish atomically so a reader never sees a partial file.
                    if let Err(e) = tokio::fs::rename(&tmp, &path).await {
                        let _ = tokio::fs::remove_file(&tmp).await;
                        fail_in(&entries, &video_id_owned, format!("cache rename: {e}")).await;
                        return;
                    }
                    let bytes = tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);
                    if bytes == 0 {
                        let _ = tokio::fs::remove_file(&path).await;
                        fail_in(&entries, &video_id_owned, "extracted file is empty".to_string()).await;
                        return;
                    }
                    let mut g = entries.lock().await;
                    tracing::info!("audio ready: {video_id_owned}");
                    g.insert(
                        video_id_owned,
                        Entry {
                            status: AudioStatus::Ready { path, mime, bytes },
                        },
                    );
                }
                Err(e) => {
                    let _ = tokio::fs::remove_file(&tmp).await;
                    tracing::warn!("audio extraction failed for {video_id_owned}: {e}");
                    fail_in(&entries, &video_id_owned, e).await;
                }
            }
        });

        AudioStatus::Pending
    }
}

async fn fail_in(entries: &Arc<Mutex<HashMap<String, Entry>>>, video_id: &str, error: String) {
    let mut g = entries.lock().await;
    g.insert(
        video_id.to_string(),
        Entry {
            status: AudioStatus::Failed { error },
        },
    );
}

/// Download the progressive rendition, pipe it through ffmpeg, and leave an
/// audio-only MP4 at `tmp`. Returns the MSE mime type for the result.
async fn extract(url: &str, tmp: &Path) -> Result<String, String> {
    let Some(ffmpeg) = locate_binary("ffmpeg") else {
        return Err("ffmpeg not found on PATH (set SAVVY_FFMPEG_DIR)".to_string());
    };
    // `kill_on_drop` on the child means a timeout really does reap ffmpeg
    // rather than leaving it running against a dead download.
    match tokio::time::timeout(EXTRACT_TIMEOUT, extract_with(&ffmpeg, url, tmp)).await {
        Ok(result) => result,
        Err(_) => Err(format!(
            "audio extraction timed out after {}s",
            EXTRACT_TIMEOUT.as_secs()
        )),
    }
}

async fn extract_with(ffmpeg: &str, url: &str, tmp: &Path) -> Result<String, String> {
    // ffmpeg opens the URL itself via `-i`, so it owns the download. Probe the
    // URL with a 1 KiB range first: a dead or expired signature then fails
    // fast with a readable reason instead of an opaque ffmpeg exit code, and we
    // never pull tens of MB we would only throw away.
    let client = reqwest::Client::builder()
        .user_agent(FETCH_UA)
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let probe = client
        .get(url)
        .header("Range", "bytes=0-1023")
        .send()
        .await
        .map_err(|e| format!("fetch progressive: {e}"))?;
    let status = probe.status();
    drop(probe);
    if !status.is_success() {
        return Err(format!("fetch progressive: HTTP {status}"));
    }

    let output = tokio::process::Command::new(ffmpeg)
        .args(FFMPEG_GLOBAL)
        // googlevideo rejects clients that send no UA, and ffmpeg's built-in
        // default trips that check.
        .args(["-user_agent", FETCH_UA])
        .arg("-i")
        .arg(url)
        .args(FFMPEG_AUDIO_OUT)
        .args(["-f", "mp4"])
        .arg(tmp)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("run ffmpeg: {e}"))?;

    if !output.status.success() {
        let _ = tokio::fs::remove_file(tmp).await;
        let stderr_text = String::from_utf8_lossy(&output.stderr);
        let detail = stderr_text
            .lines()
            .last()
            .unwrap_or("no stderr")
            .trim()
            .to_string();
        return Err(format!("ffmpeg exited {}: {detail}", output.status));
    }

    probe_mime(tmp).await
}

/// Ask ffprobe what the extracted track actually is, so the `SourceBuffer`
/// mime string matches the bytes instead of being assumed.
async fn probe_mime(path: &Path) -> Result<String, String> {
    let Some(ffprobe) = locate_binary("ffprobe") else {
        // The progressive rendition is AAC in practice; this is a safe default.
        return Ok("audio/mp4; codecs=\"mp4a.40.2\"".to_string());
    };
    let out = tokio::process::Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .await
        .map_err(|e| format!("spawn ffprobe: {e}"))?;

    let codec = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(mime_for_codec(&codec))
}

fn mime_for_codec(codec: &str) -> String {
    let codecs = match codec {
        "aac" => "mp4a.40.2",
        "opus" => "opus",
        "mp3" => "mp4a.40.5",
        "vorbis" => "mp4a.40.34",
        // Unknown: AAC is the only track itag 18 realistically carries.
        _ => "mp4a.40.2",
    };
    format!("audio/mp4; codecs=\"{codecs}\"")
}

/// Find an ffmpeg-family binary: explicit override, then next to our own
/// executable (where the bundled sidecar is installed), then `PATH`, then the
/// conventional Windows install location.
pub fn locate_binary(bin: &str) -> Option<String> {
    if let Ok(explicit) = std::env::var("SAVVY_FFMPEG_DIR") {
        let candidate = PathBuf::from(&explicit).join(exe_name(bin));
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(exe_name(bin));
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    if let Ok(paths) = std::env::var("PATH") {
        for dir in paths.split(if cfg!(windows) { ';' } else { ':' }) {
            if dir.is_empty() {
                continue;
            }
            let candidate = PathBuf::from(dir).join(exe_name(bin));
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    let fallback = PathBuf::from(r"C:\ffmpeg\bin").join(exe_name(bin));
    fallback.is_file().then(|| fallback.to_string_lossy().into_owned())
}

fn exe_name(bin: &str) -> String {
    if cfg!(windows) {
        format!("{bin}.exe")
    } else {
        bin.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_maps_known_codecs() {
        assert_eq!(
            mime_for_codec("aac"),
            "audio/mp4; codecs=\"mp4a.40.2\""
        );
        assert_eq!(mime_for_codec("opus"), "audio/mp4; codecs=\"opus\"");
        // Unknown codecs fall back rather than producing an invalid mime that
        // addSourceBuffer would reject.
        assert_eq!(
            mime_for_codec("somethingelse"),
            "audio/mp4; codecs=\"mp4a.40.2\""
        );
    }

    #[test]
    fn cache_path_rejects_traversal() {
        let cache = AudioCache::new(std::env::temp_dir().join("savvy-audio-test"));
        let path = cache.final_path("../../evil");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(!name.contains(".."), "{name}");
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains('\\'), "{name}");
    }

    #[tokio::test]
    async fn ensure_reports_pending_then_fails_without_ffmpeg_or_url() {
        let dir = std::env::temp_dir().join(format!("savvy-audio-{}", std::process::id()));
        let cache = AudioCache::new(dir);

        // No progressive rendition at all: terminal failure, not a hang.
        let status = cache.ensure("abc123", None).await;
        assert!(matches!(status, AudioStatus::Failed { .. }), "{status:?}");
        let again = cache.ensure("abc123", None).await;
        assert!(matches!(again, AudioStatus::Failed { .. }), "{again:?}");
    }
}
