//! Probe: what does googlevideo actually require to serve an *adaptive*
//! (video-only) rendition?
//!
//! The earlier sweep always sent a `Range` header AND appended `&cpn=`/`&range=`
//! query parameters together, so it could not tell which one was doing the
//! work. The webview path can only set the header, and it 403s — so isolate
//! the two variables here.
//!
//! Usage: probe_range [video_id]

use innertube_rs::Innertube;

const CHUNK: u64 = 1024 * 1024;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let video_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "-F4z0B7FOrc".to_string());

    let yt = Innertube::new().await?;
    let info = yt.get_video_info(&video_id).await?;
    let Some(sd) = info.streaming_data.as_ref() else {
        println!("no streaming_data");
        return Ok(());
    };
    let cpn = innertube_rs::utils::proto::generate_random_string(16);

    let Some(f) = sd
        .adaptive_formats
        .iter()
        .filter(|f| f.is_video_only())
        .max_by_key(|f| (f.height.unwrap_or(0), f.bitrate))
    else {
        println!("no video-only format");
        return Ok(());
    };

    let url =
        innertube_rs::endpoints::player::resolve_stream_url(f, &yt.player.decipherer)?;
    let total: u64 = f
        .content_length
        .as_ref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    println!(
        "video_id={video_id} itag={} {} total={total}",
        f.itag,
        f.mime_type.split(';').next().unwrap_or("")
    );
    println!("  base url params: {}\n", query_keys(&url));

    let dl = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
        .build()?;

    // The proxy builds its client with `reqwest::Client::new()`, which sends no
    // User-Agent at all. Check whether that alone is enough to trip the 403.
    let no_ua = reqwest::Client::builder().build()?;
    let ua_status = match no_ua
        .get(&url)
        .header("Range", "bytes=0-1048575")
        .send()
        .await
    {
        Ok(r) => {
            let s = r.status();
            let cr = r
                .headers()
                .get("content-range")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("-")
                .to_string();
            format!("{s} content-range={cr}")
        }
        Err(e) => format!("ERR {e}"),
    };
    println!("Range header, NO user-agent -> {ua_status}");

    // ffmpeg opens an input with an *open-ended* `bytes=0-` while it probes for
    // the format. If that is refused the remux dies before it reads a byte, so
    // establish whether it is usable on the adaptive URL.
    println!("\n--- what ffmpeg actually asks for ---");
    for (label, r) in [
        ("open-ended bytes=0-", "bytes=0-"),
        ("plain GET (no range)", "NONE"),
        ("bounded 1 KiB", "bytes=0-1023"),
    ] {
        let mut req = no_ua.get(&url);
        if r != "NONE" {
            req = req.header("Range", r);
        }
        match req.send().await {
            Ok(resp) => {
                let s = resp.status();
                let cr = resp
                    .headers()
                    .get("content-range")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-")
                    .to_string();
                println!("  {label:>24} -> {s} content-range={cr}");
            }
            Err(e) => println!("  {label:>24} -> ERR {e}"),
        }
    }

    // The app fetched `bytes=0-8388607` fine (206) then `bytes=8388608-16777215`
    // and got 403, while 1 MiB windows worked all the way through. Map the real
    // rule over (start, size) before guessing again.
    println!("\n--- (start, size) matrix ---");

    // Does the `cpn` continuity parameter reset the byte budget? If so, a
    // fresh cpn per window would unlock the whole rendition.
    println!("\n--- large starts WITH &cpn (fresh continuity each time) ---");
    for &start in &[
        16_777_216u64,
        33_554_432,
        67_108_864,
        134_217_728,
    ] {
        for (label, u) in [
            (
                "cpn only  ",
                format!("{url}&cpn={cpn}"),
            ),
            (
                "cpn+rn    ",
                format!("{url}&cpn={cpn}&rn=7"),
            ),
            (
                "no params ",
                url.clone(),
            ),
        ] {
            let r = match no_ua
                .get(&u)
                .header("Range", format!("bytes={start}-{}", start + 1_048_575))
                .send()
                .await
            {
                Ok(resp) => {
                    let s = resp.status();
                    let cr = resp
                        .headers()
                        .get("content-range")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("-")
                        .to_string();
                    format!("{s} content-range={cr}")
                }
                Err(e) => format!("ERR {e}"),
            };
            let ok = if r.starts_with("206") { "ok  " } else { "FAIL" };
            println!("  {ok} start={start:>10} {label} -> {r}");
        }
    }

    let sizes = [1024 * 1024u64, 4 * 1024 * 1024, 8 * 1024 * 1024];
    let starts = [0u64, 1024 * 1024, 8 * 1024 * 1024, 16 * 1024 * 1024, 64 * 1024 * 1024];
    for &size in &sizes {
        for &start in &starts {
            let r = match no_ua
                .get(&url)
                .header("Range", format!("bytes={start}-{}", start + size - 1))
                .send()
                .await
            {
                Ok(resp) => {
                    let s = resp.status();
                    let n = resp.bytes().await.map(|b| b.len()).unwrap_or(0);
                    format!("{s} got={n}")
                }
                Err(e) => format!("ERR {e}"),
            };
            let ok = if r.starts_with("206") { "ok  " } else { "FAIL" };
            println!("  {ok} start={start:>10} size={size:>9} end={:>10} -> {r}", start + size - 1);
        }
    }

    // Four combinations, for chunk 0 and chunk 1, so we can tell "cannot start"
    // apart from "cannot continue past the first megabyte".
    for chunk in 0..2u64 {
        let start = chunk * CHUNK;
        let end = start + CHUNK - 1;
        println!("--- chunk {chunk} (bytes {start}-{end}) ---");

        let cases: Vec<(&str, String, Option<String>)> = vec![
            ("plain GET, no range        ", url.clone(), None),
            (
                "Range header only          ",
                url.clone(),
                Some(format!("bytes={start}-{end}")),
            ),
            (
                "&range= query only         ",
                format!("{url}&range={start}-{end}"),
                None,
            ),
            (
                "&cpn + &range query only   ",
                format!("{url}&cpn={cpn}&range={start}-{end}&rn={chunk}"),
                None,
            ),
            (
                "Range header + &cpn/&range ",
                format!("{url}&cpn={cpn}&range={start}-{end}&rn={chunk}"),
                Some(format!("bytes={start}-{end}")),
            ),
            (
                "Range header + &cpn only   ",
                format!("{url}&cpn={cpn}"),
                Some(format!("bytes={start}-{end}")),
            ),
        ];

        for (label, u, range) in cases {
            let mut req = dl.get(&u);
            if let Some(r) = &range {
                req = req.header("Range", r);
            }
            match req.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let cr = resp
                        .headers()
                        .get("content-range")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("-")
                        .to_string();
                    let body = resp.bytes().await.map(|b| b.len()).unwrap_or(0);
                    println!("  {label} -> {status} bytes={body} content-range={cr}");
                }
                Err(e) => println!("  {label} -> ERR {e}"),
            }
        }
        println!();
    }
    Ok(())
}

fn query_keys(url: &str) -> String {
    let q = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    q.split('&')
        .filter_map(|kv| kv.split_once('=').map(|(k, _)| k))
        .collect::<Vec<_>>()
        .join(",")
}
