ALTER TABLE artifact_entries ADD COLUMN file_id TEXT;
CREATE UNIQUE INDEX artifact_entries_file_id_idx ON artifact_entries(file_id);
