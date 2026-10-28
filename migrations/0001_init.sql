-- One row per sticker pack = one im.ponies.room_emotes state event
-- (state_key = slug). The slug is immutable once created.
CREATE TABLE categories (
    id            INTEGER PRIMARY KEY,
    slug          TEXT    NOT NULL UNIQUE,
    display_name  TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    -- last successful PUT of the pack to the room
    synced_at     INTEGER,
    event_id      TEXT,
    sync_error    TEXT
);

-- Only the converted file is kept (DATA_DIR/files/<id>.webp). mxc_uri is NULL
-- until the upload to Synapse succeeded; the syncer uploads missing ones.
-- AUTOINCREMENT: ids name the files and are cached as immutable, so they
-- must never be reused after a delete.
CREATE TABLE stickers (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    category_id   INTEGER NOT NULL REFERENCES categories(id) ON DELETE CASCADE,
    shortcode     TEXT    NOT NULL,
    body          TEXT,
    filename      TEXT    NOT NULL,
    w             INTEGER NOT NULL,
    h             INTEGER NOT NULL,
    size          INTEGER NOT NULL,
    mimetype      TEXT    NOT NULL,
    mxc_uri       TEXT,
    -- sha256 of the original upload, for duplicate detection per category
    sha256        TEXT    NOT NULL,
    created_by    TEXT,
    created_at    INTEGER NOT NULL,
    UNIQUE (category_id, shortcode),
    UNIQUE (category_id, sha256)
);
CREATE INDEX stickers_category ON stickers (category_id, created_at, id);

-- Media to delete from Synapse once the pack no longer references it
-- (after the next successful sync of `slug`).
CREATE TABLE media_trash (
    id            INTEGER PRIMARY KEY,
    slug          TEXT    NOT NULL,
    mxc_uri       TEXT    NOT NULL,
    created_at    INTEGER NOT NULL
);

-- Rooms the bot publishes packs into, configured in the UI.
-- login_min_power: members of this room with at least this power level may
-- log in as editors (NULL = the room grants no logins).
-- default_publish: new categories are published here automatically.
CREATE TABLE rooms (
    id               INTEGER PRIMARY KEY,
    room_id          TEXT    NOT NULL UNIQUE,
    alias            TEXT,
    name             TEXT,
    login_min_power  INTEGER,
    default_publish  INTEGER NOT NULL DEFAULT 1,
    added_by         TEXT,
    created_at       INTEGER NOT NULL
);

-- Which rooms each category should be published in.
CREATE TABLE category_rooms (
    category_id  INTEGER NOT NULL REFERENCES categories(id) ON DELETE CASCADE,
    room_id      TEXT    NOT NULL REFERENCES rooms(room_id) ON DELETE CASCADE,
    PRIMARY KEY (category_id, room_id)
);

-- Where a pack (by slug) currently is published. Survives category/room
-- deletion so the syncer knows where to blank it.
CREATE TABLE publications (
    slug       TEXT    NOT NULL,
    room_id    TEXT    NOT NULL,
    event_id   TEXT,
    synced_at  INTEGER,
    error      TEXT,
    PRIMARY KEY (slug, room_id)
);

-- Explicitly granted dashboard users. The first Matrix user to log in while
-- there is no admin becomes one.
CREATE TABLE users (
    user_id     TEXT    PRIMARY KEY,
    role        TEXT    NOT NULL CHECK (role IN ('admin', 'editor')),
    added_by    TEXT,
    created_at  INTEGER NOT NULL,
    last_login  INTEGER
);
