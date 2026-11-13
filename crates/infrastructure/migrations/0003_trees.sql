CREATE TABLE upload_sessions_next (
    id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL UNIQUE REFERENCES operations(id),
    cancel_operation_id TEXT UNIQUE REFERENCES operations(id),
    owner_id TEXT NOT NULL REFERENCES users(id),
    mode TEXT NOT NULL CHECK (mode IN ('BareFile', 'BareTree', 'ZipImport')),
    phase TEXT NOT NULL CHECK (phase IN ('Receiving', 'Verifying', 'Importing', 'Committing', 'Completed', 'Failed', 'Cancelled')),
    terminal_code TEXT,
    inventory_complete INTEGER NOT NULL DEFAULT 1 CHECK (inventory_complete IN (0, 1)),
    target_json TEXT NOT NULL,
    file_id TEXT UNIQUE,
    file_size INTEGER CHECK (file_size >= 0),
    file_sha256 TEXT CHECK (file_sha256 IS NULL OR length(file_sha256) = 64),
    file_path TEXT,
    progress_version INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK ((mode = 'BareTree' AND file_id IS NULL AND file_size IS NULL AND file_sha256 IS NULL AND file_path IS NULL)
        OR (mode IN ('BareFile', 'ZipImport') AND file_id IS NOT NULL AND file_size IS NOT NULL AND file_sha256 IS NOT NULL AND file_path IS NOT NULL))
);

INSERT INTO upload_sessions_next
SELECT id, operation_id, cancel_operation_id, owner_id, mode, phase, terminal_code, 1,
       target_json, file_id, file_size, file_sha256, file_path, progress_version, created_at, updated_at
FROM upload_sessions;

CREATE TABLE upload_ranges_next (
    session_id TEXT NOT NULL REFERENCES upload_sessions_next(id) ON DELETE CASCADE,
    file_id TEXT NOT NULL,
    start INTEGER NOT NULL CHECK (start >= 0),
    end INTEGER NOT NULL CHECK (end > start),
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
    PRIMARY KEY(session_id, file_id, start, end)
);

INSERT INTO upload_ranges_next(session_id, file_id, start, end, sha256)
SELECT r.session_id, s.file_id, r.start, r.end, r.sha256
FROM upload_ranges r JOIN upload_sessions s ON s.id = r.session_id;

DROP TABLE upload_ranges;
DROP TABLE upload_sessions;
ALTER TABLE upload_sessions_next RENAME TO upload_sessions;
ALTER TABLE upload_ranges_next RENAME TO upload_ranges;

CREATE INDEX upload_sessions_phase_idx ON upload_sessions(phase);
CREATE INDEX upload_ranges_file_idx ON upload_ranges(session_id, file_id, start);

CREATE TABLE upload_inventory (
    session_id TEXT NOT NULL REFERENCES upload_sessions(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    entry_type TEXT NOT NULL CHECK (entry_type IN ('File', 'Directory')),
    file_id TEXT UNIQUE,
    size INTEGER CHECK (size >= 0),
    sha256 TEXT CHECK (sha256 IS NULL OR length(sha256) = 64),
    executable INTEGER CHECK (executable IN (0, 1)),
    PRIMARY KEY(session_id, path),
    CHECK ((entry_type = 'Directory' AND file_id IS NULL AND size IS NULL AND sha256 IS NULL AND executable IS NULL)
        OR (entry_type = 'File' AND file_id IS NOT NULL AND size IS NOT NULL AND sha256 IS NOT NULL AND executable IS NOT NULL))
);

CREATE TABLE artifacts_next (
    id TEXT PRIMARY KEY,
    digest TEXT REFERENCES cas_objects(digest),
    kind TEXT NOT NULL CHECK (kind IN ('File', 'Tree')),
    size INTEGER NOT NULL CHECK (size >= 0),
    CHECK ((kind = 'File' AND digest IS NOT NULL) OR (kind = 'Tree' AND digest IS NULL))
);
INSERT INTO artifacts_next SELECT id, digest, kind, size FROM artifacts;

CREATE TABLE associations_next (
    id TEXT PRIMARY KEY,
    release_id TEXT NOT NULL REFERENCES releases(id) ON DELETE CASCADE,
    artifact_id TEXT NOT NULL REFERENCES artifacts_next(id),
    etag_version INTEGER NOT NULL DEFAULT 1,
    UNIQUE(release_id, artifact_id)
);
INSERT INTO associations_next SELECT id, release_id, artifact_id, etag_version FROM associations;
DROP TABLE associations;
DROP TABLE artifacts;
ALTER TABLE artifacts_next RENAME TO artifacts;
ALTER TABLE associations_next RENAME TO associations;
CREATE INDEX associations_artifact_idx ON associations(artifact_id);

CREATE TABLE artifact_entries (
    artifact_id TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    entry_type TEXT NOT NULL CHECK (entry_type IN ('File', 'Directory')),
    size INTEGER CHECK (size >= 0),
    sha256 TEXT REFERENCES cas_objects(digest),
    executable INTEGER CHECK (executable IN (0, 1)),
    PRIMARY KEY(artifact_id, path),
    CHECK ((entry_type = 'Directory' AND size IS NULL AND sha256 IS NULL AND executable IS NULL)
        OR (entry_type = 'File' AND size IS NOT NULL AND sha256 IS NOT NULL AND executable IS NOT NULL))
);
CREATE INDEX artifact_entries_digest_idx ON artifact_entries(sha256);
