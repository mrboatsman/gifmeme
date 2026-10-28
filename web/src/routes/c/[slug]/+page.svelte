<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import { api, ago, kb, roomLabel, type Category, type Room, type Sticker } from '#lib/api.ts';
	import { app, refreshStatus } from '#lib/app.svelte.ts';

	type Job = {
		key: number;
		file: File;
		state: 'queued' | 'converting' | 'done' | 'error';
		error?: string;
	};

	let slug = $derived(page.params.slug ?? '');
	let category = $state<Category | null>(null);
	let stickers = $state<Sticker[]>([]);
	let allCategories = $state<Category[]>([]);
	let rooms = $state<Room[]>([]);
	let roomsError = $state('');
	let loadError = $state('');
	let jobs = $state<Job[]>([]);
	let dragging = $state(false);
	let selected = $state<Sticker | null>(null);
	let edit = $state({ shortcode: '', body: '', category: '' });
	let editError = $state('');
	let saving = $state(false);
	let renaming = $state(false);
	let newName = $state('');
	let filter = $state('');
	let fileInput = $state<HTMLInputElement>();
	let dialog = $state<HTMLDialogElement>();
	let jobKey = 0;
	let running = 0;

	let max = $derived(category?.max ?? app.config?.max_per_category ?? 100);
	let full = $derived(stickers.length >= max);
	let queued = $derived(jobs.filter((j) => j.state === 'queued' || j.state === 'converting').length);
	let shown = $derived(
		filter.trim()
			? stickers.filter((s) =>
					(s.shortcode + ' ' + (s.body ?? '')).toLowerCase().includes(filter.trim().toLowerCase())
				)
			: stickers
	);

	async function load(s: string) {
		try {
			const [c, list, all] = await Promise.all([api.category(s), api.stickers(s), api.categories()]);
			api.rooms().then((r) => (rooms = r)).catch(() => {});
			category = c;
			stickers = list;
			allCategories = all;
			loadError = '';
		} catch (e) {
			loadError = (e as Error).message;
		}
	}

	$effect(() => {
		load(slug);
	});

	async function refreshCategory() {
		try {
			category = await api.category(slug);
		} catch {
			/* ignore */
		}
	}

	function addFiles(files: FileList | File[]) {
		const room = max - stickers.length - queued;
		const list = Array.from(files);
		list.forEach((file, i) => {
			const job: Job = { key: ++jobKey, file, state: 'queued' };
			if (i >= room) {
				job.state = 'error';
				job.error = `A category holds at most ${max}`;
			}
			jobs.push(job);
		});
		pump();
	}

	// Two uploads in flight: the backend converts at most two at a time anyway.
	function pump() {
		while (running < 2) {
			const job = jobs.find((j) => j.state === 'queued');
			if (!job) break;
			running++;
			job.state = 'converting';
			api
				.upload(slug, job.file)
				.then((s) => {
					job.state = 'done';
					stickers.push(s);
				})
				.catch((e) => {
					job.state = 'error';
					job.error = (e as Error).message;
				})
				.finally(() => {
					running--;
					pump();
					if (running === 0) {
						refreshCategory();
						setTimeout(refreshCategory, 4000);
						refreshStatus();
					}
				});
		}
	}

	function onDrop(e: DragEvent) {
		e.preventDefault();
		dragging = false;
		if (e.dataTransfer?.files.length) addFiles(e.dataTransfer.files);
	}

	function onPaste(e: ClipboardEvent) {
		const files = Array.from(e.clipboardData?.files ?? []);
		if (files.length) addFiles(files);
	}

	function open(s: Sticker) {
		selected = s;
		edit = { shortcode: s.shortcode, body: s.body ?? '', category: s.category };
		editError = '';
		dialog?.showModal();
	}

	async function save(e: SubmitEvent) {
		e.preventDefault();
		if (!selected) return;
		saving = true;
		editError = '';
		try {
			const updated = await api.updateSticker(selected.id, {
				shortcode: edit.shortcode,
				body: edit.body,
				category: edit.category
			});
			if (updated.category !== slug) {
				stickers = stickers.filter((s) => s.id !== updated.id);
			} else {
				stickers = stickers.map((s) => (s.id === updated.id ? updated : s));
			}
			dialog?.close();
			setTimeout(refreshCategory, 3000);
		} catch (err) {
			editError = (err as Error).message;
		} finally {
			saving = false;
		}
	}

	async function remove() {
		if (!selected || !confirm(`Delete :${selected.shortcode}:?`)) return;
		try {
			await api.deleteSticker(selected.id);
			const id = selected.id;
			stickers = stickers.filter((s) => s.id !== id);
			dialog?.close();
			setTimeout(refreshCategory, 3000);
		} catch (err) {
			editError = (err as Error).message;
		}
	}

	async function toggleRoom(roomId: string, on: boolean) {
		if (!category) return;
		const next = on
			? [...new Set([...category.rooms, roomId])]
			: category.rooms.filter((r) => r !== roomId);
		roomsError = '';
		try {
			category = await api.setCategoryRooms(slug, next);
			setTimeout(refreshCategory, 3000);
		} catch (err) {
			roomsError = (err as Error).message;
		}
	}

	async function rename(e: SubmitEvent) {
		e.preventDefault();
		try {
			category = await api.renameCategory(slug, newName.trim());
			renaming = false;
		} catch (err) {
			alert((err as Error).message);
		}
	}

	async function removeCategory() {
		if (!category) return;
		const n = stickers.length;
		const msg = n
			? `Delete ${category.display_name} and all ${n} stickers? This cannot be undone.`
			: `Delete ${category.display_name}?`;
		if (!confirm(msg)) return;
		try {
			await api.deleteCategory(slug, n > 0);
			await goto('/');
		} catch (err) {
			alert((err as Error).message);
		}
	}
