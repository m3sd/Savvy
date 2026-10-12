<script lang="ts">
    import { HugeiconsIcon } from '@hugeicons/svelte';
    import {
        TagsIcon,
        UserGroupIcon,
        ViewSidebarRightIcon
    } from '@hugeicons/core-free-icons';
    import { onMount } from 'svelte';
    import { goto } from '$app/navigation';
    import { open } from '@tauri-apps/plugin-dialog';
    import {
        api,
        preResolve,
        type CategoryRow,
        type FeedItemDto,
        type ImportResult,
        type SubscriptionRow
    } from '$lib/api';

    let channels = $state<SubscriptionRow[]>([]);
    let categories = $state<CategoryRow[]>([]);
    let channelsLoading = $state(true);
    let feed = $state<FeedItemDto[]>([]);
    let feedLoading = $state(true);
    let feedError = $state<string | null>(null);
    // Scroll-driven paging: the server hands back an opaque cursor for the next
    // slice, and `feedHasMore` says whether one exists.
    let feedCursor = $state<string | null>(null);
    let feedHasMore = $state(false);
    let loadingMore = $state(false);
    /// Why the list stopped growing: 'empty' | 'done' | null while more is
    /// available. Drives the message under the last card.
    let exhaustedReason = $state<'empty' | 'done' | null>(null);
    let observer: IntersectionObserver | null = null;
    // The right rail is where subscription categories live, so it is a
    // first-class panel rather than a section under the feed.
    let railOpen = $state(true);

    // Category editing state.
    let activeCategory = $state<string | null>(null);
    let editingCategoriesFor = $state<string | null>(null);
    let categoryError = $state<string | null>(null);

    /// One dialog serves both "new category" and "manage category": give it a
    /// name and tick which channels belong (what the chip row later filters
    /// by). Creating picks a starting membership, editing is a bulk rename +
    /// re-assign in one save.
    let categoryDialog = $state<
        | { mode: 'creating' }
        | { mode: 'editing'; category: CategoryRow }
        | null
    >(null);
    let dialogName = $state('');
    let dialogSelection = $state<string[]>([]);
    let dialogSelected = $derived(new Set(dialogSelection));
    let dialogTitle = $derived.by(() => {
        const dlg = categoryDialog;
        if (!dlg) return '';
        return dlg.mode === 'creating' ? 'New category' : `Manage ${dlg.category.name}`;
    });

    // NewPipe import (spec item 1).
    let importing = $state(false);
    let importError = $state<string | null>(null);
    let importResult = $state<ImportResult | null>(null);
    let importPaste = $state(false);
    let pasteDraft = $state('');

    const activeChip = 'border-primary bg-primary/10 text-foreground';
    const idleChip = 'border-border text-muted-foreground hover:text-foreground';

    function formatDuration(s?: number | null): string {
        if (!s) return '';
        const h = Math.floor(s / 3600);
        const m = Math.floor((s % 3600) / 60);
        const sec = s % 60;
        return h > 0
            ? `${h}:${String(m).padStart(2, '0')}:${String(sec).padStart(2, '0')}`
            : `${m}:${String(sec).padStart(2, '0')}`;
    }

    /** The whole list under "All", otherwise only that category's members. */
    let visibleChannels = $derived(
        activeCategory === null
            ? channels
            : channels.filter((c) => c.categoryIds.includes(activeCategory as string))
    );

    let visibleChannelIds = $derived(new Set(visibleChannels.map((c) => c.channelId)));

    let visibleFeed = $derived(
        activeCategory === null
            ? feed
            : feed.filter((item) => visibleChannelIds.has(item.channelId))
    );

    /** Member count per category, derived from the single loaded list. */
    let counts = $derived.by(() => {
        const m = new Map<string, number>();
        for (const c of channels) {
            for (const id of c.categoryIds) m.set(id, (m.get(id) ?? 0) + 1);
        }
        return m;
    });

    async function loadChannels() {
        channelsLoading = true;
        try {
            channels = await api.subscriptionList();
        } finally {
            channelsLoading = false;
        }
    }

    /// Cosmetically top up any avatars the stored rows lack (spec item 16). The
    /// backend resolves a bounded batch per pass and persists each hit, so this
    /// is cheap and safe to call on every visit. Merged by id rather than
    /// replacing the list, so it can never clobber a concurrent category edit.
    async function refreshAvatars() {
        try {
            const fresh = await api.subscriptionRefreshAvatars();
            const byId = new Map(fresh.map((s) => [s.channelId, s.thumbnail]));
            channels = channels.map((c) =>
                c.thumbnail ? c : { ...c, thumbnail: byId.get(c.channelId) ?? c.thumbnail }
            );
            feed = feed.map((item) =>
                item.channelThumbnail
                    ? item
                    : {
                          ...item,
                          channelThumbnail:
                              byId.get(item.channelId) ?? item.channelThumbnail
                      }
            );
        } catch {
            // Cosmetic only; a failed top-up must not disturb the page.
        }
    }

    async function loadCategories() {
        categories = await api.categoryList();
    }

    // The feed and the channel list load independently: a feed that is slow or
    // throttled must not hide the subscription list.
    //
    // The feed arrives one batch at a time, newest first across every channel.
    // `feedCursor` points at the next slice; scrolling to the bottom asks for
    // it. The backend holds the merged list for the session, so a later batch
    // cannot reshuffle what is already on screen.
    async function loadFeed(reset = false) {
        if (reset) {
            feedCursor = null;
            feed = [];
        }
        if (loadingMore) return;
        loadingMore = true;
        if (reset) feedLoading = true;
        feedError = null;
        try {
            const page = await api.subscriptionFeed(feedCursor);
            // A restarted session means the server lost our cursor and began
            // again from the top, so replace rather than append.
            feed = page.restarted ? page.items : [...feed, ...page.items];
            feedCursor = page.nextCursor;
            feedHasMore = page.hasMore;
            exhaustedReason = page.hasMore ? null : page.items.length === 0 && reset ? 'empty' : 'done';
        } catch (e) {
            feedError = String(e);
        } finally {
            loadingMore = false;
            feedLoading = false;
        }
    }

    /// Called by the sentinel at the end of the list once it scrolls into view.
    async function loadMore() {
        if (!feedHasMore || loadingMore || feedLoading) return;
        await loadFeed(false);
    }

    onMount(() => {
        loadChannels().then(refreshAvatars);
        loadCategories();
        loadFeed(true);

        // Infinite scroll: the sentinel is a zero-height element after the last
        // card, so it is "in view" only once the user reaches the end.
        const sentinel = document.getElementById('feed-end');
        if (!sentinel || typeof IntersectionObserver === 'undefined') return;
        observer = new IntersectionObserver(
            (entries) => {
                if (entries.some((entry) => entry.isIntersecting)) loadMore();
            },
            { rootMargin: '600px' }
        );
        observer.observe(sentinel);
        return () => observer?.disconnect();
    });

    function openCreateCategory() {
        categoryError = null;
        dialogName = '';
        dialogSelection = [];
        categoryDialog = { mode: 'creating' };
    }

    function openManageCategory(c: CategoryRow) {
        categoryError = null;
        dialogName = c.name;
        dialogSelection = channels
            .filter((s) => s.categoryIds.includes(c.id))
            .map((s) => s.channelId);
        categoryDialog = { mode: 'editing', category: c };
    }

    function closeCategoryDialog() {
        categoryDialog = null;
    }

    function toggleDialogChannel(channelId: string) {
        dialogSelection = dialogSelected.has(channelId)
            ? dialogSelection.filter((id) => id !== channelId)
            : [...dialogSelection, channelId];
    }

    function selectAllDialogChannels(want: boolean) {
        dialogSelection = want ? channels.map((c) => c.channelId) : [];
    }

    /// Create or rename, then apply the checked membership in one go.
    ///
    /// Membership is written the way the API models it — replace-all per
    /// channel — so a tick is "push this category's id onto the channel's
    /// current set" and an untick is "drop it", never "see only this dialog".
    async function saveCategoryDialog() {
        const name = dialogName.trim();
        if (!name) {
            categoryError = 'Give the category a name.';
            return;
        }
        categoryError = null;
        const dlg = categoryDialog;
        try {
            if (dlg?.mode === 'creating') {
                const created = await api.categoryAdd(name);
                const want = dialogSelected;
                for (const c of channels) {
                    if (want.has(c.channelId) && !c.categoryIds.includes(created.id)) {
                        await api.subscriptionSetCategories(c.channelId, [...c.categoryIds, created.id]);
                    }
                }
                await loadChannels();
                categories = [...categories, created];
                closeCategoryDialog();
                activeCategory = created.id;
            } else if (dlg?.mode === 'editing') {
                const edited = dlg.category;
                if (name !== edited.name) {
                    const updated = await api.categoryRename(edited.id, name);
                    categories = categories.map((c) => (c.id === updated.id ? updated : c));
                }
                const want = dialogSelected;
                const have = new Set(
                    channels.filter((c) => c.categoryIds.includes(edited.id)).map((c) => c.channelId)
                );
                for (const c of channels) {
                    if (want.has(c.channelId) && !have.has(c.channelId)) {
                        await api.subscriptionSetCategories(c.channelId, [...c.categoryIds, edited.id]);
                    } else if (!want.has(c.channelId) && have.has(c.channelId)) {
                        await api.subscriptionSetCategories(
                            c.channelId,
                            c.categoryIds.filter((id) => id !== edited.id)
                        );
                    }
                }
                await loadChannels();
                closeCategoryDialog();
            }
        } catch (e) {
            categoryError = String(e);
        }
    }

    async function deleteCategory(c: CategoryRow) {
        categoryError = null;
        try {
            await api.categoryRemove(c.id);
            categories = categories.filter((x) => x.id !== c.id);
            if (activeCategory === c.id) activeCategory = null;
            // Membership lives on the subscription rows, so refresh those too.
            await loadChannels();
        } catch (e) {
            categoryError = String(e);
        }
    }

    async function toggleChannelCategory(channelId: string, categoryId: string) {
        const c = channels.find((x) => x.channelId === channelId);
        if (!c) return;
        const next = c.categoryIds.includes(categoryId)
            ? c.categoryIds.filter((id) => id !== categoryId)
            : [...c.categoryIds, categoryId];
        try {
            await api.subscriptionSetCategories(channelId, next);
            channels = channels.map((x) => (x.channelId === channelId ? { ...x, categoryIds: next } : x));
        } catch (e) {
            categoryError = String(e);
        }
    }

    async function unsubscribe(channelId: string) {
        await api.subscriptionRemove(channelId);
        if (editingCategoriesFor === channelId) editingCategoriesFor = null;
        await loadChannels();
        // A removed channel must leave the feed, and the server-side merged
        // list still holds it, so this has to be a full reset rather than
        // another slice.
        await loadFeed(true);
    }

    /// Finish an import by pulling the new list and categories back down. The
    /// feed is deliberately not reloaded: reloading it on a freshly imported
    /// list fires one `/browse` per channel, which is exactly the burst that
    /// gets an anonymous IP throttled. It fills in when the page is reopened.
    async function afterImport() {
        await Promise.all([loadChannels(), loadCategories()]);
        activeCategory = null;
        importing = false;
        importPaste = false;
        pasteDraft = '';
    }

    async function importFromFile() {
        importError = null;
        importResult = null;
        const picked = await open({
            multiple: false,
            directory: false,
            filters: [{ name: 'NewPipe export', extensions: ['json'] }]
        });
        // The picker returns null when cancelled.
        if (typeof picked !== 'string') return;

        importing = true;
        try {
            importResult = await api.subscriptionImportFile(picked);
            await afterImport();
        } catch (e) {
            importError = String(e);
            importing = false;
        }
    }

    async function importFromPaste() {
        importError = null;
        importResult = null;
        const json = pasteDraft.trim();
        if (!json) {
            importError = 'Paste the export JSON first.';
            return;
        }
        importing = true;
        try {
            importResult = await api.subscriptionImport(json);
            await afterImport();
        } catch (e) {
            importError = String(e);
            importing = false;
        }
    }
