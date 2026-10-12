//! Smoke test: dump formats, resolve best video + best audio URLs.
//! Run with `cargo run --bin smoke -- <video_id>`.

use innertube_rs::{FormatFilter, FormatType, Innertube, QualityPreference};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let video_id = std::env::args().nth(1).unwrap_or_else(|| "jNQXAC9IVRw".into());

    println!("→ Initialising Innertube session…");
    let yt = Innertube::new().await?;

    println!("→ Fetching player response for {video_id}");
    let info = yt.get_video_info(&video_id).await?;
    println!("  playability: {}", info.playability_status.status);
    if let Some(d) = info.video_details.as_ref() {
        println!("  title:   {}", d.title);
        println!("  length:  {}s", d.length_seconds);
    }
    let Some(sd) = info.streaming_data.as_ref() else {
        println!("✗ no streaming_data");
        return Ok(());
    };

    let audio = FormatFilter {
        format_type: FormatType::AudioOnly,
        quality: QualityPreference::Highest,
        container: None,
    };
    let video = FormatFilter {
        format_type: FormatType::VideoOnly,
        quality: QualityPreference::Highest,
        container: None,
    };

    println!();
    println!("→ Resolving audio-only…");
    match yt.get_stream_url(&video_id, &audio).await {
        Ok(u) => println!("✓ audio URL ({} chars)\n  {u}", u.len()),
        Err(e) => println!("✗ {e}"),
    }
    println!();
    println!("→ Resolving video-only…");
    match yt.get_stream_url(&video_id, &video).await {
        Ok(u) => println!("✓ video URL ({} chars)\n  {u}", u.len()),
        Err(e) => println!("✗ {e}"),
    }

    // Also dump the format inventory.
    println!();
    println!(
        "Formats (progressive {} / adaptive {}):",
        sd.formats.len(),
        sd.adaptive_formats.len()
    );
    for f in sd.adaptive_formats.iter().chain(sd.formats.iter()) {
        let w = f.width.unwrap_or(0);
        let h = f.height.unwrap_or(0);
        let fps = f.fps.unwrap_or(0);
        println!(
            "  itag={:>3} {:<26} {:>4}x{:<5} {:>2}fps  bitrate={:>7}  qlabel={:<10} aq={:<6} av={} vo={} ao={}",
            f.itag,
            f.mime_type,
            w,
            h,
            fps,
            f.bitrate,
            f.quality_label.as_deref().unwrap_or("-"),
            f.audio_quality.as_deref().unwrap_or("-"),
            f.is_audio_video(),
            f.is_video_only(),
            f.is_audio_only(),
        );
    }

    println!();
    println!("Progressive (sd.formats) classified as audio+video:");
    let mut n = 0;
    for f in sd.formats.iter() {
        if f.is_audio_video() {
            n += 1;
            println!(
                "  itag={} {}p qlabel={:?}",
                f.itag,
                f.height.unwrap_or(0),
                f.quality_label
            );
        }
    }
    if n == 0 {
        println!("  (none — every progressive format was classified video-only)");
    }

    println!();
    match sd.hls_manifest_url.as_ref() {
        Some(u) => println!("HLS manifest: present ({} chars)\n  {u}", u.len()),
        None => println!("HLS manifest: ABSENT"),
    }

    Ok(())
}
