use super::text;
use super::{Page, documents};
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
        .route("/shelters", get(list_shelters).post(create_shelter))
        .route("/shelters/{id}", patch(update_shelter))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Shelter {
    name: String,
    zone: String,
    capacity: i32,
    occupied: i32,
}
fn validate_shelter(i: &Shelter) -> Result<()> {
    text(&i.name, 200)?;
    text(&i.zone, 120)?;
    if i.capacity < 1 || i.occupied < 0 || i.occupied > i.capacity {
        return Err(error::bad("Ocupación o capacidad inválida"));
    }
    Ok(())
}
async fn list_shelters(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(p): Query<Page>,
) -> Result<Json<Value>> {
    auth::actor(&state, &headers).await?;
    Ok(Json(
        json!({"items":documents(&state,"shelters",doc!{},p.page).await?}),
    ))
}
async fn create_shelter(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(i): Json<Shelter>,
) -> Result<(StatusCode, Json<Value>)> {
    let a = auth::actor(&state, &headers).await?;
    a.operator()?;
    validate_shelter(&i)?;
    let id = uuid::Uuid::new_v4().to_string();
    db::audit(&state.db, &a.id, "shelter_create", &id).await?;
    state.db.collection::<Document>("shelters").insert_one(doc!{"_id":&id,"name":i.name,"zone":i.zone,"capacity":i.capacity,"occupied":i.occupied,"created":db::now(),"updated":db::now()}).await?;
    Ok((StatusCode::CREATED, Json(json!({"id":id}))))
}
async fn update_shelter(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(i): Json<Shelter>,
) -> Result<Json<Value>> {
    let a = auth::actor(&state, &headers).await?;
    a.operator()?;
    validate_shelter(&i)?;
    db::audit(&state.db, &a.id, "shelter_update", &id).await?;
    let r=state.db.collection::<Document>("shelters").update_one(doc!{"_id":id},doc!{"$set":{"name":i.name,"zone":i.zone,"capacity":i.capacity,"occupied":i.occupied,"updated":db::now()}}).await?;
    if r.matched_count == 0 {
        return Err(error::Error(
            StatusCode::NOT_FOUND,
            "Albergue no encontrado",
        ));
    }
    Ok(Json(json!({"ok":true})))
}
