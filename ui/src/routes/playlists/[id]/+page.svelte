<script lang="ts">
    import { HugeiconsIcon } from '@hugeicons/svelte';
    import { ComputerIcon, Delete02Icon, PlayCircleIcon, ArrowLeft01Icon } from '@hugeicons/core-free-icons';
    import { page } from '$app/state';
    import { onMount } from 'svelte';
    import { api, type PlaylistItemRow } from '$lib/api';
    import { goto } from '$app/navigation';
    import ConfirmDialog from '$lib/components/ConfirmDialog.svelte';

    const playlistId = $derived(page.params.id ?? '');
    let items = $state<PlaylistItemRow[]>([]);
    let loading = $state(true);
    let title = $state('');
    let confirmOpen = $state(false);

    async function load() {
        loading = true;
        try {
            if (!playlistId) {
                items = [];
                title = 'Playlist';
                return;
            }
            const list = await api.playlistsList();
            const pl = list.find((p) => p.id === playlistId);
            title = pl?.title ?? 'Playlist';
            items = await api.playlistItems(playlistId);
        } finally {
            loading = false;
        }
    }

    onMount(load);

    function play(videoId: string) {
        goto(`/watch/${videoId}`);
    }

    async function remove(videoId: string) {
        await api.playlistRemoveItem(playlistId, videoId);
        await load();
    }

    async function deletePlaylist() {
        await api.playlistDelete(playlistId);
        goto('/playlists');
    }

    function formatDuration(s: number): string {
        const h = Math.floor(s / 3600);
        const m = Math.floor((s % 3600) / 60);
        const sec = s % 60;
        return h > 0
            ? `${h}:${String(m).padStart(2, '0')}:${String(sec).padStart(2, '0')}`
            : `${m}:${String(sec).padStart(2, '0')}`;
    }
</script>

<div class="flex flex-col gap-4 p-6">
    <button
        type="button"
        onclick={() => goto('/playlists')}
        class="flex items-center gap-2 self-start rounded-md px-2 py-1 text-sm text-muted-foreground hover:bg-muted hover:text-foreground"
    >
        <HugeiconsIcon icon={ArrowLeft01Icon} strokeWidth={2} class="h-4 w-4" />
        Playlists
    </button>

    <div class="flex items-center gap-3">
        <div
            class="flex h-12 w-12 items-center justify-center rounded-md bg-primary/10 text-primary"
        >
            <HugeiconsIcon icon={ComputerIcon} strokeWidth={2} class="h-6 w-6" />
        </div>
        <h1 class="font-heading text-2xl font-bold tracking-tight">{title}</h1>
        <button
            type="button"
            aria-label="Delete playlist"
            title="Delete playlist"
            onclick={() => (confirmOpen = true)}
            class="ml-auto rounded-md p-2 text-muted-foreground transition-colors hover:bg-destructive hover:text-destructive-foreground"
        >
            <HugeiconsIcon icon={Delete02Icon} strokeWidth={2} class="h-4 w-4" />
        </button>
    </div>

    {#if loading}
        <p class="text-sm text-muted-foreground">Loading…</p>
    {:else if items.length === 0}
        <p class="text-sm text-muted-foreground">This playlist is empty.</p>
    {:else}
        <div class="flex flex-col gap-1">
            {#each items as it}
                <div class="flex items-center gap-3 rounded-lg p-2 hover:bg-muted/40">
                    <button
                        type="button"
                        data-ctx="card"
                        data-video-id={it.videoId}
                        data-title={it.title}
                        data-author={it.author}
                        data-thumbnail={it.thumbnail}
                        data-duration={it.durationSeconds}
                        data-channel-id={it.channelId}
                        onclick={() => play(it.videoId)}
                        class="flex flex-1 items-center gap-3 text-left"
                    >
                        <div class="relative aspect-video w-28 shrink-0 overflow-hidden rounded-md bg-muted">
                            {#if it.thumbnail}
                                <img src={it.thumbnail} alt="" class="h-full w-full object-cover" loading="lazy" />
                            {/if}
                            <HugeiconsIcon
                                icon={PlayCircleIcon}
                                class="absolute top-1/2 left-1/2 h-8 w-8 -translate-x-1/2 -translate-y-1/2 text-white opacity-0 transition-opacity group-hover:opacity-100"
                                strokeWidth={2}
                            />
                        </div>
                        <div class="min-w-0 flex-1">
                            <p class="line-clamp-2 text-sm font-medium">{it.title}</p>
                            <p class="text-xs text-muted-foreground">{it.author}</p>
                            {#if it.durationSeconds > 0}
                                <p class="text-xs text-muted-foreground">{formatDuration(it.durationSeconds)}</p>
                            {/if}
                        </div>
                    </button>
                    <button
                        type="button"
                        onclick={() => remove(it.videoId)}
                        class="rounded-md px-2 py-1 text-xs text-muted-foreground hover:bg-destructive hover:text-destructive-foreground"
                    >
                        Remove
                    </button>
                </div>
            {/each}
        </div>
    {/if}
</div>

<ConfirmDialog
    bind:open={confirmOpen}
    title="Delete playlist"
    message={`Delete "${title}"? Its videos stay in your library.`}
    confirmLabel="Delete"
    onconfirm={deletePlaylist}
/>
