//! Probe: is the googlevideo limit a hard cap, or a token bucket that refills?
//!
//! Evidence so far:
//!   * A read that returned 403 earlier in the same session later returned 206
//!     at the same offset — so refusals are not permanent.
//!   * Renditions are decodable from their first 4 MiB, so the container index
//!     is at the front and ffmpeg can start immediately.
//!
//! That combination is the signature of a rate limiter with a refill, not a
//! fixed allowance. If it refills, then simply waiting and re-issuing the
//! refused request eventually completes the download, and the fix for the app is
//! "back off and retry the chunk" instead of "give up".
//!
//! So: download sequentially, and on a 403 sleep and retry the *same* byte
//! range with growing backoff. Report whether it finishes, and how long it took.
//!
//! Usage: probe_retry [video_id] [itag]

use innertube::dto::video_info_full;
use innertube::InnerTube;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const CHUNK: u64 = 4 * 1024 * 1024;
const BACKOFF_START: u64 = 5;
const BACKOFF_MAX: u64 = 60;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let video_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Mp5asUSZBqg".to_string());
    let want_itag: Option<u32> = std::env::args().nth(2).and_then(|s| s.parse().ok());

    let mut yt = InnerTube::new().await?;
    let full = video_info_full(&mut yt, &video_id).await?;
    let client = reqwest::Client::builder().user_agent(UA).build()?;

    let v = full
        .video_only
        .iter()
        .filter(|v| want_itag.is_none_or(|i| v.itag == i))
        .max_by_key(|v| (v.height.unwrap_or(0), v.bitrate))
        .ok_or("no video-only rendition")?;
    let total = v.content_length.unwrap_or(0);

    println!(
        "video: {video_id}  itag {}  {}p  {} bytes  ({})\n",
        v.itag,
        v.height.unwrap_or(0),
        total,
        v.mime.split(';').next().unwrap_or("?")
    );

    let started = std::time::Instant::now();
    let mut pos: u64 = 0;
    let mut backoff = BACKOFF_START;
    let mut stalls = 0u32;

    while pos < total {
        let end = (pos + CHUNK).min(total) - 1;
        let spec = format!("bytes={pos}-{end}");

        let got = match client.get(&v.url).header("Range", &spec).send().await {
            Ok(r) if r.status().is_success() => r.content_length().unwrap_or(0),
            Ok(r) => {
                let s = r.status();
                stalls += 1;
                let waited = backoff.min(BACKOFF_MAX);
                println!(
                    "  {spec:>22} -> {s}; sleeping {waited}s (stall #{stalls}, at {:.1} MiB)",
                    pos as f64 / 1048576.0
                );
                tokio::time::sleep(std::time::Duration::from_secs(waited)).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
                continue;
            }
            Err(e) => {
                stalls += 1;
                let waited = backoff.min(BACKOFF_MAX);
                println!("  {spec:>22} -> transport {e}; sleeping {waited}s");
                tokio::time::sleep(std::time::Duration::from_secs(waited)).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
                continue;
            }
        };

        if got == 0 {
            println!("  {spec:>22} -> empty body; stopping");
            break;
        }
        pos += got;
        // A successful read means the bucket is full again, so shorten the next
        // wait rather than carrying a stale penalty forward.
        backoff = BACKOFF_START;

        if pos % (16 * 1024 * 1024) < CHUNK || pos >= total {
            println!(
                "  {:>6.1} / {:.1} MiB  ({:.0}s elapsed, {stalls} stalls)",
                pos as f64 / 1048576.0,
                total as f64 / 1048576.0,
                started.elapsed().as_secs_f64()
            );
        }
    }

    println!(
        "\n{} after {:.1}s with {stalls} stalls",
        if pos >= total {
            "COMPLETE"
        } else {
            "INCOMPLETE"
        },
        started.elapsed().as_secs_f64(),
    );
    Ok(())
}
