use async_trait::async_trait;
use axum::http::{header, HeaderMap, HeaderValue};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use bytes::Bytes;
use flate2::read::GzDecoder;
use serde::Deserialize;
use serde_json::{Map, Value};
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::Sha512;
use std::{collections::BTreeMap, io::Read, sync::Arc};

use crate::{
    config::ConfigService,
    error::AppError,
    model::{DistTagsResponse, Principal},
    repo::packages::{PackagesRepo, PublishFinalizeInput, PublishStartInput},
    service::{
        blob::BlobStore,
        package::{
            decode_package_path, encode_package_name, tarball_filename, validate_dist_tag,
            validate_package_name, version_from_tarball_filename,
        },
    },
};

#[derive(Deserialize)]
struct PublishRequest {
    #[serde(rename = "_id")]
    id: Option<String>,
    name: Option<String>,
    #[serde(default, rename = "dist-tags")]
    dist_tags: BTreeMap<String, String>,
    versions: BTreeMap<String, Value>,
    #[serde(default, rename = "_attachments")]
    attachments: BTreeMap<String, Attachment>,
}

#[derive(Deserialize)]
struct Attachment {
    data: String,
    length: Option<u64>,
}

pub struct TarballDownload {
    pub bytes: Bytes,
    pub headers: HeaderMap,
}

#[async_trait]
pub trait RegistryService: Send + Sync {
    async fn publish(
        &self,
        principal: &Principal,
        package_name: &str,
        body: &[u8],
    ) -> Result<Value, AppError>;
    async fn packument(&self, package_name: &str) -> Result<Value, AppError>;
    async fn download(
        &self,
        package_name: &str,
        filename: &str,
    ) -> Result<TarballDownload, AppError>;
    async fn list_dist_tags(&self, package_name: &str) -> Result<DistTagsResponse, AppError>;
    async fn set_dist_tag(
        &self,
        package_name: &str,
        tag: &str,
        body: &[u8],
    ) -> Result<DistTagsResponse, AppError>;
    async fn remove_dist_tag(
        &self,
        package_name: &str,
        tag: &str,
    ) -> Result<DistTagsResponse, AppError>;
}

pub struct RegistryServiceImpl {
    config: Arc<dyn ConfigService>,
    packages_repo: Arc<dyn PackagesRepo>,
    blob_store: Arc<dyn BlobStore>,
}

impl RegistryServiceImpl {
    pub fn new(
        config: Arc<dyn ConfigService>,
        packages_repo: Arc<dyn PackagesRepo>,
        blob_store: Arc<dyn BlobStore>,
    ) -> Self {
        Self {
            config,
            packages_repo,
            blob_store,
        }
    }

