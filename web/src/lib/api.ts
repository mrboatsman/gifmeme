export type Config = {
	auth: { enabled: boolean; pin: boolean; matrix: boolean };
	logged_in: boolean;
	/** Matrix user id, "pin" for PIN sessions, null when auth is off. */
	user: string | null;
	role: Role | null;
	/** The bot's Matrix id, once connected. */
	bot: string | null;
	max_per_category: number;
	max_upload_mb: number;
	max_px: number;
	target_kb: number;
};

export type Role = 'admin' | 'editor';

export type Room = {
	id: number;
	room_id: string;
	alias: string | null;
	name: string | null;
	login_min_power: number | null;
	default_publish: boolean;
	added_by: string | null;
	created_at: number;
	categories: number;
	bot?: { joined: boolean; power: number; needed: number; can_write: boolean; error: string | null };
};

export type User = {
	user_id: string;
	role: Role;
	added_by: string | null;
	created_at: number;
	last_login: number | null;
};

export function roomLabel(r: Room): string {
	return r.name ?? r.alias ?? r.room_id;
}

export function roomLink(r: Room): string {
	return `https://matrix.to/#/${r.alias ?? r.room_id}`;
}

export type Category = {
	id: number;
	slug: string;
	display_name: string;
	created_at: number;
	synced_at: number | null;
	event_id: string | null;
	sync_error: string | null;
	count: number;
	max: number;
	preview: string[];
	rooms: string[];
};

export type Sticker = {
	id: number;
	category: string;
	category_id: number;
	shortcode: string;
	body: string | null;
	filename: string;
	w: number;
	h: number;
	size: number;
	mimetype: string;
	mxc_uri: string | null;
	created_by: string | null;
	created_at: number;
	url: string;
};

export type Status = {
	matrix: {
		connected: boolean;
		user_id: string | null;
		error: string | null;
		last_full_sync: number | null;
		last_full_sync_error: string | null;
	};
	pending_uploads: number;
	failing_packs: number;
};

export class ApiError extends Error {
	constructor(
		public status: number,
		message: string
	) {
		super(message);
	}
}

const USER_KEY = 'gifmeme.user';

export function getUser(): string {
	try {
		return localStorage.getItem(USER_KEY) ?? '';
	} catch {
		return '';
	}
}

export function setUser(name: string) {
	try {
		localStorage.setItem(USER_KEY, name.trim());
	} catch {
		/* private mode */
	}
}

/** Called on any 401 so the layout can show the PIN form again. */
let onUnauthorized: () => void = () => {};
export function setUnauthorizedHandler(fn: () => void) {
	onUnauthorized = fn;
}

function headers(extra?: Record<string, string>): Record<string, string> {
	const h: Record<string, string> = { ...extra };
	const user = getUser();
	if (user) h['X-Gifmeme-User'] = encodeURIComponent(user);
	return h;
}

async function errorFrom(res: Response): Promise<ApiError> {
	let msg = `${res.status} ${res.statusText}`;
	try {
		const body = await res.json();
		if (body?.error) msg = body.error;
	} catch {
		/* not json */
	}
	if (res.status === 401) onUnauthorized();
	return new ApiError(res.status, msg);
}

async function request<T>(method: string, path: string, body?: unknown): Promise<T> {
	const res = await fetch(path, {
		method,
		headers: headers(body === undefined ? undefined : { 'Content-Type': 'application/json' }),
		body: body === undefined ? undefined : JSON.stringify(body)
	});
	if (!res.ok) throw await errorFrom(res);
	if (res.status === 204 || res.status === 202) return undefined as T;
	return res.json();
}

export const api = {
	config: () => request<Config>('GET', '/api/config'),
	login: (pin: string) => request<void>('POST', '/api/login', { pin }),
	logout: () => request<void>('POST', '/api/logout'),
	status: () => request<Status>('GET', '/api/status'),
	resync: () => request<void>('POST', '/api/resync'),
	categories: () => request<Category[]>('GET', '/api/categories'),
	category: (slug: string) => request<Category>('GET', `/api/categories/${encodeURIComponent(slug)}`),
	createCategory: (display_name: string, slug?: string) =>
		request<Category>('POST', '/api/categories', { display_name, slug }),
	renameCategory: (slug: string, display_name: string) =>
		request<Category>('PATCH', `/api/categories/${encodeURIComponent(slug)}`, { display_name }),
	deleteCategory: (slug: string, force = false) =>
		request<void>('DELETE', `/api/categories/${encodeURIComponent(slug)}${force ? '?force=true' : ''}`),
	setCategoryRooms: (slug: string, rooms: string[]) =>
		request<Category>('PUT', `/api/categories/${encodeURIComponent(slug)}/rooms`, { rooms }),
	rooms: () => request<Room[]>('GET', '/api/rooms'),
	addRoom: (room: string, login_min_power: number | null, default_publish: boolean) =>
		request<void>('POST', '/api/admin/rooms', { room, login_min_power, default_publish }),
	updateRoom: (id: number, login_min_power: number | null, default_publish: boolean) =>
		request<void>('PATCH', `/api/admin/rooms/${id}`, { login_min_power, default_publish }),
	joinRoom: (id: number) => request<void>('POST', `/api/admin/rooms/${id}/join`),
	deleteRoom: (id: number) => request<void>('DELETE', `/api/admin/rooms/${id}`),
	users: () => request<User[]>('GET', '/api/admin/users'),
	addUser: (user_id: string, role: Role) => request<void>('POST', '/api/admin/users', { user_id, role }),
	setUserRole: (user_id: string, role: Role) =>
		request<void>('PATCH', `/api/admin/users/${encodeURIComponent(user_id)}`, { role }),
	deleteUser: (user_id: string) =>
		request<void>('DELETE', `/api/admin/users/${encodeURIComponent(user_id)}`),
	stickers: (slug: string) =>
		request<Sticker[]>('GET', `/api/categories/${encodeURIComponent(slug)}/stickers`),
	updateSticker: (id: number, patch: { shortcode?: string; body?: string; category?: string }) =>
		request<Sticker>('PATCH', `/api/stickers/${id}`, patch),
	deleteSticker: (id: number) => request<void>('DELETE', `/api/stickers/${id}`),
	async upload(slug: string, file: File): Promise<Sticker> {
		const form = new FormData();
		form.append('file', file, file.name);
		const res = await fetch(`/api/categories/${encodeURIComponent(slug)}/stickers`, {
			method: 'POST',
			headers: headers(),
			body: form
		});
		if (!res.ok) throw await errorFrom(res);
		return res.json();
	}
};

export function kb(bytes: number): string {
	return bytes >= 1_000_000 ? `${(bytes / 1_000_000).toFixed(1)} MB` : `${Math.round(bytes / 1000)} kB`;
}

export function ago(unix: number | null): string {
	if (!unix) return 'never';
	const s = Math.max(0, Date.now() / 1000 - unix);
	if (s < 60) return 'just now';
	if (s < 3600) return `${Math.floor(s / 60)} min ago`;
	if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
	return new Date(unix * 1000).toLocaleDateString('en-GB');
}
