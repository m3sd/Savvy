<script lang="ts">
    import { HugeiconsIcon } from '@hugeicons/svelte';
    import { Search01Icon, ArrowLeft01Icon, ArrowRight01Icon } from '@hugeicons/core-free-icons';
    import { goto } from '$app/navigation';
    import { onMount } from 'svelte';
    import { api, preResolve, type SearchResultsDto, type SearchItemDto, type SearchChannelItemDto } from '$lib/api';

    let query = $state('');
    let results = $state<SearchResultsDto | null>(null);
    let channelResults = $state<SearchChannelItemDto[] | null>(null);
    // In `all` mode the backend hoists channels above the video results (see
    // `search_all`). This counts how many it hoisted so the grid can draw a
    // divider after them instead of showing a channel that looks like an
    // arbitrary first result.
    let hoistedChannels = $state(0);
    let mode = $state<'all' | 'channels'>('all');
    let loading = $state(false);
    let error = $state<string | null>(null);
    let inputEl = $state<HTMLInputElement | null>(null);
    let history = $state<string[]>([]);

    onMount(async () => {
        inputEl?.focus();
        history = JSON.parse(localStorage.getItem('savvy:search-history') ?? '[]').slice(0, 8);
    });

    /// Must match `CHANNEL_SECTION_LIMIT` in `crates/innertube/src/dto.rs`. The
    /// backend caps how many channels it hoists; the frontend only needs to know
    /// the cap to find where the divider goes.
    const MAX_HOISTED_CHANNELS = 4;

    function switchMode(next: 'all' | 'channels') {
        if (next === mode) return;
        mode = next;
        if (query.trim()) runSearch();
    }

    async function runSearch(q?: string) {
        const term = (q ?? query).trim();
        if (!term) return;
        query = term;
        loading = true;
        error = null;
        try {
            if (mode === 'channels') {
                channelResults = await api.searchChannels(term);
                results = null;
                hoistedChannels = 0;
            } else {
                results = await api.searchAll(term);
                channelResults = null;
                hoistedChannels = results.items
                    .slice(0, MAX_HOISTED_CHANNELS)
                    .filter((i) => i.kind === 'channel').length;
            }
            // de-duplicate and persist
            const next = [term, ...history.filter((h) => h !== term)].slice(0, 8);
            history = next;
            localStorage.setItem('savvy:search-history', JSON.stringify(next));
        } catch (e) {
            error = String(e);
            results = null;
            channelResults = null;
        } finally {
            loading = false;
        }
    }

    function openVideo(videoId: string) {
        goto(`/watch/${videoId}`);
    }

    function openChannel(channelId: string) {
        goto(`/channel/${channelId}`);
    }

    function openPlaylist(playlistId: string) {
        goto(`/playlist/${playlistId}`);
    }

    function formatDuration(s?: number | null): string {
        if (!s) return '';
        const h = Math.floor(s / 3600);
        const m = Math.floor((s % 3600) / 60);
        const sec = s % 60;
        return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${String(sec).padStart(2, '0')}` : `${m}:${String(sec).padStart(2, '0')}`;
    }

    function thumb(s?: string | null): string {
        return s && s.length > 0 ? s : '';
    }
</script>

<div class="mx-auto flex max-w-5xl flex-col gap-6 p-8">
    <form
        class="flex items-center gap-2"
        onsubmit={(e) => {
            e.preventDefault();
            runSearch();
        }}
    >
        <div class="relative flex-1">
            <HugeiconsIcon
                icon={Search01Icon}
                class="absolute top-1/2 left-3 h-4 w-4 -translate-y-1/2 text-muted-foreground"
                strokeWidth={2}
            />
            <input
                bind:this={inputEl}
                bind:value={query}
                type="search"
                placeholder="Search YouTube"
                class="h-10 w-full rounded-full border bg-card pr-16 pl-9 text-sm outline-hidden focus:ring-1 focus:ring-ring"
            />
            <kbd
                class="pointer-events-none absolute top-1/2 right-3 -translate-y-1/2 rounded border px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground"
                >Ctrl K</kbd
            >
        </div>
        <button
            type="submit"
            class="h-10 rounded-full bg-primary px-5 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
            disabled={loading}
        >
            Search
        </button>
    </form>

    <div class="flex gap-6 text-sm">
        <button
            type="button"
            class={mode === 'all' ? 'font-semibold text-foreground' : 'text-muted-foreground hover:text-foreground'}
            onclick={() => switchMode('all')}
        >
            All
        </button>
        <button
            type="button"
            class={mode === 'channels' ? 'font-semibold text-foreground' : 'text-muted-foreground hover:text-foreground'}
            onclick={() => switchMode('channels')}
        >
            Channels
        </button>
    </div>

    {#if loading}
        <p class="text-sm text-muted-foreground">Searching…</p>
    {/if}

    {#if error}
        <p class="text-sm text-destructive">Error: {error}</p>
    {/if}

    {#if !results && !loading && history.length > 0}
        <div class="rounded-xl border bg-card p-4">
            <div class="mb-2 flex items-center justify-between">
                <h2 class="font-heading text-sm font-semibold">Recent</h2>
                <button
                    type="button"
                    class="text-xs text-muted-foreground hover:text-foreground"
                    onclick={() => {
                        history = [];
                        localStorage.removeItem('savvy:search-history');
                    }}
                >
                    Clear
                </button>
            </div>
            <div class="flex flex-wrap gap-2">
                {#each history as h}
                    <button
                        type="button"
                        class="rounded-full border bg-background px-3 py-1 text-xs hover:bg-accent"
                        onclick={() => runSearch(h)}
                    >
                        {h}
                    </button>
                {/each}
            </div>
        </div>
    {/if}

    {#if !loading && mode === 'all' && results && results.items.length === 0}
        <p class="text-sm text-muted-foreground">No results for “{results.query}”.</p>
    {/if}

    {#if !loading && mode === 'channels' && channelResults && channelResults.length === 0}
        <p class="text-sm text-muted-foreground">No channels for “{query}”.</p>
    {/if}

    {#if results}
        <div class="grid grid-cols-1 gap-x-6 gap-y-4 md:grid-cols-2">
            {#each results.items as item, i}
                {#if i === hoistedChannels && hoistedChannels > 0 && i < results.items.length}
                    <div class="col-span-full border-t pt-4">
                        <h2 class="font-heading text-sm font-semibold">Results</h2>
                    </div>
                {/if}
                {#if item.kind === 'video'}
                    {@const v = item}
                    <button
                        type="button"
                        data-ctx="card"
                        data-video-id={v.videoId}
                        data-title={v.title}
                        data-author={v.author}
                        data-thumbnail={v.thumbnails[v.thumbnails.length - 1]?.url ?? ''}
                        data-duration={v.durationSeconds ?? 0}
                        data-channel-id={v.channelId}
                        class="group flex gap-3 rounded-lg p-2 text-left transition-colors hover:bg-muted/40"
                        onclick={() => openVideo(v.videoId)}
                        onmouseenter={() => preResolve(v.videoId)}
                        onfocus={() => preResolve(v.videoId)}
                    >
                        <div class="relative aspect-video w-44 shrink-0 overflow-hidden rounded-md bg-muted">
                            {#if thumb(v.thumbnails[v.thumbnails.length - 1]?.url)}
                                <img
                                    src={thumb(v.thumbnails[v.thumbnails.length - 1]?.url)}
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
                        </div>
                        <div class="min-w-0 flex-1">
                            <p class="line-clamp-2 text-sm font-medium leading-snug group-hover:text-primary">
                                {v.title}
                            </p>
                            <p class="mt-1 text-xs text-muted-foreground">{v.author}</p>
                            {#if v.viewCount}
                                <p class="text-xs text-muted-foreground">{v.viewCount} views</p>
                            {/if}
                        </div>
                    </button>
                {:else if item.kind === 'channel'}
                    {@const c = item}
                    <button
                        type="button"
                        class="flex items-center gap-3 rounded-lg p-2 text-left transition-colors hover:bg-muted/40"
                        onclick={() => openChannel(c.channelId)}
                    >
                        <div class="h-12 w-12 shrink-0 overflow-hidden rounded-full bg-muted">
                            {#if c.thumbnails.length > 0}
                                <img
                                    src={c.thumbnails[c.thumbnails.length - 1].url}
                                    alt=""
                                    class="h-full w-full object-cover"
                                />
                            {/if}
                        </div>
                        <div class="min-w-0 flex-1">
                            <p class="text-sm font-medium">{c.title}</p>
                            <p class="text-xs text-muted-foreground">
                                {#if c.subscriberCount}{c.subscriberCount} subscribers{/if}
                                {#if c.subscriberCount && c.videoCount} · {/if}
                                {#if c.videoCount}{c.videoCount} videos{/if}
                            </p>
                        </div>
                    </button>
                {:else if item.kind === 'playlist'}
                    {@const p = item}
                    <button
                        type="button"
                        class="flex items-center gap-3 rounded-lg p-2 text-left transition-colors hover:bg-muted/40"
                        onclick={() => openPlaylist(p.id)}
                    >
                        <div class="h-12 w-12 shrink-0 overflow-hidden rounded-md bg-muted">
                            {#if p.thumbnail}
                                <img src={p.thumbnail} alt="" class="h-full w-full object-cover" />
                            {/if}
                        </div>
                        <div class="min-w-0 flex-1">
                            <p class="text-sm font-medium">{p.title}</p>
                            <p class="text-xs text-muted-foreground">
                                {#if p.videoCount}{p.videoCount}{/if}
                                {#if p.videoCount && p.author} · {/if}
                                {#if p.author}{p.author}{/if}
                                {#if !p.videoCount && !p.author}Playlist{/if}
                            </p>
                        </div>
                    </button>
                {/if}
            {/each}
        </div>
    {/if}

    {#if channelResults && channelResults.length > 0}
        <div class="flex flex-col gap-2">
            {#each channelResults as c (c.channelId)}
                <button
                    type="button"
                    class="flex items-center gap-3 rounded-lg p-2 text-left transition-colors hover:bg-muted/40"
                    onclick={() => openChannel(c.channelId)}
                >
                    <div class="h-12 w-12 shrink-0 overflow-hidden rounded-full bg-muted">
                        {#if c.thumbnails.length > 0}
                            <img
                                src={c.thumbnails[c.thumbnails.length - 1].url}
                                alt=""
                                class="h-full w-full object-cover"
                            />
                        {/if}
                    </div>
                    <div class="min-w-0 flex-1">
                        <p class="text-sm font-medium">{c.title}</p>
                        <p class="text-xs text-muted-foreground">
                            {#if c.subscriberCount}{c.subscriberCount} subscribers{/if}
                            {#if c.subscriberCount && c.videoCount} · {/if}
                            {#if c.videoCount}{c.videoCount} videos{/if}
                        </p>
                    </div>
                </button>
            {/each}
        </div>
    {/if}
</div>
