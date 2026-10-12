<script lang="ts">
    import { page } from '$app/state';
    import { HugeiconsIcon } from '@hugeicons/svelte';
    import { ArrowLeft01Icon } from '@hugeicons/core-free-icons';
    import { goto } from '$app/navigation';
    import { onMount } from 'svelte';
    import { api, preResolve, type ChannelDto, type ChannelVideosDto } from '$lib/api';

    const channelId = page.params.id ?? '';

    let info = $state<ChannelDto | null>(null);
    let videos = $state<ChannelVideosDto | null>(null);
    let subscribed = $state(false);
    let busy = $state(false);
    let loading = $state(true);
    let error = $state<string | null>(null);

    function formatDuration(s?: number | null): string {
        if (!s) return '';
        const h = Math.floor(s / 3600);
        const m = Math.floor((s % 3600) / 60);
        const sec = s % 60;
        return h > 0 ? `${h}:${String(m).padStart(2, '0')}:${String(sec).padStart(2, '0')}` : `${m}:${String(sec).padStart(2, '0')}`;
    }

    onMount(async () => {
        const target = channelId;
        try {
            const [i, v, sub] = await Promise.all([
                api.channelGetInfo(target),
                api.channelGetVideos(target),
                api.subscriptionExists(target),
            ]);
            if (page.params.id !== target) return;
            info = i;
            videos = v;
            subscribed = sub;
        } catch (e) {
            error = String(e);
        } finally {
            loading = false;
        }
    });

    async function toggleSubscribe() {
        if (!info || busy) return;
        busy = true;
        try {
            if (subscribed) {
                await api.subscriptionRemove(info.id);
                subscribed = false;
            } else {
                await api.subscriptionAdd({
                    channelId: info.id,
                    title: info.name,
                    thumbnail: info.thumbnails.length > 0 ? info.thumbnails[info.thumbnails.length - 1].url : '',
                });
                subscribed = true;
            }
        } catch (e) {
            error = String(e);
        } finally {
            busy = false;
        }
    }
</script>

<div class="mx-auto flex max-w-5xl flex-col gap-6 p-8">
    <button
        type="button"
        onclick={() => history.back()}
        class="flex items-center gap-2 self-start rounded-md px-2 py-1 text-sm text-muted-foreground hover:bg-muted hover:text-foreground"
    >
        <HugeiconsIcon icon={ArrowLeft01Icon} strokeWidth={2} class="h-4 w-4" />
        Back
    </button>

    {#if loading}
        <p class="text-sm text-muted-foreground">Loading channel…</p>
    {/if}

    {#if error}
        <p class="text-sm text-destructive">Error: {error}</p>
    {/if}

    {#if info}
        <div class="flex items-center gap-4 rounded-xl border bg-card p-5">
            {#if info.thumbnails.length > 0}
                <img
                    src={info.thumbnails[info.thumbnails.length - 1].url}
                    alt=""
                    class="h-20 w-20 shrink-0 rounded-full object-cover"
                />
            {/if}
            <div class="min-w-0">
                <h1 class="font-heading text-2xl font-semibold">{info.name}</h1>
                <p class="mt-1 text-sm text-muted-foreground">
                    {#if info.subscriberCount}{info.subscriberCount} subscribers{/if}
                    {#if info.subscriberCount && info.videosCount} · {/if}
                    {#if info.videosCount}{info.videosCount} videos{/if}
                </p>
                {#if info.description}
                    <details class="mt-2 text-sm text-muted-foreground">
                        <summary class="cursor-pointer text-xs">Description</summary>
                        <p class="mt-1 whitespace-pre-wrap">{info.description}</p>
                    </details>
                {/if}
            </div>
            <button
                type="button"
                class="ml-auto shrink-0 rounded-full px-5 py-2 text-sm font-medium disabled:opacity-50
                    {subscribed
                        ? 'border bg-background text-foreground hover:bg-accent'
                        : 'bg-primary text-primary-foreground hover:bg-primary/90'}"
                disabled={busy}
                onclick={toggleSubscribe}
            >
                {subscribed ? 'Subscribed' : 'Subscribe'}
            </button>
        </div>
    {/if}

    {#if videos}
        <div class="grid grid-cols-1 gap-x-6 gap-y-4 md:grid-cols-2">
            {#each videos.videos as v (v.videoId)}
                <button
                    type="button"
                    data-ctx="card"
                    data-video-id={v.videoId}
                    data-title={v.title}
                    data-author={v.author ?? ''}
                    data-thumbnail={v.thumbnails[v.thumbnails.length - 1]?.url ?? ''}
                    data-duration={v.durationSeconds ?? 0}
                    data-channel-id={v.channelId}
                    class="group flex gap-3 rounded-lg p-2 text-left transition-colors hover:bg-muted/40"
                    onclick={() => goto(`/watch/${v.videoId}`)}
                    onmouseenter={() => preResolve(v.videoId)}
                    onfocus={() => preResolve(v.videoId)}
                >
                    <div class="relative aspect-video w-44 shrink-0 overflow-hidden rounded-md bg-muted">
                        {#if v.thumbnails.length > 0}
                            <img
                                src={v.thumbnails[v.thumbnails.length - 1].url}
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
                        {#if v.publishedText}
                            <p class="mt-1 text-xs text-muted-foreground">{v.publishedText}</p>
                        {/if}
                        {#if v.viewCount}
                            <p class="text-xs text-muted-foreground">{v.viewCount} views</p>
                        {/if}
                    </div>
                </button>
            {/each}
        </div>
    {/if}
</div>