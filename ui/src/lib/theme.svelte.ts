// Theme system for Savvy. The Catppuccin palette (mauve) is the default,
// the rest can be picked from the Themes panel and tuned via accent,
// hue, radius and font rows. The custom layer is inline vars applied
// after the preset class so the user's overrides win over the palette
// without re-baking any of its tokens.

import { browser } from '$app/environment';

export type ThemeId = 'catppuccin' | 'tokyonight' | 'caffeine' | 'neon' | 'breeze' | 'amoled';

const THEMES: { id: ThemeId; label: string }[] = [
    { id: 'catppuccin', label: 'Catppuccin' },
    { id: 'tokyonight', label: 'Tokyo Night' },
    { id: 'caffeine', label: 'Caffeine' },
    { id: 'neon', label: 'Neon' },
    { id: 'breeze', label: 'Breeze' },
    { id: 'amoled', label: 'AMOLED' }
];

export const FONT_OPTIONS: { label: string; value: string }[] = [
    { label: 'Oxanium', value: "'Oxanium Variable', sans-serif" },
    { label: 'IBM Plex Sans', value: "'IBM Plex Sans Variable', sans-serif" },
    { label: 'Montserrat', value: "'Montserrat Variable', sans-serif" },
    { label: 'Outfit', value: "'Outfit Variable', sans-serif" },
    { label: 'DM Sans', value: "'DM Sans Variable', sans-serif" },
    { label: 'System', value: 'ui-sans-serif, system-ui, sans-serif' }
];

export type Custom = {
    accent: string | null;
    hue: number | null;
    radius: number | null;
    fontSans: string | null;
    fontHeading: string | null;
    fontFiles: string[];
};

const KEY = 'savvy-theme';
const CUSTOM_KEY = 'savvy-custom';
const PALETTE_CLASSES = THEMES.map((t) => `theme-${t.id}`);

export const theme = $state<{ id: ThemeId }>({ id: 'catppuccin' });
export const custom = $state<Custom>({
    accent: null,
    hue: null,
    radius: null,
    fontSans: null,
    fontHeading: null,
    fontFiles: []
});

function apply(): void {
    if (!browser) return;
    const root = document.documentElement;
    root.classList.remove(...PALETTE_CLASSES);
    root.classList.add(`theme-${theme.id}`);

    if (custom.hue !== null) root.style.setProperty('--hue', String(custom.hue));
    else root.style.removeProperty('--hue');
    if (custom.radius !== null) root.style.setProperty('--radius', `${custom.radius}rem`);
    else root.style.removeProperty('--radius');
    if (custom.fontSans) root.style.setProperty('--font-sans', custom.fontSans);
    else root.style.removeProperty('--font-sans');
    if (custom.fontHeading) root.style.setProperty('--font-heading', custom.fontHeading);
    else root.style.removeProperty('--font-heading');
}

export function applyTheme(id: ThemeId): void {
    theme.id = THEMES.some((t) => t.id === id) ? id : 'catppuccin';
    apply();
    if (browser) localStorage.setItem(KEY, theme.id);
}

const persist = () => {
    if (browser) localStorage.setItem(CUSTOM_KEY, JSON.stringify(custom));
};

export function setCustom(patch: Partial<Custom>): void {
    Object.assign(custom, patch);
    apply();
    persist();
}

export function resetCustom(): void {
    setCustom({
        accent: null,
        hue: null,
        radius: null,
        fontSans: null,
        fontHeading: null
    });
}

export function isDefaultCustom(): boolean {
    return (
        !custom.accent &&
        custom.hue === null &&
        custom.radius === null &&
        !custom.fontSans &&
        !custom.fontHeading
    );
}

export function initTheme(): void {
    if (!browser) return;
    const stored = localStorage.getItem(KEY);
    theme.id = THEMES.some((t) => t.id === stored) ? (stored as ThemeId) : 'catppuccin';
    try {
        const saved = JSON.parse(localStorage.getItem(CUSTOM_KEY) ?? '{}');
        for (const k of ['accent', 'fontSans', 'fontHeading'] as const) {
            if (typeof saved?.[k] === 'string') custom[k] = saved[k];
        }
        for (const k of ['hue', 'radius'] as const) {
            if (typeof saved?.[k] === 'number') custom[k] = saved[k];
        }
        if (Array.isArray(saved?.fontFiles)) {
            custom.fontFiles = saved.fontFiles.filter((p: unknown) => typeof p === 'string');
        }
    } catch {
        // unparseable — start clean
    }
    apply();
}

export function familyName(stack: string): string {
    return (stack.split(',')[0] ?? '').replace(/["']/g, '').trim();
}

export { THEMES };
