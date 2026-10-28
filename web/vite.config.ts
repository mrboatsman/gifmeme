import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
	plugins: [
		sveltekit({
			compilerOptions: {
				// Force runes mode for the project, except for libraries. Can be removed in svelte 6.
				runes: ({ filename }) =>
					filename.split(/[/\\]/).includes('node_modules') ? undefined : true
			},
			// Pure SPA served by the Rust backend, which falls back to 200.html.
			adapter: adapter({ fallback: '200.html' })
		})
	],
	server: {
		proxy: {
			'/api': 'http://127.0.0.1:8080',
			'/files': 'http://127.0.0.1:8080'
		}
	}
});
