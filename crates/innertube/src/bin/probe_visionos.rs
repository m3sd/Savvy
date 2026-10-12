//! Probe: does the VISIONOS client dodge the 16 MiB wall?
//!
//! Background: see GOOGLEVIDEO-THROTTLE-HANDOFF.md. googlevideo permanently
//! refuses to serve any byte at or beyond offset 16,777,216 (2^24) for some
//! videos. It does not respond to waiting, fresh URLs, cpn, or ratebypass.
//!
//! Why this probe exists. The earlier "no client helps" conclusion was weaker
//! than it looked:
//!   * `probe_clients` only compared HLS availability per client.
//!   * `probe_throttle` tried the other clients at *offset 0* — which passes
//!     even on a gated URL, so it could not have detected the wall.
//! No client was ever actually tested against the wall. VISIONOS is also absent
//! from both, and it is what yt-dlp moved to when android_vr started 403ing in
//! Aug 2026.
//!
//! Two details that are easy to get wrong:
//!   1. googlevideo checks the media request's User-Agent against the client
//!      that issued the URL. The earlier "UA has no effect" result only showed
//!      that a *browser* UA does not help; it never used the issuing client's
//!      own UA. Here every media request carries the client's real UA.
//!   2. A `bytes=0-0` probe passes on gated URLs, so it cannot detect the wall.
//!      The probes below read deliberately deep.
//!
//! Usage: probe_visionos [video_id] [itag]

use innertube_rs::endpoints::player::resolve_stream_url;
use innertube_rs::{Innertube, Session};
use std::sync::Arc;

/// VISIONOS is the lead; ANDROID_VR and WEB are controls, so a positive result
/// is meaningful (it is the client differing) rather than the video happening to
/// be readable.
const CLIENTS: [&str; 3] = ["VISIONOS", "ANDROID_VR", "WEB"];