    fn normalize_publish_request(
        &self,
        path_package_name: &str,
        body: &[u8],
    ) -> Result<NormalizedPublish, AppError> {
        if body.len() > self.config.max_tarball_bytes() * 2 {
            return Err(AppError::BadRequest("publish payload too large".to_owned()));
        }
        let payload: PublishRequest = serde_json::from_slice(body)
            .map_err(|_| AppError::BadRequest("invalid publish JSON".to_owned()))?;
        let payload_name = payload
            .name
            .or(payload.id)
            .ok_or_else(|| AppError::BadRequest("publish payload missing name".to_owned()))?;
        if payload_name != path_package_name {
            return Err(AppError::BadRequest(
                "publish path and package name do not match".to_owned(),
            ));
        }
        let (scope, package) = validate_package_name(path_package_name)?;

        if payload.versions.len() != 1 {
            return Err(AppError::BadRequest(
                "publish payload must contain exactly one version".to_owned(),
            ));
        }
        let (version, version_manifest) = payload
            .versions
            .into_iter()
            .next()
            .expect("checked version count");
        semver::Version::parse(&version)
            .map_err(|_| AppError::BadRequest("package version must be semver".to_owned()))?;

        if payload.attachments.len() != 1 {
            return Err(AppError::BadRequest(
                "publish payload must contain exactly one attachment".to_owned(),
            ));
        }
        let (_, attachment) = payload
            .attachments
            .into_iter()
            .next()
            .expect("checked attachment count");
        let tarball = STANDARD
            .decode(attachment.data.as_bytes())
            .map_err(|_| AppError::BadRequest("invalid base64 attachment".to_owned()))?;
        if tarball.len() > self.config.max_tarball_bytes() {
            return Err(AppError::BadRequest("tarball too large".to_owned()));
        }
        if let Some(expected) = attachment.length {
            if expected != tarball.len() as u64 {
                return Err(AppError::BadRequest(
                    "attachment length does not match data".to_owned(),
                ));
            }
        }

        let package_json = package_json_from_tarball(&tarball)?;
        let manifest_name = package_json
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::BadRequest("tarball package.json missing name".to_owned()))?;
        let manifest_version = package_json
            .get("version")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AppError::BadRequest("tarball package.json missing version".to_owned())
            })?;
        if manifest_name != path_package_name || manifest_version != version {
            return Err(AppError::BadRequest(
                "tarball package.json name/version does not match publish payload".to_owned(),
            ));
        }

        let mut dist_tags = payload.dist_tags;
        if dist_tags.is_empty() {
            dist_tags.insert("latest".to_owned(), version.clone());
        }
        for (tag, tag_version) in &dist_tags {
            validate_dist_tag(tag)?;
            if tag_version != &version {
                return Err(AppError::BadRequest(
                    "initial publish dist-tags must point to the published version".to_owned(),
                ));
            }
        }

        let mut manifest = version_manifest;
        if !manifest.is_object() {
            manifest = package_json;
        }
        let object = manifest
            .as_object_mut()
            .ok_or_else(|| AppError::BadRequest("version manifest must be an object".to_owned()))?;
        object.insert(
            "name".to_owned(),
            Value::String(path_package_name.to_owned()),
        );
        object.insert("version".to_owned(), Value::String(version.clone()));
        object.remove("dist");

        Ok(NormalizedPublish {
            package_name: path_package_name.to_owned(),
            scope,
            package,
            version,
            manifest,
            tarball,
            dist_tags,
        })
    }

    fn tarball_url(&self, package_name: &str, version: &str) -> Result<String, AppError> {
        let base = self.config.registry_public_url().trim_end_matches('/');
        let filename = tarball_filename(package_name, version)?;
        Ok(format!(
            "{}/{}/-/{}",
            base,
            encode_package_name(package_name),
            filename
        ))
    }

    fn object_key(package_name: &str, package: &str, version: &str, sha512_hex: &str) -> String {
        let scope_path = package_name.trim_start_matches('@').replace('/', "/");
        let digest_prefix = &sha512_hex[..24.min(sha512_hex.len())];
        format!("packages/{scope_path}/{package}-{version}-{digest_prefix}.tgz")
    }
}

#[async_trait]
impl RegistryService for RegistryServiceImpl {
    async fn publish(
        &self,
        principal: &Principal,
        package_name: &str,
        body: &[u8],
    ) -> Result<Value, AppError> {
        let publish = self.normalize_publish_request(package_name, body)?;
        let version_id = self
            .packages_repo
            .begin_publish(PublishStartInput {
                name: publish.package_name.clone(),
                scope: publish.scope.clone(),
                version: publish.version.clone(),
                manifest: publish.manifest.clone(),
                publisher_token_id: principal.token_id.clone(),
            })
            .await?
            .ok_or_else(|| {
                AppError::Conflict(format!(
                    "{}@{} already exists",
                    publish.package_name, publish.version
                ))
            })?;

        let sha1_hex = hex_digest_sha1(&publish.tarball);
        let sha512_bytes = Sha512::digest(&publish.tarball);
        let sha512_b64 = STANDARD.encode(sha512_bytes);
        let sha512_hex = hex::encode(sha512_bytes);
        let integrity = format!("sha512-{sha512_b64}");
        let object_key = Self::object_key(
            &publish.package_name,
            &publish.package,
            &publish.version,
            &sha512_hex,
        );

        if let Err(err) = self
            .blob_store
            .put_tarball(&object_key, &publish.tarball)
            .await
        {
            let _ = self
                .packages_repo
                .mark_failed(version_id, &err.to_string())
                .await;
            return Err(err);
        }

        self.packages_repo
            .finalize_publish(PublishFinalizeInput {
                version_id,
                object_key,
                integrity,
                shasum: sha1_hex,
                size_bytes: publish.tarball.len() as i64,
                dist_tags: publish.dist_tags,
            })
            .await?;

        Ok(serde_json::json!({
            "ok": true,
            "id": publish.package_name,
            "rev": format!("{}-{}", publish.package_name, publish.version)
        }))
    }

