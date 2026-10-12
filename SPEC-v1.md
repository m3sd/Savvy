# Savvy — v1 Spec Sheet

Living doc. Add/edit items as v1 evolves. Each feature lists what exists, what's
missing, the intended UX/API, and acceptance criteria.

Global constraints that apply to everything:

- **InnerTube quota is scarce.** Anonymous IPs get throttled after a burst of
  `/player` requests. New per-video endpoints (captions, chapters, related
  videos) must be fetched once per video, cached in the backend resolve cache,
  and never polled. Reuse `fetch_video_info`'s single-flight + resolve-cache
  pattern. Prefer the already-fetched `player` response as the source for new
  fields before issuing new requests.
- **Streaming is proxied.** Media URLs are reminted by `proxy.rs`; the webview
  never talks to googlevideo directly. Timed text must follow the same path so
  audio/video/caption requests share one UA and one token namespace.
- **One Rust→webview DTO shape.** `db.rs` + `crates/innertube/src/dto.rs` are
  canonical. New DTOs live there; `commands.rs` must not invent parallel
  schemas.
- **Auth is anonymous, and that is a real constraint.** No signed-in YouTube
  session today. Anything requiring an account (playlist writes, watch later,
  private/subscribed-only content) is blocked on that — see items 10 and 15.
  Until then those features are local-only or unbuildable.

---

## 1. NewPipe subscription import

**Goal:** bring existing subscriptions in without retyping them.

**Existing:** `subscriptions` table (`db.rs`), `subscription_add` upsert,
`subscription_list`, `subscription_exists`, `subscriptions/+page.svelte`
placeholder.

**Format (NewPipe JSON export):**

```json
{
  "app_version": "0.23.3",
  "app_version_int": 1044,
  "subscriptions": [
    { "url": "https://www.youtube.com/channel/UCxxxx", "name": "Channel", "avatar_url": "..." }
  ],
  "groups": [
    { "name": "Music", "subscriptions": ["https://www.youtube.com/channel/UCxxxx"] }
  ]
}
```

`url` may also be `.../c/...`, `.../user/...`, or `.../@handle` — normalise all to
the channel id. `groups` feed feature 5 (categories) when present.

**API (new):**

- `subscription_import NewPipeImport` → `ImportResult { added, existing, skipped }`
  - `NewPipeImport { subscriptions_json: String }` (raw file content; parse + validate server-side).
  - Reuses `subscription_add` per channel; `avatar_url` maps to `thumbnail`.
  - If `groups` present and vue feature 5, create categories and join them.

**UX:** Settings/subs page → "Import from NewPipe…" → file picker (tauri dialog)
or paste JSON; result summary with matched/added counts.

**Acceptance:**
- Import a real NewPipe export with ≥3 channels and groups; all appear in the
  subs list; ids match YouTube; re-import is idempotent (`existing` counts the
  overlap).

---

## 2. Progress persistence / continue watching

**Status: done** (verified by the user). The watch page reports the playhead via
`history_progress` (throttled, plus pause/seek/unload), `stored_progress`
collapses the first/last 5s to "not started"/"finished", and playback resumes
from `progress_seconds` after the stream starts. The home feed surfaces the
resume row.

**Goal:** the app remembers where each video left off, across sessions, and
offers to resume.

**Existing:** `history` table already has `progress_seconds REAL`,
`duration_seconds`, `watched_at` (`db.rs:74`). The watch page writes a history
row on open — but hardcodes `progressSeconds: 0` (`+page.svelte:184`) and never
updates it afterwards.

**Missing — the actual "remember" loop:**

1. **Report progress (new command):** `history_progress { video_id, position_seconds, duration_seconds, playing }` —
   throttled client-side (every ~5s while playing, plus on pause, seek, unload,
   and before route change). Backend: `UPDATE history SET progress_seconds=?1,
   watched_at=now_unix() WHERE video_id=?2`. Do not store when within 5s of head
   or tail (treat as finished/not-started).
2. **Resume on open:** watch page fetches the history row for `[id]` before
   playback and seeks to `progress_seconds` automatically. Needs the seek to
   happen *after* the stream is actually playing, not on `loadedmetadata`
   (current buffering design streams while ffmpeg writes — seek once playback
   starts).
3. **Continue-watching UI:** history page (or a dedicated row/section) lists
   rows where `0 < progress_seconds < duration_seconds`, sorted by
   `watched_at DESC`, with "Resume at m:ss" and a "mark finished"/remove action.
   `history_list` already supports a limit; extend to flag `resumable`.

**Acceptance:**
- Play 30s of a video, quit the app, reopen, navigate → playback resumes within
  ~1s of where it stopped (after resolve).
- Finishing a video clears its resumable entry; history page reflects both.

---

## 3. Subtitles

**Status: built** (item completed — Option A shipped).

**Goal:** show captions on videos that have them.

**Existing plumbing:** innertube-rs 0.9 exposes `get_caption_tracks(video_id)`,
`get_transcript(video_id, lang)`, `extract_caption_tracks_from_player` and SRT
export (`transcript.rs`). The raw `player` response `captions` field exists.

**Design decision (decided):** backend-proxied WebVTT, rendered by a custom
overlay.

- **Option A (tried, abandoned):** native `<track kind="captions">`. WebView2's
  text-track pipeline refuses the cross-origin VTT — the element fires `error`
  even though the identical loopback URL returns `200 text/vtt` with correct
  CORS (verified by direct fetch). No HTTP-side fix helped; `default`, a
  persistent element, and `load()` reparse all failed.
