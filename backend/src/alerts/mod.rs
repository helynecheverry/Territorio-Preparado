//! Motor de alertas: a quién avisar de un evento, por qué canal y una sola vez.
//!
//! # Lo que hace y lo que no
//!
//! Este módulo **decide** y **encola**. No envía: el transporte (Web Push y SMS)
//! es la fase siguiente y se conecta consumiendo la colección `notifications`.
//! Separarlo así permite que la decisión sea verificable antes de que exista el
//! canal, en lugar de descubrir un error de criterio cuando ya se mandó.
//!
//! # El nivel es de la persona, no del sismo
//!
//! Un M7 es `evacuate` a 50 km, `prepare` a 400 km e `info` a 900 km. Así que no
//! se clasifica el evento y se reparte: se recorre a los afiliados dentro del
//! alcance del evento y se clasifica a cada uno por su distancia.
//!
//! # Una sola vez, salvo que empeore
//!
//! Cada aviso tiene un `_id` determinista `canónico:afiliación:canal`, de forma
//! que el mismo evento no puede encolar dos veces para la misma persona y canal.
//! La actualización se filtra por `level_rank` menor que el nuevo, así que:
//!
//! - Primera vez: no existe, se inserta.
//! - Revisión que **sube** de nivel: el filtro coincide, se actualiza y vuelve a
//!   quedar pendiente de envío.
//! - Revisión que baja o repite: el filtro no coincide, el `upsert` choca con la
//!   clave única y se cuenta como omitido.
//!
//! Esa última rama es deliberada. El sismo de Panamá que ya estaba en los datos
//! del proyecto se publicó como M8.0 y se revisó a M7.7; mandar un segundo
//! mensaje para decir que fue algo menos fuerte solo gastaría la confianza de la
//! gente y, si fuera por SMS, dinero.
//!
//! # Solo avisa la fuente con más autoridad
//!
//! Un mismo sismo llega por varios catálogos. Si USGS y el SGC publicaran cada
//! uno su aviso, cada persona recibiría dos. Por eso, antes de repartir, se busca
//! la observación de mayor `authority_rank` del grupo canónico: si la que está
//! pendiente no es esa, se descarta sin avisar. Cuando el SGC publica después con
//! otra magnitud, su propia observación queda pendiente y reparte ella, y como el
//! nivel puede subir, la regla anterior decide si se vuelve a avisar.
//!
//! # Escala
//!
//! Un sismo fuerte en una ciudad densa puede alcanzar a cientos de miles de
//! afiliados, así que:
//!
//! - Los afiliados se recuperan con `$geoNear` sobre el índice `2dsphere`,
//!   acotado al alcance del evento. Devuelve **del más cercano al más lejano**, y
//!   ese orden se conserva en la cola: quien está más cerca del epicentro tiene
//!   menos tiempo y más necesidad.
//! - El cursor se consume por tramos, no se acumula en memoria.
//! - Las escrituras de cada tramo se emiten con concurrencia acotada. De forma
//!   secuencial, un reparto de 300.000 avisos tardaría minutos; es la diferencia
//!   entre avisar y llegar tarde.
//! - El SMS tiene presupuesto por evento. Al agotarse, el resto se marca como
//!   omitido por presupuesto en lugar de gastarse en silencio, y como el recorrido
//!   va del más cercano al más lejano, el presupuesto se gasta en quien más lo
//!   necesita.

pub mod entitlements;

use crate::{
    AppState, auth, db,
    error::{self, Result},
    hazards::{self, Level},
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, patch},
};
use entitlements::Plan;
use futures_util::{StreamExt, TryStreamExt};
use mongodb::bson::{Document, doc};
use serde::Deserialize;
use serde_json::{Value, json};

/// Canal por el que se entrega un aviso.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
    /// Web Push. Sin coste por mensaje.
    Push,
    /// SMS. Con coste por mensaje y por eso con presupuesto.
    Sms,
}
impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Sms => "sms",
        }
    }
    /// Nombre del campo que la afiliación usa para pedir este canal.
    fn wanted_field(self) -> &'static str {
        self.as_str()
    }
}

/// Orden numérico de un nivel, para poder comparar en la base de datos.
fn rank(level: Level) -> i32 {
    match level {
        Level::None => 0,
        Level::Info => 1,
        Level::Prepare => 2,
        Level::Evacuate => 3,
    }
}

/// Un aviso decidido, antes de escribirse.
struct Intent {
    id: String,
    subscription_id: String,
    owner: String,
    territory_id: String,
    channel: Channel,
    level: Level,
    distance_km: f64,
}

