# SOLVED: the 16 MiB wall was a per-client quota, not a per-video cap

**Status:** fixed and verified end-to-end. Adaptive HD renditions now download
in full for videos that previously stopped dead. Read this first so nobody
re-investigates a dead end.

---

## 1. The short version

googlevideo meters how many bytes an anonymous **InnerTube client** may pull from
an adaptive rendition. Once that budget is spent it answers `403` to any range
beyond roughly the first 16 MiB. Our client was spending its budget, so remuxes
were silently truncated to a fragment that ffmpeg reported as success.

The budget is **per client, not per video**. Resolving the adaptive rendition
through the `VISIONOS` client and presenting *its* User-Agent on every media
request removes the wall. Measured on the worst case (148.8 MiB, H.264 1080p):
**156,068,179 / 156,068,179 bytes in 4 seconds, zero stalls.**

Three separate defects had to be fixed together. Any one alone leaves the bug.

| # | Defect | Fix |
|---|---|---|
| 1 | Adaptive URLs were resolved with the default client, which drains fast | Resolve adaptive video via `ADAPTIVE_CLIENT` (`VISIONOS`) |
| 2 | The loopback relay sent **no User-Agent at all** — the CDN checks it against the issuing client | `Target::Relay` now carries the URL's UA and uses it |
| 3 | The relay abandoned the stream on the first `403` | Retry a throttled range for up to 20 s, since the quota refills |

## 2. Why the earlier diagnosis was wrong

Worth recording, because the wrong answer was well-evidenced:

- **"The wall is permanent, waiting does not help."** Wrong. `probe_retry` retried
  one offset 10 times over ~5 minutes and never got through, which looked
  conclusive. The quota is per client; the default client's quota was simply
  spent. Under `VISIONOS` the same file streams straight through.
- **"The cap is per-video."** Wrong. On one run `VISIONOS` cleared a file that
  `WEB` and `ANDROID_VR` were both refused on, same video, same itag.
- **"The UA does not matter."** Half right, and the half that was wrong caused
  the bug. A *browser* UA does not help. But googlevideo checks the media
  request's UA against **the client that issued the URL**, so a VISIONOS URL
  fetched as a browser UA is throttled exactly as before.
- **"No client was ever tested against the wall."** Also true, and the real
  reason this took so long. `probe_clients` only compared HLS availability, and
  `probe_throttle` tried the other clients at *offset 0* — which succeeds even on
  a throttled URL, so it could not have detected anything.

A `bytes=0-0` probe cannot detect this. Read deep (past 20 MiB) or you will
measure nothing.

## 3. The evidence

Same video, same itag, three clients, in one run
(`probe_visionos Mp5asUSZBqg`):

```
=== VISIONOS ===            === ANDROID_VR ===        === WEB ===
  0-1023        206 served    0-1023      206 served    0-1023      206 served
  8 MiB         206 served    8 MiB       403 REFUSED    8 MiB       206 served
  just < 2^24   206 served    2^24        403 REFUSED    just < 2^24 206 served
  20 MiB        206 served    20 MiB      403 REFUSED    20 MiB      403 REFUSED
  COMPLETE 91,081,858/91,081,858 B
```

and the H.264 1080p rendition, the one that used to fail:

```
=== VISIONOS ===  itag 137  156,068,179 bytes
  COMPLETE: 156068179/156068179 bytes in 4s — wall cleared
```

End-to-end remux of the previously-failing video:

```
video_id=Mp5asUSZBqg itag=137 video/mp4 progressive=itag 18
first bytes after 1.0013064s
ffmpeg exited 0 after 15.78s
output: 165,144,663 bytes
ffprobe: video duration=749.67s   (was 47.4s)
```

## 4. One complication: VISIONOS returns no progressive formats

Measured: `progressive formats: 0` under `VISIONOS`, `1` under `WEB` and
`ANDROID_VR`. The audio we mux in is demuxed out of the progressive rendition,
so a single-client design cannot work.

