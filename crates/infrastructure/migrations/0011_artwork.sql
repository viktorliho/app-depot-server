CREATE TABLE artwork_bindings (
    app_id TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    id TEXT NOT NULL UNIQUE,
    etag_version INTEGER NOT NULL DEFAULT 1,
    digest TEXT NOT NULL REFERENCES cas_objects(digest),
    mime_type TEXT NOT NULL,
    size INTEGER NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    PRIMARY KEY (app_id, role)
);

CREATE INDEX artwork_bindings_digest ON artwork_bindings(digest);

CREATE TABLE artwork_claims (
    id TEXT PRIMARY KEY,
    digest TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX artwork_claims_digest ON artwork_claims(digest);
