//! End-to-end check of the HD remux, without the webview in the way.
//!
//! Measures the thing the app's responsiveness rests on: how long until the
//! remuxed file is *playable with both tracks*. It runs the two designs against
//! the same video and reports the difference.
//!
//!   serial   (what the app used to do): download and fully extract the audio
//!            track first, and only then start the remux. Nothing is playable
//!            until the whole audio file exists.
//!   parallel (what the app does now): both the adaptive video and the
//!            progressive rendition are fed to ffmpeg as loopback relay URLs, so
//!            the two download concurrently and the remux starts immediately.
//!
//! Both go through the real proxy relay rather than the CDN directly, because
//! the CDN refuses the plain GET and open-ended `Range: bytes=0-` requests that
//! ffmpeg opens a file with; only bounded ranges are served.
//!
//! Run with a short video id: `cargo run -p savvy-app --bin mux_probe -- dQw4w9WgXcQ`

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use innertube::InnerTube;

fn ffmpeg() -> String {
    if let Ok(dir) = std::env::var("SAVVY_FFMPEG_DIR") {
        let p = PathBuf::from(dir).join("ffmpeg.exe");
        if p.is_file() {
            return p.to_string_lossy().into_owned();
        }
    }
    "ffmpeg".to_string()
}

fn ffprobe() -> String {
    if let Ok(dir) = std::env::var("SAVVY_FFMPEG_DIR") {
        let p = PathBuf::from(dir).join("ffprobe.exe");
        if p.is_file() {
            return p.to_string_lossy().into_owned();
        }
    }
    "ffprobe".to_string()
}

/// Mirrors `mux::FFMPEG_GLOBAL` / `FFMPEG_OUT`.
const GLOBAL: &[&str] = &["-hide_banner", "-loglevel", "error", "-nostdin", "-y"];
const OUT: &[&str] = &[
    "-map",
    "0:v:0",
    "-map",
    "1:a:0",
    "-c",
    "copy",
    "-movflags",
    "frag_keyframe+empty_moov+default_base_moof",
    "-f",
    "mp4",
];

/// ffprobe csv mode prints one row per stream and omits the type word
/// (`av1,1920,1080` then `aac`), so track kinds are inferred by position.
fn describe(path: &Path) -> Option<(bool, bool, String)> {
    let out = std::process::Command::new(ffprobe())
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name,width,height",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let rows: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if rows.is_empty() {
        return None;
    }
    let audio: Vec<&&str> = rows
        .iter()
        .filter(|r| {
            r.starts_with("aac")
                || r.starts_with("mp4a")
                || r.starts_with("opus")
                || r.starts_with("vorbis")
        })
        .collect();
    let has_audio = !audio.is_empty();
    let has_video = rows.len() > audio.len();
    Some((has_video, has_audio, rows.join(" ")))
}

