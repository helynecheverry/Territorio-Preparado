mod admin;
mod reports;
mod shelters;
use crate::{
    AppState, auth,
    error::{self, Result},
};
use axum::{
    Json, Router,
    extract::State,
    http::HeaderMap,
    routing::{get, post},
};
use futures_util::TryStreamExt;
use mongodb::bson::{Document, doc};
use serde::Deserialize;
use serde_json::{Value, json};
pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(reports::routes())
        .merge(shelters::routes())
        .nest("/admin", admin::routes())
        .route("/dashboard", get(dashboard))
        .route("/jobs", get(crate::ai::list).post(crate::ai::enqueue))
        .route("/jobs/{id}/retry", post(crate::ai::retry))
}
#[derive(Deserialize)]
pub struct Page {
    pub page: Option<u64>,
}
pub async fn documents(
    state: &AppState,
    collection: &str,
    filter: Document,
    page: Option<u64>,
) -> Result<Vec<Document>> {
    let page = page.unwrap_or(0).min(10000);
    Ok(state
        .db
        .collection::<Document>(collection)
        .find(filter)
        .sort(doc! {"created":-1,"_id":-1})
        .skip(page * 50)
        .limit(50)
        .await?
        .try_collect()
        .await?)
}
fn text(value: &str, max: usize) -> Result<&str> {
    let v = value.trim();
    if v.is_empty() || v.len() > max {
        Err(error::bad("Texto vacío o demasiado largo"))
    } else {
        Ok(v)
    }
}
async fn dashboard(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<Value>> {
    let a = auth::actor(&state, &headers).await?;
    a.operator()?;
    let reports = state
        .db
        .collection::<Document>("reports")
        .count_documents(doc! {})
        .await?;
    let pending = state
        .db
        .collection::<Document>("jobs")
        .count_documents(doc! {"status":{"$in":["queued","running"]}})
        .await?;
    let shelters = state
        .db
        .collection::<Document>("shelters")
        .count_documents(doc! {})
        .await?;
    Ok(Json(
        json!({"reports":reports,"pending_jobs":pending,"shelters":shelters}),
    ))
}
