<script lang="ts">
	import { api, roomLabel, roomLink, type Room } from '#lib/api.ts';
	import { app } from '#lib/app.svelte.ts';

	let rooms = $state<Room[]>([]);
	$effect(() => {
		api.rooms().then((r) => (rooms = r)).catch(() => {});
	});
</script>

<article class="card">
	<h1>Getting the stickers in FluffyChat</h1>

	<h2>1. Join a sticker room</h2>
	<p>
		Packs are published in
		{#each rooms as r, i (r.id)}{i ? ', ' : ''}<a href={roomLink(r)} target="_blank" rel="noreferrer"
				><code>{r.alias ?? roomLabel(r)}</code></a
			>{:else}the rooms configured by an admin{/each}. Each category can be in one or more of them. Join
		with your Matrix account; inside the room the packs show up in the sticker picker right away.
	</p>

	<h2>2. Use the packs in every chat</h2>
	<p>
		Open the room in FluffyChat → <strong>room settings</strong> → <strong>Emotes &amp; stickers</strong> and
		turn on <strong>use globally</strong> for each pack you want everywhere. This is stored on your account
		and follows you to all your devices.
	</p>

	<h2>3. Add your own</h2>
	<p>
		Pick or create a category here and drop files in. Everything is converted to animated WebP (max {app
			.config?.max_px ?? 512} px, ideally under {app.config?.target_kb ?? 500} kB) and uploaded to Matrix.
		Changes reach clients within a few seconds, no restart needed.
	</p>
	<ul>
		<li>At most {app.config?.max_per_category ?? 100} stickers per category so the picker stays browsable.</li>
		<li>
			The shortcode (<code>:surprised-pikachu:</code>) comes from the file name and can be changed. It must be
			unique within the category.
		</li>
		<li>
			Telegram stickers: animated <code>.tgs</code> (Lottie) is not supported, <code>.webm</code> and
			<code>.webp</code> work.
		</li>
	</ul>

	<h2>Other clients</h2>
	<p>
		The packs follow MSC2545 (<code>im.ponies.room_emotes</code>), the format Cinny, Nheko and SchildiChat
		understand as well. Element does not support them.
	</p>
</article>

<style>
	article {
		max-width: 46rem;
	}
	h2 {
		margin-top: 1.5rem;
	}
	li {
		margin: 0.3rem 0;
	}
</style>
