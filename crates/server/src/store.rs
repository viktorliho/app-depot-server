use crate::auth::{authorize, ApiError, AuthState};
use app_depot_infrastructure::catalog::CatalogError;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io::Read,
    process::{Command, Stdio},
    sync::Arc,
};
use tokio::sync::Semaphore;

const OFFICIAL: &str = "https://raw.githubusercontent.com/viktorliho/app-depot-server/main/store";
const LIMIT: usize = 256 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    schema: String,
    generation: i64,
    recipes: Vec<Entry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    id: String,
    sha256: String,
}

fn invalid() -> ApiError {
    ApiError::problem(
        StatusCode::UNPROCESSABLE_ENTITY,
        "invalid_store_catalog",
        "Invalid or incompatible Store catalog",
    )
}

fn fetch(url: &str) -> Result<Vec<u8>, ApiError> {
    bounded_curl(&["--max-filesize", &LIMIT.to_string(), "--", url])
}

fn bounded_curl(args: &[&str]) -> Result<Vec<u8>, ApiError> {
    let mut child = Command::new("curl")
        .args([
            "--disable",
            "--fail",
            "--silent",
            "--proto",
            "=https",
            "--max-time",
            "20",
        ])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| ApiError::server())?;
    let mut bytes = Vec::new();
    let read = child
        .stdout
        .take()
        .unwrap()
        .take((LIMIT + 1) as u64)
        .read_to_end(&mut bytes);
    let ok = child.wait().map_err(|_| ApiError::server())?.success();
    if !ok || read.is_err() || bytes.len() > LIMIT {
        return Err(ApiError::problem(
            StatusCode::BAD_GATEWAY,
            "store_source_unavailable",
            "Catalog download failed",
        ));
    }
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn snapshot(
    base: &str,
    read: impl Fn(&str) -> Result<Vec<u8>, ApiError>,
) -> Result<(i64, String, Vec<Value>), ApiError> {
    let raw = read(&format!("{base}/catalog.json"))?;
    let index: Index = serde_json::from_slice(&raw).map_err(|_| invalid())?;
    if index.schema != "StoreCatalogV1" || index.generation < 1 || index.recipes.len() > 128 {
        return Err(invalid());
    }
    let mut ids = HashSet::new();
    let mut recipes = Vec::new();
    let mut total = 0;
    for entry in index.recipes {
        if entry.id.is_empty()
            || entry.id.len() > 128
            || !entry
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
            || !ids.insert(entry.id.clone())
        {
            return Err(invalid());
        }
        let bytes = read(&format!("{base}/recipes/{}.json", entry.id))?;
        total += bytes.len();
        if total > 4 * 1024 * 1024 || digest(&bytes) != entry.sha256 {
            return Err(invalid());
        }
        let recipe: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        if recipe["id"] != entry.id
            || recipe["schema"] != "StoreRecipeV1"
            || recipe["revision"].as_u64().unwrap_or(0) == 0
            || !matches!(
                recipe["availability"].as_str(),
                Some("available" | "reviewRequired")
            )
        {
            return Err(invalid());
        }
        let parsed: Recipe = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
        parsed.validate().map_err(|_| invalid())?;
        recipes.push(recipe);
    }
    Ok((index.generation, digest(&raw), recipes))
}

