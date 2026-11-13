ALTER TABLE users ADD COLUMN etag_version INTEGER NOT NULL DEFAULT 1;

CREATE TABLE server_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    anonymous_read_enabled INTEGER NOT NULL DEFAULT 0 CHECK (anonymous_read_enabled IN (0, 1)),
    etag_version INTEGER NOT NULL DEFAULT 1
);
INSERT INTO server_settings(id) VALUES(1);

CREATE TABLE declaration_documents (
    scope TEXT NOT NULL CHECK (scope IN ('App', 'Release', 'Association')),
    resource_id TEXT NOT NULL,
    revision_id TEXT NOT NULL,
    etag_version INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY(scope, resource_id)
);
CREATE TABLE declaration_revisions (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL,
    resource_id TEXT NOT NULL,
    data_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY(scope, resource_id) REFERENCES declaration_documents(scope, resource_id) ON DELETE CASCADE
);
CREATE INDEX declaration_revisions_scope_idx ON declaration_revisions(scope, resource_id, created_at, id);

INSERT INTO declaration_documents(scope, resource_id, revision_id)
SELECT scope, resource_id,
       lower(hex(randomblob(4))) || '-' || lower(hex(randomblob(2))) || '-4' || substr(lower(hex(randomblob(2))), 2) || '-a' || substr(lower(hex(randomblob(2))), 2) || '-' || lower(hex(randomblob(6)))
FROM (
    SELECT 'App' AS scope, id AS resource_id FROM apps
    UNION ALL SELECT 'Release', id FROM releases
    UNION ALL SELECT 'Association', id FROM associations
);
INSERT INTO declaration_revisions(id, scope, resource_id, data_json, created_at)
SELECT revision_id, scope, resource_id, '{}', strftime('%Y-%m-%dT%H:%M:%SZ', 'now') FROM declaration_documents;

CREATE TABLE channels (
    app_id TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    release_id TEXT NOT NULL REFERENCES releases(id),
    generation_id TEXT NOT NULL UNIQUE,
    etag_version INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY(app_id, name)
);
CREATE TABLE channel_moves (
    id TEXT PRIMARY KEY,
    app_id TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    release_id TEXT NOT NULL,
    moved_at TEXT NOT NULL
);
CREATE INDEX channel_moves_name_idx ON channel_moves(app_id, name, moved_at, id);