- **Option B (shipped):** the same backend VTT (`/captions/{video_id}/{lang}.vtt`,
  warmed once per (video, language) in the proxy) is `fetch`ed as text in the
  webview and parsed into `{ start, end, text }` cues; the cue covering
  `currentTime` is drawn in a `pointer-events-none` overlay inside the player
  frame. Full style control (font-family setting, shadow, position) and it
  survives fullscreen. The backend half is identical to Option A, so the timed
  text still never leaves the proxy.

**Backend (built):**

- No new resolve: the typed player response's `captions` field is extracted in
  `assemble_video_info` (one `/player` per video, single-flight, same eviction
  rules). `VideoInfoDto.captions` carries only `{ lang, label, is_asr }`; the
  timedtext `base_url` stays Rust-side (`VideoInfoFull.captions`).
- `captions_list video_id` → `[{ lang, label, is_asr }]` (from the resolve
  cache; no extra request).
- `captions_fetch video_id lang` → `{ url, lang, label }`: finds the track
  (`find_caption`: exact lang match, then case-insensitive label scan), fetches
  timedtext once per (video, language) with `&fmt=json3`, parses via
  `captions_to_vtt` (innertube-rs `parse_transcript_response` + `to_vtt`),
  rejects empty tracks, stores the VTT in `state.captions`, serves it at
  `http://{proxy}/captions/{video_id}/{lang}.vtt` (strict-id validation,
  `text/vtt; charset=utf-8`, CORS, `Cache-Control: no-cache`). Re-fetch for a
  warm language returns the cached URL without touching the network.

**UX (built):** a "Captions" select on the watch page lists tracks (including
auto-generated, marked by label); `Off` clears the cues; selecting a language
warms the VTT, fetches it as text, parses it and shows cues in sync (driven by
`timeupdate`/`seeked`). Caption errors surface under the player. The caption
font is user-configurable: the `captions.font` setting (free-form CSS
`font-family`, with presets and a live preview on the Settings page) is applied
to the overlay via `style:font-family`. **Deferred:** a settings-level
caption-on-by-default toggle (read-as-needed per video).

**Acceptance (met):**
- A video with subs (incl. auto-generated) shows the language list; selecting
  one displays timed captions in sync with playback; works in muxed HD and
  360p; two videos one after another never re-fetch captions for a warm id.

---

## 4. Chapters

**Status: built.** The `video_chapters` command serves marker chapters from the
resolve entry (zero extra requests, cached with the video), and the watch page
renders the chapter list, the current-chapter highlight, and the timeline ticks
driven off the same playback position as progress.

**Goal:** chapter markers with navigation (click a chapter to seek) and a
scrollable list/scrubber.

**Existing:** nothing modeled. innertube-rs parses `ChapterNode` in some
responses; need to locate/verify where chapters surface (player response
`chapters`, description timestamps as fallback).

> **2026 reality (measured with `probe_chapters`):** anonymous `/player`
> responses — any InnerTube client — no longer carry the marker bar
> (`playerOverlays`/`multiMarkersPlayerBarRenderer` is absent), so marker
> chapters are effectively unavailable on the native path. The description
> timestamps (read from the *full* `microformat` description, returned by a raw
> `/player` read that the typed parse drops) are therefore the effective source;
> the exact markers-map extraction is kept for when a response does carry them
> (e.g. the browser-routed player bridge).

**Backend:**

- Extend `video_info_full` (or a companion `video_chapters video_id` using the
  same player data) to yield `[{ title, start_ms, thumb_url? }]`.
- Sorted, deduplicated; `start_ms` strictly increasing. Fallback: parse
  `mm:ss Title` / `mm:ss – Title` lines from the description.
- Cache in the resolve entry so zero extra requests on repeat visits. Chapters
  for a warm id must come from cache.

**UX:**

- Chapters row under the player or a side list: click → seek.
- Current chapter highlighted while watching (updates from the same playback
  position used for progress).
- "Scrollable": the list scrolls the current chapter into view on change; the
  player timeline optionally shows tick marks at chapter starts.

**Acceptance:**
- A video with chapters shows them; clicking one seeks to the right moment;
  the active chapter tracks playback (including during resume-from-progress);
  a video without chapters shows the fallback UI (none/text).

---

## 5. Subscription categories

**Status: done** (verified by the user). The `categories`/`sub_categories`
tables, category CRUD commands, filtered `subscription_list`, the
group→category import mapping, and the category rail on the subscriptions page
(chips with counts, add/rename/delete, per-channel checkbox assignment) are all
in. The spec text below records the agreed shape.

**Goal:** organise subs into named categories; a sub can belong to several; the
subs page filters by category.

**Existing:** `subscriptions` table only. No categories.

**Data model (new tables in `db.rs::migrate`):**

```sql
CREATE TABLE categories (
    id        TEXT PRIMARY KEY,       -- ulid/slug
    name      TEXT NOT NULL UNIQUE,
    position  INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE sub_categories (
    channel_id  TEXT NOT NULL REFERENCES subscriptions(channel_id),
    category_id TEXT NOT NULL REFERENCES categories(id),
    PRIMARY KEY (channel_id, category_id)
);
```

**API:**

- `category_list` → `[{ id, name }]`
- `category_add { name }`, `category_remove { id }` (also drops join rows),
  `category_rename { id, name }`
