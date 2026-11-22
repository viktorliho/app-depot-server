ALTER TABLE apps ADD COLUMN kind TEXT NOT NULL DEFAULT 'standard' CHECK (kind IN ('standard', 'derived'));

CREATE TABLE recipes (
    release_id TEXT PRIMARY KEY REFERENCES releases(id) ON DELETE CASCADE,
    definition_json TEXT NOT NULL
);

ALTER TABLE releases ADD COLUMN publication_order INTEGER;
UPDATE releases SET publication_order = rowid WHERE state = 'Published';
CREATE INDEX releases_publication_order_idx ON releases(app_id, publication_order DESC);
