use async_trait::async_trait;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbErr, FromQueryResult, QueryResult, TransactionTrait,
};
use serde_json::Value as JsonValue;
use std::{collections::BTreeMap, sync::Arc};

use crate::{
    model::TarballRecord,
    repo::{json_value, stmt},
};

#[derive(Clone)]
pub struct PublishStartInput {
    pub name: String,
    pub scope: String,
    pub version: String,
    pub manifest: JsonValue,
    pub publisher_token_id: String,
}

#[derive(Clone)]
pub struct PublishFinalizeInput {
    pub version_id: i64,
    pub object_key: String,
    pub integrity: String,
    pub shasum: String,
    pub size_bytes: i64,
    pub dist_tags: BTreeMap<String, String>,
}

#[derive(Clone)]
pub struct PackumentData {
    pub name: String,
    pub created_at: String,
    pub modified_at: Option<String>,
    pub versions: Vec<PackumentVersion>,
    pub dist_tags: BTreeMap<String, String>,
}

#[derive(Clone)]
pub struct PackumentVersion {
    pub version: String,
    pub manifest: JsonValue,
    pub integrity: String,
    pub shasum: String,
    pub size_bytes: i64,
    pub published_at: String,
}

#[async_trait]
pub trait PackagesRepo: Send + Sync {
    async fn begin_publish(&self, input: PublishStartInput) -> Result<Option<i64>, DbErr>;
    async fn finalize_publish(&self, input: PublishFinalizeInput) -> Result<(), DbErr>;
    async fn mark_failed(&self, version_id: i64, reason: &str) -> Result<(), DbErr>;
    async fn packument(&self, name: &str) -> Result<Option<PackumentData>, DbErr>;
    async fn find_tarball(
        &self,
        package_name: &str,
        version: &str,
    ) -> Result<Option<TarballRecord>, DbErr>;
    async fn list_dist_tags(
        &self,
        package_name: &str,
    ) -> Result<Option<BTreeMap<String, String>>, DbErr>;
    async fn set_dist_tag(
        &self,
        package_name: &str,
        tag: &str,
        version: &str,
    ) -> Result<bool, DbErr>;
    async fn remove_dist_tag(&self, package_name: &str, tag: &str) -> Result<bool, DbErr>;
}

pub struct PgPackagesRepo {
    db: Arc<DatabaseConnection>,
}

impl PgPackagesRepo {
    pub fn new(db: Arc<DatabaseConnection>) -> Self {
        Self { db }
    }

    fn package_row(row: QueryResult) -> Result<PackageRow, DbErr> {
        PackageRow::from_query_result(&row, "")
    }

    fn version_row(row: QueryResult) -> Result<VersionRow, DbErr> {
        VersionRow::from_query_result(&row, "")
    }

    async fn tags_for<C>(conn: &C, package_id: i64) -> Result<BTreeMap<String, String>, DbErr>
    where
        C: ConnectionTrait,
    {
        let rows = conn
            .query_all(stmt(
                "SELECT tag, version FROM dist_tags WHERE package_id = $1 ORDER BY tag",
                vec![package_id.into()],
            ))
            .await?;
        let mut tags = BTreeMap::new();
        for row in rows {
            tags.insert(row.try_get("", "tag")?, row.try_get("", "version")?);
        }
        Ok(tags)
    }

    async fn record_event<C>(
        conn: &C,
        event_type: &str,
        actor_token_id: Option<&str>,
        package_name: Option<&str>,
        package_version: Option<&str>,
        payload: JsonValue,
    ) -> Result<(), DbErr>
    where
        C: ConnectionTrait,
    {
        conn.execute(stmt(
            r#"
INSERT INTO registry_events(event_type, actor_token_id, package_name, package_version, payload)
VALUES ($1, $2, $3, $4, $5)
"#,
            vec![
                event_type.to_owned().into(),
                actor_token_id.map(ToOwned::to_owned).into(),
                package_name.map(ToOwned::to_owned).into(),
                package_version.map(ToOwned::to_owned).into(),
                json_value(payload),
            ],
        ))
        .await?;
        Ok(())
    }
}

