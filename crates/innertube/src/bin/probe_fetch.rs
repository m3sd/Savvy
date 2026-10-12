//! Probe: is the 1 MiB audio-only cap specific to one video, or account/IP-wide?
//!
//! On -F4z0B7FOrc every client (IOS/ANDROID/ANDROID_VR) serves audio itag 140
//! chunk 0 (bytes 0-1048575) with HTTP 200 but 403s chunk 1, while video-only
//! streams fine in 1 MiB chunks. If other videos behave the same way the cap is
//! session/IP-level — i.e. missing PO token — rather than a quirk of one video.
//!
//! Usage: probe_fetch [video_id...]

use innertube_rs::constants::clients;
use innertube_rs::core::session::SessionOptions;
use innertube_rs::Innertube;

const CHUNK: u64 = 1024 * 1024;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ids: Vec<String> = std::env::args().skip(1).collect();
    let ids = if ids.is_empty() {
        vec![
            "-F4z0B7FOrc".to_string(),
            "dQw4w9WgXcQ".to_string(),
            "9bZkp7q19f0".to_string(),
            "jNQXAC9IVRw".to_string(),
        ]
    } else {
        ids
    };

    let yt = Innertube::with_options(SessionOptions {
        client_name: Some(clients::ANDROID_NAME.to_string()),
        client_version: Some(clients::ANDROID_VERSION.to_string()),
        user_agent: Some(clients::ANDROID_USER_AGENT.to_string()),
        device_category: Some("MOBILE".to_string()),
        generate_session_locally: Some(true),
        ..Default::default()
    })
    .await?;

    let dl = reqwest::Client::builder()
        .user_agent(clients::ANDROID_USER_AGENT)
        .build()?;

    for id in &ids {
        let info = match yt.get_video_info(id).await {
            Ok(i) => i,
            Err(e) => {
                println!("{id}  player ERROR {e}");
                continue;
            }
        };
        let Some(sd) = info.streaming_data.as_ref() else {
            println!("{id}  no streaming_data");
            continue;
        };
        let title = info
            .video_details
            .as_ref()
            .map(|d| d.title.clone())
            .unwrap_or_default();
        let cpn = innertube_rs::utils::proto::generate_random_string(16);

        // Best audio-only and best video-only, each probed at chunk 0 and 1.
        let audio = sd
            .adaptive_formats
            .iter()
            .filter(|f| f.is_audio_only())
            .max_by_key(|f| f.bitrate);
        let video = sd
            .adaptive_formats
            .iter()
            .filter(|f| f.is_video_only())
            .max_by_key(|f| (f.height.unwrap_or(0), f.bitrate));

        let a = match audio {
            Some(f) => probe_chunks(&dl, &yt, f, &cpn).await,
            None => "none".to_string(),
        };
        let v = match video {
            Some(f) => probe_chunks(&dl, &yt, f, &cpn).await,
            None => "none".to_string(),
        };

        println!("{id}  audio[{a}]  video[{v}]  \"{title}\"");
    }

    Ok(())
}

/// Returns e.g. "itag140 c0=200 c1=403" for the first two 1 MiB chunks.
async fn probe_chunks(
    dl: &reqwest::Client,
    yt: &Innertube,
    f: &innertube_rs::StreamingFormat,
    cpn: &str,
) -> String {
    let url = match innertube_rs::endpoints::player::resolve_stream_url(f, &yt.player.decipherer) {
        Ok(u) => u,
        Err(e) => return format!("itag{} resolve-err:{e}", f.itag),
    };
    let total: u64 = f
        .content_length
        .as_ref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    let mut out = format!("itag{}", f.itag);
    for i in 0..2u64 {
        let start = i * CHUNK;
        if total > 0 && start >= total {
            break;
        }
        let end = if total > 0 {
            std::cmp::min(start + CHUNK - 1, total - 1)
        } else {
            start + CHUNK - 1
        };
        let chunk_url = format!("{url}&cpn={cpn}&range={start}-{end}&rn={i}");
        let status = match dl
            .get(&chunk_url)
            .header("Origin", "https://www.youtube.com")
            .header("Referer", "https://www.youtube.com/")
            .send()
            .await
        {
            Ok(r) => r.status().as_u16(),
            Err(_) => 0,
        };
        out.push_str(&format!(" c{i}={status}"));
        if status != 200 && status != 206 {
            break;
        }
    }
    out
}
