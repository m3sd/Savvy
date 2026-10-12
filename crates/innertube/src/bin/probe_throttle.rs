//! Probe: is the googlevideo limit a *size* limit or an *exhausted budget*,
//! and do other InnerTube clients hand back URLs that are not throttled?
//!
//! The previous probe (`probe_budget`) showed a single cumulative budget per
//! video: ~24 MB shared across every MP4 rendition, not reset by a fresh URL.
//! That leaves two very different situations:
//!
//!   * Budget *exhausted*  -> even a 1 KiB read at offset 0 fails, and no
//!                            amount of cleverness with this URL helps.
//!   * Size *thresholded*  -> small reads still succeed, so the tail (the `moov`
//!                            atom ffmpeg needs) is reachable in pieces.
//!
//! These are distinguishable with a 1 KiB read, so measure that first. Then try
//! other clients, since throttling is attached to the URL the player hands out
//! and different clients sign differently.
//!
//! Usage: probe_throttle [video_id]

use innertube_rs::endpoints::player::resolve_stream_url;
use innertube_rs::utils::proto::generate_random_string;
use innertube_rs::{Innertube, Session};
use std::sync::Arc;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// One bounded read. Returns (status, bytes_returned, content_range).
async fn try_read(client: &reqwest::Client, url: &str, spec: &str) -> String {
    match client.get(url).header("Range", spec).send().await {
        Ok(r) => {
            let s = r.status();
            let cr = r
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
                .to_string();
            let n = r.content_length().unwrap_or(0);
            format!("{s} bytes={n} content-range={cr}")
        }
        Err(e) => format!("transport error: {e}"),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let video_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Mp5asUSZBqg".to_string());
    let client = reqwest::Client::builder().user_agent(UA).build()?;

    println!("video: {video_id}\n");

    // --- 1. Is the budget exhausted, or is it just a size threshold? ---------
    let yt = Innertube::new().await?;
    let info = yt.get_video_info(&video_id).await?;
    let sd = info
        .streaming_data
        .as_ref()
        .ok_or("no streaming_data")?;
    let cpn = generate_random_string(16);

    let Some(f) = sd
        .adaptive_formats
        .iter()
        .filter(|f| f.is_video_only())
        .max_by_key(|f| (f.height.unwrap_or(0), f.bitrate))
    else {
        println!("no video-only format");
        return Ok(());
    };
    let mut url = resolve_stream_url(f, &yt.player.decipherer)?;
    let total: u64 = f
        .content_length
        .as_ref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    println!(
        "top rendition itag {} ({}p, {total} bytes)\n",
        f.itag,
        f.height.unwrap_or(0)
    );

    println!("1. size threshold vs exhausted budget");
    for spec in [
        "bytes=0-1023",
        "bytes=0-65535",
        "bytes=0-1048575",
        "bytes=0-4194303",
    ] {
        println!("   {spec:>18} -> {}", try_read(&client, &url, spec).await);
    }
    // The `moov` atom lives at the end; if any read up there works, ffmpeg could
    // be pointed at a file we assembled ourselves.
    let tail = total.saturating_sub(65536);
    println!(
        "   {:>18} -> {}   (tail: the moov index ffmpeg needs)",
        format!("bytes={tail}-{}-1", tail),
        try_read(&client, &url, &format!("bytes={tail}-{}", total - 1)).await
    );

    // --- 2. Do the query knobs that exist for exactly this change anything? --
    println!("\n2. query parameters");
    url.push_str(&format!("&cpn={cpn}"));
    println!("   &cpn=            -> {}", try_read(&client, &url, "bytes=0-1023").await);
    if let Some(stripped) = url.split("&ratebypass=").next().map(|s| s.to_string()) {
        let mut rb = stripped;
        rb.push_str("&ratebypass=yes");
        println!("   &ratebypass=yes  -> {}", try_read(&client, &rb, "bytes=0-1023").await);
    }

    // --- 3. Is a different client given a URL that is not throttled? ---------
    println!("\n3. other InnerTube clients (1 KiB read at offset 0)");
    for client_name in ["IOS", "ANDROID_VR", "TV_EMBEDDED", "MWEB", "WEB_EMBEDDED"] {
        let Ok(mut yt2) = Innertube::new().await else {
            println!("   {client_name:<14} session failed");
            continue;
        };
        let mut context = yt2.session.context.clone();
        if let Err(e) = Session::adjust_context(&mut context, client_name) {
            println!("   {client_name:<14} adjust: {e}");
            continue;
        }
        if let Some(s) = Arc::get_mut(&mut yt2.session) {
            s.context = context;
        }
        let info2 = match yt2.get_video_info(&video_id).await {
            Ok(i) => i,
            Err(e) => {
                println!("   {client_name:<14} info: {e}");
                continue;
            }
        };
        let Some(sd2) = info2.streaming_data.as_ref() else {
            println!("   {client_name:<14} no streaming_data");
            continue;
        };
        let Some(f2) = sd2
            .adaptive_formats
            .iter()
            .filter(|f| f.is_video_only())
            .max_by_key(|f| (f.height.unwrap_or(0), f.bitrate))
        else {
            println!("   {client_name:<14} no video-only");
            continue;
        };
        let u2 = match resolve_stream_url(f2, &yt2.player.decipherer) {
            Ok(u) => u,
            Err(e) => {
                println!("   {client_name:<14} resolve: {e}");
                continue;
            }
        };
        println!(
            "   {client_name:<14} itag {:<4} -> {}",
            f2.itag,
            try_read(&client, &u2, "bytes=0-1023").await
        );
    }

    Ok(())
}
