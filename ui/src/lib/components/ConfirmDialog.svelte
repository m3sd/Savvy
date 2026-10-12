<script lang="ts">
    let {
        open = $bindable(false),
        title,
        message,
        confirmLabel = 'Confirm',
        cancelLabel = 'Cancel',
        onconfirm
    }: {
        open: boolean;
        title: string;
        message?: string;
        confirmLabel?: string;
        cancelLabel?: string;
        onconfirm: () => void;
    } = $props();

    function cancel() {
        open = false;
    }

    function accept() {
        open = false;
        onconfirm();
    }
</script>

{#if open}
    <div
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabindex="-1"
        class="fixed inset-0 z-[220] flex items-center justify-center bg-black/50 p-4"
        onclick={(e) => {
            if (e.target === e.currentTarget) cancel();
        }}
        onkeydown={(e) => {
            if (e.key === 'Escape') cancel();
        }}
    >
        <div
            class="flex w-[min(26rem,92vw)] flex-col gap-3 rounded-lg border bg-background p-4 shadow-xl"
        >
            <h2 class="font-heading text-sm font-semibold">{title}</h2>
            {#if message}
                <p class="text-sm text-muted-foreground">{message}</p>
            {/if}
            <div class="mt-1 flex justify-end gap-2">
                <!-- svelte-ignore a11y_autofocus -->
                <button
                    type="button"
                    autofocus
                    class="rounded-md px-3 py-1.5 text-sm text-muted-foreground hover:bg-muted hover:text-foreground"
                    onclick={cancel}
                >
                    {cancelLabel}
                </button>
                <button
                    type="button"
                    class="rounded-md bg-destructive px-3 py-1.5 text-sm font-medium text-destructive-foreground hover:bg-destructive/90"
                    onclick={accept}
                >
                    {confirmLabel}
                </button>
            </div>
        </div>
    </div>
{/if}
