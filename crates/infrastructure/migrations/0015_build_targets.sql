ALTER TABLE artwork_bindings ADD COLUMN source_url TEXT;
ALTER TABLE releases ADD COLUMN build_platform TEXT;
CREATE INDEX releases_build_history ON releases(app_id,build_platform,publication_order DESC);
ALTER TABLE store_catalogs ADD COLUMN refreshed_at INTEGER;

CREATE TABLE provider_cache (
    resource TEXT PRIMARY KEY,
    body BLOB NOT NULL,
    expires INTEGER NOT NULL,
    saved INTEGER NOT NULL DEFAULT(unixepoch())
);
-- Observations from the single-platform development catalog have no build key.
-- Retire only disposable Store state. Ordinary Apps and Releases remain intact.
DELETE FROM store_observations;
DELETE FROM store_catalogs;
ALTER TABLE store_observations ADD COLUMN product TEXT NOT NULL DEFAULT '';
ALTER TABLE store_observations ADD COLUMN platform TEXT NOT NULL DEFAULT '';
ALTER TABLE store_observations ADD COLUMN progress TEXT;
ALTER TABLE store_observations ADD COLUMN successful_check INTEGER;
CREATE TABLE store_products (
    source TEXT NOT NULL, id TEXT NOT NULL, app TEXT NOT NULL UNIQUE REFERENCES apps(id) ON DELETE CASCADE,
    PRIMARY KEY(source,id)
);

CREATE TABLE store_activity (
    sequence INTEGER PRIMARY KEY, source TEXT NOT NULL, build TEXT NOT NULL, title TEXT NOT NULL,
    platform TEXT NOT NULL, status TEXT NOT NULL, error TEXT, finished INTEGER NOT NULL DEFAULT(unixepoch())
);
CREATE TRIGGER store_finished AFTER UPDATE OF status ON store_observations
WHEN NEW.status IN ('Current','Failed','Update available') AND OLD.status != NEW.status
BEGIN
    INSERT INTO store_activity(source,build,title,platform,status,error)
    VALUES(NEW.source,NEW.id,json_extract(NEW.recipe,'$.title'),NEW.platform,NEW.status,NEW.error);
    DELETE FROM store_activity WHERE sequence <= (SELECT max(sequence)-256 FROM store_activity);
END;
