import tailwindcss from '@tailwindcss/vite';
import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
    server: {
        port: 5183,
        strictPort: true
    },
    plugins: [
        {
            name: 'savvy-dev-referrer-policy',
            configureServer(server) {
                server.middlewares.use((_req, res, next) => {
                    res.setHeader('Referrer-Policy', 'no-referrer');
                    next();
                });
            }
        },
        tailwindcss(),
        sveltekit({
            compilerOptions: {
                runes: ({ filename }) =>
                    filename.split(/[/\\]/).includes('node_modules') ? undefined : true
            },
            adapter: adapter({ fallback: 'index.html' })
        })
    ]
});
