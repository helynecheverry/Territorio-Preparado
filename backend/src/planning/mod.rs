mod catalog;
pub mod engine;
mod geometry;
mod reviews;
use crate::{
    AppState, auth, db,
    error::{self, Result},
    security,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use engine::PlanInput;
use futures_util::TryStreamExt;
use mongodb::{
    bson::{Document, doc},
    options::FindOptions,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path as FilePath;

pub struct Planning {
    catalog: catalog::Catalog,
    slots: std::sync::Arc<tokio::sync::Semaphore>,
}
impl Planning {
    pub fn load(frontend_dir: &str) -> anyhow::Result<Self> {
        Ok(Self {
            catalog: catalog::Catalog::load(&FilePath::new(frontend_dir).join("data/spaces.json"))?,
            slots: std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
        })
    }
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/spaces", get(spaces))
        .route("/spaces/{id}", get(space))
        .route("/preview", post(preview))
        .route("/plans", get(plans).post(save))
        .route("/plans/{id}", get(plan))
        .route(
            "/plans/{id}/reviews",
            get(reviews::list).post(reviews::create),
        )
}
#[derive(Deserialize)]
struct Search {
    q: Option<String>,
    commune: Option<String>,
    page: Option<usize>,
    measurable: Option<bool>,
}
async fn spaces(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Search>,
) -> Result<Json<Value>> {
    auth::actor(&s, &h).await?;
    let needle = q.q.unwrap_or_default().trim().to_lowercase();
    if needle.len() > 120 {
        return Err(error::bad("Búsqueda demasiado larga"));
    }
    let items: Vec<_> = s
        .planning
        .catalog
        .spaces
        .values()
        .filter(|v| {
            let searchable = format!(
                "{} {} {}",
                v.id,
                v.properties["name"].as_str().unwrap_or_default(),
                v.properties["neighborhood"].as_str().unwrap_or_default()
            )
            .to_lowercase();
            searchable.contains(&needle)
                && q.commune
                    .as_ref()
                    .is_none_or(|c| v.properties["commune"].as_str() == Some(c))
                && q.measurable.is_none_or(|m| v.measurable == m)
        })
        .collect();
    let total = items.len();
    let page = q.page.unwrap_or(0).min(1000);
    let items: Vec<_> = items
        .into_iter()
        .skip(page * 40)
        .take(40)
        .map(|v| s.planning.catalog.summary(v))
        .collect();
    Ok(Json(
        json!({"items":items,"total":total,"page":page,"page_size":40,"catalogue_total":s.planning.catalog.spaces.len(),"measurable_total":s.planning.catalog.spaces.values().filter(|v|v.measurable).count()}),
    ))
}
async fn space(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    auth::actor(&s, &h).await?;
    let item = s
        .planning
        .catalog
        .spaces
        .get(&id)
        .ok_or(error::Error(StatusCode::NOT_FOUND, "Espacio no encontrado"))?;
    Ok(Json(
        json!({"summary":s.planning.catalog.summary(item),"geometry":item.shape,"properties":item.properties,"source_sha256":s.planning.catalog.sha256}),
    ))
}
async fn compute(s: &AppState, i: PlanInput) -> Result<Value> {
    let item = s
        .planning
        .catalog
        .spaces
        .get(&i.space_id)
        .ok_or(error::Error(StatusCode::NOT_FOUND, "Espacio no encontrado"))?
        .clone();
    if !item.measurable {
        return Err(error::bad(
            "Este espacio no tiene una huella poligonal válida para calcular capacidad",
        ));
    }
    if let Err(e) = engine::validate(&i) {
        tracing::debug!(error=%e,"invalid planning parameters");
        return Err(error::bad(
            "Parámetros inválidos: revisa duración, recursos, reserva o confirmación de huella cubierta",
        ));
    }
    let permit = s.planning.slots.clone().try_acquire_owned().map_err(|_| {
        error::Error(
            StatusCode::TOO_MANY_REQUESTS,
            "El planificador está ocupado; vuelve a intentarlo",
        )
    })?;
    let hash = s.planning.catalog.sha256.clone();
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        engine::calculate(&item, &i, &hash)
    })
    .await
    .map_err(error::internal)?;
    result.map_err(|e|{tracing::warn!(error=%e,"planning calculation rejected");error::bad("La geometría no permite esta propuesta: revisa exclusiones, cierre de anillos y tamaño del espacio")})
}
async fn preview(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(i): Json<PlanInput>,
) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    security::limit(&s, "planning-preview", &a.id, 15, 60).await?;
    Ok(Json(compute(&s, i).await?))
}
async fn save(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(i): Json<PlanInput>,
) -> Result<(StatusCode, Json<Value>)> {
    let a = auth::actor(&s, &h).await?;
    a.operator()?;
    let key = i
        .request_key
        .clone()
        .filter(|v| !v.is_empty() && v.len() <= 128)
        .ok_or(error::bad(
            "Se requiere una clave de idempotencia de hasta 128 caracteres",
        ))?;
    let mut canonical = i.clone();
    canonical.request_key = None;
    let hash = security::hash(&serde_json::to_string(&canonical).map_err(error::internal)?);
    let col = s.db.collection::<Document>("plans");
    if let Some(d) = col
        .find_one(doc! {"owner":&a.id,"request_key":&key})
        .await?
    {
        return stored(d, &hash, StatusCode::OK);
    }
    security::limit(&s, "planning-save", &a.id, 40, 3600).await?;
    let result = compute(&s, i.clone()).await?;
    let id = uuid::Uuid::new_v4().to_string();
    let result_bson = mongodb::bson::to_bson(&result).map_err(error::internal)?;
    let request = mongodb::bson::to_bson(&i).map_err(error::internal)?;
    db::audit(&s.db, &a.id, "planning_proposal_save", &id).await?;
    let record = doc! {"_id":&id,"owner":&a.id,"created":db::now(),"request_key":&key,"input_hash":&hash,"request":request,"result":result_bson};
    match col.insert_one(record).await {
        Ok(_) => Ok((StatusCode::CREATED, Json(json!({"id":id,"plan":result})))),
        Err(e) if e.to_string().contains("E11000") => {
            let d = col
                .find_one(doc! {"owner":&a.id,"request_key":&key})
                .await?
                .ok_or_else(|| error::internal("missing idempotent proposal"))?;
            stored(d, &hash, StatusCode::OK)
        }
        Err(e) => Err(e.into()),
    }
}
fn stored(d: Document, hash: &str, status: StatusCode) -> Result<(StatusCode, Json<Value>)> {
    if d.get_str("input_hash").ok() != Some(hash) {
        return Err(error::Error(
            StatusCode::CONFLICT,
            "Esta clave corresponde a una propuesta con parámetros diferentes",
        ));
    }
    Ok((
        status,
        Json(json!({"id":d.get_str("_id").unwrap_or_default(),"plan":d.get("result")})),
    ))
}
async fn plans(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<crate::domain::Page>,
) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    a.operator()?;
    let options=FindOptions::builder().sort(doc!{"created":-1,"_id":-1}).skip(q.page.unwrap_or(0).min(10000)*20).limit(20).projection(doc!{"request_key":0,"input_hash":0,"request":0,"result.layout":0,"result.space.geometry":0}).build();
    let docs: Vec<Document> =
        s.db.collection::<Document>("plans")
            .find(doc! {})
            .with_options(options)
            .await?
            .try_collect()
            .await?;
    Ok(Json(json!({"items":docs,"page_size":20})))
}
async fn plan(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    a.operator()?;
    let d =
        s.db.collection::<Document>("plans")
            .find_one(doc! {"_id":id})
            .await?
            .ok_or(error::Error(
                StatusCode::NOT_FOUND,
                "Propuesta no encontrada",
            ))?;
    Ok(Json(
        json!({"id":d.get_str("_id").unwrap_or_default(),"plan":d.get("result"),"request":d.get("request")}),
    ))
}
