[![Vibe Coded](https://img.shields.io/badge/✨-vibe_coded-blueviolet)](https://github.com/mrboatsman/gifme)

DISCLAIMER: This is a vibe coded project see it as poof-of-concept or
inspiration for your own project. Everything in this repo will continue
to be slop vibe code, my time is limited but my ideas are not limited.

# gifmeme

Self-hosted GIF/sticker dashboard for Matrix. People upload gifs to 
a web service, it converts them to animated WebP and publishes them as 
[MSC2545](https://github.com/matrix-org/matrix-spec-proposals/pull/2545)
("ponies") image packs in a shared room. FluffyChat, Cinny, Nheko and
SchildiChat show them in their sticker picker right away. No Tenor/Giphy.

```
browser ──> gifmeme (SvelteKit SPA, served by the backend)
              │
              ▼
           Rust backend (axum)  ── SQLite = source of truth, ffmpeg
              │  POST /_matrix/media/v3/upload            (once per sticker)
              │  PUT  /rooms/{room}/state/im.ponies.room_emotes/{category}
              │  DELETE /_synapse/admin/v1/media/{server}/{id}
              ▼
           Synapse  ──sync──>  clients
```

## Model

- Rooms are configured in the UI (Settings, admins only). Each category is
  one state event per room it is published in: type `im.ponies.room_emotes`,
  state_key = category slug, content
  `{ pack: {display_name, avatar_url, usage: [sticker]}, images: {shortcode: {url, body, usage, info}} }`.
  The same pack in several rooms is the same content written to each room;
  media is uploaded once (an `mxc://` URI works from any room).
- The whole event is rewritten on every change (debounced 1.5 s, max 10 s),
  so it is idempotent. A full resync runs on connect and every 24 h.
- Unpublishing a category from a room, removing a room or deleting a
  category writes `{}` there (clients drop empty packs); `publications`
  remembers where each pack was written. Deleted stickers' media is removed
  via the Synapse admin API **after** no pack references it any more.
- The bot needs to be in each room with enough power for
  `im.ponies.room_emotes` (usually 50). It joins public rooms and accepts
  invites by itself.
- Max 100 stickers per category (picker has no search), ~15 kB per event.
- Uploads: anything ffmpeg decodes (gif, webp, png, jpg, mp4, webm). Output
  is WebP, ≤ 512 px, alpha kept, max 10 s; quality steps down until it is
  < 500 kB. Already-small WebPs are kept byte for byte (ffmpeg cannot decode
  animated WebP, so larger ones are refused). Telegram `.tgs` (Lottie) is not
  supported. Duplicates (same file in one category) are refused.
- Only the converted file is stored (`$GIFMEME_DATA_DIR/files/<id>.webp`).

## Access

- **Admins** manage rooms and users, **editors** manage categories and stickers.
- The first Matrix user to log in while there is no admin becomes admin.
- Admins add users explicitly (`@user:server`, admin or editor), and/or let
  a room grant logins: members with power level ≥ N get in as editors.
  Room-rule answers are cached for 5 minutes.
- PIN sessions are editors (admins when Matrix login is off). With neither
  PIN nor Matrix login configured everyone is an admin.

## Configuration (env)

| Variable                                           | Default             | Description                                                                                                                                                |
| -------------------------------------------------- | ------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `MATRIX_HOMESERVER`                                | –                   | C-S API base, e.g. `http://synapse.synapse.svc.cluster.local:8008`. Without it the app runs offline (no sync).                                             |
| `MATRIX_ACCESS_TOKEN` / `MATRIX_ACCESS_TOKEN_FILE` | –                   | bot token. Bot should be a server admin (media delete, rate-limit override).                                                                               |
| `GIFMEME_ROOM`                                     | –                   | one-time seed: on a start with no rooms configured, add this room (members may log in, publishes all existing categories). Rooms live in the UI afterwards |
| `GIFMEME_ROOM_ALIAS`                               | –                   | alias stored for the seeded room when `GIFMEME_ROOM` is an id                                                                                              |
| `GIFMEME_PUBLIC_URL`                               | –                   | browser URL of the app (`http://gif.lan`); enables "log in with Matrix"                                                                                    |
| `MATRIX_PUBLIC_URL`                                | `MATRIX_HOMESERVER` | browser URL of the homeserver for the SSO redirect (`https://matrix.verk.st`)                                                                              |
| `GIFMEME_SESSION_DAYS`                             | `30`                | session cookie lifetime                                                                                                                                    |
| `GIFMEME_PIN`                                      | –                   | optional shared PIN login. No PIN and no public URL = dashboard open                                                                                       |
| `GIFMEME_DNS_SERVER`                               | –                   | resolve via this DNS server instead of resolv.conf (NetBird sidecar pods)                                                                                  |
| `GIFMEME_LISTEN`                                   | `0.0.0.0:8080`      |                                                                                                                                                            |
| `GIFMEME_DATA_DIR`                                 | `./data`            | SQLite + files                                                                                                                                             |
| `GIFMEME_STATIC_DIR`                               | `./web/build`       | SPA build                                                                                                                                                  |
| `GIFMEME_MAX_PER_CATEGORY`                         | `100`               |                                                                                                                                                            |
| `GIFMEME_MAX_PX` / `GIFMEME_TARGET_BYTES`          | `512` / `500000`    |                                                                                                                                                            |
| `GIFMEME_MAX_UPLOAD_MB`                            | `50`                | input file limit                                                                                                                                           |
| `GIFMEME_RESYNC_INTERVAL_HOURS`                    | `24`                | `0` disables the scheduled resync                                                                                                                          |
| `GIFMEME_PRUNE_UNKNOWN_PACKS`                      | `false`             | on full resync, blank pack events in the room that are not in the DB                                                                                       |

## Login

With `GIFMEME_PUBLIC_URL` set, "Log in with Matrix" sends the browser to
`$MATRIX_PUBLIC_URL/_matrix/client/v3/login/sso/redirect[/<idp>]` (straight
to the provider when the homeserver has exactly one) with `redirectUrl =
$GIFMEME_PUBLIC_URL/api/auth/matrix/callback?state=<nonce>`. The callback
checks the nonce cookie, trades the `loginToken` for the user id with
`m.login.token`, revokes the minted access token right away, and resolves the user's role (see Access) and sets a signed session cookie. Uploads get
`created_by = @user:server`. Add `$GIFMEME_PUBLIC_URL/` to Synapse
`sso.client_whitelist` to skip its "Continue to your account" page.

## API

All bodies are JSON unless noted. `?` marks an optional field.

### Public

| Method | Path                        | Body / notes                    |
| ------ | --------------------------- | ------------------------------- |
| GET    | `/api/health`               | liveness, returns `ok`          |
| GET    | `/api/config`               | auth mode and current access    |
| POST   | `/api/login`                | `{pin}`                         |
| POST   | `/api/logout`               |                                 |
| GET    | `/api/auth/matrix/start`    | redirects to the homeserver SSO |
| GET    | `/api/auth/matrix/callback` | `?state=&loginToken=`           |

### Logged in

| Method | Path                              | Body / notes                                    |
| ------ | --------------------------------- | ----------------------------------------------- |
| GET    | `/api/status`                     | sync status                                     |
| POST   | `/api/resync`                     | start a full resync to Matrix (202)             |
| GET    | `/api/rooms`                      |                                                 |
| GET    | `/api/categories`                 |                                                 |
| POST   | `/api/categories`                 | `{display_name, slug?}`                         |
| GET    | `/api/categories/{slug}`          |                                                 |
| PATCH  | `/api/categories/{slug}`          | `{display_name}`                                |
| DELETE | `/api/categories/{slug}`          | `?force=true` deletes a non-empty one           |
| PUT    | `/api/categories/{slug}/rooms`    | `{rooms: [room_id]}`                            |
| GET    | `/api/categories/{slug}/stickers` |                                                 |
| POST   | `/api/categories/{slug}/stickers` | multipart: `file`, optional `shortcode`, `body` |
| PATCH  | `/api/stickers/{id}`              | `{shortcode?, body?, category?}`                |
| DELETE | `/api/stickers/{id}`              |                                                 |
| GET    | `/files/{id}.webp`                | sticker file                                    |

Without a Matrix session, the `X-Gifmeme-User` header (URL-encoded) is stored
as `created_by` on uploads.

### Admin

| Method | Path                         | Body / notes                                 |
| ------ | ---------------------------- | -------------------------------------------- |
| POST   | `/api/admin/rooms`           | `{room, login_min_power?, default_publish?}` |
| PATCH  | `/api/admin/rooms/{id}`      | `{login_min_power, default_publish}`         |
| DELETE | `/api/admin/rooms/{id}`      |                                              |
| POST   | `/api/admin/rooms/{id}/join` | retry join after inviting the bot            |
| GET    | `/api/admin/users`           |                                              |
| POST   | `/api/admin/users`           | `{user_id, role}` (`admin` or `editor`)      |
| PATCH  | `/api/admin/users/{user_id}` | `{role}`                                     |
| DELETE | `/api/admin/users/{user_id}` |                                              |

## Development

```bash
cargo run                         # :8080, ./data, offline unless MATRIX_* set
cd web && pnpm install && pnpm dev  # Vite proxies /api and /files to :8080
```

Needs `ffmpeg`/`ffprobe` with libwebp on PATH. Or the image:

```bash
docker build -t gifmeme:dev . && docker run --rm -p 8080:8080 gifmeme:dev
```

CI: GitHub Actions runs `cargo test` and the web build on every push and pull
request, and on `main` publishes `ghcr.io/mrboatsman/gifmeme:{latest,<sha8>}`.
