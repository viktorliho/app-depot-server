use crate::artwork::ArtworkState;
use crate::auth::{authorize, ApiError};
use app_depot_infrastructure::{
    artwork,
    catalog::{self, CatalogError},
    declarations,
};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Write;
use std::{
    io::Read,
    process::{Command, Stdio},
    sync::Arc,
};
use tokio::sync::Semaphore;

#[derive(Clone)]
pub struct Providers {
    storage: ArtworkState,
    requests: Arc<Semaphore>,
    cooldowns: Arc<std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>>,
    configuration: Arc<std::sync::Mutex<()>>,
}

const STEAM: &str = "https://store.steampowered.com";
const IMAGES: &str = "https://shared.akamai.steamstatic.com/store_item_assets/steam/apps";
const ROLES: &[(&str, &str)] = &[
    ("steam.grid.vertical", "library_600x900.jpg"),
    ("steam.grid.horizontal", "header.jpg"),
    ("steam.hero", "library_hero.jpg"),
    ("logo", "logo.png"),
];

// Store descriptions may contain publisher HTML. Persist plain text for every client.
fn description(raw: &str, max: usize) -> String {
    let mut plain = String::new();
    let mut tag = false;
    for c in raw.chars() {
        match c {
            '<' => {
                tag = true;
                plain.push(' ');
            }
            '>' => tag = false,
            _ if !tag => plain.push(c),
            _ => {}
        }
    }
    let plain = plain
        .replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    plain
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

fn unavailable() -> ApiError {
    ApiError::problem(
        StatusCode::SERVICE_UNAVAILABLE,
        "provider_unavailable",
        "Metadata provider unavailable",
    )
}

fn download(
    url: &str,
    limit: usize,
    key: Option<&str>,
    limited: &mut bool,
) -> Result<Vec<u8>, ApiError> {
    // Only provider adapters construct URLs. Never follow a provider-supplied redirect.
    let mut cmd = Command::new("curl");
    cmd.arg("--disable");
    if key.is_some() {
        cmd.args(["--header", "@-"]);
    }
    let mut child = cmd
        .args([
            "--proto",
            "=https",
            "--silent",
            "--fail",
            "--connect-timeout",
            "4",
            "--max-time",
            "12",
            "--write-out",
            "%{stderr}%{response_code}",
            "--user-agent",
            "AppDepotServer (+https://github.com/viktorliho/app-depot-server)",
            "--max-filesize",
            &limit.to_string(),
            "--",
            url,
        ])
        .stdin(if key.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| unavailable())?;
    if let Some(token) = key {
        if writeln!(child.stdin.take().unwrap(), "Authorization: Bearer {token}").is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unavailable());
        }
    }
    let mut bytes = Vec::new();
    let result = child
        .stdout
        .take()
        .unwrap()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes);
    if bytes.len() > limit || result.is_err() {
        let _ = child.kill();
    }
    let mut status = String::new();
    let _ = child
        .stderr
        .take()
        .unwrap()
        .take(3)
        .read_to_string(&mut status);
    *limited = status == "429";
    if !child.wait().map_err(|_| unavailable())?.success() || bytes.len() > limit || result.is_err()
    {
        return Err(unavailable());
    }
    Ok(bytes)
}

