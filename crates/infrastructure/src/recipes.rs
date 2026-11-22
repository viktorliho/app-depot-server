use crate::catalog::{etag, CatalogError};
use app_depot_application::{may_read_catalog, Principal};
use app_depot_domain::recipe::{self, Collision, Copy, Launch, RecipeError, RecipeV1, Selector};
use serde::Serialize;
use sqlx::{Row, Sqlite, SqliteConnection, SqlitePool, Transaction};
use std::collections::{btree_map::Entry, BTreeMap, BTreeSet};

const MAX_ENTRIES: usize = 50_000;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Lock {
    version: u32,
    app_id: String,
    release_id: String,
    platform: String,
    releases: BTreeMap<String, LockedRelease>,
    files: BTreeMap<String, File>,
    directories: BTreeSet<String>,
    launch: Launch,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LockedRelease {
    app_id: String,
    version: Option<String>,
    dependencies: BTreeMap<String, String>,
    artifacts: BTreeMap<String, String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct File {
    association_id: String,
    artifact_id: String,
    file_id: Option<String>,
    sha256: String,
    size: i64,
    executable: bool,
}

#[derive(Default)]
struct Layout {
    entries: BTreeMap<String, Option<File>>,
}

impl Layout {
    fn insert(
        &mut self,
        path: String,
        file: Option<File>,
        collision: Collision,
    ) -> Result<(), RecipeError> {
        if !recipe::path(&path) {
            return Err(RecipeError::Source);
        }
        if let Some(prior) = self.entries.get(&path) {
            return if prior.is_some()
                && file.is_some()
                && matches!(collision, Collision::ReplaceFile)
            {
                self.entries.insert(path, file);
                Ok(())
            } else if prior.is_none() && file.is_none() {
                Ok(())
            } else {
                Err(RecipeError::Collision)
            };
        }
        if let Some((parent, _)) = path.rsplit_once('/') {
            self.insert(parent.to_owned(), None, Collision::Reject)?;
        }
        if self.entries.len() >= MAX_ENTRIES {
            return Err(RecipeError::Limit);
        }
        self.entries.insert(path, file);
        Ok(())
    }

    fn copy(&mut self, source: &Self, op: &Copy) -> Result<(), RecipeError> {
        if !op.source.is_empty() {
            match source.entries.get(&op.source) {
                Some(Some(file)) => return self.copy_file(file.clone(), op),
                Some(None) => {}
                None => return Err(RecipeError::Source),
            }
        }
        if op.executable {
            return Err(RecipeError::Source);
        }
        if !op.destination.is_empty() {
            self.insert(op.destination.clone(), None, Collision::Reject)?;
        }
        let prefix = if op.source.is_empty() {
            String::new()
        } else {
            format!("{}/", op.source)
        };
        for (path, entry) in source.entries.range(prefix.clone()..) {
            let Some(suffix) = path.strip_prefix(&prefix) else {
                break;
            };
            let destination = if op.destination.is_empty() {
                suffix.to_owned()
            } else {
                format!("{}/{suffix}", op.destination)
            };
            self.insert(destination, entry.clone(), op.collision)?;
        }
        Ok(())
    }

    fn copy_file(&mut self, mut file: File, op: &Copy) -> Result<(), RecipeError> {
        file.executable |= op.executable;
        self.insert(op.destination.clone(), Some(file), op.collision)
    }
}

struct ReleaseRow {
    app_id: String,
    owner: String,
    public: bool,
    published: bool,
    derived: bool,
    version: Option<String>,
    etag_version: i64,
}

async fn release(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Option<ReleaseRow>, CatalogError> {
    let row = sqlx::query("SELECT r.app_id, r.state, r.version, r.etag_version, a.owner_id, a.visibility, a.kind FROM releases r JOIN apps a ON a.id=r.app_id WHERE r.id=?")
        .bind(id).fetch_optional(conn).await?;
    row.map(|row| {
        Ok(ReleaseRow {
            app_id: row.try_get("app_id")?,
            owner: row.try_get("owner_id")?,
            public: row.try_get::<&str, _>("visibility")? == "Public",
            published: row.try_get::<&str, _>("state")? == "Published",
            derived: row.try_get::<&str, _>("kind")? == "derived",
            version: row.try_get("version")?,
            etag_version: row.try_get("etag_version")?,
        })
    })
    .transpose()
}

async fn definition(
    conn: &mut SqliteConnection,
    id: &str,
) -> Result<Option<RecipeV1>, CatalogError> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT definition_json FROM recipes WHERE release_id=?")
            .bind(id)
            .fetch_optional(conn)
            .await?;
    value
        .map(|value| Ok(serde_json::from_str(&value)?))
        .transpose()
}

async fn anonymous_enabled(
    conn: &mut SqliteConnection,
    actor: Option<&Principal>,
) -> Result<bool, CatalogError> {
    Ok(actor.is_none()
        && sqlx::query_scalar::<_, bool>(
            "SELECT anonymous_read_enabled FROM server_settings WHERE id=1",
        )
        .fetch_one(conn)
        .await?)
}

pub async fn get(
    pool: &SqlitePool,
    actor: Option<&Principal>,
    id: &str,
) -> Result<(Option<RecipeV1>, String), CatalogError> {
    let mut tx = pool.begin().await?;
    let anonymous = anonymous_enabled(&mut tx, actor).await?;
    let root = release(&mut tx, id).await?.ok_or(CatalogError::NotFound)?;
    if !root.derived
        || !may_read_catalog(actor, &root.owner, root.public && root.published, anonymous)
    {
        return Err(CatalogError::NotFound);
    }
    let recipe = definition(&mut tx, id).await?;
    if !actor.is_some_and(|actor| actor.may_manage(&root.owner)) {
        let recipe = recipe.as_ref().ok_or(RecipeError::Required)?;
        for platform in recipe.targets.keys() {
            resolve(
                &mut tx,
                (actor, anonymous),
                id,
                &root,
                recipe,
                platform,
                true,
            )
            .await?;
        }
    }
    Ok((recipe, etag("release", id, root.etag_version)))
}

pub async fn put(
    pool: &SqlitePool,
    actor: &Principal,
    id: &str,
    if_match: Option<&str>,
    recipe: &RecipeV1,
) -> Result<String, CatalogError> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let root = release(&mut tx, id).await?.ok_or(CatalogError::NotFound)?;
    if !root.derived || !actor.may_manage(&root.owner) {
        return Err(CatalogError::NotFound);
    }
    if root.published {
        return Err(CatalogError::ImmutableRelease);
    }
    if if_match.ok_or(CatalogError::PreconditionRequired)? != etag("release", id, root.etag_version)
    {
        return Err(CatalogError::StaleEtag);
    }
    recipe.validate()?;
    sqlx::query("INSERT INTO recipes(release_id, definition_json) VALUES(?, ?) ON CONFLICT(release_id) DO UPDATE SET definition_json=excluded.definition_json")
        .bind(id).bind(serde_json::to_string(recipe)?).execute(&mut *tx).await?;
    sqlx::query("UPDATE releases SET etag_version=etag_version+1 WHERE id=?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(etag("release", id, root.etag_version + 1))
}

pub async fn lock(
    pool: &SqlitePool,
    actor: Option<&Principal>,
    id: &str,
    platform: &str,
) -> Result<Lock, CatalogError> {
    let mut tx = pool.begin().await?;
    let anonymous = anonymous_enabled(&mut tx, actor).await?;
    let root = release(&mut tx, id).await?.ok_or(CatalogError::NotFound)?;
    if !root.derived
        || !may_read_catalog(actor, &root.owner, root.public && root.published, anonymous)
    {
        return Err(CatalogError::NotFound);
    }
    let recipe = definition(&mut tx, id)
        .await?
        .ok_or(RecipeError::Required)?;
    resolve(
        &mut tx,
        (actor, anonymous),
        id,
        &root,
        &recipe,
        platform,
        true,
    )
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    files: BTreeSet<String>,
    launch_candidates: BTreeSet<String>,
    directories: BTreeSet<String>,
}

pub async fn preview(
    pool: &SqlitePool,
    actor: &Principal,
    id: &str,
    platform: &str,
    recipe: &RecipeV1,
) -> Result<Preview, CatalogError> {
    let mut tx = pool.begin().await?;
    let root = release(&mut tx, id).await?.ok_or(CatalogError::NotFound)?;
    if !root.derived || !actor.may_manage(&root.owner) {
        return Err(CatalogError::NotFound);
    }
    if root.published {
        return Err(CatalogError::ImmutableRelease);
    }
    let result = resolve(
        &mut tx,
        (Some(actor), false),
        id,
        &root,
        recipe,
        platform,
        false,
    )
    .await?;
    let launch_candidates = result
        .files
        .iter()
        .filter(|(_, file)| can_launch(platform, file))
        .map(|(path, _)| path.clone())
        .collect();
    Ok(Preview {
        launch_candidates,
        files: result.files.into_keys().collect(),
        directories: result.directories,
    })
}

pub(crate) async fn validate_publication(
    tx: &mut Transaction<'_, Sqlite>,
    actor: &Principal,
    id: &str,
) -> Result<(), CatalogError> {
    let root = release(tx, id).await?.ok_or(CatalogError::NotFound)?;
    let recipe = definition(tx, id).await?.ok_or(RecipeError::Required)?;
    recipe.validate()?;
    for platform in recipe.targets.keys() {
        resolve(tx, (Some(actor), false), id, &root, &recipe, platform, true).await?;
    }
    Ok(())
}

async fn resolve(
    conn: &mut SqliteConnection,
    access: (Option<&Principal>, bool),
    id: &str,
    root: &ReleaseRow,
    definition: &RecipeV1,
    platform: &str,
    check_launch: bool,
) -> Result<Lock, CatalogError> {
    let (actor, anonymous) = access;
    let mut resolver = Resolver {
        conn,
        actor,
        anonymous,
        platform,
        active: BTreeSet::new(),
        selected: BTreeMap::new(),
        releases: BTreeMap::new(),
        work: 0,
    };
    let layout = resolver
        .assembly(id, root, definition, check_launch)
        .await?;
    let mut files = BTreeMap::new();
    let mut directories = BTreeSet::new();
    for (path, entry) in layout.entries {
        if let Some(file) = entry {
            files.insert(path, file);
        } else {
            directories.insert(path);
        }
    }
    Ok(Lock {
        version: 1,
        app_id: root.app_id.clone(),
        release_id: id.to_owned(),
        platform: platform.to_owned(),
        releases: resolver.releases,
        files,
        directories,
        launch: definition.targets[platform].launch.clone(),
    })
}

struct Resolver<'a> {
    conn: &'a mut SqliteConnection,
    actor: Option<&'a Principal>,
    anonymous: bool,
    platform: &'a str,
    active: BTreeSet<String>,
    selected: BTreeMap<String, String>,
    releases: BTreeMap<String, LockedRelease>,
    work: usize,
}

impl Resolver<'_> {
    fn spend(&mut self, amount: usize) -> Result<(), RecipeError> {
        self.work += amount;
        if self.work > 4 * MAX_ENTRIES {
            return Err(RecipeError::Limit);
        }
        Ok(())
    }

    fn record_dependency(
        &mut self,
        app_id: &str,
        id: &str,
        row: &ReleaseRow,
    ) -> Result<(), RecipeError> {
        if self.selected.get(app_id).is_some_and(|prior| prior != id) {
            return Err(RecipeError::Conflict);
        }
        if self.selected.len() >= 128 && !self.selected.contains_key(app_id) {
            return Err(RecipeError::Limit);
        }
        self.selected.insert(app_id.to_owned(), id.to_owned());
        self.releases
            .entry(id.to_owned())
            .or_insert_with(|| LockedRelease {
                app_id: app_id.to_owned(),
                version: row.version.clone(),
                dependencies: BTreeMap::new(),
                artifacts: BTreeMap::new(),
            });
        Ok(())
    }

    async fn assembly(
        &mut self,
        id: &str,
        row: &ReleaseRow,
        recipe: &RecipeV1,
        check_launch: bool,
    ) -> Result<Layout, CatalogError> {
        recipe.validate()?;
        if self.active.len() >= 16 {
            return Err(RecipeError::Limit.into());
        }
        self.active.insert(row.app_id.clone());
        self.record_dependency(&row.app_id, id, row)?;
        let target = recipe
            .targets
            .get(self.platform)
            .ok_or(RecipeError::Platform)?;
        let mut layout = Layout::default();
        let mut sources = BTreeMap::new();
        self.spend(target.operations.len())?;
        for op in &target.operations {
            let key = (&op.dependency, &op.association_id, &op.association_label);
            if let Entry::Vacant(entry) = sources.entry(key) {
                let dep = &recipe.dependencies[&op.dependency];
                if self.active.contains(&dep.app_id) {
                    return Err(RecipeError::Cycle.into());
                }
                // Gate even channel lookup on the dependency App's ACL.
                let access = sqlx::query("SELECT owner_id, visibility, state FROM apps WHERE id=?")
                    .bind(&dep.app_id)
                    .fetch_optional(&mut *self.conn)
                    .await?
                    .ok_or(RecipeError::Unavailable)?;
                if !may_read_catalog(
                    self.actor,
                    access.try_get("owner_id")?,
                    access.try_get::<&str, _>("visibility")? == "Public"
                        && access.try_get::<&str, _>("state")? == "Published",
                    self.anonymous,
                ) {
                    return Err(RecipeError::Unavailable.into());
                }
                let selected = match &dep.selector {
                    Selector::Latest => sqlx::query_scalar(
                        "SELECT id FROM releases WHERE app_id=? AND state='Published' ORDER BY publication_order DESC LIMIT 1",
                    ).bind(&dep.app_id).fetch_optional(&mut *self.conn).await?.ok_or(RecipeError::Unavailable)?,
                    Selector::Release { release_id } => release_id.clone(),
                    Selector::Channel { name } => sqlx::query_scalar("SELECT release_id FROM channels WHERE app_id=? AND name=?")
                        .bind(&dep.app_id).bind(name).fetch_optional(&mut *self.conn).await?.ok_or(RecipeError::Unavailable)?,
                };
                let dependency = release(self.conn, &selected)
                    .await?
                    .filter(|row| row.app_id == dep.app_id && row.published)
                    .ok_or(RecipeError::Unavailable)?;
                if dep
                    .platforms
                    .as_ref()
                    .is_some_and(|platforms| !platforms.iter().any(|p| p == self.platform))
                {
                    return Err(RecipeError::Platform.into());
                }
                self.record_dependency(&dep.app_id, &selected, &dependency)?;
                self.releases
                    .get_mut(id)
                    .expect("registered release")
                    .dependencies
                    .insert(op.dependency.clone(), selected.clone());
                let source = if dependency.derived {
                    if op.association_id.is_some() || op.association_label.is_some() {
                        return Err(RecipeError::Association.into());
                    }
                    let child = definition(self.conn, &selected)
                        .await?
                        .ok_or(RecipeError::Required)?;
                    Source::Tree(
                        Box::pin(self.assembly(&selected, &dependency, &child, true)).await?,
                    )
                } else {
                    self.artifact(&selected, op).await?
                };
                entry.insert(source);
            }
            match &sources[&key] {
                Source::File(file) if op.source.is_empty() => layout.copy_file(file.clone(), op)?,
                Source::File(_) => return Err(RecipeError::Source.into()),
                Source::Tree(source) => {
                    self.spend(source.entries.len())?;
                    layout.copy(source, op)?;
                }
            }
        }
        self.active.remove(&row.app_id);
        if self.platform.starts_with("windows/") {
            let mut paths = BTreeSet::new();
            for path in layout.entries.keys() {
                if !path.split('/').all(windows_component) {
                    return Err(RecipeError::Source.into());
                }
                if !paths.insert(path.to_lowercase()) {
                    return Err(RecipeError::Collision.into());
                }
            }
        }
        if !check_launch {
            return Ok(layout);
        }
        let launch = &target.launch;
        let entry = layout
            .entries
            .get(&launch.path)
            .and_then(Option::as_ref)
            .ok_or(RecipeError::Launch)?;
        if !can_launch(self.platform, entry)
            || (!launch.working_directory.is_empty()
                && !matches!(layout.entries.get(&launch.working_directory), Some(None)))
        {
            return Err(RecipeError::Launch.into());
        }
        Ok(layout)
    }

    async fn artifact(&mut self, release_id: &str, op: &Copy) -> Result<Source, CatalogError> {
        let rows = sqlx::query(
            "SELECT x.id, x.artifact_id, a.kind, a.digest, a.size
             FROM associations x JOIN artifacts a ON a.id=x.artifact_id
             LEFT JOIN declaration_documents doc ON doc.scope='Association' AND doc.resource_id=x.id
             LEFT JOIN declaration_revisions rev ON rev.id=doc.revision_id
             WHERE x.release_id=? AND (? IS NULL OR x.id=?)
             AND (? IS NULL OR (json_type(rev.data_json, '$.appDepot.label')='text'
                  AND json_extract(rev.data_json, '$.appDepot.label')=?))
             ORDER BY x.id LIMIT 2",
        )
        .bind(release_id)
        .bind(op.association_id.as_deref())
        .bind(op.association_id.as_deref())
        .bind(op.association_label.as_deref())
        .bind(op.association_label.as_deref())
        .fetch_all(&mut *self.conn)
        .await?;
        let [row] = rows.as_slice() else {
            return Err(RecipeError::Association.into());
        };
        let id: String = row.try_get("id")?;
        let artifact_id: String = row.try_get("artifact_id")?;
        self.releases
            .get_mut(release_id)
            .expect("registered release")
            .artifacts
            .insert(id.clone(), artifact_id.clone());
        if row.try_get::<&str, _>("kind")? == "File" {
            return Ok(Source::File(File {
                association_id: id,
                artifact_id,
                file_id: None,
                sha256: row.try_get("digest")?,
                size: row.try_get("size")?,
                executable: false,
            }));
        }
        let mut layout = Layout::default();
        let rows = sqlx::query("SELECT path, file_id, sha256, size, executable FROM artifact_entries WHERE artifact_id=? ORDER BY path LIMIT ?")
            .bind(&artifact_id).bind((MAX_ENTRIES + 1) as i64).fetch_all(&mut *self.conn).await?;
        self.spend(rows.len())?;
        if rows.len() > MAX_ENTRIES {
            return Err(RecipeError::Limit.into());
        }
        for row in rows {
            let sha256: Option<String> = row.try_get("sha256")?;
            let file = if let Some(sha256) = sha256 {
                Some(File {
                    association_id: id.clone(),
                    artifact_id: artifact_id.clone(),
                    file_id: Some(row.try_get("file_id")?),
                    sha256,
                    size: row.try_get("size")?,
                    executable: row.try_get("executable")?,
                })
            } else {
                None
            };
            // Source Artifact paths follow Tree Identity v1. Validate only paths copied to the destination.
            layout.entries.insert(row.try_get("path")?, file);
        }
        Ok(Source::Tree(layout))
    }
}

enum Source {
    File(File),
    Tree(Layout),
}

fn can_launch(platform: &str, file: &File) -> bool {
    platform.starts_with("windows/") || file.executable
}

fn windows_component(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end()
        .to_ascii_uppercase();
    let device = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        });
    !device
        && !component.contains(['<', '>', '"', '|', '?', '*'])
        && !component.ends_with([' ', '.'])
}
