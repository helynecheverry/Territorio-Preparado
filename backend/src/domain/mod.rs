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
        .route("/territories", get(territories))
        .route("/sectors", get(sectors))
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
#[derive(Deserialize)]
pub struct SectorQuery {
    territory: String,
    /// `neighborhood` o `commune`.
    kind: String,
}

/// Sectores de un territorio, para que una afiliación se pueda elegir de una
/// lista en lugar de escribir coordenadas a mano.
async fn sectors(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<SectorQuery>,
) -> Result<Json<Value>> {
    auth::actor(&state, &headers).await?;
    if !crate::territory::Territory::valid_id(&q.territory) {
        return Err(error::bad("Identificador de territorio inválido"));
    }
    let kind = match q.kind.as_str() {
        "neighborhood" => crate::sectors::Kind::Neighborhood,
        "commune" => crate::sectors::Kind::Commune,
        _ => {
            return Err(error::bad(
                "El tipo de sector debe ser neighborhood o commune",
            ));
        }
    };
    Ok(Json(json!({
        "items": crate::sectors::list(&state.db, &q.territory, kind).await?,
    })))
}
/// Territorios cargados y su nivel de cobertura de datos.
///
/// Vive aquí y no en `planning` porque el motor de alertas también necesita saber
/// qué capas de amenaza existen antes de recomendar un destino.
async fn territories(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<Value>> {
    auth::actor(&state, &headers).await?;
    let items: Vec<Value> = crate::territory::list(&state.db)
        .await?
        .iter()
        .map(crate::territory::Territory::summary)
        .collect();
    Ok(Json(json!({"items": items})))
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