</script>

<svelte:window
	onpaste={onPaste}
	ondragover={(e) => {
		e.preventDefault();
		if (!full) dragging = true;
	}}
	ondragleave={(e) => {
		if (!e.relatedTarget) dragging = false;
	}}
	ondrop={onDrop}
/>

{#if loadError}
	<p class="error">{loadError}</p>
	<a href="/">← All categories</a>
{:else if category}
	<a class="back" href="/">← All categories</a>
	<div class="head">
		<div>
			{#if renaming}
				<form class="rename" onsubmit={rename}>
					<!-- svelte-ignore a11y_autofocus -->
					<input bind:value={newName} maxlength="64" autofocus />
					<button class="primary" type="submit" disabled={!newName.trim()}>Save</button>
					<button type="button" onclick={() => (renaming = false)}>Cancel</button>
				</form>
			{:else}
				<h1>
					{category.display_name}
					<button
						class="ghost small"
						title="Rename"
						onclick={() => {
							newName = category?.display_name ?? '';
							renaming = true;
						}}>✎</button
					>
				</h1>
			{/if}
			<p class="muted">
				<code>{category.slug}</code> ·
				{#if category.sync_error}
					<span class="error" title={category.sync_error}>sync error: {category.sync_error}</span>
				{:else}
					synced {ago(category.synced_at)}
				{/if}
			</p>
		</div>
		<div class="counter" class:full>
			<strong>{stickers.length}</strong>/{max}
			<div class="bar"><div style:width="{Math.min(100, (stickers.length / max) * 100)}%"></div></div>
		</div>
	</div>

	<div class="rooms">
		<span class="muted">Published in</span>
		{#each rooms as r (r.id)}
			<label class="chip" class:on={category.rooms.includes(r.room_id)} title={r.room_id}>
				<input
					type="checkbox"
					checked={category.rooms.includes(r.room_id)}
					onchange={(e) => toggleRoom(r.room_id, e.currentTarget.checked)}
				/>
				{roomLabel(r)}
				{#if r.bot && !r.bot.can_write}<span class="error" title="The bot cannot write packs there">⚠</span>{/if}
			</label>
		{:else}
			<span class="muted">
				no rooms configured{#if app.config?.role === 'admin'}, <a href="/settings">add one</a>{/if}
			</span>
		{/each}
		{#if roomsError}<span class="error">{roomsError}</span>{/if}
	</div>

	<button
		class="drop"
		class:dragging
		class:full
		disabled={full}
		onclick={() => fileInput?.click()}
	>
		{#if full}
			This category is full ({max}/{max}). Delete or move stickers to add more.
		{:else}
			<span><strong>Drop gifs here</strong>, paste, or click to choose files</span>
			<span class="muted hint">
				gif, webp, png, jpg, mp4, webm · max {app.config?.max_upload_mb ?? 50} MB · becomes animated webp ≤ {app
					.config?.max_px ?? 512} px
			</span>
		{/if}
	</button>
	<input
		bind:this={fileInput}
		type="file"
		multiple
		accept="image/*,video/mp4,video/webm,.gif,.webp,.webm,.mp4"
		hidden
		onchange={(e) => {
			const input = e.currentTarget;
			if (input.files) addFiles(input.files);
			input.value = '';
		}}
	/>

	{#if jobs.length}
		<div class="jobs card">
			<div class="jobs-head">
				<strong>Uploads</strong>
				<button class="ghost small" onclick={() => (jobs = jobs.filter((j) => j.state === 'queued' || j.state === 'converting'))}
					>Clear finished</button
				>
			</div>
			<ul>
				{#each jobs as job (job.key)}
					<li class={job.state === 'error' ? 'err' : job.state === 'done' ? 'done' : ''}>
						<span class="name">{job.file.name}</span>
						<span class="muted">{kb(job.file.size)}</span>
						<span class="state">{job.state === 'converting' ? 'converting…' : job.state}</span>
						{#if job.error}<span class="error">{job.error}</span>{/if}
					</li>
				{/each}
			</ul>
		</div>
	{/if}

	{#if stickers.length > 12}
		<input class="filter" placeholder="Filter by shortcode or title" bind:value={filter} />
	{/if}

	{#if stickers.length === 0}
		<p class="muted empty">No stickers yet.</p>
	{:else}
		<ul class="grid">
			{#each shown as s (s.id)}
				<li>
					<button class="sticker" onclick={() => open(s)} title={s.body ?? s.shortcode}>
						<img src={s.url} alt={s.body ?? s.shortcode} loading="lazy" width={s.w} height={s.h} />
						<span class="code">:{s.shortcode}:</span>
						{#if !s.mxc_uri}<span class="badge" title="Not uploaded to Matrix yet">⏳</span>{/if}
					</button>
				</li>
			{/each}
		</ul>
	{/if}

	<div class="danger-zone">
		<button class="danger" onclick={removeCategory}>Delete category</button>
	</div>
{:else}
	<p class="muted">Loading…</p>
{/if}

<dialog bind:this={dialog} onclose={() => (selected = null)}>
	{#if selected}
		<form onsubmit={save}>
			<div class="preview">
				<img src={selected.url} alt={selected.shortcode} />
			</div>
			<div class="fields">
				<label>
					Shortcode
					<input bind:value={edit.shortcode} maxlength="64" pattern="[A-Za-z0-9_\-:]+" required />
				</label>
				<label>
					Title <span class="muted">(shown as description)</span>
					<input bind:value={edit.body} maxlength="200" placeholder={selected.shortcode} />
				</label>
				<label>
					Category
					<select bind:value={edit.category}>
						{#each allCategories as c (c.id)}
							<option value={c.slug} disabled={c.slug !== selected.category && c.count >= c.max}>
								{c.display_name} ({c.count}/{c.max})
							</option>
						{/each}
					</select>
				</label>
				<p class="muted small-print">
					{selected.w}×{selected.h} · {kb(selected.size)} · {selected.filename}<br />
					{#if selected.created_by}by {selected.created_by} · {/if}{ago(selected.created_at)}<br />
					{selected.mxc_uri ?? 'not uploaded yet'}
				</p>
				{#if editError}<p class="error">{editError}</p>{/if}
				<div class="actions">
					<button type="button" class="danger" onclick={remove}>Delete</button>
					<span class="spacer"></span>
					<button type="button" onclick={() => dialog?.close()}>Cancel</button>
					<button type="submit" class="primary" disabled={saving}>Save</button>
				</div>
			</div>
		</form>
	{/if}
</dialog>

<style>
	.back {
		color: var(--muted);
		text-decoration: none;
		font-size: 0.9rem;
	}
	.head {
		display: flex;
		justify-content: space-between;
		align-items: flex-end;
		gap: 1rem;
		margin: 0.5rem 0 1rem;
		flex-wrap: wrap;
	}
	.head h1 {
		margin: 0;
		display: flex;
		align-items: center;
		gap: 0.25rem;
	}
	.head p {
		margin: 0.25rem 0 0;
	}
	.rename {
		display: flex;
		gap: 0.5rem;
	}
	.small {
		font-size: 0.9rem;
		padding: 0.2rem 0.5rem;
	}
	.counter {
		text-align: right;
		font-variant-numeric: tabular-nums;
		color: var(--muted);
		min-width: 8rem;
	}
	.counter strong {
		font-size: 1.6rem;
		color: var(--text);
	}
	.counter.full strong {
		color: var(--warn);
	}
	.bar {
		height: 6px;
		border-radius: 3px;
		background: var(--surface-2);
		overflow: hidden;
		margin-top: 0.3rem;
	}
	.bar div {
		height: 100%;
		background: var(--accent);
	}
	.counter.full .bar div {
		background: var(--warn);
	}
	.rooms {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.5rem;
		margin-bottom: 1rem;
		font-size: 0.9rem;
	}
	.chip {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		padding: 0.25rem 0.7rem;
		border: 1px solid var(--border);
		border-radius: 999px;
		background: var(--surface);
		cursor: pointer;
	}
	.chip.on {
		border-color: var(--accent);
		background: var(--surface-2);
	}
	.chip input {
		margin: 0;
		accent-color: var(--accent);
	}
	.drop {
		display: grid;
		gap: 0.3rem;
		place-items: center;
		width: 100%;
		padding: 2rem 1rem;
		border: 2px dashed var(--border);
		border-radius: var(--radius);
		background: var(--surface);
		text-align: center;
		transition:
			border-color 0.15s,
			background 0.15s;
	}
	.drop:hover:not(:disabled),
	.drop.dragging {
		border-color: var(--accent);
		background: var(--surface-2);
	}
	.drop .hint {
		font-size: 0.85rem;
	}
	.jobs {
		margin-top: 1rem;
		padding: 0.75rem 1rem;
	}
	.jobs-head {
		display: flex;
		justify-content: space-between;
		align-items: center;
	}
	.jobs ul {
		list-style: none;
		margin: 0.5rem 0 0;
		padding: 0;
		display: grid;
		gap: 0.25rem;
		max-height: 14rem;
		overflow: auto;
		font-size: 0.9rem;
	}
	.jobs li {
		display: flex;
		gap: 0.75rem;
		flex-wrap: wrap;
	}
	.jobs .name {
		flex: 1;
		min-width: 10rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.jobs li.done .state {
		color: var(--ok);
	}
	.jobs li.err .state {
		color: var(--bad);
	}
	.filter {
		margin-top: 1.25rem;
		width: 100%;
		max-width: 22rem;
	}
	.grid {
		list-style: none;
		padding: 0;
		margin: 1.25rem 0 0;
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(8.5rem, 1fr));
		gap: 0.75rem;
	}
	.sticker {
		position: relative;
		display: grid;
		gap: 0.4rem;
		width: 100%;
		padding: 0.6rem;
		background: var(--surface);
		border-radius: var(--radius);
		transition: border-color 0.15s;
	}
	.sticker:hover {
		border-color: var(--accent);
	}
	.sticker img {
		width: 100%;
		height: 7rem;
		object-fit: contain;
	}
	.code {
		font-size: 0.8rem;
		color: var(--muted);
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}
	.badge {
		position: absolute;
		top: 0.3rem;
		right: 0.4rem;
		font-size: 0.8rem;
	}
	.empty {
		text-align: center;
		margin: 2rem 0;
	}
	.danger-zone {
		margin-top: 3rem;
		padding-top: 1rem;
		border-top: 1px solid var(--border);
	}
	dialog {
		border: 1px solid var(--border);
		border-radius: var(--radius);
		background: var(--surface);
		color: var(--text);
		padding: 0;
		width: min(40rem, calc(100vw - 2rem));
	}
	dialog::backdrop {
		background: rgb(0 0 0 / 0.5);
	}
	dialog form {
		display: grid;
		grid-template-columns: minmax(0, 14rem) 1fr;
		gap: 1.25rem;
		padding: 1.25rem;
	}
	@media (max-width: 34rem) {
		dialog form {
			grid-template-columns: 1fr;
		}
	}
	.preview {
		display: grid;
		place-items: center;
		background: var(--surface-2);
		border-radius: 8px;
		padding: 0.5rem;
	}
	.preview img {
		max-width: 100%;
		max-height: 14rem;
	}
	.fields {
		display: grid;
		gap: 0.75rem;
		align-content: start;
	}
	.fields label {
		display: grid;
		gap: 0.25rem;
		font-size: 0.9rem;
	}
	.small-print {
		font-size: 0.8rem;
		margin: 0;
		word-break: break-all;
	}
	.actions {
		display: flex;
		gap: 0.5rem;
	}
	.spacer {
		flex: 1;
	}
</style>