</script>

<div class="flex flex-col gap-4 p-6">
    <div class="flex items-center justify-between gap-3">
        <div class="flex items-center gap-3">
            <HugeiconsIcon icon={UserGroupIcon} class="h-6 w-6 text-primary" strokeWidth={2} />
            <h1 class="font-heading text-2xl font-bold tracking-tight">Subscriptions</h1>
        </div>
        <div class="flex items-center gap-2">
            {#if !channelsLoading && channels.length > 0}
                <button
                    type="button"
                    onclick={() => (railOpen = !railOpen)}
                    aria-expanded={railOpen}
                    aria-controls="subscriptions-rail"
                    class="flex items-center gap-2 rounded-md border px-3 py-1.5 text-sm text-muted-foreground hover:bg-muted hover:text-foreground"
                >
                    <HugeiconsIcon icon={ViewSidebarRightIcon} strokeWidth={2} class="h-4 w-4" />
                    Channels
                    <span class="text-xs text-muted-foreground">({visibleChannels.length})</span>
                </button>
            {/if}
            <button
                type="button"
                onclick={() => {
                    importError = null;
                    importResult = null;
                    importing = true;
                }}
                disabled={importing}
                class="flex items-center gap-2 rounded-md border px-3 py-1.5 text-sm text-muted-foreground hover:bg-muted hover:text-foreground disabled:opacity-50"
            >
                {importing ? 'Importing…' : 'Import from NewPipe…'}
            </button>
        </div>
    </div>

    {#if importing || importResult || importError}
        <section class="rounded-lg border p-4">
            <div class="flex items-center justify-between gap-3">
                <h2 class="font-heading text-sm font-semibold">Import from NewPipe</h2>
                <button
                    type="button"
                    aria-label="Close import"
                    class="rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                    onclick={() => {
                        importing = false;
                        importResult = null;
                        importError = null;
                    }}
                >
                    ✕
                </button>
            </div>

            {#if importing}
                <p class="mt-2 text-sm text-muted-foreground">
                    Reading the export. Handle-based channels need one lookup each, so a large import
                    can take a moment.
                </p>
            {/if}

            {#if importError}
                <p class="mt-2 text-sm text-destructive">{importError}</p>
            {/if}

            {#if importResult}
                <div class="mt-2 space-y-1 text-sm">
                    <p>
                        <span class="font-semibold">{importResult.added}</span> added,
                        <span class="font-semibold">{importResult.existing}</span> already subscribed,
                        {#if importResult.skipped > 0}
                            <span class="font-semibold">{importResult.skipped}</span> skipped
                        {/if}
                        {#if importResult.categoriesCreated > 0}
                            , <span class="font-semibold">{importResult.categoriesCreated}</span>
                            categories from groups
                        {/if}
                    </p>
                    {#each importResult.warnings as warning (warning)}
                        <p class="text-xs text-muted-foreground">{warning}</p>
                    {/each}
                </div>
            {/if}

            {#if importing}
                <div class="mt-3 flex flex-wrap items-center gap-2">
                    <button
                        type="button"
                        onclick={importFromFile}
                        class="rounded-md border px-3 py-1.5 text-sm hover:bg-muted"
                    >
                        Choose file…
                    </button>
                    <span class="text-xs text-muted-foreground">or</span>
                    <button
                        type="button"
                        onclick={() => (importPaste = !importPaste)}
                        class="rounded-md border px-3 py-1.5 text-sm hover:bg-muted"
                    >
                        {importPaste ? 'Hide pasted JSON' : 'Paste JSON instead'}
                    </button>
                </div>

                {#if importPaste}
                    <div class="mt-3">
                        <textarea
                            bind:value={pasteDraft}
                            rows="6"
                            spellcheck="false"
                            placeholder={'{ "subscriptions": [ … ] }'}
                            class="w-full rounded-md border bg-background p-2 font-mono text-xs"
                        ></textarea>
                        <button
                            type="button"
                            onclick={importFromPaste}
                            disabled={importing}
                            class="mt-2 rounded-md border px-3 py-1.5 text-sm hover:bg-muted disabled:opacity-50"
                        >
                            Import pasted JSON
                        </button>
                    </div>
                {/if}
            {/if}
        </section>
    {/if}

    <div class="flex items-start gap-6">
        <div class="min-w-0 flex-1">
            {#if !channelsLoading && channels.length === 0 && !feedLoading && feed.length === 0}
                <p class="text-sm text-muted-foreground">
                    No subscriptions yet. Open a channel and hit Subscribe.
                </p>
            {:else}
                <h2 class="font-heading text-sm font-semibold text-muted-foreground">Latest</h2>

                {#if feedLoading}
                    <p class="mt-3 text-sm text-muted-foreground">Loading feed…</p>
                {:else if feedError}
                    <p class="mt-3 text-sm text-destructive">Could not load the feed: {feedError}</p>
                {:else if visibleFeed.length === 0}
                    <p class="mt-3 text-sm text-muted-foreground">
                        {#if activeCategory}
                            None of these channels posted in the last 90 days.
                        {:else}
                            None of your channels posted in the last 90 days.
                        {/if}
                    </p>
                {:else}
                    <div class="mt-3 grid grid-cols-1 gap-x-4 gap-y-5 sm:grid-cols-2 xl:grid-cols-3">
                        {#each visibleFeed as item (item.videoId)}
                            <button
                                type="button"
                                data-ctx="card"
                                data-video-id={item.videoId}
                                data-title={item.title}
                                data-author={item.channelName}
                                data-thumbnail={item.thumbnails[item.thumbnails.length - 1]?.url ?? ''}
                                data-duration={item.durationSeconds ?? 0}
                                data-channel-id={item.channelId}
                                class="group flex flex-col gap-2 text-left"
                                onclick={() => goto(`/watch/${item.videoId}`)}
                                onmouseenter={() => preResolve(item.videoId)}
                                onfocus={() => preResolve(item.videoId)}
                            >
                                <div class="relative aspect-video w-full overflow-hidden rounded-lg bg-muted">
                                    {#if item.thumbnails.length > 0}
                                        <img
                                            src={item.thumbnails[item.thumbnails.length - 1].url}
                                            alt=""
                                            class="h-full w-full object-cover"
                                            loading="lazy"
                                        />
                                    {/if}
                                    {#if item.durationSeconds}
                                        <span
                                            class="absolute right-1 bottom-1 rounded bg-black/70 px-1.5 py-0.5 text-[10px] font-medium text-white"
                                        >
                                            {formatDuration(item.durationSeconds)}
                                        </span>
                                    {/if}
                                </div>
                                <div class="min-w-0">
                                    <p
                                        class="line-clamp-2 text-sm font-medium leading-snug group-hover:text-primary"
                                    >
                                        {item.title}
                                    </p>
                                    <p class="mt-0.5 line-clamp-1 text-xs text-muted-foreground">
                                        {item.channelName}{item.publishedText
                                            ? ` · ${item.publishedText}`
                                            : ''}
                                    </p>
                                    {#if item.viewCount}
                                        <p class="text-xs text-muted-foreground">{item.viewCount} views</p>
                                    {/if}
                                </div>
                            </button>
                        {/each}
                    </div>

                    <!-- Infinite scroll: this zero-height marker sitting after
                         the last card is what triggers the next batch. -->
                    <div id="feed-end" aria-hidden="true"></div>

                    {#if loadingMore}
                        <p class="mt-4 text-sm text-muted-foreground">Loading more…</p>
                    {:else if feedError}
                        <p class="mt-4 text-sm text-destructive">
                            Could not load more: {feedError}
                            <button type="button" class="underline" onclick={() => loadMore()}>
                                Retry
                            </button>
                        </p>
                    {:else if exhaustedReason === 'empty'}
                        <p class="mt-4 text-sm text-muted-foreground">
                            No videos published in the last 90 days.
                        </p>
                    {:else if exhaustedReason === 'done'}
                        <p class="mt-4 text-sm text-muted-foreground">
                            That's everything from the last 90 days.
                        </p>
                    {/if}
                {/if}
            {/if}
        </div>

        {#if railOpen && channels.length > 0}
            <aside
                id="subscriptions-rail"
                class="sticky top-0 flex max-h-[calc(100vh-9rem)] w-64 shrink-0 flex-col gap-2 overflow-y-auto pr-1"
            >
                <!-- Category chips: "All", then each category with its members. -->
                <div class="flex flex-wrap items-center gap-1.5 pb-1">
                    <button
                        type="button"
                        onclick={() => (activeCategory = null)}
                        class="rounded-full border px-2 py-0.5 text-xs {activeCategory === null
                            ? activeChip
                            : idleChip}"
                    >
                        All <span class="opacity-60">{channels.length}</span>
                    </button>
                    {#each categories as c (c.id)}
                        <span
                            class="inline-flex items-center gap-0.5 rounded-full border px-2 py-0.5 text-xs {activeCategory ===
                            c.id
                                ? activeChip
                                : idleChip}"
                        >
                            <button type="button" onclick={() => (activeCategory = c.id)}>
                                {c.name}
                                <span class="opacity-60">{counts.get(c.id) ?? 0}</span>
                            </button>
                            <button
                                type="button"
                                aria-label={`Manage ${c.name}`}
                                title="Rename, or change which channels are in it"
                                class="opacity-50 hover:opacity-100"
                                onclick={() => openManageCategory(c)}
                            >
                                ✎
                            </button>
                            <button
                                type="button"
                                aria-label={`Delete ${c.name}`}
                                title="Delete"
                                class="opacity-50 hover:opacity-100"
                                onclick={() => deleteCategory(c)}
                            >
                                ✕
                            </button>
                        </span>
                    {/each}
                    <button
                        type="button"
                        onclick={openCreateCategory}
                        class="rounded-full border border-dashed border-border px-2 py-0.5 text-xs text-muted-foreground hover:text-foreground"
                    >
                        + New
                    </button>
                </div>

                {#if categoryError}
                    <p class="text-xs text-destructive">{categoryError}</p>
                {/if}

                {#if visibleChannels.length === 0}
                    <p class="px-1.5 py-2 text-xs text-muted-foreground">
                        No channels in this category.
                    </p>
                {/if}

                {#each visibleChannels as s (s.channelId)}
                    <div class="group flex items-center gap-1 rounded-lg p-1.5 transition-colors hover:bg-muted/40">
                        <button
                            type="button"
                            class="flex min-w-0 flex-1 items-center gap-2 text-left"
                            onclick={() => goto(`/channel/${s.channelId}`)}
                        >
                            <div class="h-8 w-8 shrink-0 overflow-hidden rounded-full bg-muted">
                                {#if s.thumbnail}
                                    <img
                                        src={s.thumbnail}
                                        alt=""
                                        class="h-full w-full object-cover"
                                        loading="lazy"
                                    />
                                {/if}
                            </div>
                            <p class="line-clamp-1 text-xs font-medium">{s.title}</p>
                        </button>
                        <button
                            type="button"
                            aria-label={`Categories for ${s.title}`}
                            title="Categories"
                            class="relative rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground {editingCategoriesFor ===
                            s.channelId
                                ? 'text-foreground'
                                : 'opacity-0 group-hover:opacity-100 focus-visible:opacity-100'}"
                            onclick={() =>
                                (editingCategoriesFor =
                                    editingCategoriesFor === s.channelId ? null : s.channelId)}
                        >
                            <HugeiconsIcon icon={TagsIcon} strokeWidth={2} class="h-3.5 w-3.5" />
                            {#if s.categoryIds.length > 0}
                                <span
                                    class="absolute -top-1 -right-1 rounded-full bg-primary px-1 text-[9px] leading-3 font-semibold text-primary-foreground"
                                >
                                    {s.categoryIds.length}
                                </span>
                            {/if}
                        </button>
                        <button
                            type="button"
                            aria-label={`Unsubscribe from ${s.title}`}
                            title="Unsubscribe"
                            class="rounded p-1 text-muted-foreground opacity-0 transition-opacity hover:bg-muted hover:text-foreground focus-visible:opacity-100 group-hover:opacity-100"
                            onclick={() => unsubscribe(s.channelId)}
                        >
                            ✕
                        </button>
                    </div>

                    {#if editingCategoriesFor === s.channelId}
                        <div class="flex flex-wrap gap-1 px-1.5 pb-1.5">
                            {#if categories.length === 0}
                                <p class="text-[11px] text-muted-foreground">
                                    No categories yet — create one above.
                                </p>
                            {/if}
                            {#each categories as c (c.id)}
                                <label
                                    class="inline-flex cursor-pointer items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] hover:bg-muted/40"
                                >
                                    <input
                                        type="checkbox"
                                        checked={s.categoryIds.includes(c.id)}
                                        onchange={() =>
                                            toggleChannelCategory(s.channelId, c.id)}
                                    />
                                    {c.name}
                                </label>
                            {/each}
                        </div>
                    {/if}
                {/each}
            </aside>
        {/if}
    </div>

    {#if categoryDialog}
        <div
            role="dialog"
            aria-modal="true"
            aria-label={dialogTitle}
            tabindex="-1"
            class="fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4"
            onclick={(e) => {
                if (e.target === e.currentTarget) closeCategoryDialog();
            }}
            onkeydown={(e) => {
                if (e.key === 'Escape') closeCategoryDialog();
            }}
        >
            <div class="flex max-h-[85vh] w-[min(32rem,92vw)] flex-col gap-3 rounded-lg border bg-background p-4 shadow-xl" >
                <div class="flex items-center justify-between gap-3">
                    <h2 class="font-heading text-sm font-semibold">{dialogTitle}</h2>
                    <button
                        type="button"
                        aria-label="Close"
                        class="rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                        onclick={closeCategoryDialog}
                    >
                        ✕
                    </button>
                </div>

                <!-- svelte-ignore a11y_autofocus -->
                <input
                    autofocus
                    bind:value={dialogName}
                    placeholder="Category name"
                    class="rounded-md border bg-background px-3 py-1.5 text-sm"
                    onkeydown={(e) => {
                        if (e.key === 'Enter') saveCategoryDialog();
                        if (e.key === 'Escape') closeCategoryDialog();
                    }}
                />

                <div class="flex items-center justify-between px-0.5 text-xs text-muted-foreground">
                    <span>Channels in this category</span>
                    <span class="flex gap-2">
                        <button
                            type="button"
                            class="underline hover:text-foreground"
                            onclick={() => selectAllDialogChannels(true)}
                        >
                            All
                        </button>
                        <button
                            type="button"
                            class="underline hover:text-foreground"
                            onclick={() => selectAllDialogChannels(false)}
                        >
                            None
                        </button>
                    </span>
                </div>

                <div class="-mx-1 overflow-y-auto px-1 pb-1">
                    {#if channels.length === 0}
                        <p class="px-0.5 py-2 text-xs text-muted-foreground">
                            No subscriptions yet. Open a channel and hit Subscribe.
                        </p>
                    {:else}
                        {#each channels as s (s.channelId)}
                            <label class="flex cursor-pointer items-center gap-2 rounded-md px-1.5 py-1 hover:bg-muted/40">
                                <input
                                    type="checkbox"
                                    checked={dialogSelected.has(s.channelId)}
                                    onchange={() => toggleDialogChannel(s.channelId)}
                                />
                                <div class="h-6 w-6 shrink-0 overflow-hidden rounded-full bg-muted">
                                    {#if s.thumbnail}
                                        <img src={s.thumbnail} alt="" class="h-full w-full object-cover" loading="lazy" />
                                    {/if}
                                </div>
                                <span class="line-clamp-1 text-sm">{s.title}</span>
                            </label>
                        {/each}
                    {/if}
                </div>

                {#if categoryError}
                    <p class="text-xs text-destructive">{categoryError}</p>
                {/if}

                <div class="flex justify-end gap-2 border-t pt-3">
                    <button
                        type="button"
                        onclick={closeCategoryDialog}
                        class="rounded-md border px-3 py-1.5 text-sm text-muted-foreground hover:bg-muted hover:text-foreground"
                    >
                        Cancel
                    </button>
                    <button
                        type="button"
                        onclick={saveCategoryDialog}
                        class="rounded-md bg-primary px-3 py-1.5 text-sm font-medium text-primary-foreground hover:opacity-90"
                    >
                        Save
                    </button>
                </div>
            </div>
        </div>
    {/if}
</div>
