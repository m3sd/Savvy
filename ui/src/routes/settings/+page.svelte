<script lang="ts">
    import { onMount } from 'svelte';
    import type { Snippet } from 'svelte';
    import { HugeiconsIcon } from '@hugeicons/svelte';
    import { Settings01Icon, RotateCcwIcon, TrashIcon, Shield01Icon } from '@hugeicons/core-free-icons';
    import { Button } from '$lib/components/ui/button';
    import { api, isTauri } from '$lib/api';
    import {
        applyThemeSetting,
        loadSettings,
        resetSetting,
        setSetting,
        settingBool,
        settingNum,
        settingStr
    } from '$lib/settings.svelte';
    import {
        DEFAULT_SHORTCUTS,
        SHORTCUT_LABELS,
        bindingProblem,
        comboOf,
        isModifierOnly,
        prettyCombo
    } from '$lib/shortcuts';

    /**
     * Height caps offered for the default rendition. `0` means "whatever the
     * highest available is" — the picker degrades down to what the video
     * actually offers rather than refusing to play.
     */
    const RESOLUTIONS = [
        ['0', 'Highest available'],
        ['2160', '2160p 4K'],
        ['1440', '1440p'],
        ['1080', '1080p'],
        ['720', '720p'],
        ['480', '480p'],
        ['360', '360p'],
        ['240', '240p'],
        ['144', '144p']
    ] as const;

    const HD_MODES = [
        ['auto', 'Automatic — HD when a suitable rendition exists'],
        ['always_360p', 'Always 360p — cheapest possible stream']
    ] as const;

    const THEMES = [
        ['system', 'Match the system'],
        ['light', 'Light'],
        ['dark', 'Dark']
    ] as const;

    /** The skip-capable SponsorBlock categories this app offers. */
    const SPONSOR_CATEGORIES = [
        ['sponsor', 'Sponsor'],
        ['selfpromo', 'Self-promotion'],
        ['interaction', 'Interaction reminders'],
        ['intro', 'Intro'],
        ['outro', 'Outro'],
        ['preview', 'Intermission preview'],
        ['hook', 'Hook'],
        ['filler', 'Filler'],
        ['music_offtopic', 'Off-topic music']
    ] as const;

    /** The settings rows the Shortcuts block writes, so Reset can cover them. */
    const SHORTCUT_KEYS = Object.keys(DEFAULT_SHORTCUTS).map((a) => `player.shortcuts.${a}`);

    /**
     * Starting points for the caption font. The field stays free-form, so any
     * CSS `font-family` the user types is honoured; these just save typing.
     */
    const CAPTION_FONTS = [
        ['', 'App default'],
        ['system-ui, -apple-system, sans-serif', 'Sans-serif'],
        ['Georgia, ui-serif, serif', 'Serif'],
        ['ui-monospace, SFMono-Regular, Menlo, monospace', 'Monospace'],
        ['"Comic Sans MS", "Comic Sans", cursive', 'Comic Sans']
    ] as const;

    let busy = $state<string | null>(null);
    let flash = $state<string | null>(null);
    /** Two-step confirm: the action the user must click again to run. */
    let armed = $state<string | null>(null);
    /** The action whose alternative is being recorded right now. */
    let capturing = $state<string | null>(null);
    /** Why the last key press was refused, so the editor can explain itself. */
    let captureMessage = $state<string | null>(null);

    onMount(async () => {
        await loadSettings();
        applyThemeSetting();
    });

    function changeTheme(value: string): void {
        setSetting('ui.theme', value);
        applyThemeSetting();
    }

    function resetGroup(keys: string[]): void {
        for (const k of keys) resetSetting(k);
        if (keys.includes('ui.theme')) applyThemeSetting();
        flash = 'Defaults restored';
    }

    /**
     * Destructive actions need a second click.
     *
     * This deliberately avoids `window.confirm`/`alert`: those are unreliable
     * in Tauri webviews and can resolve immediately, which would turn a
     * destructive action into a single mis-click.
     */
    function guarded(name: string, run: () => Promise<void>): () => void {
        return () => {
            if (armed !== name) {
                armed = name;
                flash = null;
                window.setTimeout(() => {
                    if (armed === name) armed = null;
                }, 4000);
                return;
            }
            armed = null;
            busy = name;
            flash = null;
            run().catch((e) => (flash = String(e))).finally(() => (busy = null));
        };
    }

    function sponsorCategoriesList(): string[] {
        return settingStr('sponsorblock.categories')
            .split(',')
            .filter((c) => c.length > 0);
    }

    function sponsorCategoryOn(value: string): boolean {
        return sponsorCategoriesList().includes(value);
    }

    function toggleSponsorCategory(value: string, on: boolean): void {
        let list = sponsorCategoriesList();
        list = on ? [...new Set([...list, value])] : list.filter((c) => c !== value);
        setSetting('sponsorblock.categories', list.join(','));
    }

    /** The stored alternative for an action; '' when it has none. */
    function shortcutAlt(action: string): string {
        return settingStr(`player.shortcuts.${action}`);
    }

    function beginCapture(action: string): void {
        if (capturing === action) {
            capturing = null;
            captureMessage = null;
            return;
        }
        capturing = action;
        captureMessage = null;
    }

    /** Clearing is a write of '': the default key is what stays, not a row delete. */
    function clearAlternative(action: string): void {
        setSetting(`player.shortcuts.${action}`, '');
    }

    function assignAlternative(action: string, combo: string): void {
        // Recording the default key back means "no alternative" — a second key
        // identical to the first is noise, not a binding.
        if (combo.toLowerCase() === DEFAULT_SHORTCUTS[action].toLowerCase()) {
            clearAlternative(action);
            captureMessage = `${SHORTCUT_LABELS[action]} already uses that key — alternative cleared`;
            return;
        }
        for (const other of Object.keys(DEFAULT_SHORTCUTS)) {
            if (other === action) continue;
            if (DEFAULT_SHORTCUTS[other].toLowerCase() === combo.toLowerCase()) {
                captureMessage = `${prettyCombo(combo)} is the default for ${SHORTCUT_LABELS[other]}`;
                return;
            }
            // One alternative cannot belong to two actions: the previous owner
            // loses it, which is what the user meant by assigning it here.
            if (shortcutAlt(other).toLowerCase() === combo.toLowerCase()) clearAlternative(other);
        }
        setSetting(`player.shortcuts.${action}`, combo);
        captureMessage = null;
    }

    /** Record the next key press while a row is armed. */
    $effect(() => {
        const action = capturing;
        if (action === null) return;
        const onKeydown = (e: KeyboardEvent) => {
            e.preventDefault();
            e.stopPropagation();
            if (e.repeat) return;
            if (e.key === 'Escape') {
                capturing = null;
                captureMessage = null;
                return;
            }
            if (e.key === 'Backspace' || e.key === 'Delete') {
                clearAlternative(action);
                capturing = null;
                captureMessage = null;
                return;
            }
            if (isModifierOnly(e)) return;
            const combo = comboOf(e);
            const problem = bindingProblem(combo);
            if (problem) {
                captureMessage = `${prettyCombo(combo)} — ${problem}`;
                return;
            }
            assignAlternative(action, combo);
            capturing = null;
        };
        window.addEventListener('keydown', onKeydown, true);
        return () => window.removeEventListener('keydown', onKeydown, true);
    });
