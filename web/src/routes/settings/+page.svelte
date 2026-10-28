<script lang="ts">
	import { api, ago, roomLabel, roomLink, type Role, type Room, type User } from '#lib/api.ts';
	import { app } from '#lib/app.svelte.ts';

	let rooms = $state<Room[] | null>(null);
	let users = $state<User[] | null>(null);
	let error = $state('');

	let newRoom = $state('');
	let newRoomLogin = $state(true);
	let newRoomPower = $state(0);
	let newRoomDefault = $state(true);
	let addingRoom = $state(false);

	let newUser = $state('');
	let newUserRole = $state<Role>('editor');

	async function load() {
		try {
			[rooms, users] = await Promise.all([api.rooms(), api.users()]);
			error = '';
		} catch (e) {
			error = (e as Error).message;
		}
	}

	$effect(() => {
		load();
	});

	async function run(fn: () => Promise<unknown>) {
		error = '';
		try {
			await fn();
			await load();
		} catch (e) {
			error = (e as Error).message;
		}
	}

	async function addRoom(e: SubmitEvent) {
		e.preventDefault();
		addingRoom = true;
		await run(async () => {
			await api.addRoom(newRoom.trim(), newRoomLogin ? newRoomPower : null, newRoomDefault);
			newRoom = '';
		});
		addingRoom = false;
	}

	function saveRoom(r: Room, login: boolean, power: number, def: boolean) {
		run(() => api.updateRoom(r.id, login ? power : null, def));
	}

	function removeRoom(r: Room) {
		const n = r.categories;
		if (
			!confirm(
				`Stop publishing in ${roomLabel(r)}?${n ? ` The ${n} pack(s) published there are removed from the room.` : ''}`
			)
		)
			return;
		run(() => api.deleteRoom(r.id));
	}

	async function addUser(e: SubmitEvent) {
		e.preventDefault();
		await run(async () => {
			await api.addUser(newUser.trim(), newUserRole);
			newUser = '';
		});
	}
</script>