pub(super) fn validate_local(directory: &str) -> std::io::Result<()> {
    let (generation, _, recipes) = snapshot(directory, |path| {
        let file = std::fs::File::open(path).map_err(|_| invalid())?;
        let mut bytes = Vec::new();
        file.take((LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() > LIMIT {
            return Err(invalid());
        }
        Ok(bytes)
    })
    .map_err(|_| std::io::Error::other("Invalid, incompatible or unreadable Store catalog"))?;
    println!(
        "Store generation {generation}: {} compatible recipes",
        recipes.len()
    );
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Store {
    auth: AuthState,
    import: crate::server_import::ImportState,
    scratch: Arc<tempfile::TempDir>,
    refresh: Arc<Semaphore>,
    base: String,
}

async fn admin(state: &Store, headers: &HeaderMap, write: bool) -> Result<(), ApiError> {
    if !authorize(&state.auth, headers, write).await?.server_admin {
        return Err(CatalogError::Forbidden.into());
    }
    Ok(())
}

async fn list(State(state): State<Store>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    admin(&state, &headers, false).await?;
    let cached: Option<(i64, String)> =
        sqlx::query_as("SELECT generation,recipes FROM store_catalogs WHERE source=?")
            .bind(&state.base)
            .fetch_optional(&state.auth.pool)
            .await
            .map_err(CatalogError::Sql)?;
    let (generation, recipes) = cached.unwrap_or((0, "[]".into()));
    let recipes: Value = serde_json::from_str(&recipes).map_err(|_| ApiError::server())?;
    Ok(Json(json!({"generation":generation,"recipes":recipes})))
}

async fn refresh(State(state): State<Store>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    admin(&state, &headers, true).await?;
    let permit = state.refresh.clone().try_acquire_owned().map_err(|_| {
        ApiError::problem(
            StatusCode::CONFLICT,
            "refresh_running",
            "Catalog refresh is running",
        )
    })?;
    let base = state.base.clone();
    let (generation, hash, recipes) = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        snapshot(&base, fetch)
    })
    .await
    .map_err(|_| ApiError::server())??;
    let mut tx = state
        .auth
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(CatalogError::Sql)?;
    let prior: Option<(i64, String, String)> =
        sqlx::query_as("SELECT generation,digest,recipes FROM store_catalogs WHERE source=?")
            .bind(&state.base)
            .fetch_optional(&mut *tx)
            .await
            .map_err(CatalogError::Sql)?;
    if let Some((old_generation, old_digest, old_entries)) = prior {
        if generation < old_generation || (generation == old_generation && old_digest != hash) {
            return Err(invalid());
        }
        let old: Vec<Value> = serde_json::from_str(&old_entries).map_err(|_| ApiError::server())?;
        for candidate in &recipes {
            if let Some(previous) = old.iter().find(|r| r["id"] == candidate["id"]) {
                if previous != candidate
                    && candidate["revision"].as_u64() <= previous["revision"].as_u64()
                {
                    return Err(invalid());
                }
            }
        }
    }
    sqlx::query("INSERT INTO store_catalogs(source,generation,digest,recipes) VALUES(?,?,?,?) ON CONFLICT(source) DO UPDATE SET generation=excluded.generation,digest=excluded.digest,recipes=excluded.recipes")
        .bind(&state.base).bind(generation).bind(hash).bind(json!(recipes).to_string()).execute(&mut *tx).await.map_err(CatalogError::Sql)?;
    tx.commit().await.map_err(CatalogError::Sql)?;
    Ok(Json(json!({"generation":generation,"recipes":recipes})))
}

use crate::{
    import_source::ImportRoot,
    server_import::{import_store_tree, ImportState},
    store_prepare::{prepare, Recipe},
};
use app_depot_application::{upload::Target, Principal};
use app_depot_infrastructure::{catalog, installation};
use axum::extract::Path;
use sqlx::Row;

async fn cached_recipe(state: &Store, id: &str) -> Result<Value, ApiError> {
    let value:Option<String>=sqlx::query_scalar("SELECT json_each.value FROM store_catalogs,json_each(store_catalogs.recipes) WHERE source=? AND json_extract(json_each.value,'$.id')=?").bind(&state.base).bind(id).fetch_optional(&state.auth.pool).await.map_err(CatalogError::Sql)?;
    let recipe: Value =
        serde_json::from_str(&value.ok_or(CatalogError::NotFound)?).map_err(|_| invalid())?;
    if recipe["availability"] != "available" {
        return Err(ApiError::problem(
            StatusCode::CONFLICT,
            "store_rights_review",
            "Acquisition requires rights review",
        ));
    }
    Ok(recipe)
}

async fn add(
    State(s): State<Store>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let actor = authorize(&s.auth, &h, true).await?;
    if !actor.server_admin {
        return Err(CatalogError::Forbidden.into());
    }
    let recipe = cached_recipe(&s, &id).await?;
    sqlx::query("INSERT INTO store_observations(source,id,owner,recipe) VALUES(?,?,?,?) ON CONFLICT(source,id) DO NOTHING").bind(&s.base).bind(&id).bind(actor.id).bind(recipe.to_string()).execute(&s.auth.pool).await.map_err(CatalogError::Sql)?;
    Ok((StatusCode::ACCEPTED, Json(json!({"id":id}))))
}
async fn adopt(
    State(s): State<Store>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&s, &h, true).await?;
    let recipe = cached_recipe(&s, &id).await?;
    let result=sqlx::query("UPDATE store_observations SET recipe=?,pending=NULL,apply_once=0,status='Queued' WHERE source=? AND id=? AND json_extract(recipe,'$.revision')<? AND status IN ('Current','Failed','Update available')").bind(recipe.to_string()).bind(&s.base).bind(&id).bind(recipe["revision"].as_i64().ok_or_else(invalid)?).execute(&s.auth.pool).await.map_err(CatalogError::Sql)?;
    if result.rows_affected() == 0 {
        return Err(CatalogError::Conflict.into());
    }
    Ok((StatusCode::ACCEPTED, Json(json!({"id":id}))))
}
#[derive(serde::Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
struct Observation {
    id: String,
    title: String,
    revision: i64,
    app: Option<String>,
    release: Option<String>,
    enabled: bool,
    automatic: bool,
    hours: i64,
    status: String,
    last_check: Option<i64>,
    detected: Option<String>,
    error: Option<String>,
    files_done: i64,
    files_total: i64,
    bytes_done: i64,
    bytes_total: i64,
}
async fn observed(
    State(state): State<Store>,
    headers: HeaderMap,
) -> Result<Json<Vec<Observation>>, ApiError> {
    admin(&state, &headers, false).await?;
    let rows = sqlx::query_as(
        "SELECT id,json_extract(recipe,'$.title') title,json_extract(recipe,'$.revision') revision,
                app,release,enabled,automatic,hours,status,last_check,detected,error,files_done,files_total,bytes_done,bytes_total
                FROM store_observations WHERE source=? ORDER BY title",
    )
    .bind(&state.base)
    .fetch_all(&state.auth.pool)
    .await
    .map_err(CatalogError::Sql)?;
    Ok(Json(rows))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    enabled: bool,
    automatic: bool,
    hours: i64,
}
async fn settings(
    State(s): State<Store>,
    Path(id): Path<String>,
    h: HeaderMap,
    Json(input): Json<Settings>,
) -> Result<StatusCode, ApiError> {
    admin(&s, &h, true).await?;
    if !(1..=8760).contains(&input.hours) {
        return Err(invalid());
    }
    let result = sqlx::query(
        "UPDATE store_observations SET enabled=?,automatic=?,hours=? WHERE source=? AND id=?",
    )
    .bind(input.enabled)
    .bind(input.automatic)
    .bind(input.hours)
    .bind(&s.base)
    .bind(id)
    .execute(&s.auth.pool)
    .await
    .map_err(CatalogError::Sql)?;
    if result.rows_affected() == 0 {
        return Err(CatalogError::NotFound.into());
    }
    Ok(StatusCode::NO_CONTENT)
}
async fn check(
    State(s): State<Store>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&s, &h, true).await?;
    let result=sqlx::query("UPDATE store_observations SET status='Queued' WHERE source=? AND id=? AND status IN ('Queued','Current','Failed','Update available')").bind(&s.base).bind(&id).execute(&s.auth.pool).await.map_err(CatalogError::Sql)?;
    if result.rows_affected() == 0 {
        return Err(CatalogError::Conflict.into());
    }
    Ok((StatusCode::ACCEPTED, Json(json!({"id":id}))))
}
async fn apply_update(
    State(s): State<Store>,
    Path(id): Path<String>,
    h: HeaderMap,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    admin(&s, &h, true).await?;
    let result=sqlx::query("UPDATE store_observations SET status='Queued',apply_once=1 WHERE source=? AND id=? AND status='Update available'")
        .bind(&s.base).bind(&id).execute(&s.auth.pool).await.map_err(CatalogError::Sql)?;
    if result.rows_affected() == 0 {
        return Err(CatalogError::Conflict.into());
    }
    Ok((StatusCode::ACCEPTED, Json(json!({"id":id}))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Version {
    version: String,
    sha256: String,
    bytes: u64,
}
fn github_release(selector: &crate::store_prepare::ReleaseAsset) -> Result<Value, &'static str> {
    let base = format!(
        "https://api.github.com/repos/{}/releases",
        selector.repository
    );
    for page in 1..=4 {
        let url = if selector.previews {
            format!("{base}/latest")
        } else {
            format!("{base}?per_page=8&page={page}")
        };
        let raw = fetch(&url).map_err(|_| "GitHub release discovery failed")?;
        let document: Value =
            serde_json::from_slice(&raw).map_err(|_| "Invalid GitHub release response")?;
        let releases = if selector.previews {
            vec![document]
        } else {
            document
                .as_array()
                .cloned()
                .ok_or("Invalid GitHub release list")?
        };
        let page_size = releases.len();
        for release in releases {
            let tag = release["tag_name"]
                .as_str()
                .filter(|v| !v.is_empty() && v.len() <= 80)
                .ok_or("Invalid GitHub release tag")?
                .to_ascii_lowercase();
            let preview = release["prerelease"] == true
                || ["-rc", "preview", "beta", "alpha"]
                    .iter()
                    .any(|marker| tag.contains(marker));
            if release["draft"] != true && (selector.previews || !preview) {
                return Ok(release);
            }
        }
        if page_size < 8 || selector.previews {
            break;
        }
    }
    Err("No compatible upstream release in the discovery window")
}

fn discover(
    mut recipe: Recipe,
    previous: Option<Value>,
    scratch: &std::path::Path,
) -> Result<(Recipe, Value), &'static str> {
    let mut validators = json!({});
    let mut versions = Vec::new();
    for component in &mut recipe.components {
        if component.discovery.is_none() && component.github.is_none() {
            let head = bounded_curl(&[
                "--head",
                "--location",
                "--max-redirs",
                "5",
                "--proto-redir",
                "=https",
                "--output",
                "/dev/null",
                "--write-out",
                "%header{etag}",
                "--",
                &component.url,
            ])
            .map_err(|_| "Pinned resource unavailable")?;
            let etag = std::str::from_utf8(&head)
                .map_err(|_| "Invalid upstream validator")?
                .trim();
            let identity = json!([component.url, component.sha256, etag]);
            if previous
                .as_ref()
                .is_some_and(|v| v[&component.name] != identity || !etag.starts_with('"'))
            {
                let temp = tempfile::tempdir_in(scratch)
                    .map_err(|_| "Verification staging unavailable")?;
                crate::store_prepare::download(component, &temp.path().join("verify"))
                    .map_err(|_| "Pinned resource changed or download failed")?;
            }
            validators[&component.name] = identity;
        }
        if let Some(selector) = &component.github {
            let release = github_release(selector)?;
            let tag = release["tag_name"]
                .as_str()
                .ok_or("Missing GitHub release tag")?;
            let assets = release["assets"]
                .as_array()
                .ok_or("Missing GitHub release assets")?;
            let mut matches = assets.iter().filter(|asset| {
                asset["name"].as_str().is_some_and(|n| {
                    n.starts_with(&selector.prefix)
                        && n.ends_with(&selector.suffix)
                        && selector
                            .exclude
                            .as_ref()
                            .is_none_or(|pattern| !n.contains(pattern))
                })
            });
            let asset = matches.next().ok_or("Patch asset unavailable")?;
            if matches.next().is_some() {
                return Err("Ambiguous patch asset");
            }
            let url = asset["browser_download_url"]
                .as_str()
                .ok_or("Missing patch URL")?;
            if !url.starts_with(&format!(
                "https://github.com/{}/releases/download/",
                selector.repository
            )) {
                return Err("Unexpected patch source");
            }
            component.url = url.into();
            component.sha256 = asset["digest"]
                .as_str()
                .and_then(|v| v.strip_prefix("sha256:"))
                .ok_or("Missing upstream SHA-256")?
                .into();
            component.bytes = asset["size"].as_u64().ok_or("Missing upstream size")?;
            versions.push(tag.to_owned());
        }
        if let Some(url) = &component.discovery {
            let bytes = fetch(url).map_err(|_| "Upstream version discovery failed")?;
            let current: Version = serde_json::from_slice(&bytes)
                .map_err(|_| "Invalid upstream version descriptor")?;
            if current.version.is_empty() || current.version.len() > 80 {
                return Err("Invalid upstream version");
            }
            versions.push(current.version);
            component.sha256 = current.sha256;
            component.bytes = current.bytes;
        }
    }
    if !versions.is_empty() {
        recipe.version = versions.join("+");
    }
    recipe
        .validate()
        .map_err(|_| "Invalid upstream component")?;
    Ok((recipe, validators))
}

pub(super) fn association_metadata(source: &str, recipe: &Recipe) -> Value {
    json!({
        "appDepot": {
            "store": {
                "source": source, "recipeId": recipe.id, "revision": recipe.revision,
                "version": recipe.version, "components": recipe.components,
                "rights": recipe.rights, "upstream": recipe.upstream
            },
            "entryPoints": [{
                "id":"store-launch", "target":{"type":"TreeFile","path":recipe.launch.path},
                "platforms":["linux/x86_64"],
                "launch":{"method":"NativePortableV1","arguments":recipe.launch.args,"workingDirectory":recipe.launch.cwd}
            }]
        }
    })
}

async fn run(s: &Store, r: sqlx::sqlite::SqliteRow) -> Result<(), String> {
    const DB: &str = "Store database operation failed";
    let id: String = r.get("id");
    let pool = &s.auth.pool;
    let actor = Principal {
        id: r.get("owner"),
        username: r.get("username"),
        server_admin: r.get("server_admin"),
        publish_permission: false,
    };
    if !actor.server_admin {
        return Err("Store owner requires Server Admin permission".into());
    }
    let recipe: Recipe =
        serde_json::from_str(&r.get::<String, _>("recipe")).map_err(|_| "Invalid stored recipe")?;
    let previous = r
        .get::<Option<String>, _>("validators")
        .map(|v| serde_json::from_str(&v))
        .transpose()
        .map_err(|_| DB)?;
    let scratch = s.scratch.clone();
    cached_recipe(s, &id)
        .await
        .map_err(|_| "Catalog entry unavailable or requires rights review")?;
    let (recipe, validators) = if let Some(pending) = r.get::<Option<String>, _>("pending") {
        serde_json::from_str(&pending).map_err(|_| DB)?
    } else {
        tokio::task::spawn_blocking(move || discover(recipe, previous, scratch.path()))
            .await
            .map_err(|_| "Discovery worker failed")??
    };
    // A catalog description, source URL or tag can change while the pinned
    // bytes and preparation rules still describe the current installation.
    let components: Vec<_> = recipe
        .components
        .iter()
        .map(|c| (&c.name, &c.format, c.bytes, &c.sha256))
        .collect();
    let inputs = (
        &recipe.platform,
        components,
        &recipe.prepare,
        &recipe.required,
        &recipe.launch,
    );
    let fingerprint = digest(&serde_json::to_vec(&inputs).map_err(|_| "Invalid build inputs")?);
    let app: Option<String> = r.get("app");
    let status = if r.get::<Option<String>, _>("release").is_some()
        && r.get::<Option<String>, _>("fingerprint").as_ref() == Some(&fingerprint)
    {
        "Current"
    } else if app.is_some() && !r.get::<bool, _>("automatic") && !r.get::<bool, _>("apply_once") {
        "Update available"
    } else {
        "Preparing"
    };
    sqlx::query(
        "UPDATE store_observations SET detected=?,status=?,pending=? WHERE source=? AND id=?",
    )
    .bind(&recipe.version)
    .bind(status)
    .bind((status == "Preparing").then(|| json!([recipe, validators]).to_string()))
    .bind(&s.base)
    .bind(&id)
    .execute(pool)
    .await
    .map_err(|_| DB)?;
    if status == "Current" {
        sqlx::query(
            "UPDATE store_observations SET apply_once=0,validators=? WHERE source=? AND id=?",
        )
        .bind(validators.to_string())
        .bind(&s.base)
        .bind(&id)
        .execute(pool)
        .await
        .map_err(|_| DB)?;
    }
    if status != "Preparing" {
        return Ok(());
    }
    let recipe_copy = recipe.clone();
    let scratch = s.scratch.clone();
    let (_work, output) =
        tokio::task::spawn_blocking(move || prepare(&recipe_copy, scratch.path()))
            .await
            .map_err(|_| "Preparation worker failed")?
            .map_err(|error| error.to_string())?;
    let root = ImportRoot::private_staging(output).map_err(|_| "Prepared tree unavailable")?;
    let key = digest(
        &serde_json::to_vec(&json!([
            s.base,
            id,
            r.get::<Option<String>, _>("release"),
            recipe.revision,
            fingerprint
        ]))
        .unwrap(),
    );
    let target = match app {
        Some(app_id) => Target::NewRelease {
            app_id,
            create_release: true,
        },
        None => Target::NewApp {
            new_app_name: recipe.title.clone(),
        },
    };
    let metadata = association_metadata(&s.base, &recipe);
    sqlx::query("UPDATE store_observations SET status='Importing' WHERE source=? AND id=?")
        .bind(&s.base)
        .bind(&id)
        .execute(pool)
        .await
        .map_err(|_| DB)?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(16);
    let import = async {
        let result = import_store_tree(&s.import, &actor, root, target, &key, metadata, &tx).await;
        drop(tx);
        result
    };
    let monitor = async {
        let mut last = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(1))
            .unwrap();
        while let Some(event) = rx.recv().await {
            let Some(data) = event.strip_prefix("data: ") else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
                continue;
            };
            if last.elapsed() < std::time::Duration::from_millis(500)
                || value["bytesTotal"].is_null()
            {
                continue;
            }
            last = std::time::Instant::now();
            let _=sqlx::query("UPDATE store_observations SET files_done=?,files_total=?,bytes_done=?,bytes_total=? WHERE source=? AND id=?")
                .bind(value["filesProcessed"].as_i64().unwrap_or(0)).bind(value["filesTotal"].as_i64().unwrap_or(0))
                .bind(value["bytesProcessed"].as_i64().unwrap_or(0)).bind(value["bytesTotal"].as_i64().unwrap_or(0))
                .bind(&s.base).bind(&id).execute(pool).await;
        }
    };
    let (result, ()) = tokio::join!(import, monitor);
    let result = result.map_err(|_| "Tree import failed")?;
    let release = result.release_id.ok_or("Missing release")?;
    let (record, etag) = catalog::get_release(pool, Some(&actor), &release)
        .await
        .map_err(|_| "Release unavailable")?;
    if record.state == "Draft" {
        cached_recipe(s, &id)
            .await
            .map_err(|_| "Catalog entry unavailable or requires rights review")?;
        installation::resolve(
            pool,
            Some(&actor),
            &release,
            &installation::Request {
                platform: "linux/x86_64".into(),
                selections: Default::default(),
                revision_id: None,
                manifest: None,
            },
        )
        .await
        .map_err(|_| "Installation declaration validation failed")?;
        let (_, etag) = catalog::patch_release(
            pool,
            &actor,
            &release,
            Some(&catalog::etag("release", &release, etag)),
            Some(Some(&recipe.version)),
        )
        .await
        .map_err(|_| "Release version update failed")?;
        catalog::publish_release(
            pool,
            &actor,
            &key,
            &release,
            Some(&catalog::etag("release", &release, etag)),
        )
        .await
        .map_err(|_| "Release publication failed")?;
    }
    sqlx::query("UPDATE store_observations SET status='Current',apply_once=0,pending=NULL,validators=?,fingerprint=?,app=?,release=? WHERE source=? AND id=?").bind(validators.to_string()).bind(fingerprint).bind(result.app_id).bind(release).bind(&s.base).bind(id).execute(pool).await.map_err(|_|DB)?;
    Ok(())
}

