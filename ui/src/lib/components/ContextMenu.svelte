<script lang="ts">
    import { HugeiconsIcon } from '@hugeicons/svelte';
    import {
        Add01Icon,
        Bookmark01Icon,
        CheckIcon,
        CheckmarkCircle01Icon,
        ChevronRightIcon,
        Delete02Icon,
        ExternalLinkIcon,
        Link01Icon,
        Playlist01Icon,
        UserIcon
    } from '@hugeicons/core-free-icons';
    import { goto } from '$app/navigation';
    import { writeText } from '@tauri-apps/plugin-clipboard-manager';
    import { api, isTauri, type PlaylistRow } from '$lib/api';
    import { closeMenu, menu, notifyHistoryChanged } from '$lib/contextmenu.svelte';
    import { isSaved, loadSavedIds, toggleSaved } from '$lib/saved.svelte';

    let root: HTMLDivElement | undefined = $state();
    let playlistOpen = $state(false);
    let playlists = $state<PlaylistRow[] | null>(null);
    let newPlaylistOpen = $state(false);
    let newPlaylistTitle = $state('');
    let newPlaylistInput: HTMLInputElement | undefined = $state();
    let notice = $state<string | null>(null);
    let busy = $state(false);
    let noticeTimer: ReturnType<typeof setTimeout> | undefined;

    const target = $derived(menu.target);
    const isPlayer = $derived(menu.surface === 'player');
    const url = $derived(target ? `https://www.youtube.com/watch?v=${target.videoId}` : '');
    /** Near the right edge the playlist list opens leftwards instead. */
    const submenuLeft = $derived(
        typeof window !== 'undefined' && menu.x > window.innerWidth - 300
    );

    function flash(message: string) {
        notice = message;
        if (noticeTimer) clearTimeout(noticeTimer);
        noticeTimer = setTimeout(() => (notice = null), 2600);
    }

    async function copyLink() {
        if (!target) return;
        try {
            if (isTauri()) {
                await writeText(url);
            } else {
                await navigator.clipboard.writeText(url);
            }
            flash('Link copied');
        } catch (e) {
            flash(`Could not copy: ${String(e)}`);
        }
        closeMenu();
    }

    async function openExternally() {
        if (!target) return;
        try {
            await api.openExternal(url);
        } catch (e) {
            flash(`Could not open a browser: ${String(e)}`);
        }
        closeMenu();
    }

    function goToChannel() {
        if (!target?.channelId) return;
        closeMenu();
        goto(`/channel/${target.channelId}`);
    }

    async function markAsWatched() {
        const t = target;
        if (!t || !t.durationSeconds || busy) return;
        busy = true;
        try {
            // Create the row if it does not exist yet, then land the playhead
            // on the tail: `stored_progress` stores that as 0, which is this
            // app's "finished" - in history, out of the resume list.
            await api.historyAdd({
                videoId: t.videoId,
                title: t.title,
                author: t.author,
                thumbnail: t.thumbnail,
                durationSeconds: t.durationSeconds,
                channelId: t.channelId ?? '',
                progressSeconds: t.durationSeconds
            });
            await api.historyProgress({
                videoId: t.videoId,
                positionSeconds: t.durationSeconds,
                durationSeconds: t.durationSeconds
            });
            notifyHistoryChanged();
            flash('Marked as watched');
        } catch (e) {
            flash(`Could not update history: ${String(e)}`);
        } finally {
            busy = false;
            closeMenu();
        }
    }

    async function removeFromHistory() {
        const t = target;
        if (!t || busy) return;
        busy = true;
        try {
            await api.historyRemove(t.videoId);
            notifyHistoryChanged();
            flash('Removed from history');
        } catch (e) {
            flash(`Could not remove: ${String(e)}`);
        } finally {
            busy = false;
            closeMenu();
        }
    }

    async function toggleLibrary() {
        const t = target;
        if (!t || busy) return;
        busy = true;
        try {
            const nowSaved = await toggleSaved(t);
            flash(nowSaved ? 'Saved to library' : 'Removed from library');
        } catch (e) {
            flash(`Could not save: ${String(e)}`);
        } finally {
            busy = false;
            closeMenu();
        }
    }

    function openPlaylists() {
        // Only a fresh open resets the inline "new playlist" field; re-hovering
        // the row while it is already expanded must not wipe what was typed.
        if (!playlistOpen) {
            newPlaylistOpen = false;
            newPlaylistTitle = '';
        }
        playlistOpen = true;
        // Lazy: most right-clicks never reach for this, so the list is fetched
        // the first time the submenu is shown rather than per menu open.
        if (playlists === null) {
            api.playlistsList()
                .then((rows) => (playlists = rows))
                .catch((e) => {
                    playlists = [];
                    flash(`Could not list playlists: ${String(e)}`);
                });
        }
    }

    /** Id shape must match `newLocalId` on the Playlists page. */
    function newLocalId() {
        return 'pl_' + Math.random().toString(36).slice(2, 10);
    }

    /** Create a playlist from the submenu and drop the current video in it. */
    async function createPlaylistAndAdd() {
        const t = target;
        const title = newPlaylistTitle.trim();
        if (!t || !title || busy) return;
        busy = true;
        try {
            const id = newLocalId();
            await api.playlistCreate(id, title);
            await api.playlistAddItem({
                playlistId: id,
                videoId: t.videoId,
                title: t.title,
                author: t.author,
                thumbnail: t.thumbnail,
                durationSeconds: t.durationSeconds,
                channelId: t.channelId ?? ''
            });
            playlists = [
                { id, title, createdAt: Math.floor(Date.now() / 1000) },
                ...(playlists ?? [])
            ];
            newPlaylistTitle = '';
            newPlaylistOpen = false;
            flash(`Added to ${title}`);
        } catch (e) {
            flash(`Could not create playlist: ${String(e)}`);
        } finally {
            busy = false;
            closeMenu();
        }
    }

    async function addToPlaylist(playlist: PlaylistRow) {
        const t = target;
        if (!t || busy) return;
        busy = true;
        try {
            await api.playlistAddItem({
                playlistId: playlist.id,
                videoId: t.videoId,
                title: t.title,
                author: t.author,
                thumbnail: t.thumbnail,
                durationSeconds: t.durationSeconds,
                channelId: t.channelId ?? ''
            });
            flash(`Added to ${playlist.title}`);
        } catch (e) {
            flash(`Could not add: ${String(e)}`);
        } finally {
            busy = false;
            closeMenu();
        }
    }

    // Re-open: the submenu must start collapsed, or a previous click leaks its
    // state into the next target. This also warms the saved-ids mirror (once)
    // so "Save to library / Saved" reflects reality on the first right-click.
    $effect(() => {
        if (menu.open) {
            playlistOpen = false;
            void loadSavedIds();
        }
    });

    // Clamp inside the viewport once the menu is laid out, then keep the
    // anchor in sync if the same menu is re-opened somewhere else.
    $effect(() => {
        const el = root;
        if (!menu.open || !el) return;
        const { x, y } = menu;
        const pad = 8;
        const left = Math.max(pad, Math.min(x, window.innerWidth - el.offsetWidth - pad));
        const top = Math.max(pad, Math.min(y, window.innerHeight - el.offsetHeight - pad));
        el.style.left = `${left}px`;
        el.style.top = `${top}px`;
    });

    // Fresh menu: start collapsed (a leftover `playlistOpen` would otherwise
    // pop the submenu with no hover) and clear the inline new-playlist field.
    $effect(() => {
        if (!menu.open) return;
        playlistOpen = false;
        newPlaylistOpen = false;
        newPlaylistTitle = '';
    });

    // Land the caret in the inline field the moment it is revealed.
    $effect(() => {
        if (newPlaylistOpen) newPlaylistInput?.focus();
    });

    // Click-away and Escape, in the capture phase so a card's own pointer
    // handling cannot swallow the dismissal.
    $effect(() => {
        if (!menu.open) return;
        const onPointerDown = (e: PointerEvent) => {
            if (root && e.target instanceof Node && root.contains(e.target)) return;
            closeMenu();
        };
        const onKeyDown = (e: KeyboardEvent) => {
            if (e.key === 'Escape') closeMenu();
        };
        document.addEventListener('pointerdown', onPointerDown, true);
        document.addEventListener('keydown', onKeyDown, true);
        return () => {
            document.removeEventListener('pointerdown', onPointerDown, true);
            document.removeEventListener('keydown', onKeyDown, true);
        };
    });

    const itemClass =
        'flex w-full items-center gap-2.5 px-3 py-1.5 text-left text-sm transition-colors hover:bg-muted hover:text-foreground focus-visible:bg-muted focus-visible:outline-none disabled:opacity-50';
