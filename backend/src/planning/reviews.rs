use crate::{
    AppState, auth, db,
    error::{self, Result},
    security,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use futures_util::TryStreamExt;
use mongodb::bson::{Document, doc};
use serde::Deserialize;
use serde_json::{Value, json};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewInput {
    status: String,
    notes: String,
    request_key: String,
}
async fn check_plan(s: &AppState, id: &str) -> Result<()> {
    if s.db
        .collection::<Document>("plans")
        .find_one(doc! {"_id":id})
        .await?
        .is_none()
    {
        return Err(error::Error(
            StatusCode::NOT_FOUND,
            "Propuesta no encontrada",
        ));
    }
    Ok(())
}
pub async fn list(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    auth::actor(&s, &h).await?.operator()?;
    check_plan(&s, &id).await?;
    let docs: Vec<Document> =
        s.db.collection::<Document>("plan_reviews")
            .find(doc! {"plan_id":id})
            .sort(doc! {"created":-1,"_id":-1})
            .limit(20)
            .await?
            .try_collect()
            .await?;
    let items:Vec<_>=docs.into_iter().map(|d|json!({"id":d.get_str("_id").unwrap_or_default(),"actor":d.get_str("actor").unwrap_or_default(),"status":d.get_str("status").unwrap_or_default(),"notes":d.get_str("notes").unwrap_or_default(),"created_ms":d.get_datetime("created").map(|v|v.timestamp_millis()).unwrap_or_default()})).collect();
    Ok(Json(
        json!({"items":items,"limit":20,"meaning":"Registro de revisión de la propuesta; no certifica aforo ni autoriza la apertura de un albergue"}),
    ))
}
pub async fn create(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(i): Json<ReviewInput>,
) -> Result<(StatusCode, Json<Value>)> {
    let a = auth::actor(&s, &h).await?;
    a.operator()?;
    let notes = i.notes.trim();
    if !["needs_changes", "reviewed"].contains(&i.status.as_str())
        || !(10..=2000).contains(&notes.len())
        || i.request_key.is_empty()
        || i.request_key.len() > 128
    {
        return Err(error::bad(
            "Revisión inválida: selecciona un estado y escribe entre 10 y 2.000 bytes de observaciones",
        ));
    }
    check_plan(&s, &id).await?;
    let digest = security::hash(&format!("{id}\n{}\n{notes}", i.status));
    let col = s.db.collection::<Document>("plan_reviews");
    let filter = doc! {"actor":&a.id,"request_key":&i.request_key};
    if let Some(d) = col.find_one(filter.clone()).await? {
        return stored(d, &digest);
    }
    security::limit(&s, "planning-review", &a.id, 40, 3600).await?;
    let review_id = uuid::Uuid::new_v4().to_string();
    db::audit(&s.db, &a.id, "planning_review_register", &id).await?;
    let record = doc! {"_id":&review_id,"plan_id":id,"actor":a.id,"request_key":i.request_key,"digest":&digest,"status":i.status,"notes":notes,"created":db::now()};
    match col.insert_one(record).await {
        Ok(_) => Ok((
            StatusCode::CREATED,
            Json(json!({"id":review_id,"recorded":true})),
        )),
        Err(e) if e.to_string().contains("E11000") => {
            let d = col
                .find_one(filter)
                .await?
                .ok_or_else(|| error::internal("missing idempotent review"))?;
            stored(d, &digest)
        }
        Err(e) => Err(e.into()),
    }
}
fn stored(d: Document, digest: &str) -> Result<(StatusCode, Json<Value>)> {
    if d.get_str("digest").ok() != Some(digest) {
        return Err(error::Error(
            StatusCode::CONFLICT,
            "La clave ya corresponde a otra revisión",
        ));
    }
    Ok((
        StatusCode::OK,
        Json(json!({"id":d.get_str("_id").unwrap_or_default(),"recorded":true})),
    ))
}