#[derive(Default, Debug)]
pub struct Pass {
    pub events: u64,
    pub enqueued: u64,
    /// Avisos que ya existían y se elevaron porque la revisión subió de nivel.
    pub raised: u64,
    /// Avisos que no se repiten porque ya se mandó ese nivel o uno mayor.
    pub duplicates: u64,
    /// SMS no encolados por haberse agotado el presupuesto del evento.
    pub over_budget: u64,
    /// Eventos descartados porque otra fuente del mismo sismo tiene más autoridad.
    pub superseded: u64,
}

/// Observación con más autoridad del grupo canónico.
///
/// A igual rango gana la publicada más tarde, que es la revisión más reciente de
/// esa misma fuente.
async fn authoritative(db: &mongodb::Database, canonical_id: &str) -> Result<Option<Document>> {
    Ok(db
        .collection::<Document>("events")
        .find_one(doc! {"canonical_id": canonical_id})
        .sort(doc! {"authority_rank": -1, "updated": -1})
        .await?)
}

/// Escribe un tramo de avisos con concurrencia acotada.
///
/// Devuelve `(encolados, elevados, duplicados)`. Un choque de clave única no es
/// un error: significa que esa persona ya tiene ese nivel o uno mayor para este
/// sismo, que es exactamente lo que la clave está ahí para garantizar.
async fn flush(
    state: &AppState,
    event: &Document,
    intents: Vec<Intent>,
) -> Result<(u64, u64, u64)> {
    let collection = state.db.collection::<Document>("notifications");
    let canonical = event.get_str("canonical_id").unwrap_or_default().to_owned();
    let event_id = event.get_str("_id").unwrap_or_default().to_owned();
    let results: Vec<std::result::Result<bool, mongodb::error::Error>> =
        futures_util::stream::iter(intents.into_iter().map(|i| {
            let collection = collection.clone();
            let canonical = canonical.clone();
            let event_id = event_id.clone();
            async move {
                let level_rank = rank(i.level);
                let outcome = collection
                    .update_one(
                        doc! {"_id": &i.id, "level_rank": {"$lt": level_rank}},
                        doc! {
                            "$set": {
                                "canonical_id": &canonical,
                                "event_id": &event_id,
                                "subscription_id": &i.subscription_id,
                                "owner": &i.owner,
                                "territory_id": &i.territory_id,
                                "channel": i.channel.as_str(),
                                "level": i.level.as_str(),
                                // Viaja con el aviso para que el reparto pueda
                                // decirle al cliente qué lección abrir sin red.
                                "hazard_type": event.get_str("hazard_type").unwrap_or("earthquake"),
                                "level_rank": level_rank,
                                // Se conserva para ordenar la cola: el reparto
                                // atiende primero a quien está más cerca.
                                "distance_km": (i.distance_km * 10.0).round() / 10.0,
                                // Una elevación vuelve a dejarlo pendiente y
                                // reinicia los intentos: es un mensaje nuevo.
                                "status": "pending",
                                "attempts": 0i32,
                                // El reparto reclama por estos dos campos, así
                                // que se inicializan aquí: una consulta con
                                // `$exists` no usaría el índice.
                                "next_attempt": db::now(),
                                "lease": db::now(),
                                "updated": db::now(),
                            },
                            "$setOnInsert": {"created": db::now()},
                        },
                    )
                    .upsert(true)
                    .await?;
                // `matched_count` distingue la elevación de la primera vez.
                Ok(outcome.matched_count > 0)
            }
        }))
        .buffer_unordered(state.config.alert_concurrency)
        .collect()
        .await;

    let mut enqueued = 0;
    let mut raised = 0;
    let mut duplicates = 0;
    for r in results {
        match r {
            Ok(true) => raised += 1,
            Ok(false) => enqueued += 1,
            Err(e) if e.to_string().contains("E11000") => duplicates += 1,
            Err(e) => return Err(error::Error::from(e)),
        }
    }
    Ok((enqueued, raised, duplicates))
}

