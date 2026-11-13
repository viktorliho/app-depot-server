CREATE TABLE association_delete_operations (
    id TEXT PRIMARY KEY,
    owner_id TEXT NOT NULL REFERENCES users(id),
    status TEXT NOT NULL CHECK (status IN ('Completed', 'Failed'))
);
