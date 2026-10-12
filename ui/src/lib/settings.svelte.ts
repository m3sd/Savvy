// Settings store.
//
// `settings_defaults()` in `src-tauri/src/db.rs` is the single authority for
// what a setting is when the user has never touched it. This module only
// resolves the stored overrides over that, once, and hands them back out
// reactively so a change on the settings page applies without a reload.
//
// Nothing here reaches the network directly — every read and write goes through
// the Tauri commands, and the table is local SQLite.

import { api, isTauri } from '$lib/api';
import { resetMode, setMode } from 'mode-watcher';

/**
 * Mirrors `settings_defaults()` in `db.rs`.
 *
 * This exists *only* to cover the frame between mounting and `settingsAll`
 * resolving — `db.rs` remains the real default. Keep it in step with that map;
 * if they ever disagree, the backend wins on the next read.
 */
const JS_DEFAULTS: Record<string, string> = {
    'player.default_resolution': '1080',
    'player.default_volume': '100',
    'playback.hd_mode': 'auto',
    'ui.theme': 'system',
    'ui.compact_sidebar': 'false',
    'player.theater': 'false',
    'captions.font': '',
    'data.clear_history_on_start': 'false',
    'sponsorblock.enabled': 'false',
    'sponsorblock.mode': 'auto',
    'sponsorblock.categories': 'sponsor,selfpromo,interaction,intro,outro,preview',
    // Alternative key bindings; '' means the default key is the only one.
    'player.shortcuts.playpause': '',
    'player.shortcuts.back10': '',
    'player.shortcuts.forward10': '',
    'player.shortcuts.back5': '',
    'player.shortcuts.forward5': '',
    'player.shortcuts.volumeUp': '',
    'player.shortcuts.volumeDown': '',
    'player.shortcuts.mute': '',
    'player.shortcuts.fullscreen': '',
    'player.shortcuts.theater': '',
    'player.shortcuts.captions': ''
};

/** The resolved values. Deeply reactive, so consumers update on change. */
export const settings = $state<Record<string, string>>({ ...JS_DEFAULTS });

let inFlight: Promise<void> | null = null;

/**
 * Fetch every setting once.
 *
 * Callers share a single in-flight request, so the layout, the settings page
 * and the watch page can all await it at startup without tripling round trips.
 * A failure leaves the defaults in place rather than blanking the UI.
 */
export function loadSettings(): Promise<void> {
    if (inFlight) return inFlight;
    if (!isTauri()) return Promise.resolve();
    inFlight = api
        .settingsAll()
        .then((values) => {
            for (const [k, v] of Object.entries(values)) settings[k] = v;
        })
        .catch((e) => {
            console.warn('settings_all failed', e);
        })
        .finally(() => {
            inFlight = null;
        });
    return inFlight;
}

/** Write through to SQLite. Optimistic: the UI updates before the round trip. */
export function setSetting(key: string, value: string): void {
    settings[key] = value;
    if (!isTauri()) return;
    void api.settingSet(key, value).catch((e) => {
        console.warn('setting_set failed', e);
    });
}

/** Revert to the documented default, locally and in the table. */
export function resetSetting(key: string): void {
    const fallback = JS_DEFAULTS[key];
    if (fallback !== undefined) settings[key] = fallback;
    if (!isTauri()) return;
    void api.settingReset(key).catch((e) => {
        console.warn('setting_reset failed', e);
    });
}

export function settingStr(key: string): string {
    return settings[key] ?? JS_DEFAULTS[key] ?? '';
}

export function settingBool(key: string): boolean {
    return settingStr(key) === 'true';
}

export function settingNum(key: string): number {
    const n = Number(settingStr(key));
    return Number.isFinite(n) ? n : 0;
}

/**
 * Push `ui.theme` into `mode-watcher`.
 *
 * Called once the settings have loaded at startup and again whenever the user
 * changes it, so both paths go through the same mapping and cannot drift.
 * `system` maps to `resetMode` rather than an explicit mode, which is what makes
 * it follow OS changes later rather than freezing the mode chosen at launch.
 */
export function applyThemeSetting(): void {
    const value = settingStr('ui.theme');
    if (value === 'light' || value === 'dark') setMode(value);
    else resetMode();
}
