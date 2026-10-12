//! Server-side remux of the adaptive video track with the extracted audio.
//!
//! MSE was the original plan for >720p, but `MediaSource` will not play these
//! renditions in this webview: the bytes append without error and the element
//! never reports them as buffered, so it sits on "Buffering" while the whole
//! track downloads. WebM (VP9) and MP4 (H.264) both behave that way, so the
//! format is not the variable.
//!
//! What does work is ffmpeg, which already produces the audio track. So instead
//! of handing two streams to `SourceBuffer`, we let ffmpeg combine them into a
//! single ordinary MP4 and let the `<video>` element do what it is good at.
//!
//! Waiting for the whole file before playing would be unusably slow on a long
//! video, so the output is a *fragmented* MP4 (`empty_moov` +
//! `frag_keyframe`). ffmpeg publishes the header within the first second and
//! keeps appending fragments, and the proxy streams the file while it is still
//! growing, holding the response open until ffmpeg exits. The player therefore
//! starts on the first fragments and catches up live. Once ffmpeg finishes the
//! file is renamed into place and a normal, fully seekable `Content-Length`
//! response is served instead.
//!
//! The video track still has to be fetchable in full. Most renditions are
//! (verified at offsets up to 128 MiB), but some hand out a limited byte budget
//! and start answering `403` partway through; ffmpeg then fails and the UI
//! falls back to progressive 360p.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::Mutex;

use crate::audio::locate_binary;

/// Same desktop UA the audio extractor uses. googlevideo rejects ffmpeg's
/// built-in default, which sends none.
pub const FETCH_UA: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
     Chrome/131.0.0.0 Safari/537.36";

/// Upper bound on one remux. This is generous because a long HD video is
/// hundreds of MB and the read is the bottleneck, but a bounded wait is what
/// stops a wedged CDN from pinning a video in `pending` forever.
const MUX_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1800);

const FFMPEG_GLOBAL: &[&str] = &["-hide_banner", "-loglevel", "error", "-nostdin", "-y"];

/// Output options. Stream copy both ways (no re-encode, so this is I/O bound),
/// and fragment the container so the file is playable while it is still being
/// written.
const FFMPEG_OUT: &[&str] = &[
    "-map",
    "0:v:0",
    "-map",
    "1:a:0",
    "-c",
    "copy",
    "-movflags",
    "frag_keyframe+empty_moov+default_base_moof",
];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "state")]
pub enum MuxStatus {
    /// ffmpeg is running. `path` is the partial file and is already playable —
    /// the caller may serve it immediately.
    Pending { path: PathBuf, bytes: u64 },
    /// ffmpeg finished; the file is complete and seekable.
    Ready { path: PathBuf, bytes: u64 },
    /// Remux failed; the caller should fall back to progressive.
    Failed { error: String },
}

struct Entry {
    status: MuxStatus,
}

#[derive(Clone)]
pub struct MuxCache {
    entries: Arc<Mutex<HashMap<String, Entry>>>,
    dir: PathBuf,
}

