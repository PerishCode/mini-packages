use axum::{
    body::{to_bytes, Body},
    extract::State,
    http::{HeaderMap, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::Value;
use std::sync::Arc;

use crate::{
    error::AppError,
    service::{
        package::{decode_package_path, validate_package_name},
        registry::package_name_from_path,
    },
    state::AppState,
};

pub fn routes(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/-/ping", get(ping))
        .route("/-/whoami", get(whoami))
        .route("/-/npm/v1/security/audits", post(audit))
        .route("/-/npm/v1/security/advisories/bulk", post(advisories_bulk))
        .fallback(npm_fallback)
        .with_state(state)
}

async fn ping() -> Json<Value> {
    Json(serde_json::json!({ "ok": true }))
}

async fn whoami(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Value>, AppError> {
    let principal = state.auth().authenticate(&headers).await?;
    Ok(Json(serde_json::json!({
        "username": principal.token_id,
        "bootstrap": principal.bootstrap
    })))
}

async fn audit() -> Json<Value> {
    Json(serde_json::json!({
        "actions": [],
        "advisories": {},
        "muted": [],
        "metadata": {
            "vulnerabilities": {
                "info": 0,
                "low": 0,
                "moderate": 0,
                "high": 0,
                "critical": 0,
                "total": 0
            },
            "dependencies": 0,
            "devDependencies": 0,
            "optionalDependencies": 0,
            "totalDependencies": 0
        }
    }))
}

async fn advisories_bulk() -> Json<Value> {
    Json(serde_json::json!({}))
}

async fn npm_fallback(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    req: Request<Body>,
) -> Response {
    match route_npm(state, headers, req).await {
        Ok(response) => response,
        Err(err) => err.into_response(),
    }
}

async fn route_npm(
    state: Arc<AppState>,
    headers: HeaderMap,
    req: Request<Body>,
) -> Result<Response, AppError> {
    let method = req.method().clone();
    let raw_path = req.uri().path().trim_start_matches('/').to_owned();
    if raw_path.is_empty() {
        return Err(AppError::NotFound);
    }

    if raw_path.starts_with("-/package/") && raw_path.contains("/dist-tags") {
        return handle_dist_tags(&state, &headers, &method, &raw_path, req).await;
    }

    if let Some((encoded_package, filename)) = raw_path.split_once("/-/") {
        if method != Method::GET {
            return Err(AppError::NotFound);
        }
        let package_name = package_name_from_path(encoded_package)?;
        let principal = state.auth().authenticate(&headers).await?;
        state.auth().require_read(&principal, &package_name)?;
        let download = state.registry().download(&package_name, filename).await?;
        return Ok((download.headers, download.bytes).into_response());
    }

    let package_name = package_name_from_path(&raw_path)?;
    match method {
        Method::GET => {
            let principal = state.auth().authenticate(&headers).await?;
            state.auth().require_read(&principal, &package_name)?;
            Ok(Json(state.registry().packument(&package_name).await?).into_response())
        }
        Method::PUT => {
            let principal = state.auth().authenticate(&headers).await?;
            state.auth().require_publish(&principal, &package_name)?;
            let limit = state.config().max_tarball_bytes() * 3;
            let body = to_bytes(req.into_body(), limit)
                .await
                .map_err(|_| AppError::BadRequest("failed to read publish body".to_owned()))?;
            Ok((
                StatusCode::CREATED,
                Json(
                    state
                        .registry()
                        .publish(&principal, &package_name, &body)
                        .await?,
                ),
            )
                .into_response())
        }
        _ => Err(AppError::NotFound),
    }
}

async fn handle_dist_tags(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    method: &Method,
    raw_path: &str,
    req: Request<Body>,
) -> Result<Response, AppError> {
    let Some(rest) = raw_path.strip_prefix("-/package/") else {
        return Err(AppError::NotFound);
    };
    let Some((encoded_package, suffix)) = rest.split_once("/dist-tags") else {
        return Err(AppError::NotFound);
    };
    let package_name = decode_package_path(encoded_package)?;
    validate_package_name(&package_name)?;

    let tag = suffix.strip_prefix('/').filter(|value| !value.is_empty());
    match (method, tag) {
        (&Method::GET, None) => {
            let principal = state.auth().authenticate(headers).await?;
            state.auth().require_read(&principal, &package_name)?;
            Ok(Json(state.registry().list_dist_tags(&package_name).await?).into_response())
        }
        (&Method::PUT, Some(tag)) => {
            let principal = state.auth().authenticate(headers).await?;
            state.auth().require_publish(&principal, &package_name)?;
            let body = to_bytes(req.into_body(), 1024)
                .await
                .map_err(|_| AppError::BadRequest("failed to read dist-tag body".to_owned()))?;
            Ok(Json(
                state
                    .registry()
                    .set_dist_tag(&package_name, tag, &body)
                    .await?,
            )
            .into_response())
        }
        (&Method::DELETE, Some(tag)) => {
            let principal = state.auth().authenticate(headers).await?;
            state.auth().require_publish(&principal, &package_name)?;
            Ok(Json(state.registry().remove_dist_tag(&package_name, tag).await?).into_response())
        }
        _ => Err(AppError::NotFound),
    }
}
