use app_depot_application::{may_read_catalog, Principal};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::catalog::{etag, CatalogError};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub revision_id: String,
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Revision {
    pub id: String,
    pub created_at: String,
    pub data: Value,
}

async fn read_allowed(
    pool: &SqlitePool,
    actor: Option<&Principal>,
    scope: &str,
    id: &str,
) -> Result<(), CatalogError> {
    let (owner, public_published): (String, i64) = match scope {
        "App" => sqlx::query_as("SELECT owner_id, CASE WHEN visibility='Public' AND state='Published' THEN 1 ELSE 0 END FROM apps WHERE id=?")
            .bind(id).fetch_optional(pool).await?,
        "Release" => sqlx::query_as("SELECT a.owner_id, CASE WHEN a.visibility='Public' AND r.state='Published' THEN 1 ELSE 0 END FROM releases r JOIN apps a ON a.id=r.app_id WHERE r.id=?")
            .bind(id).fetch_optional(pool).await?,
        "Association" => sqlx::query_as("SELECT a.owner_id, CASE WHEN a.visibility='Public' AND r.state='Published' THEN 1 ELSE 0 END FROM associations x JOIN releases r ON r.id=x.release_id JOIN apps a ON a.id=r.app_id WHERE x.id=?")
            .bind(id).fetch_optional(pool).await?,
        _ => return Err(CatalogError::Invalid),
    }.ok_or(CatalogError::NotFound)?;
    let anonymous_enabled = if actor.is_none() {
        sqlx::query_scalar::<_, i64>(
            "SELECT anonymous_read_enabled FROM server_settings WHERE id=1",
        )
        .fetch_one(pool)
        .await?
            != 0
    } else {
        false
    };
    if !may_read_catalog(actor, &owner, public_published != 0, anonymous_enabled) {
        return Err(CatalogError::NotFound);
    }
    Ok(())
}

pub async fn get(
    pool: &SqlitePool,
    actor: Option<&Principal>,
    scope: &str,
    id: &str,
) -> Result<(Document, i64), CatalogError> {
    read_allowed(pool, actor, scope, id).await?;
    let row = sqlx::query("SELECT d.revision_id, d.etag_version, r.data_json FROM declaration_documents d JOIN declaration_revisions r ON r.id=d.revision_id WHERE d.scope=? AND d.resource_id=?")
        .bind(scope).bind(id).fetch_optional(pool).await?.ok_or(CatalogError::NotFound)?;
    Ok((
        Document {
            revision_id: row.try_get("revision_id")?,
            data: serde_json::from_str(&row.try_get::<String, _>("data_json")?)?,
        },
        row.try_get("etag_version")?,
    ))
}

pub async fn put(
    pool: &SqlitePool,
    actor: &Principal,
    scope: &str,
    id: &str,
    if_match: Option<&str>,
    data: &Value,
) -> Result<(Document, i64), CatalogError> {
    let validator = if_match.ok_or(CatalogError::PreconditionRequired)?;
    let mut tx = pool.begin().await?;
    let owner: Option<String> = match scope {
        "App" => sqlx::query_scalar("SELECT owner_id FROM apps WHERE id=?").bind(id).fetch_optional(&mut *tx).await?,
        "Release" => sqlx::query_scalar("SELECT a.owner_id FROM releases r JOIN apps a ON a.id=r.app_id WHERE r.id=?")
            .bind(id).fetch_optional(&mut *tx).await?,
        "Association" => sqlx::query_scalar("SELECT a.owner_id FROM associations x JOIN releases r ON r.id=x.release_id JOIN apps a ON a.id=r.app_id WHERE x.id=?")
            .bind(id).fetch_optional(&mut *tx).await?,
        _ => return Err(CatalogError::Invalid),
    };
    let owner = owner.ok_or(CatalogError::NotFound)?;
    if !actor.may_manage(&owner) {
        return Err(CatalogError::NotFound);
    }
    let version: i64 = sqlx::query_scalar(
        "SELECT etag_version FROM declaration_documents WHERE scope=? AND resource_id=?",
    )
    .bind(scope)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if validator != etag("declarations", id, version) {
        return Err(CatalogError::StaleEtag);
    }
    let revision_id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO declaration_revisions(id, scope, resource_id, data_json, created_at) VALUES(?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))")
        .bind(&revision_id).bind(scope).bind(id).bind(serde_json::to_string(data)?).execute(&mut *tx).await?;
    sqlx::query("UPDATE declaration_documents SET revision_id=?, etag_version=etag_version+1 WHERE scope=? AND resource_id=?")
        .bind(&revision_id).bind(scope).bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((
        Document {
            revision_id,
            data: data.clone(),
        },
        version + 1,
    ))
}

