CREATE TABLE deletion_operations (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES users(id),
    kind TEXT NOT NULL CHECK (kind IN ('DeleteAssociation', 'DeleteRelease', 'DeleteApp')),
    target_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('Accepted', 'Running', 'Completed', 'Failed')),
    physical_cleanup TEXT NOT NULL CHECK (physical_cleanup IN ('Pending', 'RetryableFailure', 'Completed')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX deletion_operations_pending_idx ON deletion_operations(status, physical_cleanup);

CREATE TABLE deletion_artifacts (
    operation_id TEXT NOT NULL REFERENCES deletion_operations(id) ON DELETE CASCADE,
    artifact_id TEXT NOT NULL,
    PRIMARY KEY(operation_id, artifact_id)
);

CREATE TABLE deletion_cas (
    operation_id TEXT NOT NULL REFERENCES deletion_operations(id) ON DELETE CASCADE,
    digest TEXT NOT NULL,
    PRIMARY KEY(operation_id, digest)
);

CREATE TABLE deletion_staging (
    operation_id TEXT NOT NULL REFERENCES deletion_operations(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    PRIMARY KEY(operation_id, session_id)
);

CREATE TABLE idempotency_tombstones (
    principal_id TEXT NOT NULL REFERENCES users(id),
    action TEXT NOT NULL,
    key TEXT NOT NULL,
    input_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(principal_id, action, key)
);