const MIB: u64 = 1024 * 1024;
const CHUNK: u64 = 4 * MIB;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let video_id = args
        .first()
        .cloned()
        .unwrap_or_else(|| "Mp5asUSZBqg".to_string());
    let want_itag: Option<u32> = args.get(1).and_then(|s| s.parse().ok());
    // Third arg: how long to keep retrying a refused range before giving up.
    // This is the difference between "is there a hard wall" and "is the budget
    // simply small enough that the remux needs to be patient".
    let patient_secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(600);

    println!("video: {video_id}");
    println!("wall = 16 MiB (2^24 = 16777216). A client that clears it serves 20 MiB.");
    println!("patience: {patient_secs}s per client\n");

    let patients: Vec<&str> = args
        .get(3)
        .map(|s| s.split(',').collect())
        .unwrap_or_else(|| CLIENTS.to_vec());
    println!("clients: {}", patients.join(", "));

    for client in patients {
        println!("=== {client} ===");

        let mut yt = Innertube::new().await?;
        let mut context = yt.session.context.clone();
        if let Err(e) = Session::adjust_context(&mut context, client) {
            println!("  adjust_context: {e}\n");
            continue;
        }
        // googlevideo wants the *client's own* UA on the media request.
        let ua = context.client.user_agent.clone();
        if let Some(s) = Arc::get_mut(&mut yt.session) {
            s.context = context;
        }

        let info = match yt.get_video_info(&video_id).await {
            Ok(i) => i,
            Err(e) => {
                println!("  player: {e}\n");
                continue;
            }
        };
        if let Some(reason) = info.playability_status.reason.as_deref() {
            println!("  playability: {reason}");
        }

        let Some(sd) = info.streaming_data.as_ref() else {
            println!("  no streamingData (gated / age-restricted)\n");
            continue;
        };

        // The audio track comes from the progressive rendition, so we need to
        // know whether VISIONOS still returns one. If it does not, adaptive video
        // and progressive audio have to be resolved by different clients.
        let prog = sd.formats.len();
        let prog_with_audio = sd.formats.iter().filter(|f| !f.is_video_only()).count();
        println!("  progressive formats: {prog} (with audio: {prog_with_audio})");
        let pick = sd
            .adaptive_formats
            .iter()
            .filter(|f| f.is_video_only())
            .filter(|f| f.mime_type.starts_with("video/mp4"))
            .filter(|f| want_itag.is_none_or(|i| f.itag == i))
            .max_by_key(|f| f.height.unwrap_or(0));

        let Some(f) = pick else {
            println!("  no matching video-only MP4\n");
            continue;
        };

        let url = match resolve_stream_url(f, &yt.player.decipherer) {
            Ok(u) => u,
            Err(e) => {
                println!("  itag {}: decipher failed: {e}\n", f.itag);
                continue;
            }
        };
        let len: u64 = f
            .content_length
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        println!(
            "  itag {}  {}p  {} bytes ({:.1} MiB)",
            f.itag,
            f.height.unwrap_or(0),
            len,
            len as f64 / MIB as f64
        );

        // Fresh client per UA: reqwest would otherwise reuse a pooled connection
        // that was established under a different UA.
        let http = reqwest::Client::builder().user_agent(&ua).build()?;

        let read = |start: u64, end: u64| {
            let http = http.clone();
            let url = url.clone();
            let ua = ua.clone();
            async move {
                match http
                    .get(&url)
                    .header("User-Agent", &ua)
                    .header("Range", &format!("bytes={start}-{end}"))
                    .send()
                    .await
                {
                    Ok(r) => (r.status().as_u16(), r.content_length().unwrap_or(0)),
                    Err(_e) => (0, 0),
                }
            }
        };

        // Deliberately deep; offset 0 cannot detect a gate.
        let probes: [(u64, u64, &str); 4] = [
            (0, 1023, "offset 0, sanity"),
            (8 * MIB, 8 * MIB + 1023, "8 MiB"),
            (16 * MIB - 1024, 16 * MIB - 1, "just below 2^24"),
            (20 * MIB, 20 * MIB + 1023, "20 MiB, past the wall"),
        ];
        let mut cleared = false;
        for (start, end, what) in probes {
            let (status, n) = read(start, end).await;
            if start >= 16 * MIB && status == 206 {
                cleared = true;
            }
            let mark = if status == 206 { "served" } else { "REFUSED" };
            println!("    {start:>10}-{end:<10} {status}  {n:>9} B  {mark:<8} ({what})");
        }

        if !cleared {
            println!("    -> this client cannot cross the wall\n");
            continue;
        }

        // Deep read worked, so walk the whole file the way the remux will.
        println!("    walking the full file in 4 MiB chunks...");
        let started = std::time::Instant::now();
        let deadline = patient_secs;
        let mut pos = 0u64;
        let mut stalls = 0u32;
        let mut backoff = 3u64;
        let mut wall_at: Option<u64> = None;
        let mut last_report = 0u64;
        while pos < len {
            if started.elapsed().as_secs() > deadline {
                println!("    giving up after {deadline}s of patience");
                break;
            }
            let end = (pos + CHUNK).min(len) - 1;
            let (status, n) = read(pos, end).await;
            if status != 206 || n == 0 {
                stalls += 1;
                wall_at.get_or_insert(pos);
                // A refilling bucket is the live hypothesis, so wait it out
                // rather than declaring a hard wall after a few tries.
                tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                backoff = (backoff * 2).min(30);
                continue;
            }
            pos += n;
            backoff = 3;
            if pos - last_report >= 32 * MIB || pos >= len {
                last_report = pos;
                println!(
                    "      {:.0} / {:.0} MiB  ({:.0}s, {stalls} stalls)",
                    pos as f64 / MIB as f64,
                    len as f64 / MIB as f64,
                    started.elapsed().as_secs_f64()
                );
            }
        }
        match wall_at {
            None => println!("    COMPLETE: {pos}/{len} bytes in {:.0}s — wall cleared for {client}\n", started.elapsed().as_secs_f64()),
            Some(w) => println!(
                "    stopped at {w} bytes ({:.1} MiB) of {len} after {:.0}s and {stalls} stalls\n",
                w as f64 / MIB as f64,
                started.elapsed().as_secs_f64()
            ),
        }
    }

    Ok(())
}