/// Reparte un evento entre los afiliados a su alcance.
async fn alert_event(state: &AppState, event: &Document) -> Result<Pass> {
    let mut pass = Pass::default();
    let canonical = event.get_str("canonical_id").unwrap_or_default();

    // Solo reparte la fuente con más autoridad del grupo. Si no es esta, se
    // limpia su bandera y se sale sin avisar a nadie.
    if let Some(best) = authoritative(&state.db, canonical).await?
        && best.get_str("_id") != event.get_str("_id")
    {
        pass.superseded = 1;
        return Ok(pass);
    }

    let Ok(magnitude) = event.get_f64("magnitude") else {
        // Una amenaza sin magnitud —una inundación— no se clasifica con las
        // bandas sísmicas. Queda para cuando exista su propia fuente y su propio
        // criterio de nivel.
        tracing::debug!(event = ?event.get_str("_id"), "Evento sin magnitud: no se reparte");
        return Ok(pass);
    };
    let Some(epicentre) = event
        .get_document("point")
        .ok()
        .and_then(|p| p.get_array("coordinates").ok())
        .and_then(|c| Some([c.first()?.as_f64()?, c.get(1)?.as_f64()?]))
    else {
        return Ok(pass);
    };
    let reach_km = hazards::reach_km(magnitude);
    if reach_km <= 0.0 {
        return Ok(pass);
    }

    // `$geoNear` filtra por alcance, calcula la distancia y devuelve del más
    // cercano al más lejano en una sola pasada sobre el índice.
    let mut cursor = state
        .db
        .collection::<Document>("subscriptions")
        .aggregate(vec![doc! {"$geoNear": {
            "near": {"type": "Point", "coordinates": [epicentre[0], epicentre[1]]},
            "distanceField": "distance_m",
            "maxDistance": reach_km * 1000.0,
            "spherical": true,
        }}])
        .await?;

    let mut sms_budget = state.config.alert_sms_budget_per_event;
    let mut remaining = state.config.alert_max_per_event;
    let mut plans: std::collections::HashMap<String, Plan> = std::collections::HashMap::new();
    let mut batch: Vec<Intent> = Vec::with_capacity(state.config.alert_batch);

    while let Some(sub) = cursor.try_next().await? {
        if remaining == 0 {
            tracing::warn!(
                event = ?event.get_str("_id"),
                limit = state.config.alert_max_per_event,
                "Se alcanzó el máximo de avisos por evento; el resto no se encoló"
            );
            break;
        }
        let distance_km = sub.get_f64("distance_m").unwrap_or(f64::MAX) / 1000.0;
        let level = hazards::classify(magnitude, distance_km);
        if level == Level::None {
            continue;
        }
        // El alcance es el máximo de todos los niveles, así que dentro de él hay
        // gente a la que este nivel no le llega al mínimo que pidió.
        let wanted =
            Level::parse(sub.get_str("min_level").unwrap_or("info")).unwrap_or(Level::Info);
        if level < wanted {
            continue;
        }
        let (Ok(subscription_id), Ok(owner)) = (sub.get_str("_id"), sub.get_str("owner")) else {
            continue;
        };
        let territory_id = sub.get_str("territory_id").unwrap_or_default();
        let asked = sub.get_document("channels").ok();

        for channel in [Channel::Push, Channel::Sms] {
            // Lo que la persona pidió.
            if !asked.is_some_and(|c| c.get_bool(channel.wanted_field()).unwrap_or(false)) {
                continue;
            }
            // El presupuesto se comprueba antes del plan para que agotar el
            // presupuesto no cueste además una consulta por afiliado.
            if channel == Channel::Sms {
                if sms_budget == 0 {
                    pass.over_budget += 1;
                    continue;
                }
                let plan = match plans.get(owner) {
                    Some(p) => *p,
                    None => {
                        let p = entitlements::plan_for(&state.db, owner).await?;
                        plans.insert(owner.to_owned(), p);
                        p
                    }
                };
                if !plan.channels(level).contains(&channel) {
                    continue;
                }
                sms_budget -= 1;
            } else if !Plan::Free.channels(level).contains(&channel) {
                // Web Push no depende del plan, pero se respeta el criterio de
                // nivel en un único sitio en lugar de duplicarlo aquí.
                continue;
            }
            batch.push(Intent {
                id: format!("{canonical}:{subscription_id}:{}", channel.as_str()),
                subscription_id: subscription_id.to_owned(),
                owner: owner.to_owned(),
                territory_id: territory_id.to_owned(),
                channel,
                level,
                distance_km,
            });
            remaining -= 1;
        }

        if batch.len() >= state.config.alert_batch {
            let (e, r, d) = flush(state, event, std::mem::take(&mut batch)).await?;
            pass.enqueued += e;
            pass.raised += r;
            pass.duplicates += d;
            batch = Vec::with_capacity(state.config.alert_batch);
        }
    }
    if !batch.is_empty() {
        let (e, r, d) = flush(state, event, batch).await?;
        pass.enqueued += e;
        pass.raised += r;
        pass.duplicates += d;
    }
    Ok(pass)
}

