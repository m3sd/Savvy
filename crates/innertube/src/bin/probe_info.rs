//! Probe: what does a single resolve actually hand the player?
//!
//! Usage: probe_info [video_id...]

use innertube::InnerTube;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let ids: Vec<String> = std::env::args().skip(1).collect();
    let ids = if ids.is_empty() {
        vec!["LttGyjX-QxE".to_string(), "t28bEMboL9Y".to_string()]
    } else {
        ids
    };

    let mut yt = InnerTube::new().await?;
    for id in ids {
        println!("=== {id} ===");
        match innertube::dto::video_info_full(&mut yt, &id).await {
            Ok(full) => {
                let info = &full.info;
                let d = &info.details;
                println!("  title:        {}", d.title);
                println!("  length_s:     {}", d.length_seconds);
                println!("  publish:      {:?}", d.publish_date);
                println!("  views:        {:?}", d.view_count);
                println!(
                    "  combined:     {}  video_only: {}  audio_only: {}  adaptive: {}",
                    full.combined.len(),
                    full.video_only.len(),
                    full.audio_only.is_some(),
                    info.adaptive_formats.len()
                );
                println!(
                    "  best_video:   {}  best_combined: {}  best_audio: {}  hls: {}",
                    info.best_video_url.is_some(),
                    info.best_combined_url.is_some(),
                    info.best_audio_url.is_some(),
                    info.hls_manifest_url.is_some()
                );
                println!("  chapters:     {}", full.chapters.len());
                for c in full.chapters.iter().take(8) {
                    println!("    {:>7}ms  {}", c.start_ms, c.title);
                }
                if let Some(first) = full.combined.first() {
                    println!("  progressive:  itag {} {}p", first.itag, first.height.unwrap_or(0));
                }
            }
            Err(e) => println!("  ERROR: {e}"),
        }
    }
    Ok(())
}
