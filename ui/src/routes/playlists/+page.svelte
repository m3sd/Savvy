<script lang="ts">
    import { HugeiconsIcon } from '@hugeicons/svelte';
    import { Add01Icon, ComputerIcon, Delete02Icon, PlayListIcon } from '@hugeicons/core-free-icons';
    import { onMount } from 'svelte';
    import { api, type PlaylistRow } from '$lib/api';
    import ConfirmDialog from '$lib/components/ConfirmDialog.svelte';

    let playlists = $state<PlaylistRow[]>([]);
    let newTitle = $state('');
    let loading = $state(true);
    let pendingDelete = $state<PlaylistRow | null>(null);
    let confirmOpen = $state(false);

    function newLocalId() {
        return 'pl_' + Math.random().toString(36).slice(2, 10);
    }

    async function reload() {
        loading = true;
        try {
            playlists = await api.playlistsList();
        } finally {
            loading = false;
        }
    }

    onMount(reload);

    async function create() {
        const title = newTitle.trim();
        if (!title) return;
        await api.playlistCreate(newLocalId(), title);
        newTitle = '';
        await reload();
    }

    function askDelete(p: PlaylistRow) {
        pendingDelete = p;
        confirmOpen = true;
    }

    async function confirmDelete() {
        const p = pendingDelete;
        if (!p) return;
        pendingDelete = null;
        await api.playlistDelete(p.id);
        await reload();
    }
</script>

<div class="flex flex-col gap-6 p-8">
    <div class="flex items-center gap-3">
        <HugeiconsIcon icon={PlayListIcon} class="h-6 w-6 text-primary" strokeWidth={2} />
        <h1 class="font-heading text-3xl font-bold tracking-tight">Playlists</h1>
    </div>

    <form
        class="flex items-center gap-2"
        onsubmit={(e) => {
            e.preventDefault();
            create();
        }}
    >
        <input
            bind:value={newTitle}
            type="text"
            placeholder="New playlist title"
            class="h-10 flex-1 rounded-md border bg-card px-3 text-sm outline-hidden focus:ring-1 focus:ring-ring"
        />
        <button
            type="submit"
            class="flex h-10 items-center gap-2 rounded-md bg-primary px-4 text-sm font-medium text-primary-foreground hover:bg-primary/90 disabled:opacity-50"
            disabled={!newTitle.trim()}
        >
            <HugeiconsIcon icon={Add01Icon} class="h-4 w-4" strokeWidth={2} />
            Create
        </button>
    </form>

    {#if loading}
        <p class="text-sm text-muted-foreground">Loading…</p>
    {:else if playlists.length === 0}
        <div class="rounded-xl border bg-card p-6 text-sm text-muted-foreground">
            No local playlists yet. Create one above. Playlists live entirely on this machine —
            nothing is sent to YouTube.
        </div>
    {:else}
        <div class="flex flex-col gap-2">
            {#each playlists as p}
                <div
                    class="flex items-center gap-3 rounded-lg border bg-card p-3 transition-colors hover:bg-muted/40"
                >
                    <a href="/playlists/{p.id}" class="flex min-w-0 flex-1 items-center gap-3">
                        <div
                            class="flex h-12 w-12 items-center justify-center rounded-md bg-primary/10 text-primary"
                        >
                            <HugeiconsIcon icon={ComputerIcon} strokeWidth={2} class="h-5 w-5" />
                        </div>
                        <div class="min-w-0 flex-1">
                            <p class="text-sm font-medium">{p.title}</p>
                            <p class="text-xs text-muted-foreground">
                                Created {new Date(p.createdAt * 1000).toLocaleString()}
                            </p>
                        </div>
                    </a>
                    <button
                        type="button"
                        aria-label={`Delete ${p.title}`}
                        title="Delete playlist"
                        class="rounded-md p-2 text-muted-foreground transition-colors hover:bg-destructive hover:text-destructive-foreground"
                        onclick={() => askDelete(p)}
                    >
                        <HugeiconsIcon icon={Delete02Icon} strokeWidth={2} class="h-4 w-4" />
                    </button>
                </div>
            {/each}
        </div>
    {/if}
</div>

<ConfirmDialog
    bind:open={confirmOpen}
    title="Delete playlist"
    message={`Delete "${pendingDelete?.title ?? ''}"? Its videos stay in your library.`}
    confirmLabel="Delete"
    onconfirm={confirmDelete}
/>