- `subscription_set_categories { channel_id, category_ids: [] }` (replace-all,
  matches the "multi-category" model)
- `subscription_list` gains an optional `category_id` filter and includes each
  sub's `category_ids`. NewPipe group import maps to categories when present.

**UX:** subs page gets a category chip row ("All", then categories). Selecting
one filters the list. Editing a sub's categories happens in an (optional)
sub-detail view or the import step. Creating and managing a category shares one
dialog: a name field plus a checkbox list of every channel, so a category can
be filled in bulk at creation time and re-assigned in bulk later (no
per-channel clicking).

**Acceptance:**
- Create 2 categories, put one channel in both; filtering shows it under both;
  deleting a category removes it from all channels; import with `groups`
  populates categories.

---

## 6. Recommended videos on the watch page

**Status: implemented and verified by the user.** A "Related" row renders below
the player, reusing the card list; clicking navigates to `/watch/[id]`. Item 7
moves that row (in non-theater mode) to a right-hand column beside the player.

**Goal:** a "Related" row/panel on the watch page.

**Existing:** `innertube.rs` wrapper `get_related_videos(video_id)` →
`Vec<RelatedVideo>` exists but is **not reminted into a Savvy DTO** and there is
**no command** exposing it.

**Backend:**

- New DTO `RelatedVideoDto { video_id, title, thumbnails, duration_seconds,
  view_count, author, channel_id? }` mapped from `RelatedVideo` in
  `crates/innertube/src/dto.rs` (canonical).
- Command `related_videos video_id → RelatedVideosDto { videos }`, cached per
  video id (in-memory, TTL ~1h, single-flight like resolve) so the watch page
  never triggers a second request on repeat navigation.
- It is an extra InnerTube request per *new* video id — leave it on the watch
  page's critical path *behind* the resolve (render the player from resolve,
  fill the related panel when ready).

**UX:** below the player, above/alongside comments: list of related videos
reusing the existing card component (hover pre-resolve applies). Clicking one
navigates to `/watch/[id]` (normal route change; progress of the current video
is saved per feature 2).

**Acceptance:**
- Related list appears for a video, cards show thumbnails/titles/durations,
  clicking one plays it, hovering pre-resolves it, and returning to a viewed
  video shows the related panel instantly (cache hit).

---

## 7. Collapsible stream details

**Goal:** reclaim player space. The technical format table moves out of the
right side of the player into a button-triggered popup next to the other
below-player controls, and the freed right-hand column goes to recommended
videos.

**Existing:** the watch page has two separate things:

- the **quality dropdown** in the player controls (`selectedVideoItag`/
  `selectedItag`, `+page.svelte` ~line 413) — **keep this exactly where it is**,
- the wider **"Stream details" `<aside>`** (`+page.svelte` ~lines 465-494): a
  full table of every adaptive format with itag, kind (AV/V/A), resolution and
  bitrate, rendered next to the player. This is debug-level information for
  regular use and it is what gets moved.
- the **recommended-videos row** (item 6) currently sits below the player.

**UX:**

- Remove the aside from the player row entirely.
- Add a "Stream details" button in the row of below-player controls (next to
  the resolution picker, fallback button and the SponsorBlock manual-skip
  button). Clicking it opens a popup **overlay** with an enriched format table
  (itag, kind, format/resolution or audio specs, codec, bitrate, size) plus
  stream diagnostics (mode, views, publish date, audio relayed vs extracted,
  mux bytes/state, error text). Hidden by default; for the people who care,
  nothing of the default view is taken over by it.
- **Non-theater layout:** recommended videos move to a right-hand column beside
  the player (the space the aside vacated). In theater mode the player spans
  full width and the related column goes back below the player (item 9).
- The popup state is per-visit (no persistence needed).
- The redundant current-resolution label beside the fallback button is removed
  (the resolution picker above it already says it), and the byline above the
  description gains the video's release date (`publishDate` from the microformat,
  `uploadDate` fallback) — YouTube shows a date there and the fetch already
  carried it, only the DTO dropped it.

**Backend:** none strictly; expose the already-computed values (mux source,
bytes) if the UI needs anything currently omitted.

**Acceptance:**
- Default watch page shows the player + title + controls with recommended
  videos to the right; the format table is gone from the right side; the
  "Stream details" button opens the overlay with the enriched format list and
  stream diagnostics, and dismissing it returns to the clean layout. Quality
  stays selectable from the player control.

---

## 8. Channel navigation + channel search

**Status: implemented and verified by the user** — channels work: the watch page
author links open the channel page, channel search finds channels, and channel
pages resolve from both id- and handle-URLed channels.

**Goal:** a way to reach a channel (from any video or search result) and to find
channels by searching — currently impossible in both directions.

**Existing (backend):** `channel_get_info` / `channel_get_videos` commands exist
and are registered; `channel/[id]/+page.svelte` is a placeholder. innertube-rs
`get_channel` already accepts a channel id **or handle**, and
`search_with_filters` + `SearchTypeFilter::Channel` return `SearchChannelItem`s —
the Savvy wrapper only calls the unfiltered `search()` and `SearchResultsDto`
maps videos only.

**Missing (backend):**

- Extend callable search to channels: a `search_channels query` command (or a
  `type: video|channel` param on `search`) backed by `search_with_filters`, plus
  a `SearchChannelItemDto { channel_id, title, thumbnails, subscriber_count?,
  video_count? }` in `crates/innertube/src/dto.rs`.