impl MuxCache {
    pub fn new(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            dir,
        }
    }

    fn final_path(&self, video_id: &str) -> PathBuf {
        self.dir.join(format!("{}.mp4", sanitize(video_id)))
    }

    /// Report the current status, starting the remux if it has not run yet.
    /// Cheap to poll: the UI calls this while the video loads.
    ///
    /// `video_url` is the adaptive video-only rendition and `audio_input` the
    /// audio to pair with it. Both are loopback relay URLs (see
    /// [`Self::ensure`]); a local file path is also accepted, which is what the
    /// tests use. `video_url` should already point at the loopback relay; pass
    /// the raw googlevideo URL only from tests that bypass the proxy.
    ///
    /// Taking audio as a second URL rather than a pre-extracted file removes a
    /// whole serial phase: extracting audio meant downloading the *entire*
    /// progressive rendition and running a second ffmpeg pass before the remux
    /// could emit its first frame. Now both inputs stream in parallel, so
    /// time-to-first-frame stops scaling with the audio's size.
    ///
    /// `expected_duration` is the video's length in seconds. It is checked
    /// against the remuxed output because ffmpeg can exit `0` on a *truncated*
    /// input — the relay simply stops feeding it — and publishing a short file
    /// as `Ready` would hand the player a video that ends early with no error.
    pub async fn ensure(
        &self,
        video_id: &str,
        video_url: Option<&str>,
        audio_input: Option<&str>,
        expected_duration: Option<u32>,
    ) -> MuxStatus {
        let path = self.final_path(video_id);
        let part = part_path(&path);

        {
            let mut g = self.entries.lock().await;
            if let Some(entry) = g.get(video_id) {
                match &entry.status {
                    MuxStatus::Ready { path: p, .. } if p.exists() => {
                        return entry.status.clone();
                    }
                    // Terminal. Returning early is what stops every UI poll
                    // from respawning ffmpeg against the CDN.
                    MuxStatus::Failed { .. } => return entry.status.clone(),
                    MuxStatus::Pending { .. } => {
                        // Re-stat so the caller can show real progress.
                        let bytes = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
                        return MuxStatus::Pending { path: part, bytes };
                    }
                    MuxStatus::Ready { .. } => {}
                }
            }
            g.insert(
                video_id.to_string(),
                Entry {
                    status: MuxStatus::Pending {
                        path: part.clone(),
                        bytes: 0,
                    },
                },
            );
        }

        let Some(url) = video_url.map(str::to_string) else {
            let error = "no adaptive video rendition available for HD playback".to_string();
            self.fail(video_id, error.clone()).await;
            return MuxStatus::Failed { error };
        };

        let Some(audio) = audio_input.map(str::to_string) else {
            let error = "no audio rendition available for HD playback".to_string();
            self.fail(video_id, error.clone()).await;
            return MuxStatus::Failed { error };
        };
        // Only a local file can be stat'd. A relay URL is resolved by ffmpeg
        // itself, and a dead one surfaces as an ffmpeg error a moment later.
        if !audio.starts_with("http") && !Path::new(&audio).exists() {
            let error = "extracted audio track is missing".to_string();
            self.fail(video_id, error.clone()).await;
            return MuxStatus::Failed { error };
        }

        let entries = self.entries.clone();
        let id = video_id.to_string();
        let final_path = path.clone();
        let part = part_path(&path);

        tokio::spawn(async move {
            let started = std::time::Instant::now();
            tracing::info!(video_id = %id, "mux started");
            match mux(&url, &audio, &part, expected_duration).await {
                Ok(()) => {
                    // Publish atomically. Until this rename lands the partial
                    // file is what live connections are streaming; afterwards
                    // the `.part` is gone, which is how the proxy knows to close
                    // those responses.
                    if let Err(e) = tokio::fs::rename(&part, &final_path).await {
                        let _ = tokio::fs::remove_file(&part).await;
                        fail_in(&entries, &id, format!("cache rename: {e}")).await;
                        return;
                    }
                    let bytes = tokio::fs::metadata(&final_path)
                        .await
                        .map(|m| m.len())
                        .unwrap_or(0);
                    if bytes == 0 {
                        let _ = tokio::fs::remove_file(&final_path).await;
                        fail_in(&entries, &id, "muxed file is empty".to_string()).await;
                        return;
                    }
                    tracing::info!(
                        video_id = %id,
                        bytes,
                        secs = started.elapsed().as_secs_f64().round(),
                        mbps = ((bytes as f64 / 1_048_576.0) / started.elapsed().as_secs_f64()).round(),
                        "mux ready"
                    );
                    let mut g = entries.lock().await;
                    g.insert(
                        id,
                        Entry {
                            status: MuxStatus::Ready {
                                path: final_path,
                                bytes,
                            },
                        },
                    );
                }
                Err(e) => {
                    let _ = tokio::fs::remove_file(&part).await;
                    tracing::warn!(video_id = %id, secs = started.elapsed().as_secs_f64().round(), "mux failed: {e}");
                    fail_in(&entries, &id, e).await;
                }
            }
        });

        MuxStatus::Pending {
            path: part_path(&path),
            bytes: 0,
        }
    }

    async fn fail(&self, video_id: &str, error: String) {
        fail_in(&self.entries, video_id, error).await;
    }
}