/// Runs ffmpeg over two inputs and reports when the output first became
/// decodable with both a video and an audio track.
async fn run_and_measure(
    label: &str,
    video_input: &str,
    audio_input: &str,
    out: &Path,
    settle: Duration,
) -> Option<Duration> {
    let _ = std::fs::remove_file(out);
    let start = Instant::now();
    let mut child = tokio::process::Command::new(ffmpeg())
        .args(GLOBAL)
        .arg("-i")
        .arg(video_input)
        .arg("-i")
        .arg(audio_input)
        .args(OUT)
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .ok()?;

    let mut first_bytes = None;
    let mut both_tracks = None;
    let mut next_probe = Duration::ZERO;
    loop {
        let len = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
        if len > 0 && first_bytes.is_none() {
            first_bytes = Some(start.elapsed());
        }
        if len > 0 && start.elapsed() >= next_probe {
            next_probe = start.elapsed() + Duration::from_millis(400);
            if let Some((v, a, rows)) = describe(out) {
                if v && a && both_tracks.is_none() {
                    both_tracks = Some(start.elapsed());
                    println!("  {label}: both tracks at {:?} ({len} bytes) [{rows}]", both_tracks.unwrap());
                }
            }
        }
        if both_tracks.is_some() && start.elapsed() > settle {
            let _ = child.kill().await;
            break;
        }
        if start.elapsed() > Duration::from_secs(180) {
            let _ = child.kill().await;
            break;
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                println!("  {label}: ffmpeg finished {status} at {:?}", start.elapsed());
                break;
            }
            Ok(None) => tokio::time::sleep(Duration::from_millis(200)).await,
            Err(_) => break,
        }
    }

    println!(
        "  {label}: first bytes {:?}, both tracks {:?}",
        first_bytes.unwrap_or(Duration::MAX),
        both_tracks
    );
    both_tracks
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let video_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "dQw4w9WgXcQ".to_string());

    let dir = std::env::temp_dir().join(format!("savvy-mux-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;

    let mut yt = InnerTube::new().await?;
    // What one uncached resolution costs: two `player` round-trips plus the
    // QuickJS decipher. This is exactly what a quality switch used to pay again
    // on every click, and what the resolve cache now avoids.
    let t_resolve = Instant::now();
    let full = innertube::dto::video_info_full(&mut yt, &video_id).await?;
    let resolve_cost = t_resolve.elapsed();
    println!("resolve: {resolve_cost:.2?} (2 player requests + decipher)");

    // Match the app: the top MP4 rendition, since the remux only ever consumes
    // MP4 from the relay.
    let video = full
        .video_only
        .iter()
        .find(|v| v.mime.starts_with("video/mp4"))
        .or_else(|| full.video_only.first())
        .ok_or("no video-only rendition")?;
    let combined = full
        .combined
        .first()
        .ok_or("no progressive rendition for audio")?;
    println!(
        "video_id={video_id} video=itag {} {}  audio=itag {}",
        video.itag,
        video.mime.split(';').next().unwrap_or(""),
        combined.itag
    );

    let proxy = app_lib::proxy::spawn().await?;
    let video_relay = {
        let t = proxy
            .table
            .lock()
            .await
            .insert_relay(video.url.clone(), video.user_agent.clone());
        format!("http://{}/stream/{}", proxy.addr, t)
    };
    let audio_relay = {
        let t = proxy
            .table
            .lock()
            .await
            .insert_relay(combined.url.clone(), combined.user_agent.clone());
        format!("http://{}/stream/{}", proxy.addr, t)
    };
    println!("relays: {video_relay}\n         {audio_relay}");

    let ffmpeg = ffmpeg();

    // ---- serial: extract the whole audio track first, then remux ----------
    let audio_file = dir.join("audio.m4a");
    let _ = std::fs::remove_file(&audio_file);
    let t0 = Instant::now();
    let extract = tokio::process::Command::new(&ffmpeg)
        .args(GLOBAL)
        .arg("-i")
        .arg(&audio_relay)
        .args(["-vn", "-map", "0:a:0", "-c:a", "copy"])
        .args(["-f", "mp4"])
        .arg(&audio_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .await?;
    println!(
        "\nserial: audio extraction {} after {:?} ({} bytes)",
        if extract.status.success() { "ok" } else { "FAILED" },
        t0.elapsed(),
        std::fs::metadata(&audio_file).map(|m| m.len()).unwrap_or(0)
    );
    if !extract.status.success() {
        return Err("audio extraction failed".into());
    }
    // The extraction is part of the serial design's time-to-playback: nothing
    // is playable until the whole audio file exists, because the remux cannot
    // start before then. Fold it in so both designs are measured from the same
    // origin, "the user pressed play".
    let extract_elapsed = t0.elapsed();
    let serial_remux = run_and_measure(
        "serial  ",
        &video_relay,
        audio_file.to_str().unwrap(),
        &dir.join("serial.mp4.part"),
        Duration::from_secs(3),
    )
    .await;

    // ---- parallel: both inputs relayed, ffmpeg starts at once --------------
    println!();
    let parallel = run_and_measure(        "parallel",
        &video_relay,
        &audio_relay,
        &dir.join("parallel.mp4.part"),
        Duration::from_secs(3),
    )
    .await;

    println!("\n=== time until playable with both tracks (from 'play pressed') ===");
    let serial_total = serial_remux.map(|r| r + extract_elapsed);
    match (serial_total, parallel) {
        (Some(s), Some(p)) => {
            println!("serial   {s:.2?} total = {extract_elapsed:.2?} extraction + {:.2?} remux", s - extract_elapsed);
            println!("parallel {p:.2?} total = no pre-work");
            if p < s {
                println!(
                    "parallel is {:.2}x faster to first playback ({:.3}s saved)",
                    s.as_secs_f64() / p.as_secs_f64().max(f64::MIN_POSITIVE),
                    (s - p).as_secs_f64()
                );
            } else {
                println!("NO IMPROVEMENT: parallel was not faster");
            }
        }
        (None, None) => println!("FAIL: neither design produced a playable file"),
        (Some(_), None) => println!("FAIL: parallel design never became playable"),
        (None, Some(_)) => println!("serial design never became playable, but parallel did"),
    }
    Ok(())
}