- Nothing for `get_channel` itself — already accepts ids and handles.

**Missing (UI):**

- Watch page: author name/avatar in the header are links to `/channel/[id]`
  (when `channel_id` is known from resolve).
- Search page: a channel results section (or channel cards among results);
  clicking navigates to `/channel/[id]`.
- Channel page: fill the placeholder — profile header (name, avatar,
  subscribers, video count from `channel_get_info`) + video grid from
  `channel_get_videos` (reuse cards; hover pre-resolve applies).

**Acceptance:**
- From any video, clicking the author opens that channel page showing their
  videos. Searching a channel name finds the channel and opens its page.
  Channel pages for handle-URLed or id-URLed channels both resolve.

---

## 9. Theater mode

**Goal:** an optional watch-page layout that enlarges the player by expanding it
horizontally while keeping the top bar and page chrome — the mid-point between
the default layout and fullscreen.

**Existing:** nothing. The player size is fixed by the page layout.

**UX:**

- A "Theater" toggle (keyboard `t` and a control in the player bar, matching
  YouTube conventions).
- On: the player expands to nearly full viewport width, the page sections
  (title, details) stack below it full-width, and the related/collapsible
  sections move beneath the player; nav/sidebar stay visible.
- Off: current compact layout — recommended videos in the right-hand column
  (item 7). Persist the user's preference in the `settings` table (exists in
  `db.rs`).
- **The player must always fit on screen.** Sizing computes from the available
  viewport height (player height capped so video + controls + page chrome never
  extend below the window edge); width alone must never be allowed to push the
  bottom edge off-screen.

**Backend:** `settings` key `player.theater` (bool) — read on load, write on
toggle. No other changes.

**Acceptance:**
- Toggling theater widens the player without entering browser fullscreen; `t`
  toggles it; the choice survives an app restart; toggling off restores the
  original layout and the player keeps playing uninterrupted throughout.
- At any window size, the entire player (video + controls) is fully visible;
  expanding theater never puts part of it below the viewport edge.

---

## 10. Playlists

**Status: done** (verified by the user), scoped to local playlists. Read-only
YouTube playlist pages exist (`youtube_playlist` / `get_playlist`), and local
playlists are fully editable: create/delete, add-to-playlist from the context
menu (including creating one on the spot), and per-video remove. Playlist
results in search carry title/count/channel, and the playlist page renders
per-video metadata. The auth question below concerns only *syncing writes to
YouTube*, which is out of scope for v1.

**Goal:** playlists are currently a placeholder. Make them a real, browsable and
editable feature.

**Open questions (need a decision before implementation):**

- Scope: read-only browsing, or full playback/edit? "Add to playlist" (item 11)
  implies writing, so full edit is the likely target.
- Auth model: playlist creation/ordering requires a signed-in YouTube session.
  Savvy is currently anonymous. Anonymous playlists would have to be local-only
  (stored in the `playlists` tables) and would not sync to YouTube. This decides
  the whole design.
- Mix of local playlists vs. YouTube playlists vs. both in one view.

**Backend sketch:** `playlists` / `playlist_items` tables already exist. Local
playlists are straightforward; YouTube playlist reads can go through
innertube-rs, but writes need OAuth.

**Known gaps (blocking usable playlist discovery):**

- ~~In search results, playlist cards are listed but show no proper title, and
  clicking one does nothing.~~ **Done.** The root cause was `innertube_rs`
  falling back to "Untitled Playlist"/empty ids: modern search serves playlists
  as `lockupViewModel` (`contentType: LOCKUP_CONTENT_TYPE_PLAYLIST`), which its
  `PlaylistNode::from_value` does not read. Playlists are now parsed from the raw
  `/search` response (`search_playlists_from_raw`), same approach as channels.
  Cards show title, video count and channel, and navigate to a new read-only
  `/playlist/<id>` page backed by a `youtube_playlist` command (innertube-rs
  `get_playlist`). Discovery/browsing of *YouTube* playlists therefore works
  anonymously today; the auth question below only concerns writes and
  user-library playlists.

**Also fixed (found while probing):** innertube-rs's recursive search/playlist
parsers overflowed the ~2 MiB tokio worker stack on some live responses (e.g.
"sql full course"), aborting the process. Those calls now run on a 16 MiB
dedicated thread via `run_on_big_stack`.

**Acceptance:** TBD once scope and auth are decided (read-only YouTube browsing
is already in place).

---

## 11. Right-click context menu

**Status: built — one global `contextmenu` listener plus a shared store-backed
menu (`ui/src/lib/contextmenu.svelte.ts`, `components/ContextMenu.svelte`, wired
in `+layout.svelte`) give every video card and the player the same action set:
copy link, open in browser, go to channel (when the card carries a channel id),
add to playlist ▸ (lazy list of local playlists), save to library (with a
"Saved ✓" state, backed by the `saved_videos` table), mark as watched, and
remove from history. The WebView/Edge default menu is suppressed everywhere —
only Savvy's own menu may appear, per user decision. "Save video" means saving
to the library (item 12), not downloading to disk.**

**Goal:** right-clicking a video surfaces the actions a user expects, instead of
only the browser default.

**Candidate actions:** most of the list now ships. Dropped as out of scope: add
to watch later (no such concept — local playlists cover it) and pin/unpin the
subscription (the app organises subscriptions by categories, not pins).
Download-to-disk stays out of scope — "save video" was confirmed to mean saving
to the library.

