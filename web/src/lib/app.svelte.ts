import { api, type Config, type Status } from './api.ts';

/** Small global store shared by the layout and the pages. */
export const app = $state({
	config: null as Config | null,
	status: null as Status | null,
	needLogin: false,
	error: '' as string
});

export async function refreshStatus() {
	try {
		app.status = await api.status();
	} catch {
		/* shown via needLogin / next poll */
	}
}