</script>

{#snippet heading(title: string, hint?: string, reset?: () => void)}
    <div class="flex items-end justify-between gap-4">
        <div>
            <h2 class="font-heading text-base font-semibold">{title}</h2>
            {#if hint}
                <p class="mt-0.5 text-xs text-muted-foreground">{hint}</p>
            {/if}
        </div>
        {#if reset}
            <Button variant="ghost" size="sm" class="gap-1.5 text-muted-foreground" onclick={reset}>
                <HugeiconsIcon icon={RotateCcwIcon} strokeWidth={2} class="h-3.5 w-3.5" />
                Reset
            </Button>
        {/if}
    </div>
{/snippet}

{#snippet row(label: string, hint: string, control: Snippet)}
    <div class="flex flex-col gap-3 border-t border-border/60 py-3 sm:flex-row sm:items-center sm:justify-between sm:gap-8">
        <div class="min-w-0">
            <p class="text-sm font-medium">{label}</p>
            {#if hint}
                <p class="mt-0.5 max-w-prose text-xs text-muted-foreground">{hint}</p>
            {/if}
        </div>
        <div class="shrink-0">{@render control()}</div>
    </div>
{/snippet}

{#snippet resolutionSelect()}
    <select
        class="h-8 w-52 rounded-md border bg-card px-2 text-sm outline-hidden focus:ring-1 focus:ring-ring"
        aria-label="Default resolution"
        onchange={(e) => setSetting('player.default_resolution', e.currentTarget.value)}
    >
        {#each RESOLUTIONS as [value, label]}
            <option value={value} selected={settingStr('player.default_resolution') === value}>
                {label}
            </option>
        {/each}
    </select>
{/snippet}

{#snippet volumeSlider()}
    <div class="flex items-center gap-3">
        <input
            type="range"
            min="0"
            max="100"
            step="1"
            class="w-44"
            aria-label="Default volume"
            value={settingNum('player.default_volume')}
            oninput={(e) => setSetting('player.default_volume', e.currentTarget.value)}
        />
        <span class="w-10 text-right text-sm tabular-nums text-muted-foreground">
            {settingNum('player.default_volume')}%
        </span>
    </div>
{/snippet}

{#snippet hdModeSelect()}
    <select
        class="h-8 w-80 rounded-md border bg-card px-2 text-sm outline-hidden focus:ring-1 focus:ring-ring"
        aria-label="Stream complexity"
        onchange={(e) => setSetting('playback.hd_mode', e.currentTarget.value)}
    >
        {#each HD_MODES as [value, label]}
            <option value={value} selected={settingStr('playback.hd_mode') === value}>{label}</option>
        {/each}
    </select>
{/snippet}

{#snippet themeSelect()}
    <select
        class="h-8 w-48 rounded-md border bg-card px-2 text-sm outline-hidden focus:ring-1 focus:ring-ring"
        aria-label="Appearance"
        onchange={(e) => changeTheme(e.currentTarget.value)}
    >
        {#each THEMES as [value, label]}
            <option value={value} selected={settingStr('ui.theme') === value}>{label}</option>
        {/each}
    </select>
{/snippet}

{#snippet sidebarToggle()}
    <input
        type="checkbox"
        class="h-4 w-4 accent-primary"
        aria-label="Compact sidebar"
        checked={settingBool('ui.compact_sidebar')}
        onchange={(e) => setSetting('ui.compact_sidebar', String(e.currentTarget.checked))}
    />
{/snippet}

{#snippet captionFontInput()}
    <div class="flex w-full flex-col gap-2 sm:w-80">
        <input
            type="text"
            class="h-8 w-full rounded-md border bg-card px-2 text-sm outline-hidden focus:ring-1 focus:ring-ring"
            aria-label="Caption font"
            placeholder="App default"
            value={settingStr('captions.font')}
            oninput={(e) => setSetting('captions.font', e.currentTarget.value)}
        />
        <div class="flex flex-wrap gap-1.5">
            {#each CAPTION_FONTS as [value, label]}
                <button
                    type="button"
                    class="rounded-full border px-2.5 py-0.5 text-xs outline-hidden hover:bg-muted focus:ring-1 focus:ring-ring {settingStr(
                        'captions.font'
                    ) === value
                        ? 'border-primary text-primary'
                        : 'text-muted-foreground'}"
                    onclick={() => setSetting('captions.font', value)}
                >
                    {label}
                </button>
            {/each}
        </div>
        <div class="rounded-md bg-black px-3 py-2">
            <p
                class="text-center text-base font-medium text-white [text-shadow:0_1px_2px_rgba(0,0,0,0.9),0_0_4px_rgba(0,0,0,0.9)]"
                style:font-family={settingStr('captions.font').trim() || undefined}
            >
                The quick brown fox jumps over the lazy dog
            </p>
        </div>
    </div>
{/snippet}

{#snippet clearOnStartToggle()}
    <input
        type="checkbox"
        class="h-4 w-4 accent-primary"
        aria-label="Clear history on startup"
        checked={settingBool('data.clear_history_on_start')}
        onchange={(e) => setSetting('data.clear_history_on_start', String(e.currentTarget.checked))}
    />
{/snippet}

{#snippet clearHistoryButton()}
    <Button
        variant="outline"
        size="sm"
        class="gap-1.5"
        disabled={busy === 'history'}
        onclick={guarded('history', async () => {
            await api.historyClear();
            flash = 'History cleared';
        })}
    >
        <HugeiconsIcon icon={TrashIcon} strokeWidth={2} class="h-4 w-4" />
        {armed === 'history' ? 'Click again to confirm' : 'Clear history'}
    </Button>
{/snippet}

{#snippet resetIdentityButton()}
    <Button
        variant="outline"
        size="sm"
        class="gap-1.5"
        disabled={busy === 'identity' || !isTauri()}
        onclick={guarded('identity', async () => {
            await api.innertubeReset();
            flash = 'Identity discarded — a new one is generated on the next request';
        })}
    >
        <HugeiconsIcon icon={Shield01Icon} strokeWidth={2} class="h-4 w-4" />
        {armed === 'identity' ? 'Click again to confirm' : 'Reset identity'}
    </Button>
{/snippet}

{#snippet sponsorToggle()}
    <input
        type="checkbox"
        class="h-4 w-4 accent-primary"
        aria-label="Skip sponsored segments"
        checked={settingBool('sponsorblock.enabled')}
        onchange={(e) => setSetting('sponsorblock.enabled', String(e.currentTarget.checked))}
    />
{/snippet}

{#snippet sponsorModeSelect()}
    <select
        class="h-8 w-64 rounded-md border bg-card px-2 text-sm outline-hidden focus:ring-1 focus:ring-ring disabled:opacity-50"
        aria-label="Skip mode"
        disabled={!settingBool('sponsorblock.enabled')}
        onchange={(e) => setSetting('sponsorblock.mode', e.currentTarget.value)}
    >
        <option value="auto" selected={settingStr('sponsorblock.mode') === 'auto'}>Automatic</option>
        <option value="manual" selected={settingStr('sponsorblock.mode') === 'manual'}>Manual — show a skip button</option>
    </select>
{/snippet}

{#snippet sponsorCategories()}
    <div class="grid grid-cols-2 gap-x-6 gap-y-1.5">
        {#each SPONSOR_CATEGORIES as [value, label]}
            <label
                class="flex items-center gap-2 text-sm {settingBool('sponsorblock.enabled')
                    ? ''
                    : 'opacity-50'}"
            >
                <input
                    type="checkbox"
                    class="h-3.5 w-3.5 accent-primary"
                    disabled={!settingBool('sponsorblock.enabled')}
                    checked={sponsorCategoryOn(value)}
                    onchange={(e) => toggleSponsorCategory(value, e.currentTarget.checked)}
                />
                {label}
            </label>
        {/each}
    </div>
{/snippet}

{#snippet shortcutControl(action: string)}
    {@const alt = shortcutAlt(action)}
    <div class="flex items-center justify-end gap-1.5">
        <kbd
            class="rounded border bg-card px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground"
        >
            {prettyCombo(DEFAULT_SHORTCUTS[action])}
        </kbd>
        <span class="text-[11px] text-muted-foreground">or</span>
        <button
            type="button"
            class="min-w-36 rounded border px-2 py-1 font-mono text-[11px] outline-hidden focus:ring-1 focus:ring-ring {capturing ===
            action
                ? 'border-primary text-primary'
                : 'bg-card'}"
            onclick={() => beginCapture(action)}
        >
            {capturing === action
                ? 'Press a key…'
                : alt
                  ? prettyCombo(alt)
                  : 'Add alternative'}
        </button>
        {#if alt}
            <button
                type="button"
                class="h-6 w-6 rounded text-muted-foreground hover:bg-muted"
                aria-label="Clear the alternative for {SHORTCUT_LABELS[action]}"
                onclick={() => clearAlternative(action)}
            >
                ×
            </button>
        {/if}
    </div>
{/snippet}

<div class="mx-auto flex max-w-3xl flex-col gap-8 p-8">
    <div class="flex items-center gap-3">
        <HugeiconsIcon icon={Settings01Icon} class="h-6 w-6 text-primary" strokeWidth={2} />
        <div>
            <h1 class="font-heading text-3xl font-bold tracking-tight">Settings</h1>
            <p class="mt-1 text-sm text-muted-foreground">
                Stored locally in the app. Nothing here is sent to YouTube.
            </p>
        </div>
    </div>

    {#if flash}
        <div class="rounded-lg border bg-card px-4 py-3 text-sm">{flash}</div>
    {/if}

    <!-- ─── Playback ───────────────────────────────────────────────────── -->
    <section class="flex flex-col gap-1">
        {@render heading(
            'Playback',
            'Applies to newly opened videos.',
            () =>
                resetGroup([
                    'player.default_resolution',
                    'player.default_volume',
                    'playback.hd_mode',
                    ...SHORTCUT_KEYS
                ])
        )}

        {@render row(
            'Default resolution',
            'The highest rendition at or below this height, so a 4K video still plays in 1080p. A video with nothing that low falls back to its own best.',
            resolutionSelect
        )}
        {@render row(
            'Default volume',
            'Used when a video starts. Unlike the browser, the app keeps this across sites and sessions.',
            volumeSlider
        )}
        {@render row(
            'Stream complexity',
            'A way back to 360p if the adaptive track ever misbehaves on this machine.',
            hdModeSelect
        )}

        <div class="border-t border-border/60 pt-3">
            <p class="text-sm font-medium">Shortcuts</p>
            <p class="mt-0.5 max-w-prose text-xs text-muted-foreground">
                The default key always works; an alternative adds a second key for the same
                action. Click a slot to record — Esc cancels, Backspace clears.
            </p>
            {#if capturing}
                <p class="mt-1.5 text-xs text-primary">
                    Press a key (with its modifiers held)…
                </p>
            {:else if captureMessage}
                <p class="mt-1.5 text-xs text-muted-foreground">{captureMessage}</p>
            {/if}
        </div>
        {#each Object.keys(DEFAULT_SHORTCUTS) as action (action)}
            <div
                class="flex flex-col gap-3 border-t border-border/60 py-3 sm:flex-row sm:items-center sm:justify-between sm:gap-8"
            >
                <div class="min-w-0">
                    <p class="text-sm font-medium">{SHORTCUT_LABELS[action]}</p>
                    <p class="mt-0.5 max-w-prose text-xs text-muted-foreground">
                        Adds a second key; the default keeps working.
                    </p>
                </div>
                <div class="shrink-0">{@render shortcutControl(action)}</div>
            </div>
        {/each}
    </section>

    <!-- ─── Interface ──────────────────────────────────────────────────── -->
    <section class="flex flex-col gap-1">
        {@render heading('Interface', undefined, () =>
            resetGroup(['ui.theme', 'ui.compact_sidebar'])
        )}

        {@render row('Appearance', 'Follow the system, or force a mode.', themeSelect)}
        {@render row('Compact sidebar', 'Narrower navigation rail showing icons only.', sidebarToggle)}
    </section>

    <!-- ─── Captions ───────────────────────────────────────────────────── -->
    <section class="flex flex-col gap-1">
        {@render heading(
            'Captions',
            'How subtitles are drawn over the player.',
            () => resetGroup(['captions.font'])
        )}

        {@render row(
            'Font',
            'Any CSS font-family value — type a name like Georgia or a full stack like "Fira Sans", sans-serif. Leave blank for the app default. Fonts installed on this machine are used; others fall back.',
            captionFontInput
        )}
    </section>

    <!-- ─── Data ───────────────────────────────────────────────────────── -->
    <section class="flex flex-col gap-1">
        {@render heading('Data', undefined, () => resetGroup(['data.clear_history_on_start']))}

        {@render row(
            'Clear history on startup',
            'Erase the watch history every time the app launches.',
            clearOnStartToggle
        )}
        {@render row(
            'Clear history now',
            'Removes every watched video and resume point immediately.',
            clearHistoryButton
        )}
    </section>

    <!-- ─── Network ────────────────────────────────────────────────────── -->
    <section class="flex flex-col gap-1">
        {@render heading(
            'Network',
            'Anonymous by default — no account, no cookies, no telemetry.'
        )}

        {@render row(
            'Reset identity',
            'Discard the stored visitor data and start a fresh anonymous session. Worth doing if YouTube starts returning bot checks.',
            resetIdentityButton
        )}
    </section>

    <!-- ─── SponsorBlock ──────────────────────────────────────────────── -->
    <section class="flex flex-col gap-1">
        {@render heading(
            'SponsorBlock',
            'Community-flagged segments, fetched anonymously. The app never submits or votes on anything.',
            () => resetGroup(['sponsorblock.enabled', 'sponsorblock.mode', 'sponsorblock.categories'])
        )}

        {@render row(
            'Skip segments',
            'Fetches the selected segment types and skips them while playing.',
            sponsorToggle
        )}
        {@render row(
            'Skip mode',
            'Automatic skips segments as they start. Manual shows a skip button instead.',
            sponsorModeSelect
        )}
        {@render row(
            'Segment types',
            'Only the categories a segment skip can honour are offered; other types (chapters, points of interest, exclusive content) are never requested.',
            sponsorCategories
        )}
    </section>
</div>