**Surfaces:** in addition to video cards (feed, search, channel pages, related
cards, hover previews), right-clicking **on the player itself** offers the
player-relevant actions — copy link, open in external browser, and add to
playlist. Cards that do not store a channel id (history rows, playlist items)
omit "go to channel" rather than fetching one lazily.

**UX:** native-feeling context menu at the cursor, clamped to the viewport,
closes on click-away or `Esc`. Cards publish their payload through `data-ctx` +
`data-*` attributes and one listener in the layout handles every surface, so the
action set cannot drift per page.

**Acceptance:** right-clicking any video card offers the same set of applicable
actions everywhere, and each action completes without leaving the current page.
Pages showing history refresh through a `savvy:history-changed` event.

---

## 12. Library — flesh out or remove

**Status: built — the Library page is now the "Saved videos" surface.** Right-
clicking any video card or the player offers "Save to library" (the first menu
item, toggling to "Saved ✓"); the page (`ui/src/routes/library/+page.svelte`)
lists saved videos from the `saved_videos` table, newest first, with a hover
remove ✕, a play-on-click card that feeds the same context menu as everywhere
else, and an empty state pointing at search. Pages refresh through a
`savvy:saved-changed` event. Scope is saved videos only (per user decision) —
history and subscriptions keep their own nav entries.

---

## 13. Player visual design pass

**Status: built.** The control bar, timeline additions (buffer indicator,
sponsor highlights, chapter ticks, hover time/chapter bubble), the shortcut
layer and editable bindings shipped earlier. This round added the remaining
overlays: a big centre play button (paused state), a captions toggle in the bar
(the CC popover lists every track plus Off; the `c` binding cycles them, wired
through the shortcut table as `player.shortcuts.captions`, default empty), and a
settings gear whose popover holds playback-speed chips and the quality selector
plus the HD/360p fallback switch. The below-player captions and quality
selects were removed in favour of these two menus. Theme handling: the bar's
chrome stays dark-over-video in both themes by design (it sits on the picture);
the popovers use the `popover`/`muted`/`primary` tokens, so they re-theme
automatically. Popovers dismiss on an outside press or `Esc`, and pin the bar
open while visible.**
The player is being brought in line with the app's Catppuccin palette (Latte
light / Mocha dark, the oklch tokens in `layout.css`): a fully custom control bar
replaces the native `<video controls>` chrome, with YouTube-standard shortcuts and
the timeline additions described below folding into the same pass.

**Goal:** the player does not match the rest of the UI. Bring it in line with the
app's own visual language — spacing, radii, borders, and control bar styling —
instead of looking like a bolted-on embedded element. The native `<video
controls>` bar is replaced by a custom one, because the additions below (timeline
overlay, hover preview, shortcut layer) cannot be drawn on top of the native
controls.

**Scope to define:**

- Control bar: control sizing, icon weight, hover/active states, progress bar
  styling, and buffer indicator.
- Surrounding chrome: corner treatment, border, shadow, background colour in
  light and dark themes.
- Overlays: the big centre play button, settings/fullscreen popovers, captions
  toggle, and volume slider.
- Empty/loading states: skeleton while resolving, and the error state.

**Functional additions folded into this pass** (the original "polish only" scope
below is amended by this section):

- **SponsorBlock on the timeline.** Where segments are known, the scrubber draws
  a segment highlight sized to the segment's duration, tinted per category to
  match the Settings rows — so the user can see *what* and *how much* is being
  skipped before it happens. The highlight is a hint, not a lock: scrubbing into
  the highlighted region behaves like any other scrub, and the skip logic itself
  (item 18) is unchanged. Hidden when the feature or the video's segment list is
  empty.
- **Precise clicks without scrubbing.** Hovering the timeline previews the time
  under the cursor (the conventional hovered-timestamp bubble) and shows where a
  click would land, so the user can click to an exact position instead of
  dragging the thumb through it and possibly past the target.
- **YouTube-standard playback shortcuts.** `k`/`space` play-pause, `j`/`l`
  −10s/+10s, ←/→ −5s/+5s, ↑/↓ volume, `m` mute, `f` fullscreen, `t` theater
  (already exists), `c` captions/audio track (once item 3 lands), `.`/`,` frame
  step while paused, `>`/`<` playback speed, and `0`–`9` seek-to-percentage.
  Replacing the native controls means these bindings are now the app's
  responsibility rather than the browser's. `space`, `0`–`9`, `,` `.` `<` `>`
  are matched *before* the configurable layer and are deliberately not
  rebindable — the frame-step and speed pair uses the layout's `<`/`>` (Shift
  with `,`/`.`), speed steps 0.25×–2× and mirrors onto the dash audio track.
- **Editable bindings.** Each action ships with a default key plus one editable
  *alternative* binding, stored as `player.shortcuts.<action>` in the settings
  table and rendered in Settings → Playback. A binding is a single key,
  optionally combined with shift/alt/ctrl/meta (spelled `Ctrl+Alt+Shift+Meta+<key>`,
  subsets in that order); an empty value disables that binding. The UI resolves
  key → action with defaults as the fallback, so a cleared or invalid
  alternative silently reverts to the default. `db.rs` validates the shape, and
  a key the player matches itself (Space, a digit, `,` `.` `<` `>`) is refused
  rather than stored dead.
