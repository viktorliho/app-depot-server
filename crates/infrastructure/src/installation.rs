use crate::{
    catalog::CatalogError,
    declarations::{document_in_tx, platform_list},
    recipes::{artifact_source, lock_in_tx, windows_component, File, Layout, Source},
};
use app_depot_application::{may_read_catalog, Principal};
use app_depot_domain::{
    installation::{self, InstallationError as Error, Manifest},
    recipe::{self, RecipeError},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use std::collections::{BTreeMap, BTreeSet};

pub fn parse(value: &Value) -> Result<Manifest, Error> {
    let version = value
        .get("version")
        .and_then(Value::as_u64)
        .ok_or(Error::Invalid)?;
    if version != 1 {
        return Err(Error::Unsupported);
    }
    let manifest: Manifest = serde_json::from_value(value.clone()).map_err(|_| Error::Invalid)?;
    manifest.validate()?;
    Ok(manifest)
}

async fn member(
    tx: &mut Transaction<'_, Sqlite>,
    release: &str,
    association: &str,
) -> Result<sqlx::sqlite::SqliteRow, CatalogError> {
    Ok(sqlx::query("SELECT a.id, a.kind, a.digest, a.size FROM associations x JOIN artifacts a ON a.id=x.artifact_id WHERE x.id=? AND x.release_id=?")
        .bind(association).bind(release).fetch_optional(&mut **tx).await?.ok_or(Error::Source)?)
}

async fn entry(
    tx: &mut Transaction<'_, Sqlite>,
    release: &str,
    association: &str,
    path: &str,
    file_only: bool,
) -> Result<(String, Option<String>), CatalogError> {
    let member = member(tx, release, association).await?;
    let id: String = member.try_get("id")?;
    if member.try_get::<&str, _>("kind")? == "File" {
        if !path.is_empty() {
            return Err(Error::Source.into());
        }
        return Ok((id, None));
    }
    if path.is_empty() {
        return if file_only {
            Err(Error::Source.into())
        } else {
            Ok((id, None))
        };
    }
    let (kind, file_id): (String, Option<String>) = sqlx::query_as(
        "SELECT entry_type,file_id FROM artifact_entries WHERE artifact_id=? AND path=?",
    )
    .bind(&id)
    .bind(path)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::Source)?;
    if file_only && kind != "File" {
        return Err(Error::Source.into());
    }
    Ok((id, file_id))
}

struct Native {
    platforms: Vec<String>,
    path: String,
    working_directory: Option<String>,
    arguments: Vec<String>,
}

async fn native(
    tx: &mut Transaction<'_, Sqlite>,
    release: &str,
    launch: &installation::Launch,
) -> Result<Native, CatalogError> {
    member(tx, release, &launch.association_id).await?;
    let raw: String = sqlx::query_scalar("SELECT data_json FROM declaration_revisions WHERE id=? AND scope='Association' AND resource_id=?")
        .bind(&launch.revision_id).bind(&launch.association_id).fetch_optional(&mut **tx).await?.ok_or(Error::Launch)?;
    let data: Value = serde_json::from_str(&raw)?;
    let points = data
        .pointer("/appDepot/entryPoints")
        .and_then(Value::as_array)
        .ok_or(Error::Launch)?;
    let mut matches = points
        .iter()
        .filter(|point| point.get("id").and_then(Value::as_str) == Some(&launch.entry_point_id));
    let point = matches.next().ok_or(Error::Launch)?;
    if matches.next().is_some()
        || point.pointer("/launch/method").and_then(Value::as_str) != Some("NativePortableV1")
    {
        return Err(Error::Launch.into());
    }
    let platforms: Vec<_> = point
        .get("platforms")
        .and_then(platform_list)
        .ok_or(Error::Launch)?
        .into_iter()
        .filter(|p| recipe::platform(p))
        .map(str::to_owned)
        .collect();
    let path = match point.pointer("/target/type").and_then(Value::as_str) {
        Some("ArtifactFile") => "",
        Some("TreeFile") => point
            .pointer("/target/path")
            .and_then(Value::as_str)
            .filter(|p| recipe::path(p))
            .ok_or(Error::Launch)?,
        _ => return Err(Error::Launch.into()),
    };
    let args: Vec<String> = point
        .pointer("/launch/arguments")
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .map_err(|_| Error::Launch)?
        .unwrap_or_default();
    let working_directory = point
        .pointer("/launch/workingDirectory")
        .map(|v| v.as_str().ok_or(Error::Launch))
        .transpose()?;
    let (_, file_id) = entry(tx, release, &launch.association_id, path, true).await?;
    if platforms.is_empty()
        || !installation::arguments(&args)
        || (file_id.is_some() == path.is_empty())
    {
        return Err(Error::Launch.into());
    }
    if let Some(directory) = working_directory.filter(|path| !path.is_empty()) {
        if !recipe::path(directory)
            || entry(tx, release, &launch.association_id, directory, false)
                .await?
                .1
                .is_some()
        {
            return Err(Error::Launch.into());
        }
    }
    Ok(Native {
        platforms,
        path: path.to_owned(),
        working_directory: working_directory.map(str::to_owned),
        arguments: args,
    })
}

