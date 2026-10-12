//! Diagnostic: ask the same video through several InnerTube clients and report
//! which ones come back playable. A bot check is a per-client, per-IP judgement,
//! so the answer is empirical and can change with IP reputation — this measures it
//! instead of guessing.
//!
//! Usage: client_probe [video_id]

/// Ordered by how likely a client is to survive an anonymous bot check. The
/// WEB clients are the most heavily gated: they want a PO Token, which Savvy
/// cannot mint without a browser. The mobile/TV/VR clients are far less
/// aggressively attested, which is why LibreTube and NewPipe can return `null`
/// for every non-web PO token and still work.
const CLIENTS: &[&str] = &[
    "ANDROID_VR",
    "IOS",
    "ANDROID",
    "TV_EMBEDDED",
    "TV_SIMPLY",
    "TV",
    "WEB_EMBEDDED",
    "VISIONOS",
    "MWEB",
    "WEB",
];

/// Types come from `innertube-rs`, which is not a direct dependency of this
/// binary, so the verdict is reached without naming them: `get_video_info_as`
/// returns the concrete response type and inference pins it down.
macro_rules! verdict {
    ($r:expr, $video_id:expr) => {
        match &$r {
            Err(e) => {
                let s = e.to_string();
                let low = s.to_ascii_lowercase();
                if low.contains("not a bot") {
                    format!("BOT-CHECK ({})", first_line(&s))
                } else if low.contains("login_required") {
                    format!("LOGIN_REQUIRED ({})", first_line(&s))
                } else {
                    format!("ERR ({})", first_line(&s))
                }
            }
            Ok((p, _ua)) => {
                let ps = &p.playability_status;
                let status = ps.status.clone();
                let reason = ps.reason.clone().unwrap_or_default();
                let (prog, adap, has_url, id_match, aonly, vonly) = match &p.streaming_data {
                    Some(sd) => {
                        let all = sd.formats.iter().chain(sd.adaptive_formats.iter());
                        // Audio-only and video-only adaptive tracks are what let a
                        // progressive-less client still serve audio without running
                        // an extractor over a muxed stream.
                        let count_prefix = |needle: &str| {
                            sd.adaptive_formats
                                .iter()
                                .filter(|f| f.mime_type.starts_with(needle))
                                .count()
                        };
                        (
                            sd.formats.len(),
                            sd.adaptive_formats.len(),
                            all.clone().any(|f| f.url.is_some()),
                            p.video_details
                                .as_ref()
                                .map(|v| v.video_id == $video_id)
                                .unwrap_or(false),
                            count_prefix("audio"),
                            count_prefix("video"),
                        )
                    }
                    None => (0, 0, false, false, 0, 0),
                };
                // Progressive renditions matter separately from adaptive ones:
                // the audio extractor muxes from a progressive stream, so a client
                // that returns adaptive formats only is not a usable primary.
                format!(
                    "status={status} prog={prog} adaptive={adap} audio_only={aonly} \
                     video_only={vonly} urls={} video_id_match={id_match} {reason}",
                    if has_url { "URL_PRESENT" } else { "NO_URL" }
                )
            }
        }
    };
}

fn first_line(s: &str) -> String {
    let l = s.lines().next().unwrap_or("").trim().to_string();
    if l.len() > 110 {
        format!("{}…", &l[..110])
    } else {
        l
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let video_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "dQw4w9WgXcQ".to_string());

    // Optional 2nd arg: a data dir. When given, this exercises the same
    // visitorData reuse path the app uses on launch, so two runs must report the
    // same identity and leave the file untouched.
    let dir = std::env::args().nth(2).map(std::path::PathBuf::from);
    let before = dir
        .as_ref()
        .map(|d| std::fs::read_to_string(d.join("visitor_data.txt")).ok());

    let mut yt = match &dir {
        Some(d) => innertube::InnerTube::new_with_dir(Some(d)).await?,
        None => innertube::InnerTube::new().await?,
    };

    let now = yt.raw().session.context.client.visitor_data.clone();
    println!("video={video_id}");
    println!(
        "visitorData: {:?}",
        now.as_ref().map(|s| &s[..s.len().min(24)]).unwrap_or(&String::new())
    );
    if let Some(b) = &before {
        println!(
            "  previous_was_loaded={} identity_matches={}",
            b.is_some(),
            b.as_deref() == now.as_deref()
        );
    }
    println!();

    // The WEB client first, as a control: it is the one the app currently uses,
    // so if WEB works today the absence of a PO Token is not the bottleneck.
    for client in CLIENTS {
        let r = yt.get_video_info_as(client, &video_id).await;
        println!("{client:<32} {}", verdict!(r, video_id));
        // Politeness: these are real requests against a real quota.
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
    }

    Ok(())
}