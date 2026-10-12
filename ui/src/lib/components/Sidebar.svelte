<script lang="ts">
    import { page } from '$app/state';
    import { HugeiconsIcon } from '@hugeicons/svelte';
    import { settingBool } from '$lib/settings.svelte';
    import {
        Home01Icon,
        Search01Icon,
        LibraryIcon,
        HistoryIcon,
        PlayListIcon,
        UserGroupIcon,
        Settings01Icon
    } from '@hugeicons/core-free-icons';

    const nav = [
        { href: '/', label: 'Home', icon: Home01Icon },
        { href: '/search', label: 'Search', icon: Search01Icon },
        { href: '/library', label: 'Library', icon: LibraryIcon },
        { href: '/history', label: 'History', icon: HistoryIcon },
        { href: '/playlists', label: 'Playlists', icon: PlayListIcon },
        { href: '/subscriptions', label: 'Subscriptions', icon: UserGroupIcon }
    ];

const isActive = (href: string) =>
        href === '/' ? page.url.pathname === '/' : page.url.pathname.startsWith(href);

    /** Icons only, regardless of viewport width. */
    let compact = $derived(settingBool('ui.compact_sidebar'));
</script>

<aside
    class="flex h-full w-16 shrink-0 flex-col border-r bg-sidebar p-3 text-sidebar-foreground {compact
        ? 'lg:w-16'
        : 'lg:w-60'}"
>
    <nav class="mt-2 flex flex-col gap-1">
        {#each nav as n (n.href)}
            <a
                href={n.href}
                title={n.label}
                class="group relative flex items-center gap-3 rounded-lg px-3 py-2 text-sm font-medium transition-colors {compact
                    ? 'justify-center'
                    : 'lg:justify-start'} {isActive(n.href)
                    ? 'bg-primary/10 text-primary'
                    : 'text-sidebar-foreground/70 hover:bg-sidebar-accent/50 hover:text-sidebar-foreground'}"
            >
                <HugeiconsIcon
                    icon={n.icon}
                    class="h-5 w-5 shrink-0 transition-transform duration-200 group-hover:scale-110"
                    strokeWidth={2}
                />
                {#if !compact}
                    <span class="hidden lg:inline">{n.label}</span>
                {/if}
                {#if !compact && n.href === '/search'}
                    <kbd
                        class="ml-auto hidden rounded border px-1.5 py-0.5 text-[10px] font-medium text-muted-foreground lg:inline"
                        >Ctrl K</kbd
                    >
                {/if}
            </a>
        {/each}
    </nav>

    <div class="mt-auto pt-3">
        <a
            href="/settings"
            title="Settings"
            class="group flex items-center gap-2 rounded-lg px-3 py-2 text-sm font-medium transition-colors {compact
                ? 'justify-center'
                : 'lg:w-full'} {isActive('/settings')
                ? 'bg-primary/10 text-primary'
                : 'text-sidebar-foreground/70 hover:bg-sidebar-accent/50 hover:text-sidebar-foreground'}"
        >
            <HugeiconsIcon
                icon={Settings01Icon}
                class="h-4 w-4 shrink-0 transition-transform duration-200 group-hover:rotate-45"
                strokeWidth={2}
            />
            {#if !compact}
                <span class="hidden lg:inline">Settings</span>
            {/if}
        </a>
    </div>
</aside>
