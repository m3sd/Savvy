<script lang="ts">
    import { HugeiconsIcon } from '@hugeicons/svelte';
    import { Bookmark01Icon, Delete02Icon, LibraryIcon, PlayCircleIcon } from '@hugeicons/core-free-icons';
    import { onMount } from 'svelte';
    import { goto } from '$app/navigation';
    import { api, preResolve, type SavedVideoRow } from '$lib/api';
    import { SAVED_CHANGED, savedVideoIds } from '$lib/saved.svelte';

    let saved = $state<SavedVideoRow[]>([]);
    let loading = $state(true);

    async function reload() {
        try {
            saved = await api.savedList(500);
        } finally {
            loading = false;
        }
    }

    onMount(() => {
        void reload();
        window.addEventListener(SAVED_CHANGED, reload);
        return () => window.removeEventListener(SAVED_CHANGED, reload);
    });

    function play(videoId: string) {
        goto(`/watch/${videoId}`);
    }

    async function remove(videoId: string) {
        try {
            await api.savedRemove(videoId);
            savedVideoIds.splice(savedVideoIds.indexOf(videoId), 1);
        } catch (e) {
            console.warn('saved_remove failed', e);
        }
        void reload();
    }
</script>

<div class="flex flex-col gap-6 p-8">
    <div class="flex items-center gap-3">
        <HugeiconsIcon icon={LibraryIcon} class="h-6 w-6 text-primary" strokeWidth={2} />
        <h1 class="font-heading text-3xl font-bold tracking-tight">Library</h1>
    </div>

    {#if loading}
        <p class="text-sm text-muted-foreground">Loading…</p>
    {:else if saved.length === 0}
        <div class="flex flex-col items-center gap-3 rounded-xl border border-dashed bg-muted/20 py-16">
            <HugeiconsIcon
                icon={Bookmark01Icon}
                class="h-10 w-10 text-muted-foreground"
                strokeWidth={1.8}
            />
            <p class="text-sm text-muted-foreground">
                Nothing saved yet. Right-click a video and choose "Save to library".
            </p>
            <a href="/search" class="text-sm text-primary hover:underline">Search for a video</a>
        </div>
    {:else}
        <div class="grid grid-cols-1 gap-3 md:grid-cols-2 lg:grid-cols-3">
            {#each saved as v}
                <div class="group relative">
                    <button
                        type="button"
                        data-ctx="card"
                        data-video-id={v.videoId}
                        data-title={v.title}
                        data-author={v.author}
                        data-thumbnail={v.thumbnail}
                        data-duration={v.durationSeconds}
                        data-channel-id={v.channelId}
                        onclick={() => play(v.videoId)}
                        onmouseenter={() => preResolve(v.videoId)}
                        onfocus={() => preResolve(v.videoId)}
                        class="flex w-full flex-col gap-2 overflow-hidden rounded-lg border bg-card text-left transition-colors hover:bg-muted/40"
                    >
                        <div class="relative aspect-video w-full overflow-hidden bg-muted">
                            {#if v.thumbnail}
                                <img
                                    src={v.thumbnail}
                                    alt=""
                                    class="h-full w-full object-cover"
                                    loading="lazy"
                                />
                            {/if}
                            <HugeiconsIcon
                                icon={PlayCircleIcon}
                                class="absolute top-1/2 left-1/2 h-10 w-10 -translate-x-1/2 -translate-y-1/2 text-white opacity-0 transition-opacity group-hover:opacity-100"
                                strokeWidth={2}
                            />
                        </div>
                        <div class="flex flex-col gap-1 p-3">
                            <p class="line-clamp-2 text-sm font-medium">{v.title}</p>
                            <p class="text-xs text-muted-foreground">{v.author}</p>
                        </div>
                    </button>
                    <button
                        type="button"
                        title="Remove from library"
                        aria-label="Remove from library"
                        onclick={() => remove(v.videoId)}
                        class="absolute right-2 top-2 z-10 rounded-full bg-black/65 p-1.5 text-white opacity-0 transition-opacity group-hover:opacity-100 hover:bg-black/85 focus-visible:opacity-100"
                    >
                        <HugeiconsIcon icon={Delete02Icon} class="h-4 w-4" strokeWidth={2} />
                    </button>
                </div>
            {/each}
        </div>
    {/if}
</div>