- **Chapter markers on the timeline.** When the video has chapters (item 4), the
  scrubber marks each chapter start the way YouTube does — a small tick/gap in
  the bar at the boundary — so the chapter list and the scrubber always agree.
  Purely a marker: clicking near it still scrubs normally, and the current
  chapter's segment may be tinted while the rest stays baseline. The hover
  bubble names the chapter under the cursor as well as the time.
- **Buffer indicator and buffering states.** The scrubber carries the buffered
  range ahead of the playhead under the played fill; while the element has
  nothing decoded to show — first load, a stall, a cold seek — a spinner sits
  centred over the picture, below the control bar. Resolving a video keeps its
  own spinner, and a resolve failure keeps its error card.

**Note:** this pass now carries the functional additions above, so it should
come after the functional watch-page items (subtitles, chapters) and after item
18's segments are wired — it is built once, against the final layout.

**Acceptance:** player controls and overlays use the same tokens as the rest of
the UI, and nothing overflows or misaligns at the common window sizes. Segment
highlights and chapter ticks on the timeline are sized correctly and survive
window resizes; hovering shows the preview time and the chapter name; the
buffered range is visible and a stall shows the spinner rather than a frozen
frame with no explanation; every documented shortcut performs its action, their
alternatives remap it, and an empty alternative disables that binding.

---

## 14. Channel search requires an exact name

**Goal:** searching for a channel only returns results when the query matches the
channel name exactly. Searching `John Doe` must find `JohnDoesChannel`.

**Symptom:** a partial name returns nothing, or returns only an exact match.

**Correction to the original diagnosis:** the backend was *not* the cause.
`dto::search_channels` already uses `search_with_filters` with
`SearchTypeFilter::Channel`, the `search_channels` command is registered, and the
search page has a `channels` mode. The ordinary `search()` also maps
`SearchResultItem::Channel`. So item 8's wiring is done.

**Actual cause:** the page defaults to `mode = 'all'`, which calls the unfiltered
`api.search()`. YouTube only returns a channel shelf from the unfiltered
`/search` when the query matches a channel well — for a loose query it answers with
videos, and there is no channel to show. Channel results only appear once the user
manually switches to the `channels` mode, which forces
`SearchTypeFilter::Channel`. So the behaviour is YouTube's ranking, surfaced as a
UX trap rather than a bug.

**Fix:** in `all` mode, run the unfiltered search *and* the channel-filtered search
concurrently, merge, and de-duplicate. A channel query then resolves without the
user knowing a mode switch exists. Also consider matching handles/`@name`, since
that is how most people name-search.

**Acceptance:** searching `John Doe` surfaces `JohnDoesChannel` without toggling
any filter; partial and differently-spaced names resolve; lower case works too.

---

## 15. YouTube bot checks (`LOGIN_REQUIRED: not a bot`)

**Status: mitigated in code, environmental cause remains.** Not a feature item;
recorded because it degrades the app badly and will recur.

**Symptom:** playback fails on nearly every video, including ones that worked
minutes earlier. The raw message is
`Content is restricted: Video is not playable: LOGIN_REQUIRED (Sign in to confirm
you're not a bot)`.

**Root cause of the bug:** `LOGIN_REQUIRED` covers two unrelated conditions — the
per-video age gate (`Sign in to confirm your age`) and the IP-level bot check
(`Sign in to confirm you're not a bot`). The app collapsed them: it read the
bot check as a *per-video* failure, so it never armed the global throttle and
kept retrying a flagged IP, which kept the flag alive and turned one bad moment
into a total outage. It also reported the block as "age-restricted", pointing
users at a setting that does not exist.

**Implemented:** bot checks are classified as a genuine throttle, distinct from the
age gate, which stays per-video and never locks the app. Covered by tests for the
curly and straight apostrophe variants.

Beyond classification, three mitigations were added after measuring every available
InnerTube client with `client_probe`:

- **Client rotation.** A bot check is a judgement about a client as presented,
  not a blanket IP ban, so `video_info_full` walks `PRIMARY_CLIENTS` and takes the
  first that yields playable URLs. Measured: ANDROID_VR, IOS, ANDROID, TV_EMBEDDED,
  WEB_EMBEDDED, VISIONOS and WEB all work from an unflagged IP. Note that MWEB and
  TV_SIMPLY answer `status=OK` while withholding every URL, so "has a URL" is the
  success test, not "did not error".
- **Persisted `visitorData`.** A fresh one per launch made the app look like a new
  anonymous client every start. Now reused across launches; `innertube_reset` still
  discards it deliberately, since obtaining a fresh identity is its whole purpose.
- **Escalating backoff.** 2min → 4 → 8, capped at 15, reset by any success. 429s
  stay at 30s because they usually clear immediately.

**Not built: PO Tokens.** LibreTube's approach (running YouTube's BotGuard in a
hidden WebView) exists only to serve the WEB client. With seven working clients
available to rotate through, it was not worth the cost. Revisit only if WEB becomes
the sole option.

**Still unaddressed:** an IP-level block, which rotation cannot defeat because every
client goes down together. The durable fix is an authenticated session (cookies or
OAuth), which changes the app's privacy and setup story — needs a decision. That
same decision unblocks items 10 and 11.

---

## 16. Subscription sidebar avatars missing

**Status: done.** Root cause was not rendering: the `subscriptions.thumbnail`
column was simply empty for most rows, so the sidebar had nothing to show. A
`subscription_refresh_avatars` command now resolves blank-avatar rows via
`get_channel_info` (one `/browse` per channel, sequential and budget-capped at
25 per pass, skipped while an IP-throttle is active) and persists each hit, so
any given channel is fetched at most once ever. The subscriptions page calls it
in the background after the stored list loads and merges the results in, so the
sidebar fills in without blocking the first paint. Cosmetic and best-effort.

