CREATE TABLE instance_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT INTO instance_meta (key, value) VALUES ('schema_version', '1');

CREATE TABLE dev_fixture_versions (
    version INTEGER PRIMARY KEY CHECK (version > 0),
    sample_name TEXT NOT NULL,
    sample_sha256 TEXT NOT NULL CHECK (length(sample_sha256) = 64)
);