    async fn packument(&self, package_name: &str) -> Result<Value, AppError> {
        let data = self
            .packages_repo
            .packument(package_name)
            .await?
            .ok_or(AppError::NotFound)?;

        let mut versions = Map::new();
        let mut time = Map::new();
        time.insert("created".to_owned(), Value::String(data.created_at));
        if let Some(modified_at) = data.modified_at {
            time.insert("modified".to_owned(), Value::String(modified_at));
        }

        for version in data.versions {
            let mut manifest = version.manifest;
            let object = manifest.as_object_mut().ok_or_else(|| {
                AppError::Internal("stored package manifest is not an object".to_owned())
            })?;
            object.insert(
                "_id".to_owned(),
                Value::String(format!("{}@{}", data.name, version.version)),
            );
            object.insert("name".to_owned(), Value::String(data.name.clone()));
            object.insert("version".to_owned(), Value::String(version.version.clone()));
            object.insert(
                "dist".to_owned(),
                serde_json::json!({
                    "tarball": self.tarball_url(&data.name, &version.version)?,
                    "integrity": version.integrity,
                    "shasum": version.shasum,
                    "size": version.size_bytes
                }),
            );
            time.insert(
                version.version.clone(),
                Value::String(version.published_at.clone()),
            );
            versions.insert(version.version, manifest);
        }

        Ok(serde_json::json!({
            "_id": data.name,
            "name": data.name,
            "dist-tags": data.dist_tags,
            "versions": versions,
            "time": time
        }))
    }

    async fn download(
        &self,
        package_name: &str,
        filename: &str,
    ) -> Result<TarballDownload, AppError> {
        let version = version_from_tarball_filename(package_name, filename)?;
        let record = self
            .packages_repo
            .find_tarball(package_name, &version)
            .await?
            .ok_or(AppError::NotFound)?;
        let bytes = self.blob_store.get_tarball(&record.object_key).await?;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        headers.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
                .map_err(|_| AppError::Internal("invalid tarball filename".to_owned()))?,
        );
        Ok(TarballDownload { bytes, headers })
    }

    async fn list_dist_tags(&self, package_name: &str) -> Result<DistTagsResponse, AppError> {
        let tags = self
            .packages_repo
            .list_dist_tags(package_name)
            .await?
            .ok_or(AppError::NotFound)?;
        Ok(DistTagsResponse { tags })
    }

    async fn set_dist_tag(
        &self,
        package_name: &str,
        tag: &str,
        body: &[u8],
    ) -> Result<DistTagsResponse, AppError> {
        validate_dist_tag(tag)?;
        let version = parse_dist_tag_version(body)?;
        if !self
            .packages_repo
            .set_dist_tag(package_name, tag, &version)
            .await?
        {
            return Err(AppError::NotFound);
        }
        self.list_dist_tags(package_name).await
    }

    async fn remove_dist_tag(
        &self,
        package_name: &str,
        tag: &str,
    ) -> Result<DistTagsResponse, AppError> {
        validate_dist_tag(tag)?;
        if !self
            .packages_repo
            .remove_dist_tag(package_name, tag)
            .await?
        {
            return Err(AppError::NotFound);
        }
        self.list_dist_tags(package_name).await
    }
}

struct NormalizedPublish {
    package_name: String,
    scope: String,
    package: String,
    version: String,
    manifest: Value,
    tarball: Vec<u8>,
    dist_tags: BTreeMap<String, String>,
}

pub fn package_name_from_path(value: &str) -> Result<String, AppError> {
    let decoded = decode_package_path(value)?;
    validate_package_name(&decoded)?;
    Ok(decoded)
}

fn package_json_from_tarball(bytes: &[u8]) -> Result<Value, AppError> {
    let decoder = GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|_| AppError::BadRequest("invalid tarball archive".to_owned()))?;
    for entry in entries {
        let mut entry =
            entry.map_err(|_| AppError::BadRequest("invalid tarball entry".to_owned()))?;
        let path = entry
            .path()
            .map_err(|_| AppError::BadRequest("invalid tarball path".to_owned()))?;
        if path.to_string_lossy() == "package/package.json" {
            let mut contents = String::new();
            entry
                .read_to_string(&mut contents)
                .map_err(|_| AppError::BadRequest("invalid package.json".to_owned()))?;
            return serde_json::from_str(&contents)
                .map_err(|_| AppError::BadRequest("invalid package.json JSON".to_owned()));
        }
    }
    Err(AppError::BadRequest(
        "tarball missing package/package.json".to_owned(),
    ))
}

fn hex_digest_sha1(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn parse_dist_tag_version(body: &[u8]) -> Result<String, AppError> {
    if let Ok(value) = serde_json::from_slice::<String>(body) {
        return Ok(value);
    }
    let value = std::str::from_utf8(body)
        .map_err(|_| AppError::BadRequest("dist-tag body must be UTF-8".to_owned()))?
        .trim()
        .trim_matches('"')
        .to_owned();
    if value.is_empty() {
        return Err(AppError::BadRequest(
            "dist-tag body must contain a version".to_owned(),
        ));
    }
    Ok(value)
}
