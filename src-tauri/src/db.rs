//! Savvy's local SQLite store. Subs, playlists, history, queue and
//! settings all live here — nothing is sent to YouTube.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

pub struct Db {
    conn: Connection,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistRow {
    pub id: String,
    pub title: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistItemRow {
    pub playlist_id: String,
    pub video_id: String,
    pub title: String,
    pub author: String,
    pub thumbnail: String,
    pub duration_seconds: u32,
    pub channel_id: String,
    pub position: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryRow {
    pub video_id: String,
    pub title: String,
    pub author: String,
    pub thumbnail: String,
    pub duration_seconds: u32,
    pub channel_id: String,
    pub watched_at: i64,
    pub progress_seconds: f64,
}

/// One entry in the user's saved-videos collection ("save to library").
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedVideoRow {
    pub video_id: String,
    pub title: String,
    pub author: String,
    pub thumbnail: String,
    pub duration_seconds: u32,
    pub channel_id: String,
    pub saved_at: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionRow {
    pub channel_id: String,
    pub title: String,
    pub thumbnail: String,
    pub subscriber_count: Option<i64>,
    pub added_at: i64,
    /// Ids of the categories this subscription belongs to. Categories are a
    /// many-to-many grouping, so this is a list, never a single column.
    pub category_ids: Vec<String>,
}

/// A user-defined subscription category (a NewPipe group, or one made by hand).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryRow {
    pub id: String,
    pub name: String,
    pub position: i64,
}

/// Alternative key bindings: the actions, in the order the settings page lists
/// them. Mirrors `DEFAULT_SHORTCUTS` in `ui/src/lib/shortcuts.ts`, which is
/// what actually resolves a key press against these stored values.
const SHORTCUT_KEY_PREFIX: &str = "player.shortcuts.";
const SHORTCUT_ACTIONS: [&str; 11] = [
    "playpause",
    "back10",
    "forward10",
    "back5",
    "forward5",
    "volumeUp",
    "volumeDown",
    "mute",
    "fullscreen",
    "theater",
    "captions",
];
/// Modifiers a binding may use, in the only order they may be spelled in.
const SHORTCUT_MODIFIERS: [&str; 4] = ["Ctrl", "Alt", "Shift", "Meta"];
/// Named keys a binding may use instead of a single character.
const SHORTCUT_NAMED_KEYS: [&str; 9] = [
    "ArrowLeft",
    "ArrowRight",
    "ArrowUp",
    "ArrowDown",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Space",
];
/// Keys the player matches on its own — Space and the digits are play/pause and
/// seek-to-percentage, `,` `.` `<` `>` are frame step and speed. A stored
/// binding on any of them could never fire.
const RESERVED_SHORTCUT_KEYS: [&str; 6] = ["Space", " ", ",", ".", "<", ">"];

/// Every setting the app understands, and what it is when the user has never
/// touched it.
///
/// This lives in Rust rather than the UI so the defaults are the single
/// authority: a key that is simply *absent* from the `settings` table behaves
/// exactly like the value below, which means adding a setting never needs a
/// migration and deleting a row restores sane behaviour instead of breaking
/// playback. Anything not listed here is unknown to the app.
///
/// Only keys with a working consumer are listed — the settings page renders
/// from this set, so a row cannot ship dead.
pub fn settings_defaults() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        // Playback
        ("player.default_resolution", "1080"),
        ("player.default_volume", "100"),
        ("playback.hd_mode", "auto"),
        // Alternative key bindings. Empty means "no alternative": the default
        // key ships with every action and keeps working; a stored value only
        // adds a second key for the same action.
        ("player.shortcuts.playpause", ""),
        ("player.shortcuts.back10", ""),
        ("player.shortcuts.forward10", ""),
        ("player.shortcuts.back5", ""),
        ("player.shortcuts.forward5", ""),
        ("player.shortcuts.volumeUp", ""),
        ("player.shortcuts.volumeDown", ""),
        ("player.shortcuts.mute", ""),
        ("player.shortcuts.fullscreen", ""),
        ("player.shortcuts.theater", ""),
        ("player.shortcuts.captions", ""),
        // Interface
        ("ui.theme", "system"),
        ("ui.compact_sidebar", "false"),
        ("player.theater", "false"),
        // Captions. An empty font means "use the app's default stack"; anything
        // else is a CSS `font-family` value applied to the rendered cue.
        ("captions.font", ""),
        // Data
        ("data.clear_history_on_start", "false"),
        // SponsorBlock. The default category set mirrors what LibreTube ships;
        // it is filtered to the categories the skip feature can honour.
        ("sponsorblock.enabled", "false"),
        ("sponsorblock.mode", "auto"),
        ("sponsorblock.categories", "sponsor,selfpromo,interaction,intro,outro,preview"),
    ])
}