async fn fail_in(entries: &Arc<Mutex<HashMap<String, Entry>>>, video_id: &str, error: String) {
    let mut g = entries.lock().await;
    g.insert(
        video_id.to_string(),
        Entry {
            status: MuxStatus::Failed { error },
        },
    );
}

fn part_path(final_path: &Path) -> PathBuf {
    let name = final_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mux.mp4".to_string());
    final_path.with_file_name(format!("{name}.part"))
}

fn sanitize(video_id: &str) -> String {
    video_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

async fn mux(
    url: &str,
    audio: &str,
    part: &Path,
    expected_duration: Option<u32>,
) -> Result<(), String> {
    let Some(ffmpeg) = locate_binary("ffmpeg") else {
        return Err("ffmpeg not found on PATH (set SAVVY_FFMPEG_DIR)".to_string());
    };
    let run = mux_with(&ffmpeg, url, audio, part);
    let result = match tokio::time::timeout(MUX_TIMEOUT, run).await {
        Ok(result) => result,
        Err(_) => return Err(format!("mux timed out after {}s", MUX_TIMEOUT.as_secs())),
    };
    result?;

    // ffmpeg reports success even when its input ended early, which is exactly
    // what a CDN byte cap looks like. Compare the output against the video's
    // real length so a truncated file is never published as playable.
    if let Some(expected) = expected_duration.filter(|s| *s > 0) {
        let actual = probe_video_duration(part).await?;
        // Allow a small tolerance for container rounding, but anything short of
        // ~90% means the source was cut off.
        if actual < f64::from(expected) * 0.9 {
            return Err(format!(
                "remux is incomplete: got {actual:.1}s of video, expected {expected}s — the source download was cut short"
            ));
        }
    }
    Ok(())
}

/// Read the duration of the *video* track via ffprobe.
///
/// The container duration is the longest track, and here the audio always comes
/// from a fully-downloaded progressive rendition — so a video-only download that
/// was cut short still reports the full runtime at the container level. Only the
/// video track reveals the truncation.
async fn probe_video_duration(path: &Path) -> Result<f64, String> {
    let Some(ffprobe) = locate_binary("ffprobe") else {
        return Err("ffprobe not found on PATH (set SAVVY_FFMPEG_DIR)".to_string());
    };
    let out = tokio::process::Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("run ffprobe: {e}"))?;
    if !out.status.success() {
        return Err("ffprobe could not read the muxed file".to_string());
    }
    let raw = String::from_utf8_lossy(&out.stdout).trim().to_string();
    raw.parse::<f64>()
        .map_err(|_| format!("ffprobe reported no duration for the muxed video (got {raw:?})"))
}

