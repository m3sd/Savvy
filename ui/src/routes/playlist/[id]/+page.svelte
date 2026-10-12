<script lang="ts">
    import { HugeiconsIcon } from '@hugeicons/svelte';
    import { ArrowLeft01Icon, PlayCircleIcon } from '@hugeicons/core-free-icons';
    import { page } from '$app/state';
    import { onMount } from 'svelte';
    import { api, type YoutubePlaylistDto, type SearchPlaylistVideoDto } from '$lib/api';
    import { goto } from '$app/navigation';

    const playlistId = $derived(page.params.id ?? '');
    let playlist = $state<YoutubePlaylistDto | null>(null);
    let error = $state('');
    let loading = $state(true);
    let sort = $state<'default' | 'newest' | 'length'>('default');

    async function load() {
        loading = true;
        error = '';
        try {
            if (!playlistId) {
                error = 'Missing playlist id.';
                return;
            }
            playlist = await api.youtubePlaylist(playlistId);
        } catch (e) {
            error = String(e);
            playlist = null;
        } finally {
            loading = false;
        }
    }

    onMount(load);

    const sortedVideos = $derived.by<SearchPlaylistVideoDto[]>(() => {
        const list = playlist?.videos ?? [];
        if (sort === 'default') return list;
        const copy = [...list];
        if (sort === 'newest') {
            // `published` is relative text ("1 year ago"); approximate it to
            // seconds so the newest uploads rise to the top. Undated rows sink.
            copy.sort((a, b) => {
                const av = ageSeconds(a.published);
                const bv = ageSeconds(b.published);
                if (av === null && bv === null) return 0;
                if (av === null) return 1;
                if (bv === null) return -1;
                return av - bv;
            });
        } else {
            copy.sort(
                (a, b) => (b.durationSeconds ?? -1) - (a.durationSeconds ?? -1)
            );
        }
        return copy;
    });

    function back() {
        goto('/search');
    }

    function play(videoId: string) {
        goto(`/watch/${videoId}`);
    }

    function formatDuration(s: number): string {
        const h = Math.floor(s / 3600);
        const m = Math.floor((s % 3600) / 60);
        const sec = s % 60;
        return h > 0
            ? `${h}:${String(m).padStart(2, '0')}:${String(sec).padStart(2, '0')}`
            : `${m}:${String(sec).padStart(2, '0')}`;
    }

    /// Approximate an age label to seconds so relative upload times can be
    /// sorted. Returns null when the text carries no recognisable age.
    function ageSeconds(text?: string | null): number | null {
        if (!text) return null;
        const s = text.toLowerCase();
        if (/\bjust now\b/.test(s) || /\bnow\b/.test(s)) return 0;
        if (s.includes('yesterday')) return 86400;
        const m = s.match(/(\d+)\s*(second|minute|hour|day|week|month|year)/);
        if (!m) return null;
        const n = parseInt(m[1], 10);
        const mult: Record<string, number> = {
            second: 1,
            minute: 60,
            hour: 3600,
            day: 86400,
            week: 604800,
            month: 2592000,
            year: 31536000
        };
        return n * (mult[m[2]] ?? 1);
    }

    const sortOptions: { id: 'default' | 'newest' | 'length'; label: string }[] = [
        { id: 'default', label: 'Default' },
        { id: 'newest', label: 'Newest' },
        { id: 'length', label: 'Length' }
    ];
</script>

<div class="flex flex-col gap-4 p-6">
    <button
        type="button"
        onclick={back}
        class="flex items-center gap-2 self-start rounded-md px-2 py-1 text-sm text-muted-foreground hover:bg-muted hover:text-foreground"
    >
        <HugeiconsIcon icon={ArrowLeft01Icon} strokeWidth={2} class="h-4 w-4" />
        Back
    </button>

    {#if loading}
        <p class="text-sm text-muted-foreground">Loading…</p>
    {:else if error}
        <p class="text-sm text-destructive">{error}</p>
    {:else if playlist}
        <header class="flex items-center gap-4">
            <div class="aspect-video w-40 shrink-0 overflow-hidden rounded-md bg-muted">
                {#if playlist.thumbnail}
                    <img
                        src={playlist.thumbnail}
                        alt=""
                        class="h-full w-full object-cover"
                        loading="lazy"
                    />
                {/if}
            </div>
            <div class="min-w-0">
                <h1 class="font-heading text-2xl font-bold tracking-tight">{playlist.title}</h1>
                <p class="text-sm text-muted-foreground">
                    {#if playlist.author}{playlist.author}{/if}
                    {#if playlist.author && playlist.videoCount} · {/if}
                    {#if playlist.videoCount}{playlist.videoCount} videos{/if}
                </p>
                {#if playlist.description}
                    <p class="mt-1 line-clamp-2 text-xs text-muted-foreground">
                        {playlist.description}
                    </p>
                {/if}
            </div>
        </header>

        <div class="flex items-center gap-1">
            {#each sortOptions as option}
                <button
                    type="button"
                    onclick={() => (sort = option.id)}
                    class={sort === option.id
                        ? 'rounded-full bg-muted px-3 py-1 text-xs font-medium text-foreground'
                        : 'rounded-full px-3 py-1 text-xs text-muted-foreground hover:bg-muted/60 hover:text-foreground'}
                >
                    {option.label}
                </button>
            {/each}
        </div>

        <div class="flex flex-col gap-1">
            {#each sortedVideos as v (v.videoId)}
                <button
                    type="button"
                    data-ctx="card"
                    data-video-id={v.videoId}
                    data-title={v.title}
                    data-author={v.author}
                    data-channel-id={v.channelId ?? ''}
                    data-thumbnail={v.thumbnail ?? ''}
                    data-duration={v.durationSeconds ?? 0}
                    onclick={() => play(v.videoId)}
                    class="group flex items-center gap-3 rounded-lg p-2 text-left hover:bg-muted/40"
                >
                    <div class="relative aspect-video w-28 shrink-0 overflow-hidden rounded-md bg-muted">
                        {#if v.thumbnail}
                            <img
                                src={v.thumbnail}
                                alt=""
                                class="h-full w-full object-cover"
                                loading="lazy"
                            />
                        {/if}
                        {#if v.durationSeconds}
                            <span
                                class="absolute right-1 bottom-1 rounded bg-black/70 px-1.5 py-0.5 text-[10px] font-medium text-white"
                            >
                                {formatDuration(v.durationSeconds)}
                            </span>
                        {/if}
                        <HugeiconsIcon
                            icon={PlayCircleIcon}
                            class="absolute top-1/2 left-1/2 h-8 w-8 -translate-x-1/2 -translate-y-1/2 text-white opacity-0 transition-opacity group-hover:opacity-100"
                            strokeWidth={2}
                        />
                    </div>
                    <div class="min-w-0 flex-1">
                        <p class="line-clamp-2 text-sm font-medium">{v.title}</p>
                        <p class="mt-0.5 text-xs text-muted-foreground">
                            {#if v.author}{v.author}{/if}
                            {#if v.viewCount}{v.author ? ' · ' : ''}{v.viewCount} views{/if}
                            {#if v.published}{v.author || v.viewCount ? ' · ' : ''}{v.published}{/if}
                        </p>
                    </div>
                </button>
            {/each}
            {#if playlist.videos.length === 0}
                <p class="text-sm text-muted-foreground">No videos returned.</p>
            {/if}
        </div>
    {/if}
</div>