**Priority: low, cosmetic.** Not blocking anything.

**Symptom:** in the expandable subscriptions list on the right of the Subs page,
only the first few channels have avatar images; most render as a blank or
placeholder circle.

**Likely causes to confirm** (in order of probability):

1. The channel rows come from the local `subscriptions` table, which the NewPipe
   import populates. NewPipe exports channel icons, but they may be dropped on
   import rather than stored — in which case there is nothing to display and the
   UI is showing an honest gap.
2. The avatar URL is stored but the app requests it directly from YouTube, where
   it 404s without the right headers. If so it must route through the existing
   proxy, like every other upstream fetch.
3. Avatars are simply never fetched for rows loaded lazily as the list expands.

**Backend:** confirm whether `subscriptions` has an avatar column and whether the
import writes it. If not, fetch avatars during import or on first expansion.

**Acceptance:** every channel in the sidebar shows its avatar, including ones
scrolled into view after the initial render.

---

## 17. Settings

**Goal:** one real settings surface, so per-user preferences stop being implicit
hardcoded choices scattered through the watch page.

**Existing:** the `settings` table (`key TEXT PRIMARY KEY, value TEXT NOT NULL`)
plus `db.rs::setting_set` / `setting_get` (line 328/337) — and that is all. There
are **no settings commands**, **no settings route** (`ui/src/routes/` has none),
and no reader anywhere: item 9's `player.theater` key is only *planned*. Every
behavioural default currently lives as a literal in `+page.svelte`
(`pickDefaultHd` → 1080p, `mode` → `'progressive'`, captions → proposed
on-by-default, volume → browser default).

**Backend:**

- `settings_get_all` → `HashMap<String, String>`; `settings_set { key, value }`.
  Typed per-setting reads are unnecessary for v1 — the webview coerces and
  validates, the backend stores strings.
- **Defaults live in Rust**, as `fn settings_defaults() -> HashMap<&'static str, &'static str>`,
  merged under stored values. A setting absent from the table must behave as the
  documented default, so adding a key is never a migration.
- **All reads for the current session are fetched once** at app start and held in
  one store; the watch page reads from that store rather than calling per toggle.

**Settings to expose in v1:**

| Key | Values | Default | Why |
|---|---|---|---|
| `player.default_resolution` | `144`…`2160` | `1080` | `pickDefaultHd` hardcodes 1080. Value is a **height cap**, not a preferred rendition: pick the best MP4 at or below it. A video with no rendition that high falls back to the next best down. `0` = "highest available". |
| `player.default_volume` | `0`–`100` | `100` | Browsers persist this themselves; a desktop app should too. Restored on element create, never mid-playback. |
| `player.theater` | bool | `false` | Item 9. |
| `player.autoplay` | bool | `true` | Plays the next related video when one ends. |
| `player.skip_intro` | `0`…`60` s | `0` (off) | Overlaps item 18's intro handling; kept here because it is a per-user number, not a SponsorBlock setting. |
| `player.skip_outro` | `0`…`60` s | `0` (off) | As above. |
| `player.shortcuts.<action>` | `""` or a binding | `""` (no alternative) | One row per action — `playpause`, `back10`, `forward10`, `back5`, `forward5`, `volumeUp`, `volumeDown`, `mute`, `fullscreen`, `theater`. Stores an *additional* key for that action (`k`, `ArrowLeft`, `Ctrl+Shift+j`); the default key keeps working regardless, and `""` means none. Reserved keys (Space, `0`–`9`, `,` `.` `<` `>`) are refused by `validate_setting`. Item 13. |
| `playback.audio_language` | `original` \| BCP-47 \| `any` | `original` | `original` means "play the video's own audio", **not** "whatever the API ranks first". See item 18 — ranking by bitrate alone picks a dub. `any` opts out and restores current behaviour. |
| `playback.preferred_codec` | `auto` \| `h264` | `auto` | `h264` restricts to MP4/H.264, which every WebView2 build decodes. `auto` still filters AV1/VP9, so this is about escape-hatching a bad default rather than changing it. |
| `playback.hd_mode` | `auto` \| `always_360p` | `auto` | Escape hatch back to the progressive fallback for machines where the adaptive pair misbehaves. |
| `ui.theme` | `system` \| `light` \| `dark` | `system` | Must apply without a reload. |
| `ui.compact_sidebar` | bool | `false` | |
| `data.confirm_mark_watched` | bool | `true` | |
| `data.clear_history_on_start` | bool | `false` | |
| `network.reset_identity` | action, not a value | — | Button that calls the existing `innertube_reset` to discard persisted `visitorData`. Item 15. |

**UX:** a `/settings` route reachable from the nav, grouped into **Playback**,
**Interface**, **Data**, **Network**. Toggles, a resolution dropdown populated from
the current video's real MP4 renditions when on the watch page, and number inputs
with units. Changes apply immediately where possible; the store writes through to
the backend on change (debounced for the number fields).

**Acceptance:**
- Set default resolution to 720p, open a 1080p video → it starts at 720p. Open a
  360p-only video → it plays 360p, with no empty picker and no error.