pub(crate) async fn validate_references(
    tx: &mut Transaction<'_, Sqlite>,
    release: &str,
    manifest: &Manifest,
) -> Result<(), CatalogError> {
    let derived: bool = sqlx::query_scalar(
        "SELECT a.kind='derived' FROM releases r JOIN apps a ON a.id=r.app_id WHERE r.id=?",
    )
    .bind(release)
    .fetch_one(&mut **tx)
    .await?;
    if derived != manifest.recipe_payload {
        return Err(Error::Invalid.into());
    }
    for op in manifest.copies() {
        let member = member(tx, release, &op.association_id).await?;
        let single = member.try_get::<&str, _>("kind")? == "File";
        let (_, file_id) = entry(tx, release, &op.association_id, &op.source, false).await?;
        if op.destination.is_empty() && (single || file_id.is_some()) {
            return Err(Error::Source.into());
        }
    }
    for item in &manifest.prerequisites {
        entry(tx, release, &item.association_id, &item.path, true).await?;
    }
    if let Some(launch) = &manifest.launch {
        native(tx, release, launch).await?;
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub platform: String,
    #[serde(default)]
    pub selections: BTreeMap<String, Vec<String>>,
    pub revision_id: Option<String>,
    pub manifest: Option<Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Installer<'a> {
    id: &'a str,
    name: &'a str,
    file: File,
    path: &'a str,
    arguments: &'a [String],
    optional: bool,
    evidence: &'a str,
    success_exit_codes: &'a [i32],
}

fn layout_error(error: CatalogError) -> CatalogError {
    match error {
        CatalogError::Recipe(RecipeError::Limit) => Error::Limit.into(),
        CatalogError::Recipe(RecipeError::Collision) => Error::Collision.into(),
        CatalogError::Recipe(RecipeError::Source) => Error::Source.into(),
        other => other,
    }
}

pub async fn resolve(
    pool: &SqlitePool,
    actor: Option<&Principal>,
    release: &str,
    input: &Request,
) -> Result<Value, CatalogError> {
    let mut tx = pool.begin().await?;
    let (app, owner, public): (String, String, bool) = sqlx::query_as("SELECT a.id,a.owner_id,a.visibility='Public' AND r.state='Published' FROM releases r JOIN apps a ON a.id=r.app_id WHERE r.id=?")
        .bind(release).fetch_optional(&mut *tx).await?.ok_or(CatalogError::NotFound)?;
    let anonymous = actor.is_none()
        && sqlx::query_scalar::<_, bool>(
            "SELECT anonymous_read_enabled FROM server_settings WHERE id=1",
        )
        .fetch_one(&mut *tx)
        .await?;
    if !may_read_catalog(actor, &owner, public, anonymous) {
        return Err(CatalogError::NotFound);
    }
    let (revision_id, data, value) = match (&input.manifest, &input.revision_id) {
        (Some(value), None) if actor.is_some_and(|actor| actor.may_manage(&owner)) => (
            None,
            document_in_tx(&mut tx, "Release", release).await?.data,
            value.clone(),
        ),
        (Some(_), None) => return Err(CatalogError::NotFound),
        (None, revision) => {
            let (revision, data) = if let Some(revision) = revision {
                let raw: String = sqlx::query_scalar("SELECT data_json FROM declaration_revisions WHERE id=? AND scope='Release' AND resource_id=?")
                    .bind(revision).bind(release).fetch_optional(&mut *tx).await?.ok_or(CatalogError::NotFound)?;
                (revision.clone(), serde_json::from_str(&raw)?)
            } else {
                let doc = document_in_tx(&mut tx, "Release", release).await?;
                (doc.revision_id, doc.data)
            };
            let value = data
                .pointer("/appDepot/installation")
                .ok_or(Error::Missing)?
                .clone();
            (Some(revision), data, value)
        }
        _ => return Err(Error::Invalid.into()),
    };
    let manifest = parse(&value)?;
    validate_references(&mut tx, release, &manifest).await?;
    let platform = &input.platform;
    if !recipe::platform(platform)
        || data.pointer("/appDepot/platforms").is_some_and(|value| {
            platform_list(value)
                .is_none_or(|list| !list.is_empty() && !list.contains(&platform.as_str()))
        })
    {
        return Err(Error::Platform.into());
    }
    let selections = manifest.selections(&input.selections)?;
    if manifest.recipe_payload {
        let recipe = lock_in_tx(&mut tx, actor, release, platform).await?;
        let mut plan = json!({"version":1,"appId":app,"releaseId":release,"revisionId":revision_id,
            "platform":platform,"selections":selections,"recipe":recipe,"unresolved":manifest.unresolved});
        if let Some(localization) = manifest.localization {
            plan["localization"] = json!(localization);
        }
        return Ok(plan);
    }
    let mut layout = Layout::default();
    let mut sources = BTreeMap::new();
    let mut work = 0;
    let operations: Vec<_> = manifest
        .payload
        .iter()
        .chain(manifest.options.iter().flat_map(|group| {
            group
                .choices
                .iter()
                .filter(|choice| selections[&group.id].contains(&choice.id))
                .flat_map(|choice| &choice.payload)
        }))
        .filter(|op| op.platforms.is_empty() || op.platforms.contains(platform))
        .collect();
    for op in operations {
        if !sources.contains_key(&op.association_id) {
            let row = member(&mut tx, release, &op.association_id).await?;
            let artifact_id = row.try_get("id")?;
            sources.insert(
                op.association_id.clone(),
                artifact_source(&mut tx, &row, op.association_id.clone(), artifact_id)
                    .await
                    .map_err(layout_error)?,
            );
        }
        let copy = recipe::Copy {
            dependency: String::new(),
            association_id: None,
            association_label: None,
            source: op.source.clone(),
            destination: op.destination.clone(),
            executable: false,
            collision: op.collision,
        };
        match &sources[&op.association_id] {
            Source::File(file) => {
                work += 1;
                layout
                    .copy_file(file.clone(), &copy)
                    .map_err(|error| layout_error(error.into()))?;
            }
            Source::Tree(tree) => {
                work += tree.entries.len();
                if work > 200_000 {
                    return Err(Error::Limit.into());
                }
                layout
                    .copy(tree, &copy)
                    .map_err(|error| layout_error(error.into()))?;
            }
        }
    }
    let mut files = BTreeMap::new();
    let mut directories = BTreeSet::new();
    let mut folded = BTreeSet::new();
    for (path, entry) in layout.entries {
        if platform.starts_with("windows/") {
            if !path.split('/').all(windows_component) {
                return Err(Error::Source.into());
            }
            if !folded.insert(path.to_lowercase()) {
                return Err(Error::Collision.into());
            }
        }
        if let Some(file) = entry {
            files.insert(path, file);
        } else {
            directories.insert(path);
        }
    }
    if files.is_empty() {
        return Err(Error::Source.into());
    }
    let mut prerequisites = Vec::new();
    for item in &manifest.prerequisites {
        if !item.platforms.contains(platform)
            || item.when.as_ref().is_some_and(|condition| {
                !selections[&condition.option_id].contains(&condition.choice_id)
            })
        {
            continue;
        }
        let (artifact_id, file_id) =
            entry(&mut tx, release, &item.association_id, &item.path, true).await?;
        let (sha256, size, executable) = if let Some(file_id) = &file_id {
            sqlx::query_as("SELECT sha256,size,executable FROM artifact_entries WHERE artifact_id=? AND file_id=?")
                .bind(&artifact_id).bind(file_id).fetch_one(&mut *tx).await?
        } else {
            sqlx::query_as("SELECT digest,size,0 FROM artifacts WHERE id=?")
                .bind(&artifact_id)
                .fetch_one(&mut *tx)
                .await?
        };
        prerequisites.push(Installer {
            id: &item.id,
            name: &item.name,
            file: File {
                association_id: item.association_id.clone(),
                artifact_id,
                file_id,
                sha256,
                size,
                executable,
            },
            path: &item.path,
            arguments: &item.arguments,
            optional: item.optional,
            evidence: &item.evidence,
            success_exit_codes: &item.success_exit_codes,
        });
    }
    let launch = if let Some(launch) = &manifest.launch {
        let native = native(&mut tx, release, launch).await?;
        if !native.platforms.contains(platform) {
            None
        } else {
            let (_, file_id) =
                entry(&mut tx, release, &launch.association_id, &native.path, true).await?;
            let target = files
                .get_mut(&launch.destination)
                .filter(|file| {
                    file.association_id == launch.association_id && file.file_id == file_id
                })
                .ok_or(Error::Launch)?;
            if target.file_id.is_none() && !platform.starts_with("windows/") {
                target.executable = true;
            }
            let parent = || {
                launch
                    .destination
                    .rsplit_once('/')
                    .map_or("", |(dir, _)| dir)
            };
            let directory = launch
                .working_directory
                .as_deref()
                .or(native.working_directory.as_deref())
                .unwrap_or_else(parent);
            if (!platform.starts_with("windows/") && !target.executable)
                || (!directory.is_empty() && !directories.contains(directory))
            {
                return Err(Error::Launch.into());
            }
            Some(
                json!({"method":"NativePortableV1","associationId":launch.association_id,
                "entryPointId":launch.entry_point_id,"revisionId":launch.revision_id,
                "path":launch.destination,"workingDirectory":directory,"arguments":native.arguments}),
            )
        }
    } else {
        None
    };
    let mut plan = json!({"version":1,"appId":app,"releaseId":release,"revisionId":revision_id,"platform":platform,
        "selections":selections,"files":files,"directories":directories,"launch":launch,
        "prerequisites":prerequisites,"unresolved":manifest.unresolved});
    if let Some(localization) = manifest.localization {
        plan["localization"] = json!(localization);
    }
    Ok(plan)
}

/// Adapt discovery evidence to the existing generic manifest, without recovering
/// or inventing a vendor command line. The discovery registry determines order.
pub fn discovery_prerequisites(
    report: &Value,
    association: &str,
    disabled: Option<&Value>,
) -> Vec<Value> {
    let disabled = disabled.and_then(Value::as_array);
    report
        .get("packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, package)| {
            let path = package.get("path")?.as_str()?;
            if package["status"] != "ready"
                || disabled.is_some_and(|paths| paths.iter().any(|value| value == path))
            {
                return None;
            }
            Some(json!({
                "id":format!("discovered-{association}-{index}"),
                "name":package.get("name")?,
                "associationId":association,
                "path":path,
                "platforms":package.get("platforms")?,
                "arguments":[],
                "evidence":"Recognized package structure; App Depot fallback order; original arguments and order unknown"
            }))
        })
        .take(32)
        .collect()
}

