use app_depot_application::Principal;
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::catalog::{etag, get_app, CatalogError};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub app_id: String,
    pub name: String,
    pub release_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelMove {
    pub id: String,
    pub app_id: String,
    pub name: String,
    pub release_id: String,
    pub moved_at: String,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/')
}

pub async fn list(
    pool: &SqlitePool,
    actor: Option<&Principal>,
    app_id: &str,
) -> Result<Vec<Channel>, CatalogError> {
    get_app(pool, actor, app_id).await?;
    let rows = sqlx::query(
        "SELECT app_id, name, release_id FROM channels WHERE app_id=? ORDER BY name COLLATE BINARY",
    )
    .bind(app_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(Channel {
                app_id: row.try_get("app_id")?,
                name: row.try_get("name")?,
                release_id: row.try_get("release_id")?,
            })
        })
        .collect()
}

pub async fn get(
    pool: &SqlitePool,
    actor: Option<&Principal>,
    app_id: &str,
    name: &str,
) -> Result<(Channel, String), CatalogError> {
    if !valid_name(name) {
        return Err(CatalogError::Invalid);
    }
    get_app(pool, actor, app_id).await?;
    let row = sqlx::query(
        "SELECT release_id, generation_id, etag_version FROM channels WHERE app_id=? AND name=?",
    )
    .bind(app_id)
    .bind(name)
    .fetch_optional(pool)
    .await?
    .ok_or(CatalogError::NotFound)?;
    let generation: String = row.try_get("generation_id")?;
    let version: i64 = row.try_get("etag_version")?;
    Ok((
        Channel {
            app_id: app_id.into(),
            name: name.into(),
            release_id: row.try_get("release_id")?,
        },
        etag("channel", &generation, version),
    ))
}