/// Procesa los eventos pendientes de reparto.
///
/// La bandera `alert_pending` la pone la ingesta al insertar o revisar un evento,
/// y se limpia aquí. Es un campo indexado en lugar de una comparación entre
/// fechas para que la consulta no tenga que recorrer el catálogo entero.
pub async fn run_once(state: &AppState) -> Result<Pass> {
    let collection = state.db.collection::<Document>("events");
    let pending: Vec<Document> = collection
        .find(doc! {"alert_pending": true})
        // Del más reciente al más antiguo: si hay cola, lo que acaba de pasar
        // importa más que lo de hace horas.
        .sort(doc! {"origin_utc": -1})
        .limit(state.config.alert_events_per_pass as i64)
        .await?
        .try_collect()
        .await?;

    let mut total = Pass::default();
    for event in pending {
        let id = event.get_str("_id").unwrap_or_default().to_owned();
        let pass = alert_event(state, &event).await?;
        // La bandera se limpia después de repartir. Si el proceso muere a mitad,
        // el evento sigue pendiente y se vuelve a repartir; la clave única evita
        // que alguien reciba el aviso dos veces.
        collection
            .update_one(
                doc! {"_id": &id},
                doc! {"$set": {"alert_pending": false, "alerted_at": db::now()}},
            )
            .await?;
        if pass.enqueued > 0 || pass.raised > 0 {
            // Se audita el reparto, no cada aviso: un millón de registros de
            // auditoría por sismo no se puede revisar y oculta lo que importa.
            db::audit(&state.db, "alerts-engine", "alert_dispatch", &id).await?;
            tracing::info!(
                event = %id,
                enqueued = pass.enqueued,
                raised = pass.raised,
                duplicates = pass.duplicates,
                over_budget = pass.over_budget,
                "Evento repartido"
            );
        }
        if pass.over_budget > 0 {
            tracing::warn!(
                event = %id,
                skipped = pass.over_budget,
                budget = state.config.alert_sms_budget_per_event,
                "Presupuesto de SMS agotado para este evento"
            );
        }
        total.events += 1;
        total.enqueued += pass.enqueued;
        total.raised += pass.raised;
        total.duplicates += pass.duplicates;
        total.over_budget += pass.over_budget;
        total.superseded += pass.superseded;
    }
    Ok(total)
}

/// Bucle del motor. Vive junto a la ingesta, en los modos `serve` y `worker`.
pub async fn worker(state: AppState) {
    let period = std::time::Duration::from_secs(state.config.alert_poll_seconds);
    tracing::info!(
        seconds = state.config.alert_poll_seconds,
        sms_budget = state.config.alert_sms_budget_per_event,
        "Motor de alertas iniciado; encola avisos, el envío es de la fase siguiente"
    );
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if let Err(e) = run_once(&state).await {
            tracing::error!(error = ?e.1, "Fallo en el reparto de alertas");
        }
    }
}

// --- API -------------------------------------------------------------------

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/alerts/mine", get(mine))
        .route("/alerts/queue", get(queue))
        .route("/alerts/entitlements/{owner}", patch(set_entitlement))
}

fn summary(d: &Document) -> Value {
    json!({
        "id": d.get_str("_id").unwrap_or_default(),
        "event_id": d.get_str("event_id").ok(),
        "canonical_id": d.get_str("canonical_id").ok(),
        "subscription_id": d.get_str("subscription_id").ok(),
        "channel": d.get_str("channel").ok(),
        "level": d.get_str("level").ok(),
        "distance_km": d.get_f64("distance_km").ok(),
        "status": d.get_str("status").unwrap_or("pending"),
        "attempts": d.get_i32("attempts").ok(),
        "created": d.get_datetime("created").ok().map(ToString::to_string),
        "updated": d.get_datetime("updated").ok().map(ToString::to_string),
    })
}

/// Avisos dirigidos a la propia cuenta.
///
/// Permite a cada persona ver qué se decidió para ella y por qué canal, antes y
/// después de que exista el envío.
async fn mine(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    let items: Vec<Document> =
        s.db.collection::<Document>("notifications")
            .find(doc! {"owner": &a.id})
            .sort(doc! {"level_rank": -1, "created": -1})
            .limit(50)
            .await?
            .try_collect()
            .await?;
    Ok(Json(json!({
        "items": items.iter().map(summary).collect::<Vec<_>>(),
        "plan": entitlements::plan_for(&s.db, &a.id).await?.as_str(),
        "delivery": "pending_implementation",
        "meaning": "Avisos decididos por el motor. El envío por Web Push y SMS es \
                    la fase siguiente: aparecer aquí no significa que se haya entregado.",
    })))
}

