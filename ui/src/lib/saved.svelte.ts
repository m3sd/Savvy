// Saved-videos store ("save to library", spec item 11).
//
// The backend table is the source of truth; this module only mirrors which ids
// are saved so the context menu can toggle and the Library page reacts without
// a round trip. Plain string[] instead of a Set because the rune proxy does
// not observe Set.add/delete.

import { api } from '$lib/api';
import type { MenuTarget } from '$lib/contextmenu.svelte';

export const savedVideoIds = $state<string[]>([]);

export const SAVED_CHANGED = 'savvy:saved-changed';

function notifySavedChanged(): void {
    window.dispatchEvent(new CustomEvent(SAVED_CHANGED));
}

function setSaved(videoId: string, saved: boolean): void {
    const i = savedVideoIds.indexOf(videoId);
    if (saved && i === -1) savedVideoIds.push(videoId);
    if (!saved && i !== -1) savedVideoIds.splice(i, 1);
}

/** Mirror the saved set once per app session (ids are cheap to list). */
let loaded = false;
export async function loadSavedIds(): Promise<void> {
    if (loaded) return;
    loaded = true;
    try {
        const ids = await api.savedIds();
        savedVideoIds.splice(0, savedVideoIds.length, ...ids);
    } catch (e) {
        // Non-fatal: the menu will just not know until a save happens. Saving
        // still works; the flag is reset so the next call retries.
        console.warn('saved_ids failed', e);
        loaded = false;
    }
}

export function isSaved(videoId: string): boolean {
    return savedVideoIds.includes(videoId);
}

/** Save or unsave a video; returns whether it is now saved. */
export async function toggleSaved(target: MenuTarget): Promise<boolean> {
    const next = !isSaved(target.videoId);
    if (next) {
        await api.savedAdd({
            videoId: target.videoId,
            title: target.title,
            author: target.author,
            thumbnail: target.thumbnail,
            durationSeconds: target.durationSeconds,
            channelId: target.channelId ?? ''
        });
    } else {
        await api.savedRemove(target.videoId);
    }
    setSaved(target.videoId, next);
    notifySavedChanged();
    return next;
}