- Set volume, restart the app → the new volume is used.
- Every key above survives a restart, and deleting a row from `settings` restores
  the documented default rather than breaking playback.
- Theater toggle and this page agree — one source of truth for `player.theater`.

---

## 18. SponsorBlock

**Goal:** skip sponsor reads and other flagged segments, as LibreTube does.
Crowdsourced, opt-out, no account, no YouTube integration.

**Status: done** (verified by the user). The `sponsor_segments` command fetches
from the backend with a per-video cache (24h TTL, ETag revalidation, negative
results cached, any failure degrading to "no segments"), and the Settings →
Playback surface exposes the master toggle, per-category checkboxes and the
auto-skip/manual choice. Segments also drive the timeline highlights in item 13.

**Upstream contract (verified against `SponsorBlockServer` `master`, not guessed):**

- `GET https://sponsor.ajay.app/api/skipSegments?videoID=<id>&category=<csv>`
  — **read-only; we never call the write, vote, or submit endpoints.**
- `category` (singular, plain CSV) is what we want. `categories` (plural) is
  `JSON.parse`d, so `categories=sponsor` is a **400**, not a filter. Defaults if
  omitted: `category=sponsor`, `actionType=skip`, `service=youtube`.
- **A video with no matching segments answers `404`, not `[]`.** `getSkipSegments`
  does `res.sendStatus(404)` on an empty result. Treat 404 as "nothing to skip";
  only `400`/`5xx`/transport errors are real failures. This is the single easiest
  thing to get wrong here.
- `200` carries an `ETag` (reuse it on revalidation) and a JSON array:

  ```json
  [{ "category": "sponsor", "actionType": "skip",
     "segment": [123.4, 567.8], "UUID": "…",
     "videoDuration": 600.0, "locked": false, "votes": 3,
     "description": "…" }]
  ```

  `segment` is `[startSeconds, endSeconds]`.
- Categories: `sponsor`, `selfpromo`, `intro`, `outro`, `interaction`, `preview`,
  `hook`, `filler`, `music_offtopic`, `exclusive_access`, `poi_highlight`,
  `chapter`. LibRetube's set is `sponsor`, `selfpromo`, `interaction`, `intro`,
  `outro`, `preview` — the sensible default; the rest are opt-in.
- `404` and `429` are both rate-limit-adjacent. The server's own view budget is
  ~10 reads per 15 min, so **one fetch per video, cached, never polled.**

**Backend:**

- `SponsorSegmentDto { category, start_seconds, end_seconds, uuid }` in
  `crates/innertube/src/dto.rs` (canonical, per the global constraints).
- `sponsor_segments video_id → { segments: [] }`, fetched by the **backend** — the
  webview must not reach the network directly.
- Cache: in-memory per video id, TTL ~24h (these change rarely, and it protects
  the request budget), single-flight, and the `ETag` retained for revalidation.
  Cache **negative** results too: a 404 for a given id must be remembered or we
  will re-request every visit and get rate-limited for videos that simply have no
  segments.
- Timeout ~2s, and **any failure degrades to "no segments"** — never surface a
  SponsorBlock error to the user, never block playback on it.
- Merging/dedupe is the server's job; we only sort by `start_seconds`.

**UX (LibRetube's shape):**

- Settings → Playback → "SponsorBlock", master toggle, a per-category checkbox
  list, and an "auto-skip / show a skip button" choice. LibRetube's useful
  distinction is kept: **auto-skip** jumps the player forward, whereas **manual**
  surfaces a "Skip sponsor ▸" control the user acts on.
- Auto-skip fires when playback **enters** a segment, seeking to `end_seconds`.
  Segments must be **merged/ignored while overlapping a seek** so a user scrub
  does not trigger a jump they did not ask for — this is the main correctness trap.
- `music_offtopic` is `skip`-only, `poi_highlight` is `poi`-only, and
  `chapter`/`exclusive_access` do not support `skip` at all (`categorySupport`),
  so the category list must hide actions a category cannot honour.

**Acceptance:**
- A video with a known `sponsor` segment auto-skips it when enabled, and shows a
  skip button when set to manual. Disabling every category disables the fetch.
- Seeking across a segment boundary never triggers a skip.
- A video with no segments costs exactly one request per 24h and no visible error;
  repeated visits to such a video issue no further requests.
- A network failure or timeout leaves playback completely unaffected and silent in
  the UI.

**Note:** the segment list should also drive the timeline highlights defined in
item 13 (the player design pass).

---

## Where to continue (priority order)

All v1 feature items are done: 1 (NewPipe import), 2 (progress persistence),
3 (subtitles), 4 (chapters), 5 (subscription categories), 6 (related videos),
7 (collapsible stream details), 8 (channel navigation), 9 (theater mode),
10 (playlists, local + read-only YouTube), 11 (context menu), 12 (library),
13 (player design pass), 14 (channel search), 16 (sidebar avatars),
17 (settings) and 18 (SponsorBlock). The specs above record the agreed shape.

Remaining, non-feature:

1. **Auth decision.** Playlist *writes/sync*, watch-later, private/subscribed-only
   content and durable recovery from an IP-level bot block (item 15) all hinge on
   a decision about an authenticated session (cookies or OAuth). Out of scope for
   v1, which is local-only for user-owned data.
2. **Packaging.** No Tauri CLI / no root `package.json` yet — a distributable
   build is still to set up.
3. **Item 15 IP-level block.** Mitigated in code (client rotation, throttle
   classification); a durable fix needs item 1 above.
