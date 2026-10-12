//! Probe: does switching the InnerTube client change HLS availability?
//! Usage: probe_clients <video_id> [video_id...]

use innertube_rs::{Innertube, Session};
use std::sync::Arc;

const CLIENTS: [&str; 6] = ["WEB", "IOS", "TV_EMBEDDED", "WEB_EMBEDDED", "MWEB", "ANDROID_VR"];

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ids: Vec<String> = std::env::args().skip(1).collect();
    let ids = if ids.is_empty() {
        vec!["-F4z0B7FOrc".to_string()]
    } else {
        ids
    };

    for id in &ids {
        println!("\n=== {id} ===");
        for client in CLIENTS {
            let Ok(mut yt) = Innertube::new().await else {
                println!("  {client:<14} session failed");
                continue;
            };
            let mut context = yt.session.context.clone();
            if let Err(e) = Session::adjust_context(&mut context, client) {
                println!("  {client:<14} adjust_context: {e}");
                continue;
            }
            Arc::get_mut(&mut yt.session).map(|s| s.context = context);
            match yt.get_video_info(id).await {
                Ok(info) => {
                    let sd = info.streaming_data.as_ref();
                    let hls = sd
                        .and_then(|s| s.hls_manifest_url.as_ref())
                        .map(|u| format!("{} chars", u.len()))
                        .unwrap_or_else(|| "ABSENT".into());
                    let prog = sd.map(|s| s.formats.len()).unwrap_or(0);
                    let adap = sd.map(|s| s.adaptive_formats.len()).unwrap_or(0);
                    let max_h = sd
                        .map(|s| {
                            s.adaptive_formats
                                .iter()
                                .filter(|f| f.is_video_only())
                                .filter_map(|f| f.height)
                                .max()
                                .unwrap_or(0)
                        })
                        .unwrap_or(0);
                    println!(
                        "  {client:<14} hls={hls:<12} prog={prog} adaptive={adap} max_video={max_h}p"
                    );
                }
                Err(e) => println!("  {client:<14} ERROR {e}"),
            }
        }
    }
    Ok(())
}