`video_info_full` therefore issues **two** player requests: the default client
for details, the progressive rendition and audio; `VISIONOS` for adaptive video
only. Each `ResolvedVariant` carries the `user_agent` of the client that produced
its URL, so the two never get crossed. There is a fallback to the default client
if `VISIONOS` is gated or returns no MP4, so the app degrades to 360p rather
than failing.

## 5. What changed in the code

- `crates/innertube/src/client.rs` — `get_video_info_as(client, video_id)`
  switches the session context, returns the response plus that client's UA, and
  restores the context afterwards. Fails loudly if the session is shared rather
  than silently using the wrong client.
- `crates/innertube/src/dto.rs` — `ADAPTIVE_CLIENT = "VISIONOS"`; two-request
  resolution; `ResolvedVariant::user_agent`; `VideoInfoFull::video_user_agent`.
  `video_info_full`/`video_info` now take `&mut InnerTube`.
- `src-tauri/src/proxy.rs` — `Target::Relay { url, user_agent }`; the relay uses
  a UA-bearing pooled client; a throttled range is retried for up to
  `RELAY_THROTTLE_BUDGET` (20 s) at `RELAY_THROTTLE_BACKOFF` (2 s) intervals.
- `src-tauri/src/state.rs` — new `Upstream { url, user_agent }`; the adaptive
  caches store it. `progressive_upstream` is unchanged.
- `src-tauri/src/commands.rs` — caches `Upstream`, passes the UA to
  `insert_relay`, and takes the innertube lock mutably.

`mux.rs` was deliberately left alone: its `FETCH_UA` is used against the
*loopback* relay, where the UA is irrelevant.

### 5a. Two follow-up bugs found while verifying playback

Both were introduced by the change above and only showed up once the remux
actually produced a full-length file.

**1. The issued UA was being thrown away.** `get_video_info_as` returns the
issuing client's UA precisely so the media request can match it, but
`video_info_full` did `.and_then(|(info, _)| Ok(info))` and then tagged the
renditions with `default_ua`. The VISIONOS URL was therefore fetched with the
default client — i.e. the fix did not actually take effect. It is now bound and
propagated into `resolve_variants` and `VideoInfoFull::video_user_agent`.

The same call also ran `resolve_variants` a second time over the default
response purely to merge the adaptive format list. Each of those runs deciphers
every format URL through QuickJS, so the merge cost a full extra decipher pass
on every resolve. The merge now reuses the list built by the first call.

**2. The UI reloaded the player every 500 ms.** `video_mux_source` mints a fresh
growing token per poll, and the poll loop adopted each one
(`if (s.url && s.url !== muxUrl)`). Every new URL triggered `videoEl.src = url`
plus `load()`, which discards the buffer, so the element reloaded ~120 times
during a 60 s remux and never reached `canplay` — it looked like an infinite
buffer.

This was masked before: the truncated remuxes were small (47 MB) and finished in
about a second, so there was barely a window to thrash. A successful 400 MB
remux exposes it immediately.

The pending URL is stable for a given `.part`, so the loop now takes the first
one and holds it. Only `ready` swaps, because ffmpeg has by then renamed the
file and the growing URL is dead. If a growing stream is ever adopted more than
once deliberately, remember that `load()` is the expensive part.

## 6. Keeping the video-track duration check

This must not be reverted, and it is unrelated to the fix — it is what turned a
silent truncation into a visible failure. Two lessons from the first incident:

- **Compare the video track, not the container.** The container reports the
  longest track, and the audio always comes from a *complete* progressive
  rendition, so a truncated video still reported the full 749.76 s.
- **ffmpeg exits `0` on a truncated input.** Without the check a 47-second
  fragment of a 750-second video was cached and served as a success.

A live sample of that bug was found in the cache and removed:
`%APPDATA%\Savvy\mux\-F4z0B7FOrc137.mp4` reported `video=60.2s` against
`format=702.0s`. *Broken Stave Repair* was capped too, not full-access as
previously recorded.

## 7. If YouTube gates VISIONOS next

The other agent's warning applies, and the design is ready for it: clients get
gated every few weeks (yt-dlp dropped `android_vr` from its defaults in Aug 2026
for exactly this reason). `ADAPTIVE_CLIENT` is a single constant, and the
fallback chain means the app keeps working at 360p rather than breaking.

