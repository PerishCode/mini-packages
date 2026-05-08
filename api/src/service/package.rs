use percent_encoding::percent_decode_str;

use crate::error::AppError;

pub fn validate_package_name(name: &str) -> Result<(String, String), AppError> {
    if !name.starts_with('@') {
        return Err(AppError::BadRequest(
            "only scoped packages are supported".to_owned(),
        ));
    }
    let Some((scope, package)) = name[1..].split_once('/') else {
        return Err(AppError::BadRequest(
            "package name must be in @scope/name form".to_owned(),
        ));
    };
    if !valid_component(scope) || !valid_component(package) {
        return Err(AppError::BadRequest(
            "package scope/name contains unsupported characters".to_owned(),
        ));
    }
    Ok((format!("@{scope}"), package.to_owned()))
}

pub fn validate_scope_pattern(pattern: &str) -> Result<(), AppError> {
    if let Some(scope) = pattern.strip_suffix("/*") {
        validate_scope_name(scope)?;
        return Ok(());
    }
    validate_package_name(pattern).map(|_| ())
}

pub fn validate_dist_tag(tag: &str) -> Result<(), AppError> {
    if tag.is_empty() || tag.len() > 128 {
        return Err(AppError::BadRequest("invalid dist-tag length".to_owned()));
    }
    let mut chars = tag.chars();
    let Some(first) = chars.next() else {
        return Err(AppError::BadRequest("invalid dist-tag".to_owned()));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(AppError::BadRequest(
            "dist-tag must start with an ASCII letter or digit".to_owned(),
        ));
    }
    if !tag
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '_' || ch == '-')
    {
        return Err(AppError::BadRequest(
            "dist-tag contains unsupported characters".to_owned(),
        ));
    }
    if semver::Version::parse(tag).is_ok() || semver::VersionReq::parse(tag).is_ok() {
        return Err(AppError::BadRequest(
            "dist-tag must not be semver-like".to_owned(),
        ));
    }
    Ok(())
}

pub fn claim_matches(pattern: &str, package_name: &str) -> bool {
    if pattern == package_name {
        return true;
    }
    if let Some(scope) = pattern.strip_suffix("/*") {
        return package_name
            .strip_prefix(scope)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some();
    }
    false
}

pub fn decode_package_path(value: &str) -> Result<String, AppError> {
    percent_decode_str(value)
        .decode_utf8()
        .map(|value| value.into_owned())
        .map_err(|_| AppError::BadRequest("invalid percent-encoded package path".to_owned()))
}

pub fn encode_package_name(name: &str) -> String {
    name.replace('/', "%2F")
}

pub fn package_basename(package_name: &str) -> Result<String, AppError> {
    validate_package_name(package_name).map(|(_, package)| package)
}

pub fn tarball_filename(package_name: &str, version: &str) -> Result<String, AppError> {
    Ok(format!("{}-{version}.tgz", package_basename(package_name)?))
}

pub fn version_from_tarball_filename(
    package_name: &str,
    filename: &str,
) -> Result<String, AppError> {
    let package = package_basename(package_name)?;
    let prefix = format!("{package}-");
    let Some(version) = filename
        .strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix(".tgz"))
    else {
        return Err(AppError::BadRequest(
            "tarball filename does not match package name".to_owned(),
        ));
    };
    Ok(version.to_owned())
}

fn validate_scope_name(scope: &str) -> Result<(), AppError> {
    let Some(scope_name) = scope.strip_prefix('@') else {
        return Err(AppError::BadRequest(
            "scope pattern must start with @".to_owned(),
        ));
    };
    if !valid_component(scope_name) {
        return Err(AppError::BadRequest(
            "scope pattern contains unsupported characters".to_owned(),
        ));
    }
    Ok(())
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.chars().all(|ch| {
            ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_' || ch == '-'
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_scoped_packages_only() {
        assert!(validate_package_name("@demo/pkg").is_ok());
        assert!(validate_package_name("pkg").is_err());
        assert!(validate_package_name("@demo/Bad").is_err());
    }

    #[test]
    fn rejects_semver_like_dist_tags() {
        assert!(validate_dist_tag("latest").is_ok());
        assert!(validate_dist_tag("beta.1").is_ok());
        assert!(validate_dist_tag("1.2.3").is_err());
        assert!(validate_dist_tag("^1.0.0").is_err());
    }
}
