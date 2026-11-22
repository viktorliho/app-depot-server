CREATE TABLE import_declarations (
    session_id TEXT PRIMARY KEY REFERENCES upload_sessions(id) ON DELETE CASCADE,
    data_json TEXT NOT NULL CHECK (json_valid(data_json))
);

CREATE TRIGGER retire_import_declaration_on_terminal_session
AFTER UPDATE OF phase ON upload_sessions
WHEN NEW.phase IN ('Failed', 'Cancelled')
BEGIN
    DELETE FROM import_declarations WHERE session_id=NEW.id;
END;
