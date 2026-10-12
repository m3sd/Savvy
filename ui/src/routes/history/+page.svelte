<script lang="ts">
    import { HugeiconsIcon } from '@hugeicons/svelte';
    import { HistoryIcon, PlayCircleIcon } from '@hugeicons/core-free-icons';
    import { onMount } from 'svelte';
    import { api, preResolve, type HistoryRow } from '$lib/api';
    import { goto } from '$app/navigation';
    import { HISTORY_CHANGED } from '$lib/contextmenu.svelte';

    let items = $state<HistoryRow[]>([]);
    let loading = $state(true);

    /** Rows with a real playhead that stopped short of the end — i.e. resumes. */
    let resumable = $derived(
        items.filter(
            (h) =>
                h.progressSeconds > 5 &&
                (h.durationSeconds <= 0 || h.progressSeconds < h.durationSeconds - 5)
        )
    );

    async function reload() {
        loading = true;
        try {
            items = await api.historyList();
        } finally {
            loading = false;
        }
    }

    onMount(() => {
        void reload();
        window.addEventListener(HISTORY_CHANGED, reload);
        return () => window.removeEventListener(HISTORY_CHANGED, reload);
    });

    async function clear() {
        await api.historyClear();
        await reload();
    }

    async function remove(videoId: string) {
        // Zero the playhead so the row leaves "continue watching" but stays in
        // history; the backend treats a sub-5s position as "not started".
        await api.historyProgress({
            videoId,
            positionSeconds: 0,
            durationSeconds: 0
        });
        await reload();
    }

    function formatDate(t: number): string {
        return new Date(t * 1000).toLocaleString();
    }

    function formatClock(sec: number): string {
        const s = Math.max(0, Math.round(sec));
        const m = Math.floor(s / 60);
        return `${m}:${String(s % 60).padStart(2, '0')}`;
    }

    function remaining(h: HistoryRow): string {
        if (h.durationSeconds > 0) {
            return `${formatClock(Math.max(0, h.durationSeconds - h.progressSeconds))} left`;
        }
        return `at ${formatClock(h.progressSeconds)}`;
    }

    function play(videoId: string) {
        goto(`/watch/${videoId}`);
    }
</script>

<div class="flex flex-col gap-4 p-6">
    <div class="flex items-center justify-between">
        <div class="flex items-center gap-3">
            <HugeiconsIcon icon={HistoryIcon} class="h-6 w-6 text-primary" strokeWidth={2} />
            <h1 class="font-heading text-2xl font-bold tracking-tight">History</h1>
        </div>
        {#if items.length > 0}
            <button
                type="button"
                onclick={clear}
                class="rounded-md px-3 py-1 text-xs text-muted-foreground hover:bg-muted hover:text-foreground"
            >
                Clear
            </button>
        {/if}
    </div>

    {#if loading}
        <p class="text-sm text-muted-foreground">Loading…</p>
    {:else if items.length === 0}
        <p class="text-sm text-muted-foreground">No watch history yet.</p>
    {:else}
        {#if resumable.length > 0}
            <section class="flex flex-col gap-2">
                <h2 class="font-heading text-sm font-semibold text-muted-foreground">
                    Continue watching
                </h2>
                <div class="flex flex-col gap-1">
                    {#each resumable as h}
                        <div
                            role="button"
                            tabindex="0"
                            data-ctx="card"
                            data-video-id={h.videoId}
                            data-title={h.title}
                            data-author={h.author}
                            data-thumbnail={h.thumbnail}
                            data-duration={h.durationSeconds}
                            data-channel-id={h.channelId}
                            onclick={() => play(h.videoId)}
                            onkeydown={(e) => {
                                if (e.key === 'Enter' || e.key === ' ') {
                                    e.preventDefault();
                                    play(h.videoId);
                                }
                            }}
                            onmouseenter={() => preResolve(h.videoId)}
                            onfocus={() => preResolve(h.videoId)}
                            class="group flex items-center gap-3 rounded-lg p-2 text-left transition-colors hover:bg-muted/40"
                        >
                            <div class="relative aspect-video w-32 shrink-0 overflow-hidden rounded-md bg-muted">
                                {#if h.thumbnail}
                                    <img src={h.thumbnail} alt="" class="h-full w-full object-cover" loading="lazy" />
                                {/if}
                                <div
                                    class="absolute right-0 bottom-0 px-1.5 py-0.5 text-[10px] font-medium text-white/95"
                                    style="background-color: rgba(0,0,0,0.6)"
                                >
                                    {formatClock(h.progressSeconds)}
                                </div>
                            </div>
                            <div class="min-w-0 flex-1">
                                <p class="line-clamp-2 text-sm font-medium">{h.title}</p>
                                <p class="text-xs text-muted-foreground">{h.author}</p>
                                <p class="text-xs font-medium text-primary">
                                    Resume · {remaining(h)}
                                </p>
                            </div>
                            <button
                                type="button"
                                aria-label="Remove from continue watching"
                                onclick={(e) => {
                                    e.stopPropagation();
                                    remove(h.videoId);
                                }}
                                class="rounded-md p-1.5 text-muted-foreground opacity-0 transition-opacity hover:bg-muted focus-visible:opacity-100 group-hover:opacity-100"
                            >
                                ✕
                            </button>
                        </div>
                    {/each}
                </div>
            </section>
        {/if}

        <div class="flex flex-col gap-1">
            {#each items as h}
                <button
                    type="button"
                    data-ctx="card"
                    data-video-id={h.videoId}
                    data-title={h.title}
                    data-author={h.author}
                    data-thumbnail={h.thumbnail}
                    data-duration={h.durationSeconds}
                    data-channel-id={h.channelId}
                    onclick={() => play(h.videoId)}
                    onmouseenter={() => preResolve(h.videoId)}
                    onfocus={() => preResolve(h.videoId)}
                    class="flex items-center gap-3 rounded-lg p-2 text-left transition-colors hover:bg-muted/40"
                >
                    <div class="relative aspect-video w-32 shrink-0 overflow-hidden rounded-md bg-muted">
                        {#if h.thumbnail}
                            <img src={h.thumbnail} alt="" class="h-full w-full object-cover" loading="lazy" />
                        {/if}
                        {#if h.progressSeconds > 5}
                            <div
                                class="absolute right-0 bottom-0 px-1.5 py-0.5 text-[10px] font-medium text-white/95"
                                style="background-color: rgba(0,0,0,0.6)"
                            >
                                {formatClock(h.progressSeconds)}
                            </div>
                        {/if}
                        <HugeiconsIcon
                            icon={PlayCircleIcon}
                            class="absolute top-1/2 left-1/2 h-8 w-8 -translate-x-1/2 -translate-y-1/2 text-white opacity-0 transition-opacity hover:opacity-100"
                            strokeWidth={2}
                        />
                    </div>
                    <div class="min-w-0 flex-1">
                        <p class="line-clamp-2 text-sm font-medium">{h.title}</p>
                        <p class="text-xs text-muted-foreground">{h.author}</p>
                        <p class="text-xs text-muted-foreground">{formatDate(h.watchedAt)}</p>
                    </div>
                </button>
            {/each}
        </div>
    {/if}
</div>