pub async fn revisions(
    pool: &SqlitePool,
    actor: Option<&Principal>,
    scope: &str,
    id: &str,
) -> Result<Vec<Revision>, CatalogError> {
    read_allowed(pool, actor, scope, id).await?;
    let rows = sqlx::query("SELECT id, created_at, data_json FROM declaration_revisions WHERE scope=? AND resource_id=? ORDER BY id")
        .bind(scope).bind(id).fetch_all(pool).await?;
    rows.into_iter()
        .map(|row| {
            Ok(Revision {
                id: row.try_get("id")?,
                created_at: row.try_get("created_at")?,
                data: serde_json::from_str(&row.try_get::<String, _>("data_json")?)?,
            })
        })
        .collect()
}

pub async fn revision(
    pool: &SqlitePool,
    actor: Option<&Principal>,
    scope: &str,
    id: &str,
    revision_id: &str,
) -> Result<Revision, CatalogError> {
    read_allowed(pool, actor, scope, id).await?;
    let row = sqlx::query("SELECT id, created_at, data_json FROM declaration_revisions WHERE scope=? AND resource_id=? AND id=?")
        .bind(scope).bind(id).bind(revision_id).fetch_optional(pool).await?.ok_or(CatalogError::NotFound)?;
    Ok(Revision {
        id: row.try_get("id")?,
        created_at: row.try_get("created_at")?,
        data: serde_json::from_str(&row.try_get::<String, _>("data_json")?)?,
    })
}

pub async fn snapshot(
    pool: &SqlitePool,
    actor: Option<&Principal>,
    release_id: &str,
) -> Result<(Value, String), CatalogError> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query("SELECT r.app_id, r.state, a.owner_id, a.visibility FROM releases r JOIN apps a ON a.id=r.app_id WHERE r.id=?")
        .bind(release_id).fetch_optional(&mut *tx).await?.ok_or(CatalogError::NotFound)?;
    let owner: String = row.try_get("owner_id")?;
    let anonymous_enabled = if actor.is_none() {
        sqlx::query_scalar::<_, i64>(
            "SELECT anonymous_read_enabled FROM server_settings WHERE id=1",
        )
        .fetch_one(&mut *tx)
        .await?
            != 0
    } else {
        false
    };
    if !may_read_catalog(
        actor,
        &owner,
        row.try_get::<String, _>("visibility")? == "Public"
            && row.try_get::<String, _>("state")? == "Published",
        anonymous_enabled,
    ) {
        return Err(CatalogError::NotFound);
    }
    let app_id: String = row.try_get("app_id")?;
    let app = document_in_tx(&mut tx, "App", &app_id).await?;
    let release = document_in_tx(&mut tx, "Release", release_id).await?;
    let rows = sqlx::query("SELECT id FROM associations WHERE release_id=? ORDER BY id")
        .bind(release_id)
        .fetch_all(&mut *tx)
        .await?;
    let mut associations = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.try_get("id")?;
        let declaration = document_in_tx(&mut tx, "Association", &id).await?;
        associations.push(json!({"associationId":id,"declaration":declaration}));
    }
    let output =
        json!({"releaseId":release_id,"app":app,"release":release,"associations":associations});
    let validator = format!(
        "\"snapshot-{:x}\"",
        Sha256::digest(serde_json::to_vec(&output)?)
    );
    tx.commit().await?;
    Ok((output, validator))
}