</script>

{#if menu.open && target}
    <div
        bind:this={root}
        role="menu"
        class="fixed z-[200] min-w-56 rounded-lg border bg-popover py-1 text-popover-foreground shadow-lg"
        style="left: {menu.x}px; top: {menu.y}px"
    >
        {#if isSaved(target.videoId)}
            <button
                type="button"
                role="menuitem"
                class={itemClass}
                disabled={busy}
                onmouseenter={() => (playlistOpen = false)}
                onclick={toggleLibrary}
            >
                <HugeiconsIcon icon={CheckIcon} strokeWidth={1.8} class="h-4 w-4 shrink-0" />
                <span class="flex-1">Saved</span>
            </button>
        {:else}
            <button
                type="button"
                role="menuitem"
                class={itemClass}
                disabled={busy}
                onmouseenter={() => (playlistOpen = false)}
                onclick={toggleLibrary}
            >
                <HugeiconsIcon icon={Bookmark01Icon} strokeWidth={1.8} class="h-4 w-4 shrink-0" />
                <span class="flex-1">Save to library</span>
            </button>
        {/if}

        <div class="relative">
            <button
                type="button"
                role="menuitem"
                class={itemClass}
                aria-haspopup="menu"
                aria-expanded={playlistOpen}
                onmouseenter={openPlaylists}
                onclick={openPlaylists}
            >
                <HugeiconsIcon icon={Playlist01Icon} strokeWidth={1.8} class="h-4 w-4 shrink-0" />
                <span class="flex-1">Add to playlist</span>
                <HugeiconsIcon icon={ChevronRightIcon} strokeWidth={1.8} class="h-3.5 w-3.5" />
            </button>

            {#if playlistOpen}
                <div
                    role="menu"
                    class="absolute top-[-5px] z-10 min-w-52 rounded-lg border bg-popover py-1 shadow-lg {submenuLeft
                        ? 'right-full mr-1'
                        : 'left-full ml-1'}"
                >
                    {#if newPlaylistOpen}
                        <form
                            class="flex items-center gap-1 px-2 py-1"
                            onsubmit={(e) => {
                                e.preventDefault();
                                createPlaylistAndAdd();
                            }}
                        >
                            <input
                                bind:this={newPlaylistInput}
                                bind:value={newPlaylistTitle}
                                type="text"
                                placeholder="Playlist name"
                                class="h-7 w-full min-w-0 rounded-md border bg-card px-2 text-sm outline-hidden focus:ring-1 focus:ring-ring"
                            />
                            <button
                                type="submit"
                                disabled={busy || !newPlaylistTitle.trim()}
                                class="shrink-0 rounded-md bg-primary px-2 py-1 text-xs font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
                            >
                                Add
                            </button>
                        </form>
                    {:else}
                        <button
                            type="button"
                            role="menuitem"
                            class={itemClass}
                            disabled={busy}
                            onclick={() => (newPlaylistOpen = true)}
                        >
                            <HugeiconsIcon
                                icon={Add01Icon}
                                strokeWidth={1.8}
                                class="h-4 w-4 shrink-0"
                            />
                            New playlist
                        </button>
                        {#if playlists === null}
                            <p class="px-3 py-1.5 text-sm text-muted-foreground">Loading…</p>
                        {:else if playlists.length === 0}
                            <p class="px-3 py-1.5 text-sm text-muted-foreground">
                                No playlists yet.
                            </p>
                        {:else}
                            {#each playlists as playlist (playlist.id)}
                                <button
                                    type="button"
                                    role="menuitem"
                                    class="{itemClass} line-clamp-1"
                                    disabled={busy}
                                    onclick={() => addToPlaylist(playlist)}
                                >
                                    {playlist.title}
                                </button>
                            {/each}
                        {/if}
                    {/if}
                </div>
            {/if}
        </div>

        <div class="my-1 border-t"></div>

        <button
            type="button"
            role="menuitem"
            class={itemClass}
            onmouseenter={() => (playlistOpen = false)}
            onclick={copyLink}
        >
            <HugeiconsIcon icon={Link01Icon} strokeWidth={1.8} class="h-4 w-4 shrink-0" />
            Copy link
        </button>
        <button
            type="button"
            role="menuitem"
            class={itemClass}
            onmouseenter={() => (playlistOpen = false)}
            onclick={openExternally}
        >
            <HugeiconsIcon icon={ExternalLinkIcon} strokeWidth={1.8} class="h-4 w-4 shrink-0" />
            Open in browser
        </button>

        {#if target.channelId}
            <button
                type="button"
                role="menuitem"
                class={itemClass}
                onmouseenter={() => (playlistOpen = false)}
                onclick={goToChannel}
            >
                <HugeiconsIcon icon={UserIcon} strokeWidth={1.8} class="h-4 w-4 shrink-0" />
                Go to channel
            </button>
        {/if}

        {#if !isPlayer}
            <div class="my-1 border-t"></div>
            {#if target.durationSeconds > 0}
                <button
                    type="button"
                    role="menuitem"
                    class={itemClass}
                    disabled={busy}
                    onmouseenter={() => (playlistOpen = false)}
                    onclick={markAsWatched}
                >
                    <HugeiconsIcon
                        icon={CheckmarkCircle01Icon}
                        strokeWidth={1.8}
                        class="h-4 w-4 shrink-0"
                    />
                    Mark as watched
                </button>
            {/if}
            <button
                type="button"
                role="menuitem"
                class={itemClass}
                disabled={busy}
                onmouseenter={() => (playlistOpen = false)}
                onclick={removeFromHistory}
            >
                <HugeiconsIcon icon={Delete02Icon} strokeWidth={1.8} class="h-4 w-4 shrink-0" />
                Remove from history
            </button>
        {/if}
    </div>
{/if}

{#if notice}
    <div
        class="fixed bottom-6 left-1/2 z-[210] -translate-x-1/2 rounded-lg border bg-card px-4 py-2 text-sm text-foreground shadow-lg"
    >
        {notice}
    </div>
{/if}
