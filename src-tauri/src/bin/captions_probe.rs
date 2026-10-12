//! Diagnostic: which InnerTube clients return caption tracks for a video, and
//! what the app's own resolve tree (`video_info_full`) exposes today.
//!
//! YouTube trims the `captionTracks` list in some player responses to just the
//! auto-generated track, so "only English (auto-generated) is shown" is decided
//! here, per client, empirically.
//!
//! Usage: captions_probe [video_id]

use serde_json::Value;

/// Same set the app rotates through, so the report says exactly who *could*
/// have fed the resolve's `default_info.captions`.
const CLIENTS: &[&str] = &[
    "WEB",
    "ANDROID",
    "ANDROID_VR",
    "IOS",
    "TV_EMBEDDED",
    "WEB_EMBEDDED",
    "VISIONOS",
    "MWEB",
];

fn caption_tracks(body: &Value) -> Vec<(String, Option<String>, String)> {
    body.pointer("/captions/playerCaptionsTracklistRenderer/captionTracks")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
        .map(|item| {
            let lang = item
                .get("languageCode")
                .and_then(|l| l.as_str())
                .unwrap_or("?")
                .to_string();
            let kind = item.get("kind").and_then(|k| k.as_str()).map(|s| s.to_string());
            let name = item
                .pointer("/name/simpleText")
                .or_else(|| item.pointer("/name/runs/0/text"))
                .and_then(|t| t.as_str())
                .unwrap_or(&lang)
                .to_string();
            (lang, kind, name)
        })
        .collect()
}

fn human((lang, kind, name): &(String, Option<String>, String)) -> String {
    let kind_s = kind.as_deref().unwrap_or("manual");
    format!("{lang:<8} {kind_s:<7} {name}")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let video_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "lARiBU25bBo".to_string());

    let mut yt = innertube::InnerTube::new().await?;
    println!("video={video_id}");

    for client in CLIENTS {
        match yt.get_video_info_as_raw(client, &video_id).await {
            Ok((body, _ua)) => {
                let tracks = caption_tracks(&body);
                if tracks.is_empty() {
                    println!("{client:<14} no captions in response");
                } else {
                    println!("{client:<14} {} track(s)", tracks.len());
                    for t in &tracks {
                        println!("{client:<14}   {}", human(t));
                    }
                }
            }
            Err(e) => {
                println!("{client:<14} ERR {}", e.to_string().lines().next().unwrap_or(""));
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    }

    println!();
    println!("app resolve tree (video_info_full):");
    // A fresh session keeps app-rotation behaviour honest (each request gets a
    // fair shot at its own client), rather than this probe's last client swap
    // from the loop above.
    let mut yt = innertube::InnerTube::new().await?;
    match innertube::dto::video_info_full(&mut yt, &video_id).await {
        Ok(full) => {
            if full.captions.is_empty() {
                println!("  0 captions tracked by the resolve");
            } else {
                for c in &full.captions {
                    println!("  lang={:<8} kind={:?}", c.lang, c.kind);
                }

                let lang = std::env::args().nth(2).unwrap_or_else(|| "en".to_string());
                if let Some(track) = innertube::dto::find_caption(&full.captions, &lang) {
                    println!();
                    println!("  fetching {lang} timedtext through the app's path…");
                    let upstream = if track.base_url.contains("fmt=") {
                        track.base_url.clone()
                    } else {
                        format!("{}&fmt=json3", track.base_url)
                    };
                    println!("  upstream starts with: {:?}", &upstream[..upstream.len().min(160)]);
                    let http = yt.raw().session.http_client.clone();
                    match http.get(&upstream).send().await {
                        Ok(resp) => {
                            let status = resp.status();
                            let bytes = resp.bytes().await.unwrap_or_default();
                            println!("  status={status} body_len={}", bytes.len());
                            if status.is_success() {
                                let body = String::from_utf8_lossy(&bytes);
                                let vtt = innertube::dto::captions_to_vtt(
                                    &video_id,
                                    &track.lang,
                                    &body,
                                );
                                match vtt {
                                    Ok(vtt) => {
                                        let cues = vtt
                                            .lines()
                                            .filter(|l| l.contains(" --> "))
                                            .count();
                                        println!("  vtt_bytes={} cues={cues}", vtt.len());
                                        for l in vtt.lines().take(5) {
                                            println!("    {l}");
                                        }
                                    }
                                    Err(e) => println!("  parse FAILED: {e}"),
                                }
                            } else {
                                println!("  body head: {:?}", &String::from_utf8_lossy(&bytes)[..bytes.len().min(200)]);
                            }
                        }
                        Err(e) => println!("  fetch FAILED: {e}"),
                    }
                }
            }
        }
        Err(e) => println!("  resolve FAILED: {}", e.to_string().lines().next().unwrap_or("")),
    }

    Ok(())
}