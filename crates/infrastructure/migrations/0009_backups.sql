CREATE TABLE backup_operations (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES users(id),
    backup_id TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL CHECK (status IN ('Accepted', 'Running', 'Completed', 'Failed')),
    problem_json TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE backup_pins (
    operation_id TEXT NOT NULL REFERENCES backup_operations(id) ON DELETE CASCADE,
    digest TEXT NOT NULL,
    PRIMARY KEY(operation_id, digest)
);
