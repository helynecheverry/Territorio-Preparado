use super::text;
use super::{Page, documents};
use crate::security;
use crate::{
    AppState, auth, db,
    error::{self, Result},
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, patch},
};
use mongodb::bson::{Document, doc};
use serde::Deserialize;
use serde_json::{Value, json};
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/reports", get(list_reports).post(create_report))
        .route("/reports/{id}", patch(update_report))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    text: String,
    zone: String,
    latitude: Option<f64>,
    longitude: Option<f64>,
}
async fn create_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<Report>,
) -> Result<(StatusCode, Json<Value>)> {
    let a = auth::actor(&state, &headers).await?;
    security::limit(&state, "reports", &a.id, 30, 3600).await?;
    text(&input.text, 5000)?;
    text(&input.zone, 120)?;
    if input.latitude.is_some() != input.longitude.is_some()
        || input
            .latitude
            .is_some_and(|v| !v.is_finite() || !(-90.0..=90.0).contains(&v))
        || input
            .longitude
            .is_some_and(|v| !v.is_finite() || !(-180.0..=180.0).contains(&v))
    {
        return Err(error::bad("Coordenadas inválidas"));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let mut d = doc! {"_id":&id,"owner":&a.id,"text":input.text.trim(),"zone":input.zone.trim(),"status":"new","created":db::now(),"updated":db::now()};
    if let (Some(lat), Some(lon)) = (input.latitude, input.longitude) {
        d.insert("latitude", lat);
        d.insert("longitude", lon);
    }
    db::audit(&state.db, &a.id, "report_create", &id).await?;
    state
        .db
        .collection::<Document>("reports")
        .insert_one(d)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({"id":id}))))
}
async fn list_reports(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(page): Query<Page>,
) -> Result<Json<Value>> {
    let a = auth::actor(&state, &headers).await?;
    let filter = if a.role == "citizen" {
        doc! {"owner":a.id}
    } else {
        doc! {}
    };
    Ok(Json(
        json!({"items":documents(&state,"reports",filter,page.page).await?,"page_size":50}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportUpdate {
    status: String,
}
async fn update_report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(input): Json<ReportUpdate>,
) -> Result<Json<Value>> {
    let a = auth::actor(&state, &headers).await?;
    a.operator()?;
    if !["new", "reviewing", "resolved", "dismissed"].contains(&input.status.as_str()) {
        return Err(error::bad("Estado inválido"));
    }
    db::audit(&state.db, &a.id, "report_update", &id).await?;
    let r = state
        .db
        .collection::<Document>("reports")
        .update_one(
            doc! {"_id":id},
            doc! {"$set":{"status":input.status,"updated":db::now(),"reviewer":a.id}},
        )
        .await?;
    if r.matched_count == 0 {
        return Err(error::Error(StatusCode::NOT_FOUND, "Reporte no encontrado"));
    }
    Ok(Json(json!({"ok":true})))
}