impl Providers {
    pub fn new(storage: ArtworkState) -> Self {
        Self {
            storage,
            cooldowns: Default::default(),
            requests: Arc::new(Semaphore::new(6)),
            configuration: Default::default(),
        }
    }
    fn config(&self) -> Result<Config, ApiError> {
        match std::fs::read(self.storage.root.join("ops/metadata-providers.json")) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| ApiError::server()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(_) => Err(ApiError::server()),
        }
    }
    async fn sg_images(&self, id: u32, role: &str) -> Result<Vec<(u64, String)>, ApiError> {
        let (category, dimensions) = match role {
            "icon" => ("icons", ""),
            "logo" => ("logos", ""),
            "steam.hero" => ("heroes", ""),
            "steam.grid.vertical" => ("grids", "&dimensions=600x900"),
            "steam.grid.horizontal" => ("grids", "&dimensions=920x430,460x215"),
            _ => return Err(CatalogError::Invalid.into()),
        };
        let bytes=self.bytes(format!("https://www.steamgriddb.com/api/v2/{category}/steam/{id}?nsfw=false&types=static{dimensions}"),512*1024).await?;
        let doc: Value = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
        Ok(doc["data"]
            .as_array()
            .into_iter()
            .flatten()
            .take(24)
            .filter_map(|v| {
                let url = v["url"].as_str()?;
                let path = url.strip_prefix("https://cdn2.steamgriddb.com/")?;
                if path.is_empty()
                    || !path.split('/').all(|part| {
                        !part.is_empty()
                            && part != "."
                            && part != ".."
                            && part
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                    })
                {
                    return None;
                }
                Some((v["id"].as_u64()?, url.into()))
            })
            .collect())
    }
    async fn bytes(&self, url: String, limit: usize) -> Result<Vec<u8>, ApiError> {
        let db = &self.storage.auth.pool;
        let cfg = self.config()?;
        let host = url
            .parse::<axum::http::Uri>()
            .ok()
            .and_then(|u| u.host().map(str::to_owned))
            .ok_or_else(unavailable)?;
        let key = match host.as_str() {
            "www.steamgriddb.com" if cfg.steam_grid_db => {
                Some(cfg.key.filter(|k| !k.is_empty()).ok_or_else(unavailable)?)
            }
            "cdn2.steamgriddb.com" if cfg.steam_grid_db && cfg.key.is_some() => None,
            "shared.akamai.steamstatic.com" | "store.steampowered.com" if cfg.steam => None,
            _ => return Err(unavailable()),
        };
        let prior: Option<(Vec<u8>, bool)> =
            sqlx::query_as("SELECT body,expires>unixepoch() FROM provider_cache WHERE resource=?")
                .bind(&url)
                .fetch_optional(db)
                .await
                .map_err(CatalogError::Sql)?;
        if let Some((bytes, true)) = &prior {
            return if bytes.is_empty() {
                Err(unavailable())
            } else {
                Ok(bytes.clone())
            };
        }
        let limited = self
            .cooldowns
            .lock()
            .map_err(|_| ApiError::server())?
            .get(&host)
            .is_some_and(|t| *t > std::time::Instant::now());
        if limited {
            return prior
                .map(|p| p.0)
                .filter(|b| !b.is_empty())
                .ok_or_else(unavailable);
        }
        let permit = self
            .requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| unavailable())?;
        let source = url.clone();
        let (result, limited) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut limited = false;
            let result = download(&source, limit, key.as_deref(), &mut limited);
            (result, limited)
        })
        .await
        .map_err(|_| unavailable())?;
        if limited {
            self.cooldowns
                .lock()
                .map_err(|_| ApiError::server())?
                .insert(
                    host,
                    std::time::Instant::now() + std::time::Duration::from_secs(300),
                );
        }
        let (body, ttl) = match result {
            Ok(bytes) if !bytes.is_empty() => (bytes, 86400),
            _ => (prior.map(|p| p.0).unwrap_or_default(), 300),
        };
        let mut tx = db
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(CatalogError::Sql)?;
        sqlx::query("INSERT INTO provider_cache(resource,body,expires) VALUES(?,?,unixepoch()+?) ON CONFLICT(resource) DO UPDATE SET body=excluded.body,expires=excluded.expires,saved=unixepoch()").bind(url).bind(&body).bind(ttl).execute(&mut *tx).await.map_err(CatalogError::Sql)?;
        sqlx::query("DELETE FROM provider_cache WHERE resource IN (SELECT resource FROM (SELECT resource,sum(length(body)) OVER (ORDER BY saved DESC,rowid DESC) bytes,row_number() OVER (ORDER BY saved DESC,rowid DESC) n FROM provider_cache) WHERE bytes>67108864 OR n>512)").execute(&mut *tx).await.map_err(CatalogError::Sql)?;
        tx.commit().await.map_err(CatalogError::Sql)?;
        if body.is_empty() {
            Err(unavailable())
        } else {
            Ok(body)
        }
    }
    pub async fn fill(
        &self,
        actor: &app_depot_application::Principal,
        app: &str,
        id: Option<u32>,
        summary: &str,
        publisher: &str,
    ) {
        let mut fields = match id {
            Some(id) => self.steam_details(id).await.unwrap_or_else(|_| json!({})),
            None => json!({}),
        };
        let mut origins = json!({});
        if let Some(object) = fields.as_object() {
            for name in object.keys() {
                origins[name] = "steam".into();
            }
        }
        for (key, value) in [("summary", summary), ("publisher", publisher)] {
            if fields[key].is_null() {
                fields[key] = json!(value);
                origins[key] = "store".into();
            }
        }
        let db = &self.storage.auth.pool;
        if let Ok((mut doc, rev)) = declarations::get(db, Some(actor), "App", app).await {
            if let Some(block) = doc.data.as_object_mut().and_then(|o| {
                o.entry("appDepot")
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
            }) {
                let mut applied = json!({});
                for (key, value) in fields.as_object().into_iter().flatten() {
                    if !block.contains_key(key) {
                        block.insert(key.clone(), value.clone());
                        applied[key] = origins[key].clone();
                    }
                }
                if let Some(id) = id {
                    if let Some(ids) = block
                        .entry("externalIds")
                        .or_insert_with(|| json!({}))
                        .as_object_mut()
                    {
                        ids.entry("steam").or_insert(json!(id));
                    }
                }
                block.entry("metadataSources").or_insert(applied);
                let _ = declarations::put(
                    db,
                    actor,
                    "App",
                    app,
                    Some(&catalog::etag("declarations", app, rev)),
                    &doc.data,
                )
                .await;
            }
        }
        if let Some(id) = id {
            for role in ROLES.iter().map(|(r, _)| *r).chain(["icon"]) {
                if let Ok((_, bytes, source)) = self.image(id, role).await {
                    let _ = artwork::put_with_source(
                        db,
                        &self.storage.root,
                        actor,
                        &uuid::Uuid::new_v4().to_string(),
                        app,
                        role,
                        Some("*"),
                        &bytes,
                        Some(&source),
                    )
                    .await;
                }
            }
        }
    }
    async fn details(&self, id: u32) -> Result<Value, ApiError> {
        Ok(self.details_with_source(id).await?.0)
    }
    async fn details_with_source(&self, id: u32) -> Result<(Value, &'static str), ApiError> {
        match self.steam_details(id).await {
            Ok(data) => Ok((data, "steam")),
            Err(error) => {
                let recipe:Option<(String,String,String)> = sqlx::query_as("SELECT json_extract(r.value,'$.title'),json_extract(r.value,'$.summary'),json_extract(r.value,'$.publisher') FROM store_catalogs,json_each(recipes) r WHERE json_extract(r.value,'$.steamId')=? ORDER BY source LIMIT 1")
                    .bind(id).fetch_optional(&self.storage.auth.pool).await.map_err(CatalogError::Sql)?;
                recipe
                    .map(|(title, summary, publisher)| {
                        (
                            json!({"canonicalTitle":title,"summary":summary,"publisher":publisher}),
                            "store",
                        )
                    })
                    .ok_or(error)
            }
        }
    }
    async fn steam_details(&self, id: u32) -> Result<Value, ApiError> {
        let url = format!("{STEAM}/api/appdetails?appids={id}&l=english&cc=us");
        let body = self.bytes(url, 512 * 1024).await?;
        let raw: Value = serde_json::from_slice(&body).map_err(|_| unavailable())?;
        let data = &raw[id.to_string()]["data"];
        if data["steam_appid"].as_u64() != Some(u64::from(id)) {
            return Err(unavailable());
        }
        let mut fields = json!({});
        for (from, to) in [
            ("short_description", "summary"),
            ("detailed_description", "description"),
            ("website", "homepageUrl"),
            ("name", "canonicalTitle"),
        ] {
            if let Some(v) = data[from].as_str().filter(|v| !v.is_empty()) {
                if to == "homepageUrl" {
                    if let Ok(url) = v.parse::<axum::http::Uri>() {
                        if matches!(url.scheme_str(), Some("http" | "https"))
                            && url.authority().is_some_and(|a| !a.as_str().contains('@'))
                            && v.len() <= 2048
                        {
                            fields[to] = json!(v);
                        }
                    }
                } else {
                    fields[to] = json!(description(
                        v,
                        match to {
                            "description" => 10000,
                            "summary" => 280,
                            _ => 160,
                        }
                    ));
                }
            }
        }
        for (from, to) in [("developers", "developer"), ("publishers", "publisher")] {
            if let Some(values) = data[from].as_array() {
                fields[to] = json!(values
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
                    .chars()
                    .take(160)
                    .collect::<String>());
            }
        }
        for (from, to) in [("genres", "tags"), ("categories", "categories")] {
            if let Some(values) = data[from].as_array() {
                fields[to] = json!(values
                    .iter()
                    .filter_map(|v| v["description"].as_str())
                    .collect::<Vec<_>>());
            }
        }
        if let Some(date) = data["release_date"]["date"].as_str() {
            fields["releaseDate"] = json!(date);
        }
        Ok(fields)
    }
    async fn chosen_image(
        &self,
        id: u32,
        role: &str,
        asset: Option<u64>,
    ) -> Result<(&'static str, Vec<u8>, String), ApiError> {
        if asset.is_none() {
            if let Some((_, file)) = ROLES.iter().find(|(r, _)| *r == role) {
                if let Ok(bytes) = self
                    .bytes(format!("{IMAGES}/{id}/{file}"), artwork::MAX_IMAGE_BYTES)
                    .await
                {
                    if let Ok(mime) = artwork::validate_provider_image(role, &bytes) {
                        return Ok((mime, bytes, format!("{IMAGES}/{id}/{file}")));
                    }
                }
            }
        }
        let candidates = self.sg_images(id, role).await?;
        let (_, url) = candidates
            .into_iter()
            .find(|(n, _)| asset.is_none_or(|a| *n == a))
            .ok_or_else(unavailable)?;
        let bytes = self.bytes(url.clone(), artwork::MAX_IMAGE_BYTES).await?;
        let mime = artwork::validate_provider_image(role, &bytes)?;
        Ok((mime, bytes, url))
    }
    async fn image(
        &self,
        id: u32,
        role: &str,
    ) -> Result<(&'static str, Vec<u8>, String), ApiError> {
        self.chosen_image(id, role, None).await
    }
}

