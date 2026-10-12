<script lang="ts">
    import './layout.css';
    import { ModeWatcher } from 'mode-watcher';
    import { HugeiconsIcon } from '@hugeicons/svelte';
    import { Cancel01Icon } from '@hugeicons/core-free-icons';
    import { onMount, tick } from 'svelte';
    import { fly } from 'svelte/transition';
    import { cubicOut } from 'svelte/easing';
    import { goto } from '$app/navigation';
    import { Button } from '$lib/components/ui/button';
    import Sidebar from '$lib/components/Sidebar.svelte';
    import Titlebar from '$lib/components/Titlebar.svelte';
    import ContextMenu from '$lib/components/ContextMenu.svelte';
    import { closeMenu, openMenu, targetFromElement } from '$lib/contextmenu.svelte';
    import { initTheme } from '$lib/theme.svelte';
    import { applyThemeSetting, loadSettings, settingBool } from '$lib/settings.svelte';
    import { api, isTauri } from '$lib/api';

    let { children } = $props();
    let toast = $state<string | null>(null);

    /** Focus the search field, wherever the app currently is. */
    async function focusSearch() {
        await goto('/search');
        await tick();
        // Reached from another page the input mounts and autofocuses itself;
        // when already on /search the route does not remount, so focus here.
        document.querySelector<HTMLInputElement>('input[type="search"]')?.focus();
    }

    onMount(() => {
        // One listener serves every surface (spec item 11): a card only has to
        // mark itself with `data-ctx` plus the `data-*` payload, so a page
        // never hosts a menu of its own. The webview's built-in context menu
        // is suppressed everywhere — our menu is the only one the app has.
        const onContextMenu = (e: MouseEvent) => {
            e.preventDefault();
            const el = (e.target as HTMLElement | null)?.closest(
                '[data-ctx]'
            ) as HTMLElement | null;
            if (!el) {
                closeMenu();
                return;
            }
            const target = targetFromElement(el);
            if (!target) return;
            openMenu(
                e.clientX,
                e.clientY,
                el.dataset.ctx === 'player' ? 'player' : 'card',
                target
            );
        };
        document.addEventListener('contextmenu', onContextMenu);

        // Ctrl+K jumps to search from anywhere, including while an input has
        // focus (the YouTube-style palette gesture). Left Ctrl only, so it never
        // shadows the player's own unmodified `k` play/pause binding.
        const onKeyDown = (e: KeyboardEvent) => {
            if (e.ctrlKey && !e.altKey && !e.shiftKey && !e.metaKey && e.key === 'k') {
                e.preventDefault();
                void focusSearch();
            }
        };
        document.addEventListener('keydown', onKeyDown);

        initTheme();
        if (isTauri()) {
            // Settings first: theme and the startup-history rule both read from
            // them, so nothing below should run before they land.
            void loadSettings().then(() => {
                applyThemeSetting();
                if (settingBool('data.clear_history_on_start')) {
                    void api.historyClear().catch((e) => console.warn('clear history failed', e));
                }
            });
            api
                .bootstrap()
                .then((b) => {
                    if (b.innertubeReady) toast = null;
                    // Note: a brief false negative at startup is fine — the watch page
                    // re-bootstraps and the InnerTube client gets initialized lazily.
                })
                .catch((e) => (toast = `Bootstrap: ${String(e)}`));
        }

        return () => {
            document.removeEventListener('contextmenu', onContextMenu);
            document.removeEventListener('keydown', onKeyDown);
        };
    });
</script>

<svelte:head><title>Savvy</title></svelte:head>

<ModeWatcher />

<div class="flex h-screen flex-col overflow-hidden bg-background text-foreground rounded-[12px]">
    <Titlebar />
    <div class="relative flex min-h-0 flex-1">
        <Sidebar />
        <main class="min-w-0 flex-1 overflow-y-auto">
            {@render children()}
        </main>
    </div>
</div>

{#if toast}
    <div
        transition:fly={{ y: 16, duration: 220, easing: cubicOut }}
        class="fixed bottom-6 left-1/2 z-[100] flex -translate-x-1/2 items-center gap-3 rounded-lg border bg-card px-4 py-2 text-sm shadow-lg"
    >
        <span>{toast}</span>
        <Button
            variant="ghost"
            size="icon-sm"
            class="-mr-2 text-muted-foreground hover:text-foreground"
            aria-label="close"
            onclick={() => (toast = null)}
        >
            <HugeiconsIcon icon={Cancel01Icon} strokeWidth={2} />
        </Button>
    </div>
{/if}

<ContextMenu />