/// Reject anything the app would not be able to interpret.
///
/// Values are stored as plain text by design, but the *shape* is checked here
/// rather than only in the webview: a typo reaching the table would otherwise
/// persist silently across restarts and only misbehave on the next load.
pub fn validate_setting(key: &str, value: &str) -> Result<()> {
    let bool_ok = |v: &str| v == "true" || v == "false";
    let bounded = |v: &str, max: u32| v.parse::<u32>().map_or(false, |n| n <= max);

    let ok = match key {
        // A height cap; `0` means "highest available".
        "player.default_resolution" => bounded(value, 4320),
        "player.default_volume" => bounded(value, 100),
        "playback.hd_mode" => matches!(value, "auto" | "always_360p"),
        "ui.theme" => matches!(value, "system" | "light" | "dark"),
        "ui.compact_sidebar" | "data.clear_history_on_start" | "player.theater" => bool_ok(value),
        // A CSS `font-family` value, or '' for the app default. Bounded and kept
        // single-line so a stray paste cannot smuggle in a rule break.
        "captions.font" => value.len() <= 200 && !value.contains(['\n', '\r']),
        // SponsorBlock: the enabled flag, the auto/manual mode, and a
        // comma-separated category list restricted to the ones the app can act
        // on (an empty list means the feature is effectively off).
        "sponsorblock.enabled" => bool_ok(value),
        "sponsorblock.mode" => matches!(value, "auto" | "manual"),
        "sponsorblock.categories" => value.split(',').all(|c| {
            c.is_empty() || crate::sponsorblock::CATEGORIES.contains(&c)
        }),
        // Alternative key bindings: a canonical combination for a known action,
        // or the empty string, which means "no alternative".
        k if k.starts_with(SHORTCUT_KEY_PREFIX) => {
            let action = &k[SHORTCUT_KEY_PREFIX.len()..];
            SHORTCUT_ACTIONS.contains(&action) && is_shortcut_binding(value)
        }
        _ => false,
    };
    if !ok {
        anyhow::bail!("invalid value for {key}: {value}");
    }
    Ok(())
}