The next escalation would be PO Tokens — BotGuard, a video-id-bound token in
`serviceIntegrityDimensions.poToken` and a `pot` parameter on the stream URL.
That is untested here and is a much larger piece of work; it should not be
started until VISIONOS is actually observed to be gated.

Note that the project's QuickJS engine (used for signature and `n`-deciphering)
does **not** cover this. BotGuard needs a stripped-down Deno runtime with JSDOM,
which is a different problem from signature deciphering.

## 8. Project layout and reproduction

Root: `D:\Nextcloud\[02] Projekte\[04] Coding\Savvy`

```
Cargo.toml                 workspace: members = ["crates/innertube", "src-tauri"]
crates/innertube/          InnerTube client wrapper (over innertube-rs 0.9)
src-tauri/                 Tauri app, crate/lib name `app_lib`
  src/proxy.rs             loopback stream proxy: upstream / local / growing / relay
  src/mux.rs               ffmpeg remux + video-track duration validation
  src/audio.rs             audio extracted from the progressive rendition
  src/commands.rs          video_resolve_streams, video_mux_source
  src/state.rs             AppState caches
ui/                        SvelteKit frontend
```

- `ffmpeg` / `ffprobe` at `C:\ffmpeg\bin\`, overridable with `SAVVY_FFMPEG_DIR`
- App data: `%APPDATA%\Savvy\` (`audio/`, `mux/`, `savvy.db`)
- Dev launcher: `C:\Users\Anwender\AppData\Local\Temp\opencode\run-tauri-dev.bat`
- innertube-rs 0.9 already supports VISIONOS natively — `Session::adjust_context`,
  the client constants, and the `101` client id are all in the library, so no
  hand-rolled player POST is needed. (A hand-rolled one without the session's
  `visitorData` just gets "Sign in to confirm you're not a bot".)

Probes live in `crates/innertube/src/bin/`, run from the repo root:

```bash
# The decisive test. Reads deep ranges per client, then walks the whole file.
cargo run -p innertube --bin probe_visionos -- <videoId> [itag] [patience_s] [clients]

# Are renditions decodable from their first 4 MiB alone? (index at the front?)
cargo run -p innertube --bin probe_head -- <videoId>

# Per-rendition limits; also tests whether a fresh URL resumes
cargo run -p innertube --bin probe_budget -- <videoId>

# Sequential download with retry/backoff
cargo run --release -p innertube --bin probe_retry -- <videoId> <itag>

# Whole remux end-to-end, incl. probing the partial file while ffmpeg runs
cargo run -p savvy-app --bin mux_probe -- <videoId>

# HLS availability per client
cargo run -p innertube --bin probe_clients -- <videoId>
```

`probe_visionos` args: `itag` picks a specific rendition, `patience_s` is how
long to keep retrying a refused range per client, `clients` is a comma-separated
list defaulting to `VISIONOS,ANDROID_VR,WEB`.

Videos used as fixtures:

- `dQw4w9WgXcQ` — *Rick Astley - Never Gonna Give You Up (Official Video) (4K Remaster)*, 213 s
- `Mp5asUSZBqg` — *Memes to Heal Your Soul*, 750 s (**previously capped**, now fine)
- `-F4z0B7FOrc` — *Broken Stave Repair*, 702 s (**previously capped**, now fine)

Note the leading `-` in the last id: it is a valid YouTube id but a common
source of "command not found" confusion if passed unquoted.

## 9. Behaviour that must not regress

- HD (1080p and 4K) plays, autoplays, and the quality picker remuxes and caches
  per rendition (cache key `videoId#itag`).
- Default is 1080p, not the top rendition — start latency scales with size.
- Truncated remuxes are rejected, against the **video track** duration (section 6).
- Capped or failed videos fall back to 360p cleanly, with no error left on screen.
- MSE must not come back: downloads completed but `SourceBuffer` never buffered,
  leaving the UI at "Buffering 0%". `ui/src/lib/mse-player.ts` was deleted.
