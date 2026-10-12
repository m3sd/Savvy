<script lang="ts" module>
    export type ButtonVariant =
        | 'default'
        | 'destructive'
        | 'outline'
        | 'secondary'
        | 'ghost'
        | 'link';

    export type ButtonSize = 'default' | 'sm' | 'lg' | 'icon' | 'icon-sm';

    export type ButtonProps = {
        class?: string;
        variant?: ButtonVariant;
        size?: ButtonSize;
        type?: 'button' | 'submit' | 'reset';
        children?: import('svelte').Snippet;
        [key: string]: unknown;
    };
</script>

<script lang="ts">
    import { cn } from '$lib/utils';

    let {
        class: className,
        variant = 'default',
        size = 'default',
        type = 'button',
        children,
        ...rest
    }: ButtonProps = $props();

    const variantClasses: Record<string, string> = {
        default: 'bg-primary text-primary-foreground shadow-sm hover:bg-primary/90',
        destructive:
            'bg-destructive text-destructive-foreground shadow-xs hover:bg-destructive/90',
        outline:
            'border border-input bg-background shadow-xs hover:bg-accent hover:text-accent-foreground',
        secondary: 'bg-secondary text-secondary-foreground shadow-xs hover:bg-secondary/80',
        ghost: 'hover:bg-accent hover:text-accent-foreground',
        link: 'text-primary underline-offset-4 hover:underline'
    };

    const sizeClasses: Record<string, string> = {
        default: 'h-9 px-4 py-2',
        sm: 'h-8 rounded-md px-3 text-xs',
        lg: 'h-10 rounded-md px-8',
        icon: 'h-9 w-9',
        'icon-sm': 'h-8 w-8'
    };

    const base =
        "inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-sm font-medium transition-colors focus-visible:outline-hidden focus-visible:ring-1 focus-visible:ring-ring disabled:pointer-events-none disabled:opacity-50 [&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0";
</script>

<button
    {type}
    class={cn(base, variantClasses[variant], sizeClasses[size], className)}
    {...rest}
>
    {@render children?.()}
</button>
