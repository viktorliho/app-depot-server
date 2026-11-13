CREATE TABLE users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    server_admin INTEGER NOT NULL CHECK (server_admin IN (0, 1)),
    publish_permission INTEGER NOT NULL CHECK (publish_permission IN (0, 1)),
    token_hash TEXT UNIQUE,
    token_issued_at TEXT
);

CREATE TABLE browser_sessions (
    id_hash TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    csrf_hash TEXT NOT NULL,
    expires_at INTEGER NOT NULL
);

CREATE TABLE apps (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    owner_id TEXT NOT NULL REFERENCES users(id),
    visibility TEXT NOT NULL DEFAULT 'Private' CHECK (visibility IN ('Private', 'Public')),
    state TEXT NOT NULL DEFAULT 'Draft' CHECK (state IN ('Draft', 'Published')),
    etag_version INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE releases (
    id TEXT PRIMARY KEY,
    app_id TEXT NOT NULL REFERENCES apps(id) ON DELETE CASCADE,
    version TEXT,
    state TEXT NOT NULL DEFAULT 'Draft' CHECK (state IN ('Draft', 'Published')),
    etag_version INTEGER NOT NULL DEFAULT 1
);

CREATE TABLE cas_objects (
    digest TEXT PRIMARY KEY CHECK (length(digest) = 64),
    size INTEGER NOT NULL CHECK (size >= 0),
    verified INTEGER NOT NULL CHECK (verified IN (0, 1))
);

CREATE TABLE artifacts (
    id TEXT PRIMARY KEY,
    digest TEXT NOT NULL REFERENCES cas_objects(digest),
    kind TEXT NOT NULL CHECK (kind = 'File'),
    size INTEGER NOT NULL CHECK (size >= 0)
);

CREATE TABLE associations (
    id TEXT PRIMARY KEY,
    release_id TEXT NOT NULL REFERENCES releases(id) ON DELETE CASCADE,
    artifact_id TEXT NOT NULL REFERENCES artifacts(id),
    etag_version INTEGER NOT NULL DEFAULT 1,
    UNIQUE(release_id, artifact_id)
);

CREATE TABLE operations (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES users(id),
    kind TEXT NOT NULL CHECK (kind IN ('Finalize', 'CancelUpload')),
    status TEXT NOT NULL CHECK (status IN ('Accepted', 'Running', 'Completed', 'Failed')),
    result_json TEXT,
    problem_json TEXT
);

CREATE TABLE upload_sessions (
    id TEXT PRIMARY KEY,
    operation_id TEXT NOT NULL UNIQUE REFERENCES operations(id),
    cancel_operation_id TEXT UNIQUE REFERENCES operations(id),
    owner_id TEXT NOT NULL REFERENCES users(id),
    mode TEXT NOT NULL CHECK (mode = 'BareFile'),
    phase TEXT NOT NULL CHECK (phase IN ('Receiving', 'Verifying', 'Committing', 'Completed', 'Failed', 'Cancelled')),
    terminal_code TEXT,
    target_json TEXT NOT NULL,
    file_id TEXT NOT NULL UNIQUE,
    file_size INTEGER NOT NULL CHECK (file_size >= 0),
    file_sha256 TEXT NOT NULL CHECK (length(file_sha256) = 64),
    file_path TEXT NOT NULL,
    progress_version INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE upload_ranges (
    session_id TEXT NOT NULL REFERENCES upload_sessions(id) ON DELETE CASCADE,
    start INTEGER NOT NULL CHECK (start >= 0),
    end INTEGER NOT NULL CHECK (end > start),
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
    PRIMARY KEY(session_id, start, end)
);

CREATE TABLE prepared_claims (
    operation_id TEXT NOT NULL REFERENCES operations(id) ON DELETE CASCADE,
    digest TEXT NOT NULL CHECK (length(digest) = 64),
    PRIMARY KEY(operation_id, digest)
);

CREATE TABLE cleanup_obligations (
    operation_id TEXT PRIMARY KEY REFERENCES operations(id) ON DELETE CASCADE,
    staging_path TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('Pending', 'Completed', 'RetryableFailure'))
);

CREATE TABLE idempotency_bindings (
    principal_id TEXT NOT NULL REFERENCES users(id),
    action TEXT NOT NULL,
    key TEXT NOT NULL,
    input_hash TEXT NOT NULL,
    result_id TEXT NOT NULL,
    result_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(principal_id, action, key)
);

CREATE INDEX upload_sessions_phase_idx ON upload_sessions(phase);
CREATE INDEX associations_artifact_idx ON associations(artifact_id);