{#if app.config?.role !== 'admin'}
	<p class="error">Admins only.</p>
{:else}
	<h1>Settings</h1>
	{#if error}<p class="error banner">{error}</p>{/if}

	<section class="card">
		<h2>Rooms</h2>
		<p class="muted">
			Packs are published as state events in these rooms. The bot
			<code>{app.config?.bot ?? '@gifbot'}</code> must be in the room with enough power to write
			<code>im.ponies.room_emotes</code> (usually moderator, 50). For invite-only rooms, invite it first.
		</p>

		{#if rooms === null}
			<p class="muted">Loading…</p>
		{:else}
			<ul class="list">
				{#each rooms as r (r.id)}
					{@const login = r.login_min_power !== null}
					<li>
						<div class="row">
							<div class="title">
								<a href={roomLink(r)} target="_blank" rel="noreferrer"><strong>{roomLabel(r)}</strong></a>
								<code class="muted">{r.alias ?? r.room_id}</code>
							</div>
							<div class="bot">
								{#if !r.bot}
									<span class="muted">bot status unknown</span>
								{:else if !r.bot.joined}
									<span class="error">bot not in room</span>
									<button class="small" onclick={() => run(() => api.joinRoom(r.id))}>Join now</button>
								{:else if !r.bot.can_write}
									<span class="error">bot power {r.bot.power} &lt; {r.bot.needed} needed</span>
								{:else}
									<span class="ok">bot ok</span>
								{/if}
							</div>
						</div>
						<div class="row controls">
							<label>
								<input
									type="checkbox"
									checked={login}
									onchange={(e) => saveRoom(r, e.currentTarget.checked, r.login_min_power ?? 0, r.default_publish)}
								/>
								Members may log in
							</label>
							{#if login}
								<label>
									with power level ≥
									<input
										type="number"
										min="0"
										max="100"
										value={r.login_min_power}
										onchange={(e) => saveRoom(r, true, Number(e.currentTarget.value), r.default_publish)}
									/>
								</label>
							{/if}
							<label>
								<input
									type="checkbox"
									checked={r.default_publish}
									onchange={(e) => saveRoom(r, login, r.login_min_power ?? 0, e.currentTarget.checked)}
								/>
								Publish new categories here
							</label>
							<span class="muted">{r.categories} categories</span>
							<span class="spacer"></span>
							<button class="danger small" onclick={() => removeRoom(r)}>Remove</button>
						</div>
					</li>
				{:else}
					<li class="muted">No rooms yet.</li>
				{/each}
			</ul>
		{/if}

		<form class="add" onsubmit={addRoom}>
			<input placeholder="#room:server or !roomid:server" bind:value={newRoom} required />
			<label><input type="checkbox" bind:checked={newRoomLogin} /> members may log in</label>
			{#if newRoomLogin}
				<label>power ≥ <input type="number" min="0" max="100" bind:value={newRoomPower} /></label>
			{/if}
			<label><input type="checkbox" bind:checked={newRoomDefault} /> publish new categories</label>
			<button class="primary" type="submit" disabled={addingRoom || !newRoom.trim()}>Add room</button>
		</form>
	</section>

	<section class="card">
		<h2>Users</h2>
		<p class="muted">
			Admins manage rooms and users; editors manage stickers. Besides the people listed here, anyone matching a
			room's login rule above gets in as an editor.
		</p>
		{#if users === null}
			<p class="muted">Loading…</p>
		{:else}
			<ul class="list">
				{#each users as u (u.user_id)}
					<li class="row">
						<div class="title">
							<strong>{u.user_id}</strong>
							<span class="muted small-print"
								>added by {u.added_by ?? '?'} · last login {ago(u.last_login)}</span
							>
						</div>
						<select
							value={u.role}
							onchange={(e) => run(() => api.setUserRole(u.user_id, e.currentTarget.value as Role))}
						>
							<option value="admin">admin</option>
							<option value="editor">editor</option>
						</select>
						<button
							class="danger small"
							onclick={() => confirm(`Remove ${u.user_id}?`) && run(() => api.deleteUser(u.user_id))}
							>Remove</button
						>
					</li>
				{/each}
			</ul>
		{/if}
		<form class="add" onsubmit={addUser}>
			<input placeholder="@name:server" bind:value={newUser} required />
			<select bind:value={newUserRole}>
				<option value="editor">editor</option>
				<option value="admin">admin</option>
			</select>
			<button class="primary" type="submit" disabled={!newUser.trim()}>Add user</button>
		</form>
	</section>
{/if}

<style>
	section {
		margin-bottom: 1.5rem;
	}
	h2 {
		margin-top: 0;
	}
	.banner {
		margin-bottom: 1rem;
	}
	.list {
		list-style: none;
		padding: 0;
		margin: 1rem 0;
		display: grid;
		gap: 0.5rem;
	}
	.list > li {
		border: 1px solid var(--border);
		border-radius: 8px;
		padding: 0.75rem;
		display: grid;
		gap: 0.5rem;
	}
	.list > li.row {
		display: flex;
	}
	.row {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		flex-wrap: wrap;
	}
	.title {
		display: grid;
		flex: 1;
		min-width: 12rem;
	}
	.title code {
		font-size: 0.8rem;
		background: none;
		padding: 0;
	}
	.controls {
		font-size: 0.9rem;
	}
	.controls label,
	.add label {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
	}
	input[type='number'] {
		width: 4.5rem;
		padding: 0.25rem 0.4rem;
	}
	.add {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: 0.6rem;
		font-size: 0.9rem;
	}
	.add input:not([type]) {
		flex: 1;
		min-width: 14rem;
	}
	.ok {
		color: var(--ok);
	}
	.small {
		font-size: 0.85rem;
		padding: 0.25rem 0.6rem;
	}
	.small-print {
		font-size: 0.8rem;
	}
	.spacer {
		flex: 1;
	}
</style>
