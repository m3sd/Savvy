// Player keyboard bindings.
//
// One module owns the binding list so the Settings editor, the player's key
// handler and the defaults printed next to each row cannot drift apart.
// `DEFAULT_SHORTCUTS` mirrors `SHORTCUT_ACTIONS` in `src-tauri/src/db.rs`,
// which is what accepts a stored binding in the first place.
//
// A binding is a canonical combination — `Ctrl`, `Alt`, `Shift`, `Meta` in that
// order, then one key — for example `k`, `ArrowLeft` or `Ctrl+Alt+j`. The
// stored value is an *alternative*: the default key keeps working alongside it,
// and an empty (or invalid, or hand-edited) value simply adds nothing.

/** Actions the player understands, and the key each ships with. */
export const DEFAULT_SHORTCUTS: Record<string, string> = {
    playpause: 'k',
    back10: 'j',
    forward10: 'l',
    back5: 'ArrowLeft',
    forward5: 'ArrowRight',
    volumeUp: 'ArrowUp',
    volumeDown: 'ArrowDown',
    mute: 'm',
    fullscreen: 'f',
    theater: 't',
    captions: 'c'
};

/** Human names for the same actions, used as row labels in Settings. */
export const SHORTCUT_LABELS: Record<string, string> = {
    playpause: 'Play / pause',
    back10: 'Back 10 seconds',
    forward10: 'Forward 10 seconds',
    back5: 'Back 5 seconds',
    forward5: 'Forward 5 seconds',
    volumeUp: 'Volume up',
    volumeDown: 'Volume down',
    mute: 'Mute',
    fullscreen: 'Fullscreen',
    theater: 'Theater mode',
    captions: 'Captions'
};

/** Modifiers, in the only order a binding may spell them out in. */
const MODIFIERS = ['Ctrl', 'Alt', 'Shift', 'Meta'];

/** Named keys a binding may use instead of a single character. */
const NAMED_KEYS = [
    'ArrowLeft',
    'ArrowRight',
    'ArrowUp',
    'ArrowDown',
    'Home',
    'End',
    'PageUp',
    'PageDown',
    'Space'
];

const MODIFIER_KEY_NAMES = new Set(['Control', 'Alt', 'Shift', 'Meta', 'AltGraph', 'CapsLock']);

/** The name a `KeyboardEvent.key` is stored under. */
export function normalizeKey(key: string): string {
    if (key === ' ') return 'Space';
    return key.length === 1 ? key.toLowerCase() : key;
}

/** The canonical binding for a key press, e.g. `Ctrl+Shift+j`. */
export function comboOf(e: KeyboardEvent): string {
    const parts: string[] = [];
    if (e.ctrlKey) parts.push('Ctrl');
    if (e.altKey) parts.push('Alt');
    if (e.shiftKey) parts.push('Shift');
    if (e.metaKey) parts.push('Meta');
    parts.push(normalizeKey(e.key));
    return parts.join('+');
}

/** `Ctrl+Shift+j` reads as `Ctrl + Shift + j`. */
export function prettyCombo(combo: string): string {
    return combo.split('+').join(' + ');
}

/** True when the press is one of the modifier keys arriving on its own. */
export function isModifierOnly(e: KeyboardEvent): boolean {
    return MODIFIER_KEY_NAMES.has(e.key);
}

/**
 * Keys the player matches itself before it ever consults a binding: Space and
 * the digits are fixed (`play/pause`, seek-to-percentage), and `,` `.` `<` `>`
 * are the frame-step and speed pair. Accepting any of them as an alternative
 * would store a key that can never fire.
 */
const RESERVED_KEYS = new Set(['Space', ',', '.', '<', '>']);

/**
 * Why `combo` cannot be stored, or `null` when it can.
 *
 * Mirrors `is_shortcut_binding` in `db.rs`, which has the final say — this is
 * only so the editor can explain itself instead of failing silently on write.
 */
export function bindingProblem(combo: string): string | null {
    if (combo === '') return null;
    const parts = combo.split('+');
    const key = parts.pop() ?? '';
    const mods = parts;
    // Modifiers in the canonical order, each at most once; subsets are fine.
    let previous = -1;
    for (const modifier of mods) {
        const i = MODIFIERS.indexOf(modifier);
        if (i < 0 || i <= previous) return 'Modifiers run in the order Ctrl, Alt, Shift, Meta';
        previous = i;
    }
    if (RESERVED_KEYS.has(key)) return 'Fixed in the player — Space, 0-9, , . < >';
    if (/^[0-9]$/.test(key)) return 'Fixed in the player — Space, 0-9, , . < >';
    if (key.length === 1) return null;
    if (NAMED_KEYS.includes(key)) return null;
    return 'Pick a single key';
}
