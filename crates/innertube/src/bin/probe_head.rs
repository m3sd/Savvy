//! Probe: can ffmpeg work from just the *head* of a rendition?
//!
//! Measurement so far (`probe_budget`, `probe_throttle`):
//!
//!   * Reads near the start succeed, repeatedly, even after earlier reads were
//!     refused — so there is no permanent per-URL budget.
//!   * Reads far into the file are refused: the tail is `403`.
//!   * A fresh URL, `&cpn=`, `&ratebypass=yes` and other InnerTube clients all
//!     behave identically.
//!
//! That tail refusal is the problem, because a plain MP4 keeps its index (`moov`)
//! at the *end*. ffmpeg cannot demux a file whose index it cannot read, which is
//! why it produced a 47-second "success" for a 750-second video.
//!
//! The fix would be to use a container that puts its index at the *front*, since
//! the front is exactly the part we can always read. YouTube's WebM/VP9
//! renditions may well be written that way.
//!
//! So: fetch only the first few MiB of every video-only rendition, hand each to
//! ffprobe on its own, and report which ones are decodable from the head alone.
//! Those are the ones ffmpeg can start muxing immediately.
//!
//! Usage: probe_head [video_id]

use innertube::dto::video_info_full;
use innertube::InnerTube;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const HEAD: u64 = 4 * 1024 * 1024;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let video_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Mp5asUSZBqg".to_string());
    let mut yt = InnerTube::new().await?;
    let full = video_info_full(&mut yt, &video_id).await?;

    let client = reqwest::Client::builder().user_agent(UA).build()?;
    let dir = std::env::temp_dir().join("savvy-head-probe");
    std::fs::create_dir_all(&dir)?;

    println!("video: {video_id} — decoding each rendition from its first {HEAD} bytes only\n");
    println!(
        "{:<6} {:<7} {:<10} {:<13} {:<22} {}",
        "itag", "height", "container", "head read", "decodable from head?", "streams"
    );

    for v in &full.video_only {
        let container = v.mime.split(';').next().unwrap_or("?");
        let head_path = dir.join(format!("{}.head", v.itag));

        let fetched = match client
            .get(&v.url)
            .header("Range", &format!("bytes=0-{}", HEAD - 1))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => match r.bytes().await {
                Ok(b) => {
                    std::fs::write(&head_path, &b)?;
                    format!("{} bytes", b.len())
                }
                Err(e) => format!("body error: {e}"),
            },
            Ok(r) => format!("HTTP {}", r.status()),
            Err(e) => format!("transport: {e}"),
        };

        let probe = tokio::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_name,width,height",
                "-of",
                "csv=p=0",
            ])
            .arg(&head_path)
            .output()
            .await;

        let (verdict, streams) = match probe {
            Ok(p) if p.status.success() => {
                let s = String::from_utf8_lossy(&p.stdout)
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                ("YES".to_string(), s)
            }
            Ok(p) => {
                let e = String::from_utf8_lossy(&p.stderr);
                let first = e.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                ("no".to_string(), first.chars().take(40).collect())
            }
            Err(e) => ("?".to_string(), e.to_string()),
        };

        println!(
            "{:<6} {:<7} {:<10} {:<13} {:<22} {}",
            v.itag,
            v.height.map(|h| h.to_string()).unwrap_or("-".into()),
            container,
            fetched,
            verdict,
            streams
        );
        let _ = std::fs::remove_file(&head_path);
    }

    Ok(())
}
