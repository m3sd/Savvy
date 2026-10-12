//! Probe: why does googlevideo stop serving an adaptive rendition partway?
//!
//! Two candidate explanations, and two candidate workarounds, are tested here
//! rather than assumed:
//!
//!   A. A hard per-*video* ceiling. Then no amount of retrying helps and the
//!      only fix is a different (smaller) rendition.
//!   B. A per-*URL* budget. Then re-running the player request for a fresh URL
//!      should hand back a full budget, and a download can be resumed by
//!      re-issuing player requests whenever it stalls.
//!
//! So for each MP4 rendition we download in bounded chunks until the first 403,
//! then immediately re-run the player request and try to carry on from exactly
//! where we stopped. If the resumed read succeeds, the cap is per-URL and
//! retryable.
//!
//! Usage: probe_budget [video_id]

use innertube::dto::video_info_full;
use innertube::InnerTube;

const CHUNK: u64 = 4 * 1024 * 1024;
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Download from `start` in bounded chunks, stopping at the first non-success.
/// Returns the offset actually reached.
async fn pull(
    client: &reqwest::Client,
    url: &str,
    start: u64,
    total: u64,
    budget: u64,
) -> (u64, bool) {
    let mut pos = start;
    let stop = total.min(start + budget);
    while pos < stop {
        let end = stop.min(pos + CHUNK) - 1;
        let spec = format!("bytes={pos}-{end}");
        let resp = match client.get(url).header("Range", &spec).send().await {
            Ok(r) => r,
            Err(e) => {
                println!("      {spec} -> transport error {e}");
                return (pos, false);
            }
        };
        let status = resp.status();
        if !status.is_success() {
            println!("      {spec} -> HTTP {status}  <-- stopped here");
            return (pos, false);
        }
        let n = resp.content_length().unwrap_or(0);
        println!("      {spec} -> {status} ({n} bytes)");
        if n == 0 {
            return (pos, false);
        }
        pos += n;
    }
    (pos, pos >= total)
}

/// Fetch a fresh set of resolved URLs for this video.
async fn fresh(
    video_id: &str,
) -> Result<Vec<(u32, String, u64, Option<u32>)>, Box<dyn std::error::Error>> {
    let mut yt = InnerTube::new().await?;
    let full = video_info_full(&mut yt, video_id).await?;
    Ok(full
        .video_only
        .iter()
        .filter(|v| v.mime.starts_with("video/mp4"))
        .map(|v| (v.itag, v.url.clone(), v.content_length.unwrap_or(0), v.height))
        .collect())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let video_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Mp5asUSZBqg".to_string());
    println!("video: {video_id}\n");

    let client = reqwest::Client::builder()
        .user_agent(UA)
        .build()?;

    let renditions = fresh(&video_id).await?;
    println!("MP4 video-only renditions: {}\n", renditions.len());

    for (itag, url, total, height) in &renditions {
        println!("--- itag {itag} ({height:?}p, {total} bytes) ---");

        // Attempt 1: one URL, no retry.
        let (reached, complete) = pull(&client, url, 0, *total, 64 * 1024 * 1024).await;
        if complete {
            println!("    => whole file readable on one URL\n");
            continue;
        }
        println!("    => single URL stopped at {reached} / {total}");

        // Attempt 2: does a *fresh* URL give a fresh budget? Re-run the player
        // request and resume from exactly where we stopped.
        let all = fresh(&video_id).await?;
        let Some((_, new_url, new_total, _)) = all.iter().find(|(i, ..)| i == itag) else {
            println!("    => rendition vanished on retry\n");
            continue;
        };
        println!(
            "    retrying on a fresh URL (total {} vs {} — same file? {})",
            new_total,
            total,
            *new_total == *total
        );
        let (reached2, complete2) = pull(&client, new_url, reached, *total, 64 * 1024 * 1024).await;
        if complete2 {
            println!("    => RESUME SUCCEEDED: the cap is per-URL, retrying defeats it\n");
        } else {
            println!(
                "    => resume also stopped ({reached2} / {total}); looks like a per-video ceiling\n"
            );
        }
    }

    Ok(())
}