#[derive(Deserialize)]
struct QueueFilter {
    status: Option<String>,
    page: Option<u64>,
}

/// Estado de la cola de avisos, para operación.
async fn queue(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<QueueFilter>,
) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    a.operator()?;
    let mut filter = doc! {};
    if let Some(status) = &q.status {
        if !["pending", "sent", "failed", "skipped"].contains(&status.as_str()) {
            return Err(error::bad(
                "El estado debe ser pending, sent, failed o skipped",
            ));
        }
        filter.insert("status", status);
    }
    let collection = s.db.collection::<Document>("notifications");
    let page = q.page.unwrap_or(0).min(1000);
    let items: Vec<Document> = collection
        .find(filter.clone())
        // El mismo orden en el que se debe repartir: lo más grave primero y,
        // dentro de eso, quien está más cerca del epicentro.
        .sort(doc! {"level_rank": -1, "distance_km": 1})
        .skip(page * 50)
        .limit(50)
        .await?
        .try_collect()
        .await?;
    Ok(Json(json!({
        "items": items.iter().map(summary).collect::<Vec<_>>(),
        "total": collection.count_documents(filter).await?,
        "pending": collection.count_documents(doc! {"status": "pending"}).await?,
        "page": page,
        "page_size": 50,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantInput {
    plan: String,
    /// Días de vigencia. Cero deja la cuenta en el plan gratuito.
    days: i64,
    #[serde(default)]
    payment_reference: Option<String>,
}

/// Concede o retira un plan.
///
/// Es administrativo y provisional: cuando se integre la pasarela de pago, su
/// webhook escribirá en la misma colección y esta ruta quedará solo para
/// correcciones y soporte.
async fn set_entitlement(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(owner): Path<String>,
    Json(i): Json<GrantInput>,
) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    a.admin()?;
    let plan = Plan::parse(&i.plan).ok_or(error::bad("El plan debe ser free o paid"))?;
    if !(0..=3650).contains(&i.days) {
        return Err(error::bad("La vigencia debe estar entre 0 y 3650 días"));
    }
    if s.db
        .collection::<Document>("users")
        .find_one(doc! {"_id": &owner})
        .await?
        .is_none()
    {
        return Err(error::Error(StatusCode::NOT_FOUND, "Cuenta no encontrada"));
    }
    let reference = i
        .payment_reference
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.len() <= 120);
    let granted = entitlements::grant(&s.db, &a.id, &owner, plan, i.days, reference).await?;
    Ok(Json(json!({
        "owner": owner,
        "plan": granted.get_str("plan").unwrap_or_default(),
        "valid_until": granted.get_datetime("valid_until").ok().map(ToString::to_string),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_order_the_levels_the_same_way_the_enum_does() {
        // La base de datos compara enteros y el código compara el enum; si los
        // dos órdenes se separaran, una revisión a la baja volvería a avisar.
        let levels = [Level::None, Level::Info, Level::Prepare, Level::Evacuate];
        for pair in levels.windows(2) {
            assert!(pair[0] < pair[1]);
            assert!(rank(pair[0]) < rank(pair[1]));
        }
    }

    #[test]
    fn the_notification_id_identifies_person_event_and_channel() {
        let id = format!("{}:{}:{}", "c-1", "sub-7", Channel::Sms.as_str());
        assert_eq!(id, "c-1:sub-7:sms");
        // Cambiar cualquiera de los tres da una clave distinta, que es lo que
        // permite un aviso por canal sin permitir dos del mismo.
        assert_ne!(
            id,
            format!("{}:{}:{}", "c-1", "sub-7", Channel::Push.as_str())
        );
        assert_ne!(
            id,
            format!("{}:{}:{}", "c-2", "sub-7", Channel::Sms.as_str())
        );
        assert_ne!(
            id,
            format!("{}:{}:{}", "c-1", "sub-8", Channel::Sms.as_str())
        );
    }

    #[test]
    fn the_channel_name_matches_the_field_the_subscription_uses() {
        // `wanted_field` lee el documento de la afiliación, que guarda
        // `{push: bool, sms: bool}`. Si los nombres divergieran, el canal
        // quedaría siempre desactivado sin error visible.
        assert_eq!(Channel::Push.wanted_field(), "push");
        assert_eq!(Channel::Sms.wanted_field(), "sms");
    }
}