#[derive(Deserialize)]
struct Search {
    q: String,
}
async fn search(
    State(s): State<Providers>,
    h: HeaderMap,
    Query(q): Query<Search>,
) -> Result<Json<Value>, ApiError> {
    authorize(&s.storage.auth, &h, false).await?;
    let q = q.q.trim();
    if q.is_empty() || q.len() > 160 {
        return Err(CatalogError::Invalid.into());
    }
    let catalog: Vec<(u32, String)> = sqlx::query_as("SELECT DISTINCT json_extract(r.value,'$.steamId'), json_extract(r.value,'$.title') FROM store_catalogs, json_each(recipes) r WHERE json_type(r.value,'$.steamId')='integer' AND (instr(lower(json_extract(r.value,'$.title')),lower(?)) > 0 OR CAST(json_extract(r.value,'$.steamId') AS TEXT)=? OR EXISTS(SELECT 1 FROM json_each(r.value,'$.aliases') a WHERE instr(lower(a.value),lower(?))>0)) ORDER BY 2 LIMIT 24")
        .bind(q).bind(q).bind(q).fetch_all(&s.storage.auth.pool).await.map_err(CatalogError::Sql)?;
    let mut items: Vec<Value> = catalog
        .into_iter()
        .map(|(id, name)| json!({"id":id,"name":name}))
        .collect();
    if let Ok(id) = q.parse::<u32>() {
        if items.is_empty() {
            items.push(json!({"id":id,"name":s.details(id).await?["canonicalTitle"]}));
        }
        return Ok(Json(json!(items)));
    }
    let encoded = q.bytes().map(|b| format!("%{b:02X}")).collect::<String>();
    let remote = s
        .bytes(
            format!("{STEAM}/api/storesearch/?term={encoded}&l=english&cc=us"),
            256 * 1024,
        )
        .await;
    match remote {
        Ok(bytes) => {
            let doc: Value = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
            for item in doc["items"].as_array().into_iter().flatten().take(24) {
                if let (Some(id), Some(name)) = (item["id"].as_u64(), item["name"].as_str()) {
                    if !items.iter().any(|v| v["id"].as_u64() == Some(id)) {
                        items.push(json!({"id":id,"name":name}));
                    }
                }
            }
        }
        Err(err) if items.is_empty() => return Err(err),
        Err(_) => {}
    }
    Ok(Json(json!(items)))
}
async fn detail(
    State(s): State<Providers>,
    h: HeaderMap,
    Path(id): Path<u32>,
) -> Result<Json<Value>, ApiError> {
    authorize(&s.storage.auth, &h, false).await?;
    Ok(Json(s.details(id).await?))
}
async fn image(
    State(s): State<Providers>,
    h: HeaderMap,
    Path((id, role)): Path<(u32, String)>,
    Query(choice): Query<ImageChoice>,
) -> Result<impl IntoResponse, ApiError> {
    authorize(&s.storage.auth, &h, false).await?;
    let (mime, bytes, _) = s.chosen_image(id, &role, choice.asset).await?;
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "private, max-age=3600"),
        ],
        bytes,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    id: u32,
    fields: Vec<String>,
    artwork: Vec<String>,
    #[serde(default)]
    assets: std::collections::HashMap<String, u64>,
}
async fn apply(
    State(s): State<Providers>,
    h: HeaderMap,
    Path(app): Path<String>,
    Json(input): Json<Selection>,
) -> Result<StatusCode, ApiError> {
    let actor = authorize(&s.storage.auth, &h, true).await?;
    let db = &s.storage.auth.pool;
    let (record, _) = catalog::get_app(db, Some(&actor), &app).await?;
    if !actor.may_manage(&record.owner_id) {
        return Err(CatalogError::NotFound.into());
    }
    if input.fields.len() > 16
        || input.artwork.len() > 5
        || input.assets.len() > 5
        || input
            .artwork
            .iter()
            .any(|role| role != "icon" && !ROLES.iter().any(|(r, _)| r == role))
    {
        return Err(CatalogError::Invalid.into());
    }
    let (fields, source) = s.details_with_source(input.id).await?;
    let (mut doc, revision) = declarations::get(db, Some(&actor), "App", &app).await?;
    let object = doc.data.as_object_mut().ok_or(CatalogError::Conflict)?;
    let block = object
        .entry("appDepot")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(CatalogError::Conflict)?;
    for field in input.fields {
        let v = fields.get(&field).ok_or(CatalogError::Invalid)?;
        block
            .entry("metadataSources")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or(CatalogError::Conflict)?
            .insert(field.clone(), json!(source));
        block.insert(field, v.clone());
    }
    block
        .entry("externalIds")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or(CatalogError::Conflict)?
        .insert("steam".into(), json!(input.id));
    declarations::put(
        db,
        &actor,
        "App",
        &app,
        Some(&catalog::etag("declarations", &app, revision)),
        &doc.data,
    )
    .await?;
    for role in input.artwork {
        let validator = match artwork::get(db, Some(&actor), &app, &role).await {
            Ok(item) => item.etag,
            Err(CatalogError::NotFound) => "*".into(),
            Err(e) => return Err(e.into()),
        };
        let (_, bytes, source) = s
            .chosen_image(input.id, &role, input.assets.get(&role).copied())
            .await?;
        artwork::put_with_source(
            db,
            &s.storage.root,
            &actor,
            &uuid::Uuid::new_v4().to_string(),
            &app,
            &role,
            Some(&validator),
            &bytes,
            Some(&source),
        )
        .await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
#[derive(Deserialize)]
struct ImageChoice {
    asset: Option<u64>,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    steam: bool,
    steam_grid_db: bool,
    key: Option<String>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            steam: true,
            steam_grid_db: false,
            key: None,
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConfigPatch {
    steam: Option<bool>,
    steam_grid_db: Option<bool>,
    key: Option<String>,
}
async fn settings_admin(s: &Providers, h: &HeaderMap, write: bool) -> Result<(), ApiError> {
    if !authorize(&s.storage.auth, h, write).await?.server_admin {
        return Err(CatalogError::Forbidden.into());
    }
    Ok(())
}
async fn settings(State(s): State<Providers>, h: HeaderMap) -> Result<Json<Value>, ApiError> {
    settings_admin(&s, &h, false).await?;
    let c = s.config()?;
    Ok(Json(
        json!({"steam":c.steam,"steamGridDb":c.steam_grid_db,"hasKey":c.key.is_some()}),
    ))
}
async fn configure(
    State(s): State<Providers>,
    h: HeaderMap,
    Json(p): Json<ConfigPatch>,
) -> Result<StatusCode, ApiError> {
    settings_admin(&s, &h, true).await?;
    let _guard = s.configuration.lock().map_err(|_| ApiError::server())?;
    let mut c = s.config()?;
    if let Some(v) = p.steam {
        c.steam = v;
    }
    if let Some(v) = p.steam_grid_db {
        c.steam_grid_db = v;
    }
    if let Some(v) = p.key {
        if v.len() > 160
            || !v
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err(CatalogError::Invalid.into());
        }
        c.key = (!v.is_empty()).then_some(v);
    }
    let dir = s.storage.root.join("ops");
    let mut f = tempfile::NamedTempFile::new_in(&dir).map_err(|_| ApiError::server())?;
    serde_json::to_writer(&mut f, &c).map_err(|_| ApiError::server())?;
    f.as_file().sync_all().map_err(|_| ApiError::server())?;
    f.persist(dir.join("metadata-providers.json"))
        .map_err(|_| ApiError::server())?;
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|_| ApiError::server())?;
    Ok(StatusCode::NO_CONTENT)
}
async fn test_connection(State(s): State<Providers>, h: HeaderMap) -> Result<StatusCode, ApiError> {
    settings_admin(&s, &h, true).await?;
    let cfg = s.config()?;
    if cfg.steam {
        s.steam_details(13250).await?;
    }
    if cfg.steam_grid_db {
        s.sg_images(13250, "steam.grid.vertical").await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
async fn images(
    State(s): State<Providers>,
    h: HeaderMap,
    Path((id, role)): Path<(u32, String)>,
) -> Result<Json<Value>, ApiError> {
    authorize(&s.storage.auth, &h, false).await?;
    Ok(Json(json!(s
        .sg_images(id, &role)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(id, _)| id)
        .collect::<Vec<_>>())))
}
pub fn router(s: Providers) -> Router {
    Router::new()
        .route("/admin/metadata-providers", get(settings).put(configure))
        .route("/admin/metadata-providers/test", post(test_connection))
        .route("/metadata/steam/{id}/candidates/{role}", get(images))
        .route("/metadata/steam/search", get(search))
        .route("/metadata/steam/{id}", get(detail))
        .route("/metadata/steam/{id}/artwork/{role}", get(image))
        .route("/apps/{app}/metadata/apply", post(apply))
        .with_state(s)
}