#[async_trait]
impl PackagesRepo for PgPackagesRepo {
    async fn begin_publish(&self, input: PublishStartInput) -> Result<Option<i64>, DbErr> {
        let txn = self.db.begin().await?;
        let package_row = txn
            .query_one(stmt(
                r#"
INSERT INTO packages(name, scope)
VALUES ($1, $2)
ON CONFLICT (name) DO UPDATE SET name = EXCLUDED.name
RETURNING id
"#,
                vec![input.name.clone().into(), input.scope.clone().into()],
            ))
            .await?
            .expect("package upsert returned no row");
        let package_id: i64 = package_row.try_get("", "id")?;

        let version_row = txn
            .query_one(stmt(
                r#"
INSERT INTO package_versions(package_id, version, status, manifest, publisher_token_id)
VALUES ($1, $2, 'publishing', $3, $4)
ON CONFLICT (package_id, version) DO NOTHING
RETURNING id
"#,
                vec![
                    package_id.into(),
                    input.version.clone().into(),
                    json_value(input.manifest),
                    input.publisher_token_id.clone().into(),
                ],
            ))
            .await?;

        let Some(version_row) = version_row else {
            txn.rollback().await?;
            return Ok(None);
        };
        let version_id: i64 = version_row.try_get("", "id")?;

        Self::record_event(
            &txn,
            "publish_started",
            Some(&input.publisher_token_id),
            Some(&input.name),
            Some(&input.version),
            serde_json::json!({}),
        )
        .await?;
        txn.commit().await?;
        Ok(Some(version_id))
    }

