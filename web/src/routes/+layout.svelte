<script lang="ts">
	import favicon from '#lib/assets/favicon.svg';
	import { api, ago, getUser, setUnauthorizedHandler, setUser } from '#lib/api.ts';
	import { app, refreshStatus } from '#lib/app.svelte.ts';
	import type { LayoutProps } from './$types';
	import './app.css';

	let { children }: LayoutProps = $props();

	let pin = $state('');
	let loginError = $state('');
	let loggingIn = $state(false);
	let user = $state(getUser());
	let ready = $state(false);

	setUnauthorizedHandler(() => (app.needLogin = true));

	let isMatrixUser = $derived(app.config?.user?.startsWith('@') ?? false);

	async function init() {
		// Set by the SSO callback when the login was refused.
		const params = new URLSearchParams(location.search);
		const err = params.get('login_error');
		if (err) {
			loginError = err;
			history.replaceState(null, '', location.pathname);
		}
		try {
			app.config = await api.config();
			app.needLogin = app.config.auth.enabled && !app.config.logged_in;
			if (!app.needLogin) await refreshStatus();
		} catch (e) {
			app.error = `Backend is not responding: ${(e as Error).message}`;
		}
		ready = true;
	}

	$effect(() => {
		init();
		const t = setInterval(() => {
			if (!app.needLogin && !document.hidden) refreshStatus();
		}, 15000);
		return () => clearInterval(t);
	});

	async function login(e: SubmitEvent) {
		e.preventDefault();
		loggingIn = true;
		loginError = '';
		try {
			await api.login(pin);
			pin = '';
			app.needLogin = false;
			await refreshStatus();
		} catch (err) {
			loginError = (err as Error).message;
		} finally {
			loggingIn = false;
		}
	}

	let matrix = $derived(app.status?.matrix);
	let health = $derived.by(() => {
		if (!app.status) return { cls: 'muted', text: '…' };
		if (!matrix?.connected) return { cls: 'bad', text: 'Matrix disconnected' };
		if (app.status.failing_packs > 0 || app.status.pending_uploads > 0)
			return { cls: 'warn', text: 'Syncing…' };
		return { cls: 'ok', text: 'Synced' };
	});
	let healthTitle = $derived(
		[
			matrix?.error,
			matrix?.user_id && `Bot: ${matrix.user_id}`,
			`Last full sync: ${ago(matrix?.last_full_sync ?? null)}`,
			matrix?.last_full_sync_error,
			app.status?.pending_uploads ? `${app.status.pending_uploads} waiting for upload` : '',
			app.status?.failing_packs ? `${app.status.failing_packs} packs failing to sync` : ''
		]
			.filter(Boolean)
			.join('\n')
	);
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
</svelte:head>

<header>
	<a class="brand" href="/">
		<img src={favicon} alt="" width="28" height="28" />
		<span>gif.lan</span>
	</a>
	<nav>
		<a href="/help">Help</a>
		{#if app.config?.role === 'admin'}<a href="/settings">Settings</a>{/if}
	</nav>
	<div class="spacer"></div>
	{#if ready && !app.needLogin}
		{#if isMatrixUser}
			<span class="who" title="Logged in with Matrix ({app.config?.role})"
				>{app.config?.user}{#if app.config?.role === 'admin'}<span class="role">admin</span>{/if}</span
			>
		{:else}
			<label class="who" title="Stored on uploaded stickers">
				<span>You:</span>
				<input
					placeholder="your name"
					bind:value={user}
					onchange={() => setUser(user)}
					maxlength="64"
				/>
			</label>
		{/if}
		<button
			class="pill {health.cls}"
			title={healthTitle}
			onclick={async () => {
				await api.resync();
				setTimeout(refreshStatus, 3000);
			}}
		>
			<span class="dot"></span>{health.text}
		</button>
		{#if app.config?.auth.enabled}
			<button
				class="ghost"
				onclick={async () => {
					await api.logout();
					location.href = '/';
				}}>Log out</button
			>
		{/if}
	{/if}
</header>

<main>
	{#if app.error}
		<p class="error">{app.error}</p>
	{:else if !ready}
		<p class="muted">Loading…</p>
	{:else if app.needLogin}
		<div class="login card">
			<h1>Log in</h1>
			{#if app.config?.auth.matrix}
				<p class="muted">With your Matrix account.</p>
				<a class="button primary" href="/api/auth/matrix/start">Log in with Matrix</a>
			{/if}
			{#if app.config?.auth.pin}
				<form onsubmit={login}>
					<p class="muted">{app.config.auth.matrix ? 'Or use' : 'Enter'} the shared PIN.</p>
					<input
						type="password"
						inputmode="numeric"
						autocomplete="current-password"
						aria-label="PIN"
						bind:value={pin}
					/>
					<button type="submit" disabled={loggingIn || !pin}>Log in with PIN</button>
				</form>
			{/if}
			{#if loginError}<p class="error">{loginError}</p>{/if}
		</div>
	{:else}
		{@render children()}
	{/if}
</main>

<style>
	header {
		display: flex;
		align-items: center;
		gap: 1rem;
		padding: 0.75rem 1.25rem;
		border-bottom: 1px solid var(--border);
		background: var(--surface);
		position: sticky;
		top: 0;
		z-index: 10;
		flex-wrap: wrap;
	}
	.brand {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		font-weight: 800;
		font-size: 1.15rem;
		color: var(--text);
		text-decoration: none;
	}
	nav {
		display: flex;
		gap: 1rem;
	}
	nav a {
		color: var(--muted);
	}
	.spacer {
		flex: 1;
	}
	.who {
		display: flex;
		align-items: center;
		gap: 0.4rem;
		color: var(--muted);
		font-size: 0.9rem;
	}
	.role {
		margin-left: 0.4rem;
		font-size: 0.75rem;
		padding: 0.05rem 0.4rem;
		border-radius: 999px;
		background: var(--surface-2);
		color: var(--accent);
	}
	.who input {
		width: 9rem;
		padding: 0.3rem 0.5rem;
	}
	.pill {
		display: inline-flex;
		align-items: center;
		gap: 0.4rem;
		border-radius: 999px;
		padding: 0.3rem 0.75rem;
		font-size: 0.85rem;
		white-space: pre-line;
	}
	.dot {
		width: 0.55rem;
		height: 0.55rem;
		border-radius: 50%;
		background: currentColor;
	}
	.pill.ok {
		color: var(--ok);
	}
	.pill.warn {
		color: var(--warn);
	}
	.pill.bad {
		color: var(--bad);
	}
	main {
		max-width: 72rem;
		margin: 0 auto;
		padding: 1.5rem 1.25rem 4rem;
	}
	.login {
		max-width: 22rem;
		margin: 4rem auto;
		display: grid;
		gap: 0.75rem;
	}
	.login h1 {
		margin: 0;
	}
	.login p {
		margin: 0;
	}
	.login form {
		display: grid;
		gap: 0.5rem;
		margin-top: 0.75rem;
	}
</style>
