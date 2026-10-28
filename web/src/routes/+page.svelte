<script lang="ts">
	import { goto } from '$app/navigation';
	import { api, ago, roomLabel, roomLink, type Category, type Room } from '#lib/api.ts';
	import { app } from '#lib/app.svelte.ts';

	let categories = $state<Category[] | null>(null);
	let rooms = $state<Room[]>([]);
	let loadError = $state('');
	let name = $state('');
	let creating = $state(false);
	let createError = $state('');

	async function load() {
		try {
			categories = await api.categories();
			loadError = '';
			api.rooms().then((r) => (rooms = r)).catch(() => {});
		} catch (e) {
			loadError = (e as Error).message;
		}
	}

	$effect(() => {
		load();
	});

	async function create(e: SubmitEvent) {
		e.preventDefault();
		creating = true;
		createError = '';
		try {
			const c = await api.createCategory(name.trim());
			name = '';
			await goto(`/c/${c.slug}`);
		} catch (err) {
			createError = (err as Error).message;
		} finally {
			creating = false;
		}
	}

	let total = $derived(categories?.reduce((n, c) => n + c.count, 0) ?? 0);
</script>

<div class="head">
	<div>
		<h1>Sticker packs</h1>
		<p class="muted">
			{categories?.length ?? 0} categories · {total} stickers
			{#if rooms.length}
				in
				{#each rooms as r, i (r.id)}{i ? ', ' : ''}<a href={roomLink(r)} target="_blank" rel="noreferrer"
						>{roomLabel(r)}</a
					>{/each}
			{:else if app.config?.role === 'admin'}
				· <a href="/settings">add a room</a> to publish them
			{/if}
		</p>
	</div>
	<form class="new" onsubmit={create}>
		<input placeholder="New category, e.g. Cats" bind:value={name} maxlength="64" />
		<button class="primary" type="submit" disabled={creating || !name.trim()}>Create</button>
	</form>
</div>
{#if createError}<p class="error">{createError}</p>{/if}

{#if loadError}
	<p class="error">{loadError}</p>
{:else if !categories}
	<p class="muted">Loading…</p>
{:else if categories.length === 0}
	<div class="card empty">
		<p>No categories yet. Create one above and drop in some gifs.</p>
	</div>
{:else}
	<ul class="grid">
		{#each categories as c (c.id)}
			<li>
				<a class="card cat" href="/c/{c.slug}">
					<div class="previews">
						{#each c.preview as src (src)}
							<img {src} alt="" loading="lazy" />
						{:else}
							<span class="muted">empty</span>
						{/each}
					</div>
					<div class="meta">
						<strong>{c.display_name}</strong>
						<span class="count" class:full={c.count >= c.max}>{c.count}/{c.max}</span>
					</div>
					<div class="sub muted">
						<code>{c.slug}</code>
						{#if c.rooms.length === 0}
							<span class="warn" title="Not published in any room">unpublished</span>
						{:else if c.sync_error}
							<span class="error" title={c.sync_error}>sync error</span>
						{:else}
							<span title={c.event_id ?? ''}>synced {ago(c.synced_at)}</span>
						{/if}
					</div>
				</a>
			</li>
		{/each}
	</ul>
{/if}

<style>
	.head {
		display: flex;
		justify-content: space-between;
		align-items: flex-end;
		gap: 1rem;
		flex-wrap: wrap;
		margin-bottom: 1.25rem;
	}
	.head h1 {
		margin: 0;
	}
	.head p {
		margin: 0.25rem 0 0;
	}
	.new {
		display: flex;
		gap: 0.5rem;
	}
	.new input {
		width: 16rem;
	}
	.grid {
		list-style: none;
		padding: 0;
		margin: 0;
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(15rem, 1fr));
		gap: 1rem;
	}
	.cat {
		display: grid;
		gap: 0.6rem;
		color: inherit;
		text-decoration: none;
		padding: 1rem;
		transition:
			border-color 0.15s,
			transform 0.15s;
	}
	.cat:hover {
		border-color: var(--accent);
		transform: translateY(-2px);
	}
	.previews {
		display: grid;
		grid-template-columns: repeat(4, 1fr);
		gap: 0.35rem;
		height: 4rem;
		align-items: center;
		background: var(--surface-2);
		border-radius: 8px;
		padding: 0.35rem;
	}
	.previews img {
		width: 100%;
		height: 3.3rem;
		object-fit: contain;
	}
	.previews span {
		grid-column: 1 / -1;
		text-align: center;
	}
	.meta {
		display: flex;
		justify-content: space-between;
		align-items: baseline;
		gap: 0.5rem;
	}
	.count {
		font-variant-numeric: tabular-nums;
		color: var(--muted);
		font-size: 0.9rem;
	}
	.count.full {
		color: var(--warn);
		font-weight: 600;
	}
	.sub {
		display: flex;
		justify-content: space-between;
		font-size: 0.8rem;
	}
	.empty {
		text-align: center;
	}
	.warn {
		color: var(--warn);
	}
</style>