pub async fn put(
    pool: &SqlitePool,
    actor: &Principal,
    app_id: &str,
    name: &str,
    release_id: &str,
    if_match: Option<&str>,
    if_none_match: Option<&str>,
) -> Result<(Channel, String, bool), CatalogError> {
    if !valid_name(name) || if_match.is_some() && if_none_match.is_some() {
        return Err(CatalogError::Invalid);
    }
    let mut tx = pool.begin().await?;
    let owner: String = sqlx::query_scalar("SELECT owner_id FROM apps WHERE id=?")
        .bind(app_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(CatalogError::NotFound)?;
    if !actor.may_manage(&owner) {
        return Err(CatalogError::NotFound);
    }
    let target: Option<String> =
        sqlx::query_scalar("SELECT state FROM releases WHERE id=? AND app_id=?")
            .bind(release_id)
            .bind(app_id)
            .fetch_optional(&mut *tx)
            .await?;
    if target.as_deref() != Some("Published") {
        return Err(CatalogError::InvalidChannelTarget);
    }
    let current =
        sqlx::query("SELECT generation_id, etag_version FROM channels WHERE app_id=? AND name=?")
            .bind(app_id)
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?;
    let (validator, created) = if let Some(row) = current {
        if if_none_match.is_some() {
            return Err(CatalogError::StaleEtag);
        }
        let supplied = if_match.ok_or(CatalogError::PreconditionRequired)?;
        let generation: String = row.try_get("generation_id")?;
        let version: i64 = row.try_get("etag_version")?;
        if supplied != etag("channel", &generation, version) {
            return Err(CatalogError::StaleEtag);
        }
        sqlx::query("UPDATE channels SET release_id=?, etag_version=etag_version+1 WHERE app_id=? AND name=?")
            .bind(release_id).bind(app_id).bind(name).execute(&mut *tx).await?;
        (etag("channel", &generation, version + 1), false)
    } else {
        if if_match.is_some() {
            return Err(CatalogError::StaleEtag);
        }
        if if_none_match != Some("*") {
            return Err(CatalogError::PreconditionRequired);
        }
        let generation = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO channels(app_id, name, release_id, generation_id) VALUES(?, ?, ?, ?)",
        )
        .bind(app_id)
        .bind(name)
        .bind(release_id)
        .bind(&generation)
        .execute(&mut *tx)
        .await?;
        (etag("channel", &generation, 1), true)
    };
    sqlx::query("INSERT INTO channel_moves(id, app_id, name, release_id, moved_at) VALUES(?, ?, ?, ?, strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))")
        .bind(Uuid::new_v4().to_string()).bind(app_id).bind(name).bind(release_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((
        Channel {
            app_id: app_id.into(),
            name: name.into(),
            release_id: release_id.into(),
        },
        validator,
        created,
    ))
}

pub async fn delete(
    pool: &SqlitePool,
    actor: &Principal,
    app_id: &str,
    name: &str,
    if_match: Option<&str>,
) -> Result<(), CatalogError> {
    if !valid_name(name) {
        return Err(CatalogError::Invalid);
    }
    let supplied = if_match.ok_or(CatalogError::PreconditionRequired)?;
    let mut tx = pool.begin().await?;
    let owner: String = sqlx::query_scalar("SELECT owner_id FROM apps WHERE id=?")
        .bind(app_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(CatalogError::NotFound)?;
    if !actor.may_manage(&owner) {
        return Err(CatalogError::NotFound);
    }
    let row =
        sqlx::query("SELECT generation_id, etag_version FROM channels WHERE app_id=? AND name=?")
            .bind(app_id)
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(CatalogError::NotFound)?;
    let generation: String = row.try_get("generation_id")?;
    if supplied != etag("channel", &generation, row.try_get("etag_version")?) {
        return Err(CatalogError::StaleEtag);
    }
    sqlx::query("DELETE FROM channels WHERE app_id=? AND name=?")
        .bind(app_id)
        .bind(name)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn history(
    pool: &SqlitePool,
    actor: &Principal,
    app_id: &str,
    name: &str,
) -> Result<Vec<ChannelMove>, CatalogError> {
    if !valid_name(name) {
        return Err(CatalogError::Invalid);
    }
    let owner: String = sqlx::query_scalar("SELECT owner_id FROM apps WHERE id=?")
        .bind(app_id)
        .fetch_optional(pool)
        .await?
        .ok_or(CatalogError::NotFound)?;
    if !actor.may_manage(&owner) {
        return Err(CatalogError::NotFound);
    }
    let rows = sqlx::query("SELECT id, app_id, name, release_id, moved_at FROM channel_moves WHERE app_id=? AND name=? ORDER BY id")
        .bind(app_id).bind(name).fetch_all(pool).await?;
    rows.into_iter()
        .map(|row| {
            Ok(ChannelMove {
                id: row.try_get("id")?,
                app_id: row.try_get("app_id")?,
                name: row.try_get("name")?,
                release_id: row.try_get("release_id")?,
                moved_at: row.try_get("moved_at")?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::{bootstrap_admin, verify_password},
        catalog::{create_app, create_release, publish_release},
        init_local, lock_local, open_catalog, LocalMode,
    };
    use std::fs;

    #[tokio::test]
    async fn channel_requires_published_target_and_preserves_move_history() {
        let root = tempfile::tempdir().unwrap();
        init_local(root.path(), "channels", LocalMode::Dev).unwrap();
        let lock = lock_local(root.path(), "channels", LocalMode::Dev).unwrap();
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
        let app = create_app(&pool, &actor, "channel-app", "Channels")
            .await
            .unwrap();
        let first = create_release(&pool, &actor, "channel-first", &app.id, None)
            .await
            .unwrap();
        let second = create_release(&pool, &actor, "channel-second", &app.id, None)
            .await
            .unwrap();
        assert!(matches!(
            put(&pool, &actor, &app.id, "stable", &first.id, None, Some("*")).await,
            Err(CatalogError::InvalidChannelTarget)
        ));
        let digest = "a".repeat(64);
        sqlx::query("INSERT INTO cas_objects(digest, size, verified) VALUES(?, 1, 1)")
            .bind(&digest)
            .execute(&pool)
            .await
            .unwrap();
        let artifact_id = format!("adfile:sha256:{digest}");
        sqlx::query("INSERT INTO artifacts(id, digest, kind, size) VALUES(?, ?, 'File', 1)")
            .bind(&artifact_id)
            .bind(&digest)
            .execute(&pool)
            .await
            .unwrap();
        for release in [&first, &second] {
            sqlx::query("INSERT INTO associations(id, release_id, artifact_id) VALUES(?, ?, ?)")
                .bind(Uuid::new_v4().to_string())
                .bind(&release.id)
                .bind(&artifact_id)
                .execute(&pool)
                .await
                .unwrap();
            publish_release(
                &pool,
                &actor,
                &format!("publish-{}", release.id),
                &release.id,
                Some(&etag("release", &release.id, 1)),
            )
            .await
            .unwrap();
        }
        let (_, first_tag, created) =
            put(&pool, &actor, &app.id, "stable", &first.id, None, Some("*"))
                .await
                .unwrap();
        assert!(created);
        assert!(matches!(
            put(
                &pool,
                &actor,
                &app.id,
                "stable",
                &second.id,
                None,
                Some("*")
            )
            .await,
            Err(CatalogError::StaleEtag)
        ));
        let (_, second_tag, created) = put(
            &pool,
            &actor,
            &app.id,
            "stable",
            &second.id,
            Some(&first_tag),
            None,
        )
        .await
        .unwrap();
        assert!(!created);
        assert_ne!(first_tag, second_tag);
        assert_eq!(
            get(&pool, Some(&actor), &app.id, "stable")
                .await
                .unwrap()
                .0
                .release_id,
            second.id
        );
        assert_eq!(
            history(&pool, &actor, &app.id, "stable")
                .await
                .unwrap()
                .len(),
            2
        );
        assert!(matches!(
            delete(&pool, &actor, &app.id, "stable", Some(&first_tag)).await,
            Err(CatalogError::StaleEtag)
        ));
        delete(&pool, &actor, &app.id, "stable", Some(&second_tag))
            .await
            .unwrap();
        let (_, new_tag, created) =
            put(&pool, &actor, &app.id, "stable", &first.id, None, Some("*"))
                .await
                .unwrap();
        assert!(created);
        assert_ne!(new_tag, first_tag);
        assert_eq!(
            history(&pool, &actor, &app.id, "stable")
                .await
                .unwrap()
                .len(),
            3
        );
    }
}
