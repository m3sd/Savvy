//! Diagnostic: run the real server-side audio extraction against a live video
//! and report what came out. This is the one step that cannot be unit-tested —
//! it needs YouTube, ffmpeg and a real progressive rendition.
//!
//! Usage: audio_probe [video_id]

use app_lib::audio::{AudioCache, AudioStatus};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let video_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "-F4z0B7FOrc".to_string());

    let mut yt = innertube::InnerTube::new().await?;
    let info = innertube::dto::video_info_full(&mut yt, &video_id).await?;
    let Some(url) = info.info.best_combined_url.clone() else {
        println!("no progressive rendition for {video_id}");
        return Ok(());
    };
    println!("{video_id}: progressive url ok ({} chars)", url.len());

    let dir = std::env::temp_dir().join("savvy-audio-probe");
    let cache = AudioCache::new(dir);

    let started = std::time::Instant::now();
    loop {
        let status = cache.ensure(&video_id, Some(&url)).await;
        match &status {
            AudioStatus::Pending => {
                println!("  pending… ({:.0}s)", started.elapsed().as_secs_f64());
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            AudioStatus::Failed { error } => {
                println!("  FAILED: {error}");
                return Ok(());
            }
            AudioStatus::Ready { path, mime, bytes } => {
                println!(
                    "  READY in {:.1}s: {} bytes, mime={mime}",
                    started.elapsed().as_secs_f64(),
                    bytes
                );
                println!("  path={}", path.display());
                println!("  ffprobe says:");
                let out = tokio::process::Command::new("ffprobe")
                    .args([
                        "-v",
                        "error",
                        "-show_entries",
                        "stream=codec_name,codec_type,sample_rate,channels,duration",
                        "-of",
                        "default=noprint_wrappers=1",
                    ])
                    .arg(path)
                    .output()
                    .await?;
                println!("{}", String::from_utf8_lossy(&out.stdout));
                return Ok(());
            }
        }
    }
}