pub(crate) async fn worker(s: Store, boundary: Arc<tokio::sync::RwLock<()>>) {
    let pool = &s.auth.pool;
    loop {
        let _guard = boundary.read().await;
        // Claim and read the same row atomically, including its pinned pending
        // inputs. A settings change or catalog adoption cannot race a stale SELECT.
        let next=sqlx::query("UPDATE store_observations SET status='Checking',files_done=0,files_total=0,bytes_done=0,bytes_total=0,last_check=unixepoch(),error=NULL
            WHERE (source,id)=(SELECT source,id FROM store_observations WHERE source=? AND
                (status IN ('Queued','Checking','Preparing','Importing') OR (enabled AND last_check+3600*hours<=unixepoch()))
                ORDER BY coalesce(last_check,0),id LIMIT 1)
            RETURNING *, (SELECT username FROM users WHERE users.id=store_observations.owner) username,
                         (SELECT server_admin FROM users WHERE users.id=store_observations.owner) server_admin")
            .bind(&s.base).fetch_optional(pool).await;
        if let Ok(Some(row)) = next {
            let id: String = row.get("id");
            if let Err(error) = run(&s, row).await {
                let _=sqlx::query("UPDATE store_observations SET status='Failed',apply_once=0,pending=CASE WHEN status='Importing' THEN pending END,error=? WHERE source=? AND id=?")
                    .bind(error).bind(&s.base).bind(id).execute(pool).await;
            }
        }
        drop(_guard);
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

pub fn router(import: ImportState, work: &std::path::Path) -> std::io::Result<(Router, Store)> {
    let scratch = Arc::new(
        tempfile::Builder::new()
            .prefix("store-worker-")
            .tempdir_in(work)?,
    );
    let state = Store {
        auth: import.auth.clone(),
        import,
        scratch,
        refresh: Arc::new(Semaphore::new(1)),
        base: std::env::var("APP_DEPOT_STORE_SOURCE").unwrap_or_else(|_| OFFICIAL.into()),
    };
    let router = Router::new()
        .route("/admin/store", get(list))
        .route("/admin/store/refresh", post(refresh))
        .route("/admin/store/{id}/add", post(add))
        .route("/admin/store/observations", get(observed))
        .route(
            "/admin/store/{id}/observation",
            axum::routing::put(settings),
        )
        .route("/admin/store/{id}/check", post(check))
        .route("/admin/store/{id}/adopt", post(adopt))
        .route("/admin/store/{id}/update", post(apply_update))
        .route_layer(axum::middleware::from_fn(
            |request: axum::extract::Request, next: axum::middleware::Next| async move {
                let mut response = next.run(request).await;
                response.headers_mut().insert(
                    axum::http::header::CACHE_CONTROL,
                    axum::http::HeaderValue::from_static("no-store"),
                );
                response
            },
        ))
        .with_state(state.clone());
    Ok((router, state))
}
