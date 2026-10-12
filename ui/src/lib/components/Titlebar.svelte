<script lang="ts">
    import { browser } from '$app/environment';
    import { goto } from '$app/navigation';
    import { getCurrentWindow } from '@tauri-apps/api/window';
    import { HugeiconsIcon } from '@hugeicons/svelte';
    import {
        Settings01Icon,
        Sun01Icon,
        Moon02Icon
    } from '@hugeicons/core-free-icons';
    import { Button } from '$lib/components/ui/button';
    import { toggleMode } from 'mode-watcher';

    let win = browser ? getCurrentWindow() : null;
    let chromium = browser && '__TAURI_INTERNALS__' in window;
    let maximized = $state(false);

    $effect(() => {
        if (!win) return;
        win.isMaximized().then((v) => (maximized = v));
        const un = win.onResized(() => {
            win!.isMaximized().then((v) => (maximized = v));
        });
        return () => {
            un.then((u) => u());
        };
    });

    function minimize() {
        win?.minimize();
    }
    function toggleMax() {
        if (!win) return;
        if (maximized) win.unmaximize();
        else win.maximize();
    }
    function close() {
        win?.close();
    }
    function openSettings() {
        void goto('/settings');
    }
</script>

<header
    data-tauri-drag-region
    class="flex h-9 shrink-0 select-none items-center justify-between border-b bg-background/60 px-3 backdrop-blur"
>
    <div class="flex items-center gap-2">
        <div class="font-heading text-sm font-bold tracking-tight">Savvy</div>
        <span class="rounded-full bg-primary/10 px-2 py-0.5 text-[10px] font-medium text-primary">
            YouTube
        </span>
    </div>

    <div class="flex items-center gap-1">
        <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Toggle theme"
            onclick={() => toggleMode()}
        >
            <HugeiconsIcon icon={Sun01Icon} strokeWidth={2} class="h-4 w-4 dark:hidden" />
            <HugeiconsIcon icon={Moon02Icon} strokeWidth={2} class="hidden h-4 w-4 dark:block" />
        </Button>
        <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Settings"
            onclick={openSettings}
        >
            <HugeiconsIcon icon={Settings01Icon} strokeWidth={2} class="h-4 w-4" />
        </Button>

        <div class="mx-1 h-4 w-px bg-border"></div>

        <button
            type="button"
            aria-label="Minimize"
            onclick={minimize}
            class="flex h-7 w-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        >
            <svg viewBox="0 0 12 12" class="h-3 w-3" aria-hidden="true">
                <line x1="2" y1="6" x2="10" y2="6" stroke="currentColor" stroke-width="1" />
            </svg>
        </button>
        <button
            type="button"
            aria-label={maximized ? 'Restore' : 'Maximize'}
            onclick={toggleMax}
            class="flex h-7 w-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
        >
            {#if maximized}
                <svg viewBox="0 0 12 12" class="h-3 w-3" aria-hidden="true">
                    <rect x="2" y="4" width="6.5" height="6.5" fill="none" stroke="currentColor" stroke-width="1" />
                    <path d="M4.5 2 H9.5 A1.5 1.5 0 0 1 10 2.5 V7.5" fill="none" stroke="currentColor" stroke-width="1" />
                </svg>
            {:else}
                <svg viewBox="0 0 12 12" class="h-3 w-3" aria-hidden="true">
                    <rect x="2.5" y="2.5" width="7" height="7" fill="none" stroke="currentColor" stroke-width="1" />
                </svg>
            {/if}
        </button>
        <button
            type="button"
            aria-label="Close"
            onclick={close}
            class="flex h-7 w-9 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-destructive hover:text-destructive-foreground"
        >
            <svg viewBox="0 0 12 12" class="h-3 w-3" aria-hidden="true">
                <line x1="3" y1="3" x2="9" y2="9" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" />
                <line x1="9" y1="3" x2="3" y2="9" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" />
            </svg>
        </button>
    </div>
</header>
