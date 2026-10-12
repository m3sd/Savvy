// Context-menu store (spec item 11).
//
// Every surface that can be right-clicked — feed, search, channel, history,
// playlists, related cards, the player — reads from this one store, so the
// action set cannot drift per page. The cards do not call it themselves: they
// publish their payload as `data-*` attributes and a single `contextmenu`
// listener in the layout opens the menu. That is also what keeps a right-click
// working when a card is a plain `<button>` here and a row somewhere else.

export interface MenuTarget {
    videoId: string;
    title: string;
    author: string;
    thumbnail: string;
    /** Seconds; 0 when the card does not carry a duration. */
    durationSeconds: number;
    /** Absent on history and playlist rows, which only store the author name. */
    channelId?: string;
}

export type MenuSurface = 'card' | 'player';

interface MenuState {
    open: boolean;
    /** Cursor position, in viewport coordinates (the menu is `position: fixed`). */
    x: number;
    y: number;
    surface: MenuSurface;
    target: MenuTarget | null;
}

export const menu = $state<MenuState>({
    open: false,
    x: 0,
    y: 0,
    surface: 'card',
    target: null
});

/** Parse the payload a surface published through its `data-*` attributes. */
export function targetFromElement(el: HTMLElement): MenuTarget | null {
    const videoId = el.dataset.videoId;
    if (!videoId) return null;
    return {
        videoId,
        title: el.dataset.title ?? '',
        author: el.dataset.author ?? '',
        thumbnail: el.dataset.thumbnail ?? '',
        durationSeconds: Number(el.dataset.duration ?? 0) || 0,
        channelId: el.dataset.channelId || undefined
    };
}

export function openMenu(
    x: number,
    y: number,
    surface: MenuSurface,
    target: MenuTarget
): void {
    menu.target = target;
    menu.surface = surface;
    menu.x = x;
    menu.y = y;
    menu.open = true;
}

export function closeMenu(): void {
    menu.open = false;
}

/** Watch pages that show history refresh themselves when this fires. */
export const HISTORY_CHANGED = 'savvy:history-changed';

export function notifyHistoryChanged(): void {
    window.dispatchEvent(new CustomEvent(HISTORY_CHANGED));
}