    async fn finalize_publish(&self, input: PublishFinalizeInput) -> Result<(), DbErr> {
        let txn = self.db.begin().await?;
        let row = txn
            .query_one(stmt(
                r#"
UPDATE package_versions
SET status = 'ready',
    object_key = $2,
    integrity = $3,
    shasum = $4,
    size_bytes = $5,
    published_at = now()
WHERE id = $1
RETURNING package_id, version, publisher_token_id
"#,
                vec![
                    input.version_id.into(),
                    input.object_key.clone().into(),
                    input.integrity.clone().into(),
                    input.shasum.clone().into(),
                    input.size_bytes.into(),
                ],
            ))
            .await?
            .expect("finalize publish returned no row");
        let package_id: i64 = row.try_get("", "package_id")?;
        let version: String = row.try_get("", "version")?;
        let publisher_token_id: Option<String> = row.try_get("", "publisher_token_id")?;

        for (tag, tag_version) in &input.dist_tags {
            txn.execute(stmt(
                r#"
INSERT INTO dist_tags(package_id, tag, version)
VALUES ($1, $2, $3)
ON CONFLICT (package_id, tag)
DO UPDATE SET version = EXCLUDED.version, updated_at = now()
"#,
                vec![
                    package_id.into(),
                    tag.clone().into(),
                    tag_version.clone().into(),
                ],
            ))
            .await?;
        }

        Self::record_event(
            &txn,
            "publish_ready",
            publisher_token_id.as_deref(),
            None,
            Some(&version),
            serde_json::json!({
                "object_key": input.object_key,
                "integrity": input.integrity,
                "shasum": input.shasum,
                "size_bytes": input.size_bytes,
                "dist_tags": input.dist_tags,
            }),
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    async fn mark_failed(&self, version_id: i64, reason: &str) -> Result<(), DbErr> {
        self.db
            .execute(stmt(
                r#"
UPDATE package_versions
SET status = 'failed'
WHERE id = $1 AND status = 'publishing'
"#,
                vec![version_id.into()],
            ))
            .await?;
        Self::record_event(
            self.db.as_ref(),
            "publish_failed",
            None,
            None,
            None,
            serde_json::json!({ "version_id": version_id, "reason": reason }),
        )
        .await?;
        Ok(())
    }

    async fn packument(&self, name: &str) -> Result<Option<PackumentData>, DbErr> {
        let Some(row) = self
            .db
            .query_one(stmt(
                r#"
SELECT id, name, created_at::text AS created_at
FROM packages
WHERE name = $1
"#,
                vec![name.to_owned().into()],
            ))
            .await?
        else {
            return Ok(None);
        };
        let package = Self::package_row(row)?;

        let version_rows = self
            .db
            .query_all(stmt(
                r#"
SELECT version,
       manifest,
       integrity,
       shasum,
       size_bytes,
       published_at::text AS published_at
FROM package_versions
WHERE package_id = $1
  AND status = 'ready'
ORDER BY published_at ASC
"#,
                vec![package.id.into()],
            ))
            .await?;
        let mut versions = Vec::with_capacity(version_rows.len());
        let mut modified_at = None;
        for row in version_rows {
            let row = Self::version_row(row)?;
            modified_at = Some(row.published_at.clone());
            versions.push(PackumentVersion {
                version: row.version,
                manifest: row.manifest,
                integrity: row.integrity,
                shasum: row.shasum,
                size_bytes: row.size_bytes,
                published_at: row.published_at,
            });
        }
        if versions.is_empty() {
            return Ok(None);
        }

        Ok(Some(PackumentData {
            name: package.name,
            created_at: package.created_at,
            modified_at,
            versions,
            dist_tags: Self::tags_for(self.db.as_ref(), package.id).await?,
        }))
    }

    async fn find_tarball(
        &self,
        package_name: &str,
        version: &str,
    ) -> Result<Option<TarballRecord>, DbErr> {
        let Some(row) = self
            .db
            .query_one(stmt(
                r#"
SELECT pv.object_key
FROM package_versions pv
JOIN packages p ON p.id = pv.package_id
WHERE p.name = $1
  AND pv.version = $2
  AND pv.status = 'ready'
"#,
                vec![package_name.to_owned().into(), version.to_owned().into()],
            ))
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(TarballRecord {
            object_key: row.try_get("", "object_key")?,
        }))
    }

    async fn list_dist_tags(
        &self,
        package_name: &str,
    ) -> Result<Option<BTreeMap<String, String>>, DbErr> {
        let Some(row) = self
            .db
            .query_one(stmt(
                "SELECT id FROM packages WHERE name = $1",
                vec![package_name.to_owned().into()],
            ))
            .await?
        else {
            return Ok(None);
        };
        let package_id: i64 = row.try_get("", "id")?;
        Ok(Some(Self::tags_for(self.db.as_ref(), package_id).await?))
    }

    async fn set_dist_tag(
        &self,
        package_name: &str,
        tag: &str,
        version: &str,
    ) -> Result<bool, DbErr> {
        let Some(row) = self
            .db
            .query_one(stmt(
                r#"
SELECT p.id
FROM packages p
JOIN package_versions pv ON pv.package_id = p.id
WHERE p.name = $1
  AND pv.version = $2
  AND pv.status = 'ready'
"#,
                vec![package_name.to_owned().into(), version.to_owned().into()],
            ))
            .await?
        else {
            return Ok(false);
        };
        let package_id: i64 = row.try_get("", "id")?;
        self.db
            .execute(stmt(
                r#"
INSERT INTO dist_tags(package_id, tag, version)
VALUES ($1, $2, $3)
ON CONFLICT (package_id, tag)
DO UPDATE SET version = EXCLUDED.version, updated_at = now()
"#,
                vec![
                    package_id.into(),
                    tag.to_owned().into(),
                    version.to_owned().into(),
                ],
            ))
            .await?;
        Ok(true)
    }

    async fn remove_dist_tag(&self, package_name: &str, tag: &str) -> Result<bool, DbErr> {
        let result = self
            .db
            .execute(stmt(
                r#"
DELETE FROM dist_tags
WHERE tag = $2
  AND package_id = (SELECT id FROM packages WHERE name = $1)
"#,
                vec![package_name.to_owned().into(), tag.to_owned().into()],
            ))
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

#[derive(Debug, FromQueryResult)]
struct PackageRow {
    id: i64,
    name: String,
    created_at: String,
}

#[derive(Debug, FromQueryResult)]
struct VersionRow {
    version: String,
    manifest: JsonValue,
    integrity: String,
    shasum: String,
    size_bytes: i64,
    published_at: String,
}