pub fn discovery_notes(report: &Value) -> Vec<String> {
    let packages = report
        .get("packages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|package| {
            let status = package.get("status")?.as_str()?;
            if matches!(status, "ready" | "duplicate") {
                return None;
            }
            let path = package
                .get("path")?
                .as_str()?
                .chars()
                .take(200)
                .collect::<String>();
            Some(format!(
                "Discovered {path}: {}",
                if status == "alternative" {
                    "Conflicting package alternatives require publisher review"
                } else {
                    package.get("evidence")?.as_str()?
                }
            ))
        });
    report
        .get("unresolved")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .chain(packages)
        .take(96)
        .collect()
}

/// Seed a Release configuration during registration when the publisher has no existing configuration.
pub(crate) async fn seed_import(
    tx: &mut Transaction<'_, Sqlite>,
    release: &str,
    association: &str,
    metadata: &Value,
    mut data: Value,
) -> Result<(), CatalogError> {
    if metadata.pointer("/appDepot/store").is_some() {
        let point = &metadata["appDepot"]["entryPoints"][0];
        let revision = document_in_tx(tx, "Association", association).await?;
        let manifest = json!({"version":1,"payload":[{"associationId":association,"destination":"","platforms":point["platforms"]}],
            "launch":{"associationId":association,"revisionId":revision.revision_id,"entryPointId":point["id"],"destination":point["target"]["path"]}});
        parse(&manifest)?;
        data["appDepot"]["installation"] = manifest;
        crate::declarations::append(tx, "Release", release, &data).await?;
        return Ok(());
    }
    let Some(source) = metadata.pointer("/appDepot/gogOfflineInstaller") else {
        return Ok(());
    };
    let Some(document) = data.as_object_mut() else {
        return Ok(());
    };
    let Some(depot) = document
        .entry("appDepot")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    else {
        return Ok(());
    };
    if depot.contains_key("installation") {
        return Ok(());
    }
    let mut unresolved =
        vec!["Review the recovered payload layout and prerequisites before installing.".to_owned()];
    if let Some(actions) = source
        .pointer("/analysis/unknownActions")
        .and_then(Value::as_array)
    {
        unresolved.extend(actions.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    let discovery = source.pointer("/detected/installationDiscovery");
    if let Some(report) = discovery {
        unresolved.extend(discovery_notes(report));
    } else if let Some(paths) = source
        .pointer("/detected/redistributableRoots")
        .and_then(Value::as_array)
    {
        unresolved.extend(paths.iter().filter_map(Value::as_str).take(64).map(|path| {
            format!(
                "Review detected redistributable: {}",
                path.chars().take(200).collect::<String>()
            )
        }));
    }
    let launch = if let Some(path) = metadata
        .pointer("/appDepot/entryPoints/0/target/path")
        .and_then(Value::as_str)
    {
        let revision = document_in_tx(tx, "Association", association).await?;
        Some(
            json!({"associationId":association,"entryPointId":"gog-launch","revisionId":revision.revision_id,"destination":path}),
        )
    } else {
        None
    };
    let prerequisites = discovery
        .map(|report| {
            discovery_prerequisites(
                report,
                association,
                source.pointer("/user/disabledPrerequisites"),
            )
        })
        .unwrap_or_default();
    let localization = if let Some(tag) = source
        .pointer("/user/confirmedLocale")
        .and_then(Value::as_str)
    {
        json!({"mode":"fixed","language":tag})
    } else if let Some(languages) = source
        .pointer("/user/confirmedBundledLocales")
        .and_then(Value::as_array)
    {
        json!({"mode":"bundled","languages":languages})
    } else {
        json!({"mode":"unknown"})
    };
    let mut value = json!({"version":1,"payload":[{"associationId":association,"destination":"",
        "platforms":["windows/x86_64","windows/aarch64"]}],"unresolved":unresolved,"launch":launch});
    if !prerequisites.is_empty() {
        value["prerequisites"] = json!(prerequisites);
    }
    value["localization"] = localization;
    // GOG has a broader source path contract. A generic Launch selection is optional
    // when the recovered path is beyond the installation contract's limits.
    if parse(&value).is_err() {
        value["launch"] = Value::Null;
    }
    parse(&value)?;
    depot.insert("installation".into(), value);
    crate::declarations::append(tx, "Release", release, &data).await?;
    Ok(())
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    #[test]
    fn disabling_one_package_preserves_remaining_order_and_excludes_alternatives() {
        let report = json!({"packages":[
            {"path":"dx.exe","name":"DirectX","platforms":["windows/x86_64"],"status":"ready"},
            {"path":"vc.exe","name":"Visual C++","platforms":["windows/x86_64"],"status":"ready"},
            {"path":"physx.exe","name":"PhysX","platforms":["windows/x86_64"],"status":"alternative"}
        ]});
        let baseline = discovery_prerequisites(&report, "association", None);
        assert_eq!(baseline.len(), 2);
        let result = discovery_prerequisites(&report, "association", Some(&json!(["dx.exe"])));
        assert_eq!(result, baseline[1..]);
        assert_eq!(result[0]["arguments"], json!([]));
        assert!(discovery_notes(&report)[0].contains("Conflicting package alternatives"));
    }
}
