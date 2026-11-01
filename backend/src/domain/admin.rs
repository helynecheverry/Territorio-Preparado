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
        .route("/users", get(users))
        .route("/users/{id}", patch(update_user))
        .route("/audit", get(audit))
}
async fn users(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(p): Query<Page>,
) -> Result<Json<Value>> {
    auth::actor(&state, &headers).await?.admin()?;
    let mut docs = documents(&state, "users", doc! {}, p.page).await?;
    for d in &mut docs {
        d.remove("password");
        d.remove("google_sub");
    }
    Ok(Json(json!({"items":docs})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UserUpdate {
    role: String,
    disabled: bool,
}
async fn update_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(i): Json<UserUpdate>,
) -> Result<Json<Value>> {
    let a = auth::actor(&state, &headers).await?;
    a.admin()?;
    if a.id == id {
        return Err(error::bad("No puedes modificar tus propios permisos"));
    }
    if !["citizen", "operator", "admin"].contains(&i.role.as_str()) {
        return Err(error::bad("Rol inválido"));
    }
    db::audit(&state.db, &a.id, "user_permissions_update", &id).await?;
    let r = state
        .db
        .collection::<Document>("users")
        .update_one(
            doc! {"_id":&id},
            doc! {"$set":{"role":i.role,"disabled":i.disabled},"$inc":{"auth_epoch":1_i64}},
        )
        .await?;
    if r.matched_count == 0 {
        return Err(error::Error(StatusCode::NOT_FOUND, "Usuario no encontrado"));
    }
    state
        .db
        .collection::<Document>("sessions")
        .delete_many(doc! {"user":id})
        .await?;
    Ok(Json(json!({"ok":true})))
}
async fn audit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(p): Query<Page>,
) -> Result<Json<Value>> {
    auth::actor(&state, &headers).await?.admin()?;
    Ok(Json(
        json!({"items":documents(&state,"audit",doc!{},p.page).await?}),
    ))
}