/// Whether `value` is a binding this player could ever fire.
///
/// This checks the *shape* only — which key does what belongs to the webview.
/// Canonical modifier order, one key, and none of the keys the player matches
/// itself before it consults a binding: a stored key that can never fire would
/// look configurable in Settings while doing nothing. `""` is the documented
/// "no alternative" and is valid on purpose, so clearing a binding is a write
/// rather than a delete.
fn is_shortcut_binding(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    let mut parts: Vec<&str> = value.split('+').collect();
    let Some(key) = parts.pop() else {
        return false;
    };
    // Modifiers may only appear in the canonical order, each at most once, so
    // `Shift+Ctrl+x` and `Ctrl+Ctrl+x` are rejected rather than stored as two
    // spellings of one thing. Subsets are fine: `Alt+Shift+m` is a real binding.
    let mut previous = -1i32;
    for modifier in &parts {
        match SHORTCUT_MODIFIERS.iter().position(|m| m == modifier) {
            Some(position) if position as i32 > previous => previous = position as i32,
            _ => return false,
        }
    }
    if RESERVED_SHORTCUT_KEYS.contains(&key) {
        return false;
    }
    // Digits too: the player owns them for seek-to-percentage, and a digit
    // pressed with Shift arrives as whatever that layout's shifted character is
    // (`%`, not `5`), so the stored form could never match a press either.
    if key.len() == 1 && key.as_bytes()[0].is_ascii_digit() {
        return false;
    }
    key.chars().count() == 1 || SHORTCUT_NAMED_KEYS.contains(&key)
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("opening sqlite db at {}", path.display()))?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS history (
                video_id         TEXT PRIMARY KEY,
                title            TEXT NOT NULL,
                author           TEXT NOT NULL,
                thumbnail        TEXT NOT NULL DEFAULT '',
                duration_seconds INTEGER NOT NULL DEFAULT 0,
                channel_id       TEXT NOT NULL DEFAULT '',
                watched_at       INTEGER NOT NULL,
                progress_seconds REAL NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_history_watched_at ON history (watched_at DESC);

            -- The user's saved-videos collection ("save to library"). The full
            -- text/metadata is denormalised so the Library page renders and the
            -- context menu re-saves without another InnerTube round trip.
            CREATE TABLE IF NOT EXISTS saved_videos (
                video_id         TEXT PRIMARY KEY,
                title            TEXT NOT NULL,
                author           TEXT NOT NULL,
                thumbnail        TEXT NOT NULL DEFAULT '',
                duration_seconds INTEGER NOT NULL DEFAULT 0,
                channel_id       TEXT NOT NULL DEFAULT '',
                saved_at         INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_saved_videos_saved_at ON saved_videos (saved_at DESC);

            CREATE TABLE IF NOT EXISTS playlists (
                id          TEXT PRIMARY KEY,
                title       TEXT NOT NULL,
                created_at  INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS playlist_items (
                playlist_id      TEXT NOT NULL,
                video_id         TEXT NOT NULL,
                title            TEXT NOT NULL,
                author           TEXT NOT NULL,
                thumbnail        TEXT NOT NULL DEFAULT '',
                duration_seconds INTEGER NOT NULL DEFAULT 0,
                channel_id       TEXT NOT NULL DEFAULT '',
                position         INTEGER NOT NULL,
                PRIMARY KEY (playlist_id, video_id)
            );

            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS subscriptions (
                channel_id       TEXT PRIMARY KEY,
                title            TEXT NOT NULL,
                thumbnail        TEXT NOT NULL DEFAULT '',
                subscriber_count INTEGER,
                added_at         INTEGER NOT NULL
            );

            -- Categories are a many-to-many grouping: one subscription can sit
            -- in several (a "Tech" channel is usually both Tech and News). The
            -- join table is cleaned up explicitly rather than leaning on
            -- `ON DELETE CASCADE`, which SQLite only honours when foreign keys
            -- are enabled per-connection.
            CREATE TABLE IF NOT EXISTS categories (
                id       TEXT PRIMARY KEY,
                name     TEXT NOT NULL UNIQUE,
                position INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS sub_categories (
                channel_id  TEXT NOT NULL,
                category_id TEXT NOT NULL,
                PRIMARY KEY (channel_id, category_id)
            );
            CREATE INDEX IF NOT EXISTS idx_sub_categories_category ON sub_categories (category_id);
            "#,
        )?;

        // Columns added after the first release. `CREATE TABLE IF NOT EXISTS`
        // leaves an existing table untouched, so these are backfilled here for
        // databases written by an older build.
        self.ensure_column("history", "channel_id")?;
        self.ensure_column("playlist_items", "channel_id")?;

        Ok(())
    }

    /// Adds `column` to `table` if a database predating it is opened. SQLite has
    /// no `ADD COLUMN IF NOT EXISTS`, so the presence is checked first.
    fn ensure_column(&self, table: &str, column: &str) -> Result<()> {
        let exists: bool = self
            .conn
            .prepare(&format!("PRAGMA table_info({table})"))?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<String>>>()?
            .iter()
            .any(|name| name == column);
        if !exists {
            self.conn.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} TEXT NOT NULL DEFAULT ''"
            ))?;
        }
        Ok(())
    }

    // ---- history -----------------------------------------------------------------

    pub fn upsert_history(
        &self,
        video_id: &str,
        title: &str,
        author: &str,
        thumbnail: &str,
        duration_seconds: u32,
        channel_id: &str,
        watched_at: i64,
        progress_seconds: f64,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO history (video_id, title, author, thumbnail, duration_seconds, channel_id, watched_at, progress_seconds)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(video_id) DO UPDATE SET
                title            = excluded.title,
                author           = excluded.author,
                thumbnail        = excluded.thumbnail,
                duration_seconds = excluded.duration_seconds,
                channel_id       = excluded.channel_id,
                watched_at       = excluded.watched_at
            "#,
            params![video_id, title, author, thumbnail, duration_seconds, channel_id, watched_at, progress_seconds],
        )?;
        Ok(())
    }

    pub fn history(&self, limit: u32) -> Result<Vec<HistoryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT video_id, title, author, thumbnail, duration_seconds, channel_id, watched_at, progress_seconds
             FROM history ORDER BY watched_at DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(HistoryRow {
                    video_id: r.get(0)?,
                    title: r.get(1)?,
                    author: r.get(2)?,
                    thumbnail: r.get(3)?,
                    duration_seconds: r.get::<_, i64>(4)? as u32,
                    channel_id: r.get(5)?,
                    watched_at: r.get(6)?,
                    progress_seconds: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn clear_history(&self) -> Result<()> {
        self.conn.execute("DELETE FROM history", [])?;
        Ok(())
    }

    /// Delete one row - the context menu's "remove from history". Removing a
    /// video that was never watched is not an error, so this is a no-op.
    pub fn remove_history(&self, video_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM history WHERE video_id = ?1", [video_id])?;
        Ok(())
    }

    pub fn get_history(&self, video_id: &str) -> Result<Option<HistoryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT video_id, title, author, thumbnail, duration_seconds, channel_id, watched_at, progress_seconds
             FROM history WHERE video_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![video_id], |r| {
            Ok(HistoryRow {
                video_id: r.get(0)?,
                title: r.get(1)?,
                author: r.get(2)?,
                thumbnail: r.get(3)?,
                duration_seconds: r.get::<_, i64>(4)? as u32,
                channel_id: r.get(5)?,
                watched_at: r.get(6)?,
                progress_seconds: r.get(7)?,
            })
        })?;
        rows.next().transpose().map_err(Into::into)
    }

    /// Update only the playhead (and recency) of an existing history row — the
    /// title/thumbnail have already been captured by `upsert_history`, so a
    /// progress heartbeat needs no other columns.
    pub fn update_progress(&self, video_id: &str, progress_seconds: f64, watched_at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE history SET progress_seconds = ?1, watched_at = ?2 WHERE video_id = ?3",
            params![progress_seconds, watched_at, video_id],
        )?;
        Ok(())
    }

    // ---- saved videos ("save to library") ------------------------------------------

    /// Upsert one saved video. Re-saving is idempotent and refreshes the row's
    /// metadata and recency, so an old entry becomes visible again at the top.
    pub fn saved_add(
        &self,
        video_id: &str,
        title: &str,
        author: &str,
        thumbnail: &str,
        duration_seconds: u32,
        channel_id: &str,
        saved_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO saved_videos (video_id, title, author, thumbnail, duration_seconds, channel_id, saved_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ON CONFLICT(video_id) DO UPDATE SET
                title            = excluded.title,
                author           = excluded.author,
                thumbnail        = excluded.thumbnail,
                duration_seconds = excluded.duration_seconds,
                channel_id       = excluded.channel_id,
                saved_at         = excluded.saved_at
            "#,
            params![video_id, title, author, thumbnail, duration_seconds, channel_id, saved_at],
        )?;
        Ok(())
    }

    pub fn saved_list(&self, limit: u32) -> Result<Vec<SavedVideoRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT video_id, title, author, thumbnail, duration_seconds, channel_id, saved_at
             FROM saved_videos ORDER BY saved_at DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map(params![limit], |r| {
                Ok(SavedVideoRow {
                    video_id: r.get(0)?,
                    title: r.get(1)?,
                    author: r.get(2)?,
                    thumbnail: r.get(3)?,
                    duration_seconds: r.get::<_, i64>(4)? as u32,
                    channel_id: r.get(5)?,
                    saved_at: r.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Ids only, for the context menu's cheap "already saved?" check.
    pub fn saved_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT video_id FROM saved_videos")?;
        let ids = stmt
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(ids)
    }

    /// Unsave. A video that was never saved is a no-op, never an error.
    pub fn saved_remove(&self, video_id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM saved_videos WHERE video_id = ?1", [video_id])?;
        Ok(())
    }

    // ---- playlists ---------------------------------------------------------------

    pub fn playlist_create(&self, id: &str, title: &str, created_at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO playlists (id, title, created_at) VALUES (?1, ?2, ?3)",
            params![id, title, created_at],
        )?;
        Ok(())
    }

    pub fn playlists(&self) -> Result<Vec<PlaylistRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, created_at FROM playlists ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(PlaylistRow {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    created_at: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    pub fn playlist_delete(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM playlist_items WHERE playlist_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM playlists WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn playlist_add_item(
        &self,
        playlist_id: &str,
        video_id: &str,
        title: &str,
        author: &str,
        thumbnail: &str,
        duration_seconds: u32,
        channel_id: &str,
    ) -> Result<()> {
        // Compute the next position for this playlist.
        let position: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM playlist_items WHERE playlist_id = ?1",
                params![playlist_id],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);

        self.conn.execute(
            r#"
            INSERT OR REPLACE INTO playlist_items
                (playlist_id, video_id, title, author, thumbnail, duration_seconds, channel_id, position)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![playlist_id, video_id, title, author, thumbnail, duration_seconds, channel_id, position],
        )?;
        Ok(())
    }

    pub fn playlist_remove_item(&self, playlist_id: &str, video_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM playlist_items WHERE playlist_id = ?1 AND video_id = ?2",
            params![playlist_id, video_id],
        )?;
        Ok(())
    }

    pub fn playlist_items(&self, playlist_id: &str) -> Result<Vec<PlaylistItemRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT playlist_id, video_id, title, author, thumbnail, duration_seconds, channel_id, position
             FROM playlist_items WHERE playlist_id = ?1 ORDER BY position ASC",
        )?;
        let rows = stmt
            .query_map(params![playlist_id], |r| {
                Ok(PlaylistItemRow {
                    playlist_id: r.get(0)?,
                    video_id: r.get(1)?,
                    title: r.get(2)?,
                    author: r.get(3)?,
                    thumbnail: r.get(4)?,
                    duration_seconds: r.get::<_, i64>(5)? as u32,
                    channel_id: r.get(6)?,
                    position: r.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    // ---- settings ----------------------------------------------------------------

    pub fn setting_set(&self, key: &str, value: &str) -> Result<()> {
        validate_setting(key, value)?;
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn setting_get(&self, key: &str) -> Result<Option<String>> {
        let v = self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |r| r.get::<_, String>(0),
            )
            .optional()?;
        Ok(v)
    }

    /// Delete a stored override so the key falls back to its default.
    ///
    /// The key is validated against `settings_defaults` so a stray call cannot
    /// leave unknown junk behind.
    pub fn setting_reset(&self, key: &str) -> Result<()> {
        if !settings_defaults().contains_key(key) {
            anyhow::bail!("unknown setting: {key}");
        }
        self.conn
            .execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        Ok(())
    }

    /// Every setting the app has a default for: the stored override where one
    /// exists, the documented default where none does.
    ///
    /// Read in one pass and merged here rather than in the caller, so the
    /// "absent means default" rule cannot be applied inconsistently.
    pub fn settings_all(&self) -> Result<HashMap<String, String>> {
        let mut out: HashMap<String, String> = settings_defaults()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        let mut stmt = self.conn.prepare("SELECT key, value FROM settings")?;
        let rows =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (k, v) = row?;
            // Unknown keys and unparseable values are ignored, never surfaced:
            // `settings_defaults` is the contract, and a row left behind by an older
            // build (or edited by hand) must not become a phantom setting in the UI.
            if out.contains_key(&k) && validate_setting(&k, &v).is_ok() {
                out.insert(k, v);
            }
        }
        Ok(out)
    }

    // ---- subscriptions -----------------------------------------------------------

    /// Insert a subscription, or refresh the metadata of an existing one.
    ///
    /// `added_at` is deliberately *not* refreshed on conflict: re-importing a
    /// NewPipe export must not reorder the whole list by pretending every
    /// channel was just subscribed. Unsubscribe-then-resubscribe still gets a
    /// fresh timestamp, because that path deletes the row first.
    pub fn subscription_add(
        &self,
        channel_id: &str,
        title: &str,
        thumbnail: &str,
        subscriber_count: Option<i64>,
        added_at: i64,
    ) -> Result<()> {
        self.conn.execute(
            r#"
            INSERT INTO subscriptions (channel_id, title, thumbnail, subscriber_count, added_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(channel_id) DO UPDATE SET
                title            = excluded.title,
                thumbnail        = excluded.thumbnail,
                subscriber_count = excluded.subscriber_count
            "#,
            params![channel_id, title, thumbnail, subscriber_count, added_at],
        )?;
        Ok(())
    }

    /// Store a subscription's avatar once it has been resolved (spec item 16).
    /// Only ever widens the row: the caller only calls this for a subscription
    /// whose `thumbnail` was empty, so a bad resolve can never overwrite a good
    /// one. `subscriber_count` is deliberately left alone — a backfill knows the
    /// avatar, not the count.
    pub fn subscription_set_thumbnail(&self, channel_id: &str, thumbnail: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE subscriptions SET thumbnail = ?2 WHERE channel_id = ?1",
            params![channel_id, thumbnail],
        )?;
        Ok(())
    }

    pub fn subscription_remove(&self, channel_id: &str) -> Result<()> {
        // Drop the join rows too: they are not covered by a foreign key, and a
        // leftover row would silently re-attach if the user re-subscribes.
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM sub_categories WHERE channel_id = ?1", params![channel_id])?;
        tx.execute("DELETE FROM subscriptions WHERE channel_id = ?1", params![channel_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn subscriptions(&self, category_id: Option<&str>) -> Result<Vec<SubscriptionRow>> {
        // `?1 IS NULL` is the unfiltered case, so one query covers both. The
        // EXISTS sub-select is what makes a subscription show up under several
        // categories at once instead of only the first match.
        let mut stmt = self.conn.prepare(
            "SELECT channel_id, title, thumbnail, subscriber_count, added_at
             FROM subscriptions
             WHERE ?1 IS NULL OR EXISTS (
                 SELECT 1 FROM sub_categories sc
                 WHERE sc.channel_id = subscriptions.channel_id
                   AND sc.category_id = ?1
             )
             ORDER BY added_at DESC",
        )?;
        let mut rows: Vec<SubscriptionRow> = stmt
            .query_map(params![category_id], |r| {
                Ok(SubscriptionRow {
                    channel_id: r.get(0)?,
                    title: r.get(1)?,
                    thumbnail: r.get(2)?,
                    subscriber_count: r.get(3)?,
                    added_at: r.get(4)?,
                    category_ids: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        // One extra query for all the joins rather than a lookup per row.
        let mut by_channel: HashMap<String, Vec<String>> = HashMap::new();
        {
            let mut joins = self.conn.prepare(
                "SELECT sc.channel_id, sc.category_id
                 FROM sub_categories sc
                 JOIN categories c ON c.id = sc.category_id
                 ORDER BY c.position, c.name",
            )?;
            let pairs = joins
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (channel_id, category_id) in pairs {
                by_channel.entry(channel_id).or_default().push(category_id);
            }
        }
        for row in &mut rows {
            if let Some(ids) = by_channel.remove(&row.channel_id) {
                row.category_ids = ids;
            }
        }
        Ok(rows)
    }

    pub fn subscription_exists(&self, channel_id: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM subscriptions WHERE channel_id = ?1",
            params![channel_id],
            |r| r.get(0),
        )?;
        Ok(count > 0)
    }

    // ---- categories --------------------------------------------------------------

    pub fn categories(&self) -> Result<Vec<CategoryRow>> {
        let mut stmt = self.conn.prepare("SELECT id, name, position FROM categories ORDER BY position, name")?;
        let rows = stmt
            .query_map([], |r| {
                Ok(CategoryRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    position: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn category_by_name(&self, name: &str) -> Result<Option<CategoryRow>> {
        Ok(self
            .conn
            .query_row("SELECT id, name, position FROM categories WHERE name = ?1", params![name], |r| {
                Ok(CategoryRow {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    position: r.get(2)?,
                })
            })
            .optional()?)
    }

    fn category_exists(&self, id: &str) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM categories WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )?;
        Ok(count > 0)
    }

    /// Return the category with this name, creating it at the end if needed.
    /// Reports whether it was created, which is what lets an import say "3
    /// categories added" without counting the ones that already existed.
    pub fn category_ensure(&self, name: &str) -> Result<(CategoryRow, bool)> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("category name cannot be empty");
        }
        if let Some(existing) = self.category_by_name(name)? {
            return Ok((existing, false));
        }
        let position = self
            .categories()?
            .iter()
            .map(|c| c.position)
            .max()
            .unwrap_or(0)
            + 1;
        Ok((self.category_add(name, position)?, true))
    }

    /// Create a category, deriving its id from the name. Uniqueness of the name
    /// is checked here rather than left to a `UNIQUE` violation so the caller
    /// gets a sentence instead of a driver error.
    pub fn category_add(&self, name: &str, position: i64) -> Result<CategoryRow> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("category name cannot be empty");
        }
        if self.category_by_name(name)?.is_some() {
            anyhow::bail!("a category named {:?} already exists", name);
        }
        let base = slugify(name);
        let mut id = base.clone();
        let mut n = 2;
        while self.category_exists(&id)? {
            id = format!("{base}-{n}");
            n += 1;
        }
        self.conn.execute(
            "INSERT INTO categories (id, name, position) VALUES (?1, ?2, ?3)",
            params![id, name, position],
        )?;
        Ok(CategoryRow {
            id,
            name: name.to_string(),
            position,
        })
    }

    pub fn category_rename(&self, id: &str, name: &str) -> Result<CategoryRow> {
        let name = name.trim();
        if name.is_empty() {
            anyhow::bail!("category name cannot be empty");
        }
        // Uniqueness is on the name, and the id is deliberately *not* rewritten
        // on rename: join rows already point at it.
        if let Some(other) = self.category_by_name(name)? {
            if other.id != id {
                anyhow::bail!("a category named {:?} already exists", name);
            }
        }
        if !self.category_exists(id)? {
            anyhow::bail!("no category with id {:?}", id);
        }
        let position = self
            .conn
            .query_row("SELECT position FROM categories WHERE id = ?1", params![id], |r| r.get::<_, i64>(0))
            .optional()?
            .unwrap_or(0);
        self.conn.execute(
            "UPDATE categories SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(CategoryRow {
            id: id.to_string(),
            name: name.to_string(),
            position,
        })
    }

    /// Delete a category and unhook it from every subscription, so a channel
    /// that was only in this category simply becomes uncategorised.
    pub fn category_remove(&self, id: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM sub_categories WHERE category_id = ?1", params![id])?;
        tx.execute("DELETE FROM categories WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(())
    }

    /// Replace-all assignment, matching the many-to-many model. Unknown ids are
    /// dropped rather than inserted: a stale id from the UI must not create a
    /// category that was never named. The subscription must exist, so a late
    /// assignment cannot leave an orphaned join row behind.
    pub fn set_sub_categories(&self, channel_id: &str, category_ids: &[String]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM sub_categories WHERE channel_id = ?1", params![channel_id])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO sub_categories (channel_id, category_id)
                 SELECT ?1, id FROM categories
                 WHERE id = ?2 AND EXISTS (SELECT 1 FROM subscriptions WHERE channel_id = ?1)",
            )?;
            for category_id in category_ids {
                stmt.execute(params![channel_id, category_id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

/// Anchor-safe id derived from a category name: `"Tech & News"` → `"tech-news"`.
/// Empty or symbol-only names fall back to a usable slug rather than producing
/// an empty primary key.
fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash {
                out.push('-');
                pending_dash = false;
            }
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() {
            pending_dash = true;
        }
    }
    if out.is_empty() {
        return "category".to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(db: &Db, id: &str) {
        db.subscription_add(id, id, "", None, 1).unwrap();
    }

    #[test]
    fn a_subscription_can_live_in_several_categories() {
        let db = Db::open_in_memory().unwrap();
        sub(&db, "UCa");
        let tech = db.category_add("Tech", 0).unwrap();
        let news = db.category_add("News", 1).unwrap();
        db.set_sub_categories("UCa", &[tech.id.clone(), news.id.clone()])
            .unwrap();

        assert_eq!(db.subscriptions(None).unwrap()[0].category_ids.len(), 2);
        for id in [&tech.id, &news.id] {
            let filtered = db.subscriptions(Some(id)).unwrap();
            assert_eq!(filtered.len(), 1, "must show under {id}");
            assert_eq!(filtered[0].channel_id, "UCa");
        }
    }

    #[test]
    fn set_sub_categories_replaces_rather_than_appends() {
        let db = Db::open_in_memory().unwrap();
        sub(&db, "UCa");
        let a = db.category_add("A", 0).unwrap();
        let b = db.category_add("B", 1).unwrap();
        db.set_sub_categories("UCa", &[a.id.clone(), b.id.clone()]).unwrap();
        db.set_sub_categories("UCa", &[b.id.clone()]).unwrap();
        assert_eq!(db.subscriptions(None).unwrap()[0].category_ids, vec![b.id]);
    }

    /// A category id the UI still holds after the category was deleted must not
    /// be resurrected as an unnamed row by a late assignment.
    #[test]
    fn unknown_category_ids_are_dropped() {
        let db = Db::open_in_memory().unwrap();
        sub(&db, "UCa");
        let a = db.category_add("A", 0).unwrap();
        db.set_sub_categories("UCa", &[a.id.clone(), "ghost".into()])
            .unwrap();
        assert_eq!(db.subscriptions(None).unwrap()[0].category_ids, vec![a.id]);
    }

    #[test]
    fn deleting_a_category_never_deletes_a_subscription() {
        let db = Db::open_in_memory().unwrap();
        sub(&db, "UCa");
        let gone = db.category_add("Gone", 0).unwrap();
        let kept = db.category_add("Kept", 1).unwrap();
        db.set_sub_categories("UCa", &[gone.id.clone(), kept.id.clone()])
            .unwrap();

        db.category_remove(&gone.id).unwrap();

        let subs = db.subscriptions(None).unwrap();
        assert_eq!(subs.len(), 1, "the subscription must survive");
        assert_eq!(subs[0].category_ids, vec![kept.id]);
        assert!(db.categories().unwrap().iter().all(|c| c.id != gone.id));
    }

    #[test]
    fn unsubscribing_also_unhooks_the_categories() {
        let db = Db::open_in_memory().unwrap();
        sub(&db, "UCa");
        let a = db.category_add("A", 0).unwrap();
        db.set_sub_categories("UCa", &[a.id.clone()]).unwrap();
        db.subscription_remove("UCa").unwrap();
        // Re-subscribing must not resurrect the previous assignment.
        sub(&db, "UCa");
        assert!(db.subscriptions(None).unwrap()[0].category_ids.is_empty());
    }

    #[test]
    fn a_category_cannot_be_assigned_to_a_channel_that_is_not_subscribed() {
        let db = Db::open_in_memory().unwrap();
        let a = db.category_add("A", 0).unwrap();
        db.set_sub_categories("UCghost", &[a.id.clone()]).unwrap();
        sub(&db, "UCa");
        assert!(
            db.subscriptions(None).unwrap()[0].category_ids.is_empty(),
            "no orphaned assignment may survive a later subscribe"
        );
    }

    #[test]
    fn renaming_a_missing_category_is_an_error() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.category_rename("ghost", "name").is_err());
    }

    #[test]
    fn an_avatar_backfill_fills_a_blank_thumbnail() {
        let db = Db::open_in_memory().unwrap();
        db.subscription_add("UCa", "A", "", None, 1).unwrap();
        db.subscription_set_thumbnail("UCa", "https://yt3.example/a").unwrap();
        assert_eq!(
            db.subscriptions(None).unwrap()[0].thumbnail,
            "https://yt3.example/a"
        );
        // A channel with no row is a no-op, not an error.
        assert!(db.subscription_set_thumbnail("UCmissing", "x").is_ok());
    }

    #[test]
    fn category_ensure_reuses_an_existing_name() {
        let db = Db::open_in_memory().unwrap();
        let (first, created) = db.category_ensure("Music").unwrap();
        assert!(created);
        let (again, created_again) = db.category_ensure("Music").unwrap();
        assert!(!created_again, "a second import must not duplicate it");
        assert_eq!(first.id, again.id);
        assert_eq!(db.categories().unwrap().len(), 1);
        assert!(db.category_ensure("  ").is_err());
    }

    /// Re-importing must refresh names without reshuffling the whole list.
    #[test]
    fn re_adding_a_subscription_keeps_its_original_added_at() {
        let db = Db::open_in_memory().unwrap();
        db.subscription_add("UCa", "Old Name", "", None, 100).unwrap();
        db.subscription_add("UCa", "New Name", "thumb", None, 999).unwrap();
        let subs = db.subscriptions(None).unwrap();
        assert_eq!(subs[0].title, "New Name", "metadata is refreshed");
        assert_eq!(subs[0].thumbnail, "thumb");
        assert_eq!(subs[0].added_at, 100, "but the order is not");
    }

    #[test]
    fn category_names_are_unique_and_ids_are_slugified() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.category_add("Tech & News", 0).unwrap().id, "tech-news");
        assert!(db.category_add("Tech & News", 1).is_err(), "duplicate name");
        assert!(db.category_add("   ", 2).is_err(), "empty name");
        // "A/B" and "A B" slug identically, so the second must get a distinct id.
        let a = db.category_add("A/B", 3).unwrap();
        let b = db.category_add("A B", 4).unwrap();
        assert_ne!(a.id, b.id);
    }

    /// The id is the join key, so a rename must leave it alone.
    #[test]
    fn renaming_keeps_the_id_and_still_rejects_duplicates() {
        let db = Db::open_in_memory().unwrap();
        let a = db.category_add("Alpha", 0).unwrap();
        let b = db.category_add("Beta", 1).unwrap();
        let renamed = db.category_rename(&a.id, "Gamma").unwrap();
        assert_eq!(renamed.id, a.id);
        assert_eq!(renamed.name, "Gamma");
        assert!(db.category_rename(&b.id, "Gamma").is_err());
        assert!(
            db.category_rename(&a.id, "Gamma").is_ok(),
            "renaming to its own name is a no-op"
        );
    }

    // ---- settings ---------------------------------------------------------------

    #[test]
    fn settings_all_is_pure_defaults_when_nothing_is_stored() {
        let db = Db::open_in_memory().unwrap();
        let all = db.settings_all().unwrap();
        assert_eq!(all.get("player.default_resolution"), Some(&"1080".into()));
        assert_eq!(all.get("ui.theme"), Some(&"system".into()));
        assert_eq!(
            all.len(),
            settings_defaults().len(),
            "nothing stored, so every key must come back at its default"
        );
    }

    #[test]
    fn a_stored_override_wins_and_reset_restores_the_default() {
        let db = Db::open_in_memory().unwrap();
        db.setting_set("player.default_resolution", "720").unwrap();
        assert_eq!(
            db.settings_all().unwrap().get("player.default_resolution"),
            Some(&"720".into())
        );

        db.setting_reset("player.default_resolution").unwrap();
        assert_eq!(
            db.settings_all().unwrap().get("player.default_resolution"),
            Some(&"1080".into()),
            "reset must fall back to the documented default, not delete the key"
        );
    }

    #[test]
    fn unknown_keys_and_malformed_values_are_rejected() {
        let db = Db::open_in_memory().unwrap();
        assert!(db.setting_set("player.nonexistent", "x").is_err());
        assert!(db.setting_set("player.default_resolution", "banana").is_err());
        assert!(db.setting_set("player.default_volume", "250").is_err());
        assert!(db.setting_set("ui.theme", "chartreuse").is_err());
        assert!(db.setting_set("captions.font", &"a".repeat(201)).is_err());
        assert!(db.setting_set("captions.font", "Georgia,\nserif").is_err());
        assert!(db.setting_reset("ui.nonexistent").is_err());

        // Valid values all round-trip.
        for (k, v) in [
            ("player.default_resolution", "0"),
            ("player.default_resolution", "2160"),
            ("player.default_volume", "0"),
            ("playback.hd_mode", "always_360p"),
            ("ui.theme", "dark"),
            ("ui.compact_sidebar", "true"),
            ("captions.font", ""),
            ("captions.font", "\"Fira Sans\", Georgia, serif"),
        ] {
            db.setting_set(k, v).unwrap_or_else(|e| panic!("{k}={v} should be valid: {e}"));
            assert_eq!(db.settings_all().unwrap().get(k).unwrap(), v);
        }
    }

    /// A row that was written by an older build (or by hand) must not surface.
    #[test]
    fn a_corrupt_stored_value_falls_back_to_the_default() {
        let db = Db::open_in_memory().unwrap();
        db.conn
            .execute(
                "INSERT INTO settings (key, value) VALUES ('ui.theme', 'chartreuse')",
                [],
            )
            .unwrap();
        assert_eq!(db.settings_all().unwrap().get("ui.theme"), Some(&"system".into()));
    }

    #[test]
    fn shortcut_bindings_round_trip_and_the_impossible_ones_are_rejected() {
        let db = Db::open_in_memory().unwrap();
        let set = |action: &str, value: &str| {
            db.setting_set(&format!("player.shortcuts.{action}"), value)
        };
        let get = |action: &str| {
            db.settings_all()
                .unwrap()
                .get(&format!("player.shortcuts.{action}"))
                .cloned()
        };

        // Every action ships with an empty alternative, and reset restores it.
        for action in SHORTCUT_ACTIONS {
            assert_eq!(get(action), Some(String::new()), "{action} needs a default row");
            set(action, "Ctrl+Alt+j").unwrap();
            assert_eq!(get(action).as_deref(), Some("Ctrl+Alt+j"));
            db.setting_reset(&format!("player.shortcuts.{action}")).unwrap();
            assert_eq!(get(action), Some(String::new()));
        }

        // Keys the player matches itself, and shapes it could never resolve.
        for refused in [
            "Space", " ", ",", ".", "<", ">", "0", "5", "Shift+5", "Ctrl+0", "Enter",
            "Ctrl+Shift+Ctrl+x", "Shift+Ctrl+x", "x+y+z", "+",
        ] {
            assert!(set("playpause", refused).is_err(), "{refused:?} must be refused");
        }
        // An action the player does not have, spelled like a real one.
        assert!(db.setting_set("player.shortcuts.skip", "x").is_err());
        assert!(db.setting_set("player.shortcuts.", "x").is_err());

        // Shapes that are worth accepting.
        for accepted in ["x", "Z", "ArrowUp", "Home", "Alt+Shift+m", "Ctrl+Alt+j", "Ctrl+Shift+j"] {
            set("fullscreen", accepted)
                .unwrap_or_else(|e| panic!("{accepted:?} should be valid: {e}"));
        }
    }

    /// "Remove from history" must take the one row out and leave the rest, and
    /// must not complain about a video that is not in history at all.
    #[test]
    fn removing_history_touches_only_the_asked_for_row() {
        let db = Db::open_in_memory().unwrap();
        for id in ["vidA", "vidB"] {
            db.upsert_history(id, id, "author", "thumb", 100, "UCh", 1, 50.0).unwrap();
        }

        db.remove_history("vidA").unwrap();

        let left = db.history(10).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].video_id, "vidB");
        assert!(db.get_history("vidA").unwrap().is_none());
        // Idempotent: a second removal of the same id is still fine.
        db.remove_history("vidA").unwrap();
    }

    /// "Save to library" must upsert (never duplicate) and re-ordering on a
    /// re-save, and unsave must be an idempotent no-op.
    #[test]
    fn saving_a_video_upserts_and_unsaving_drops_it() {
        let db = Db::open_in_memory().unwrap();
        db.saved_add("v1", "One", "A", "", 100, "UCh1", 1).unwrap();
        db.saved_add("v2", "Two", "B", "", 200, "UCh2", 2).unwrap();

        let list = db.saved_list(10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].video_id, "v2", "newest first");
        assert_eq!(list[1].channel_id, "UCh1");

        // Re-saving an existing entry refreshes metadata and recency without
        // duplicating the row.
        db.saved_add("v1", "One again", "A", "", 100, "UCh1", 3).unwrap();
        let list = db.saved_list(10).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].video_id, "v1");
        assert_eq!(list[0].title, "One again");
        assert_eq!(db.saved_ids().unwrap(), vec!["v1", "v2"]);

        db.saved_remove("v1").unwrap();
        assert!(db.saved_list(10).unwrap().iter().all(|s| s.video_id != "v1"));
        // Unsave of a never-saved video is a no-op, not an error.
        db.saved_remove("ghost").unwrap();
    }
}