async fn document_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    scope: &str,
    id: &str,
) -> Result<Document, CatalogError> {
    let row = sqlx::query("SELECT d.revision_id, r.data_json FROM declaration_documents d JOIN declaration_revisions r ON r.id=d.revision_id WHERE d.scope=? AND d.resource_id=?")
        .bind(scope).bind(id).fetch_one(&mut **tx).await?;
    Ok(Document {
        revision_id: row.try_get("revision_id")?,
        data: serde_json::from_str(&row.try_get::<String, _>("data_json")?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::{bootstrap_admin, verify_password},
        catalog::{create_app, create_release},
        init_local, lock_local, open_catalog, LocalMode,
    };
    use std::fs;

    #[tokio::test]
    async fn null_and_all_json_values_create_revisions_and_consistent_snapshot() {
        let root = tempfile::tempdir().unwrap();
        init_local(root.path(), "declarations", LocalMode::Dev).unwrap();
        let lock = lock_local(root.path(), "declarations", LocalMode::Dev).unwrap();
        let pool = open_catalog(&lock).await.unwrap();
        let secret = root.path().join("bootstrap");
        fs::write(&secret, "safe secret phrase 12345").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&secret, fs::Permissions::from_mode(0o600)).unwrap();
        }
        bootstrap_admin(&pool, Some(&secret)).await.unwrap();
        let actor = verify_password(&pool, "admin", "safe secret phrase 12345")
            .await
            .unwrap()
            .unwrap();
        let app = create_app(&pool, &actor, "declarations-app", "Declarations")
            .await
            .unwrap();
        let release = create_release(&pool, &actor, "declarations-release", &app.id, None)
            .await
            .unwrap();
        let (first, first_version) = get(&pool, Some(&actor), "App", &app.id).await.unwrap();
        assert_eq!(first.data, json!({}));
        assert_eq!(first_version, 1);
        let (before, before_etag) = snapshot(&pool, Some(&actor), &release.id).await.unwrap();
        assert_eq!(before["app"]["data"], json!({}));
        let (null_doc, version) = put(
            &pool,
            &actor,
            "App",
            &app.id,
            Some(&etag("declarations", &app.id, 1)),
            &Value::Null,
        )
        .await
        .unwrap();
        assert_eq!(null_doc.data, Value::Null);
        assert_eq!(version, 2);
        assert!(matches!(
            put(
                &pool,
                &actor,
                "App",
                &app.id,
                Some(&etag("declarations", &app.id, 1)),
                &json!(true)
            )
            .await,
            Err(CatalogError::StaleEtag)
        ));
        for data in [
            json!([1, "x"]),
            json!("text"),
            json!(42),
            json!(false),
            json!({"nested":null}),
        ] {
            let (_, current_version) = get(&pool, Some(&actor), "App", &app.id).await.unwrap();
            put(
                &pool,
                &actor,
                "App",
                &app.id,
                Some(&etag("declarations", &app.id, current_version)),
                &data,
            )
            .await
            .unwrap();
        }
        let history = revisions(&pool, Some(&actor), "App", &app.id)
            .await
            .unwrap();
        assert_eq!(history.len(), 7);
        assert_eq!(
            revision(&pool, Some(&actor), "App", &app.id, &null_doc.revision_id)
                .await
                .unwrap()
                .data,
            Value::Null
        );
        let (after, after_etag) = snapshot(&pool, Some(&actor), &release.id).await.unwrap();
        assert_ne!(before_etag, after_etag);
        assert_eq!(after["app"]["data"], json!({"nested":null}));
    }
}