/// ffmpeg expects an input it can probe and seek in. It gets a loopback relay
/// URL, never the googlevideo URL: the CDN refuses both of the requests ffmpeg
/// opens a file with (a plain GET and an open-ended `Range: bytes=0-`), so a
/// direct input fails before it reads a byte. The relay rewrites every read into
/// a bounded range, which is the only form the CDN serves.
async fn mux_with(
    ffmpeg: &str,
    video_input: &str,
    audio: &str,
    part: &Path,
) -> Result<(), String> {
    // Confirm both relays are actually producing bytes before committing a
    // 30-minute ffmpeg run, so a dead signature or a stale URL fails fast and
    // readably instead of surfacing as a truncated remux.
    let client = reqwest::Client::builder()
        .user_agent(FETCH_UA)
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    for (label, input) in [("adaptive video", video_input), ("audio", audio)] {
        // The audio input is either a relay URL or, when no progressive
        // rendition is known, a locally extracted track. reqwest cannot open a
        // bare filesystem path, so only URLs are probed over HTTP.
        if !input.starts_with("http") {
            if !Path::new(input).exists() {
                return Err(format!("{label} input is missing: {input}"));
            }
            continue;
        }
        let probe = client
            .get(input)
            .header("Range", "bytes=0-1023")
            .send()
            .await
            .map_err(|e| format!("fetch {label}: {e}"))?;
        let status = probe.status();
        drop(probe);
        if !status.is_success() {
            return Err(format!("fetch {label}: HTTP {status}"));
        }
    }

    let output = tokio::process::Command::new(ffmpeg)
        .args(FFMPEG_GLOBAL)
        .arg("-i")
        .arg(video_input)
        .arg("-i")
        .arg(audio)
        .args(FFMPEG_OUT)
        .args(["-f", "mp4"])
        .arg(part)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("run ffmpeg: {e}"))?;

    if !output.status.success() {
        let stderr_text = String::from_utf8_lossy(&output.stderr);
        // A truncated download surfaces here, and the reason is usually that
        // this rendition's byte budget ran out mid-file.
        let detail = stderr_text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .rev()
            .find(|l| !l.contains("Press [q]"))
            .unwrap_or("no stderr")
            .trim()
            .to_string();
        return Err(format!("ffmpeg exited {}: {detail}", output.status));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_path_rejects_traversal() {
        let cache = MuxCache::new(std::env::temp_dir().join("savvy-mux-test"));
        let path = cache.final_path("../../evil");
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        assert!(!name.contains(".."), "{name}");
        assert!(!name.contains('/'), "{name}");
        assert!(!name.contains('\\'), "{name}");
    }

    #[test]
    fn part_sits_beside_the_final_file() {
        let final_path = PathBuf::from(r"C:\cache\abc.mp4");
        let part = part_path(&final_path);
        assert_eq!(part, PathBuf::from(r"C:\cache\abc.mp4.part"));
        // The rename the cache performs must be a same-directory move.
        assert_eq!(part.parent(), final_path.parent());
    }

    #[tokio::test]
    async fn missing_inputs_fail_terminally() {
        let dir = std::env::temp_dir().join(format!("savvy-mux-{}", std::process::id()));
        let cache = MuxCache::new(dir);
        let status = cache.ensure("abc123", None, None, None).await;
        assert!(matches!(status, MuxStatus::Failed { .. }), "{status:?}");
        // A second poll must not respawn anything.
        let again = cache.ensure("abc123", None, None, None).await;
        assert!(matches!(again, MuxStatus::Failed { .. }), "{again:?}");
    }

    /// The extracted-audio fallback passes a filesystem path where the relay
    /// path passes a URL. If the pre-flight probe fed that path to reqwest it
    /// failed with "relative URL without a base" and took down a remux that had
    /// perfectly good inputs.
    #[tokio::test]
    async fn local_audio_path_is_not_probed_over_http() {
        let dir = std::env::temp_dir().join(format!("savvy-mux-local-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("v.mp4");
        std::fs::write(&video, b"not really a video").unwrap();

        let missing_audio = dir.join("nope.m4a");
        let err = mux_with(
            "ffmpeg-does-not-exist",
            video.to_str().unwrap(),
            missing_audio.to_str().unwrap(),
            &dir.join("out.part"),
        )
        .await
        .unwrap_err();

        assert!(
            err.contains("audio input is missing"),
            "local paths must be checked on disk, not handed to reqwest: {err}"
        );
        assert!(
            !err.contains("relative URL") && !err.contains("builder error"),
            "regression: path was sent to the HTTP client: {err}"
        );
    }
}
