pub mod catalog;
pub mod engine;
pub mod geometry;
mod reviews;
use crate::{
    AppState, auth, db,
    error::{self, Result},
    security, territory,
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

/// Estado del planificador. Ya no guarda el inventario: el catálogo vive en
/// MongoDB y se consulta por territorio. Lo único que queda en memoria es el
/// límite de cálculos geométricos simultáneos por proceso.
pub struct Planning {
    slots: std::sync::Arc<tokio::sync::Semaphore>,
}
impl Planning {
    pub fn new() -> Self {
        Self {
            slots: std::sync::Arc::new(tokio::sync::Semaphore::new(2)),
        }
    }
}
impl Default for Planning {
    fn default() -> Self {
        Self::new()
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
    territory: Option<String>,
    commune: Option<String>,
    page: Option<u64>,
    measurable: Option<bool>,
}
/// Territorio sobre el que trabaja la petición.
///
/// Si no se indica y solo hay un territorio cargado, se usa ese: evita que cada
/// consulta tenga que nombrarlo mientras el despliegue cubre una sola ciudad. En
/// cuanto hay más de uno, nombrarlo es obligatorio, porque elegir por omisión
/// entre varias ciudades devolvería espacios de la equivocada en silencio.
async fn resolve_territory(s: &AppState, requested: Option<&str>) -> Result<territory::Territory> {
    if let Some(id) = requested {
        if !territory::Territory::valid_id(id) {
            return Err(error::bad("Identificador de territorio inválido"));
        }
        return territory::get(&s.db, id).await;
    }
    let mut all = territory::list(&s.db).await?;
    match all.len() {
        1 => Ok(all.remove(0)),
        0 => Err(error::Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "No hay territorios cargados; importa un inventario",
        )),
        _ => Err(error::bad(
            "Indica el territorio: hay varios inventarios cargados",
        )),
    }
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
    let t = resolve_territory(&s, q.territory.as_deref()).await?;
    let page = q.page.unwrap_or(0).min(1000);
    let (items, total) = catalog::search(
        &s.db,
        catalog::Query {
            territory_id: &t.id,
            needle: &needle,
            commune: q.commune.as_deref(),
            measurable: q.measurable,
            page,
        },
    )
    .await?;
    Ok(Json(json!({
        "items": items,
        "total": total,
        "page": page,
        "page_size": 40,
        "territory": t.summary(),
        "catalogue_total": t.spaces_total,
        "measurable_total": t.measurable_total,
    })))
}
async fn space(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    auth::actor(&s, &h).await?;
    let item = catalog::get(&s.db, &id).await?;
    let t = territory::get(&s.db, &item.territory_id).await?;
    Ok(Json(json!({
        "summary": catalog::summary(&item),
        "geometry": item.shape,
        "properties": item.properties,
        "territory": t.summary(),
        "source_sha256": t.source_sha256,
    })))
}
async fn compute(s: &AppState, i: PlanInput) -> Result<Value> {
    let item = catalog::get(&s.db, &i.space_id).await?;
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
    // El hash se lee antes de tomar la plaza de cálculo: ocupar una de las dos
    // plazas del proceso durante una consulta a la base de datos reduciría sin
    // motivo la concurrencia del planificador.
    let hash = territory::get(&s.db, &item.territory_id)
        .await?
        .source_sha256;
    let permit = s.planning.slots.clone().try_acquire_owned().map_err(|_| {
        error::Error(
            StatusCode::TOO_MANY_REQUESTS,
            "El planificador está ocupado; vuelve a intentarlo",
        )
    })?;
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
