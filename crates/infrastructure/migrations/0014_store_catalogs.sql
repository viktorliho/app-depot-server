CREATE TABLE store_catalogs (
    source TEXT PRIMARY KEY,
    generation INTEGER NOT NULL CHECK (generation > 0),
    digest TEXT NOT NULL,
    recipes TEXT NOT NULL
);

CREATE TABLE store_observations (
    source TEXT NOT NULL, id TEXT NOT NULL, recipe TEXT NOT NULL,
    owner TEXT NOT NULL REFERENCES users(id), app TEXT REFERENCES apps(id) ON DELETE CASCADE, release TEXT REFERENCES releases(id) ON DELETE SET NULL, fingerprint TEXT, validators TEXT, pending TEXT,
    enabled BOOLEAN NOT NULL DEFAULT 1, automatic BOOLEAN NOT NULL DEFAULT 0, apply_once BOOLEAN NOT NULL DEFAULT 0,
    hours INTEGER NOT NULL DEFAULT 24 CHECK(hours BETWEEN 1 AND 8760),
    status TEXT NOT NULL DEFAULT 'Queued', last_check INTEGER, detected TEXT, error TEXT,
    files_done INTEGER NOT NULL DEFAULT 0, files_total INTEGER NOT NULL DEFAULT 0,
    bytes_done INTEGER NOT NULL DEFAULT 0, bytes_total INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(source,id)
);
