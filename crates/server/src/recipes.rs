use crate::auth::{authorize, reader, ApiError, AuthState};
use crate::catalog::json_response;
use app_depot_domain::recipe::{RecipeError, RecipeV1};
use app_depot_infrastructure::{catalog::CatalogError, recipes};
use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Path, Query, State,
    },
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Platform {
    platform: String,
}

fn recipe_input(input: Result<Json<RecipeV1>, JsonRejection>) -> Result<RecipeV1, ApiError> {
    input
        .map(|Json(recipe)| recipe)
        .map_err(|_| CatalogError::from(RecipeError::Invalid).into())
}

async fn definition(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let actor = reader(&state, &headers).await?;
    let (recipe, etag) = recipes::get(&state.pool, actor.as_ref(), &id).await?;
    json_response(StatusCode::OK, None, etag, recipe)
}

async fn put(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    input: Result<Json<RecipeV1>, JsonRejection>,
) -> Result<Response, ApiError> {
    let actor = authorize(&state, &headers, true).await?;
    let recipe = recipe_input(input)?;
    let if_match = headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok());
    let etag = recipes::put(&state.pool, &actor, &id, if_match, &recipe).await?;
    json_response(StatusCode::OK, None, etag, recipe)
}

async fn preview(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    target: Result<Query<Platform>, QueryRejection>,
    input: Result<Json<RecipeV1>, JsonRejection>,
) -> Result<Json<recipes::Preview>, ApiError> {
    let actor = authorize(&state, &headers, true).await?;
    let recipe = recipe_input(input)?;
    let Query(target) = target.map_err(|_| CatalogError::from(RecipeError::Platform))?;
    Ok(Json(
        recipes::preview(&state.pool, &actor, &id, &target.platform, &recipe).await?,
    ))
}

async fn resolve(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    target: Result<Query<Platform>, QueryRejection>,
) -> Result<Json<recipes::Lock>, ApiError> {
    let actor = reader(&state, &headers).await?;
    let Query(target) = target.map_err(|_| CatalogError::from(RecipeError::Platform))?;
    Ok(Json(
        recipes::lock(&state.pool, actor.as_ref(), &id, &target.platform).await?,
    ))
}

pub fn router(state: AuthState) -> Router {
    Router::new()
        .route("/releases/{release_id}/recipe", get(definition).put(put))
        .route("/releases/{release_id}/recipe/lock", get(resolve))
        .route("/releases/{release_id}/recipe/preview", post(preview))
        .route_layer(middleware::from_fn(
            |request: axum::extract::Request, next: Next| async move {
                let mut response = next.run(request).await.into_response();
                response
                    .headers_mut()
                    .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                response
            },
        ))
        .with_state(state)
}
