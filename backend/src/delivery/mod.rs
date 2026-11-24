//! Entrega de los avisos que el motor de alertas dejó en cola.
//!
//! # Separación de responsabilidades
//!
//! El motor de alertas decide **a quién** avisar y **con qué nivel**; este módulo
//! solo se ocupa de que el mensaje llegue. Esa frontera es la que permitió
//! verificar el criterio antes de que existiera el canal, y la que permite ahora
//! cambiar de proveedor de SMS sin volver a razonar sobre umbrales.
//!
//! # Orden de reparto
//!
//! La cola se consume por `level_rank` descendente y `distance_km` ascendente: lo
//! más grave primero y, dentro de eso, quien está más cerca del epicentro. No es
//! una preferencia estética. Si un sismo alcanza a cientos de miles de personas,
//! el reparto tarda, y quien está a dos kilómetros del epicentro tiene menos
//! tiempo que quien está a doscientos.
//!
//! # Reintentos
//!
//! Cada resultado se traduce a una de cuatro decisiones, y la distinción importa:
//!
//! - `Sent`: entregado al servicio de push o al proveedor. No significa que la
//!   persona lo haya leído; ninguno de los dos canales lo garantiza.
//! - `Retry`: fallo de red, 429 o 5xx. Se reintenta con espera creciente.
//! - `Permanent`: el mensaje no va a entrar nunca así —cuerpo rechazado, firma
//!   inválida, credenciales mal— y reintentarlo solo gasta intentos.
//! - `Gone`: la suscripción del navegador ya no existe. El dispositivo se borra,
//!   porque un registro muerto consume un envío en cada emergencia.
//!
//! El arrendamiento es por lotes y no de uno en uno como en `ai::worker`. Allí
//! cada trabajo tarda segundos de CPU y tomarlos uno a uno es correcto; aquí hay
//! que mover cientos de miles de mensajes y un viaje a la base de datos por
//! mensaje para reclamarlo sería el cuello de botella.

pub mod message;
pub mod sms;
pub mod webpush;

use crate::{
    AppState, auth, db,
    error::{self, Result},
    hazards::Level,
    security, territory,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get},
};
use futures_util::TryStreamExt;
use mongodb::bson::{Document, doc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;

/// Resultado de un intento de entrega.
#[derive(Debug)]
pub enum Outcome {
    Sent,
    Retry(String),
    Permanent(String),
    /// Solo Web Push: la suscripción del navegador fue cancelada.
    Gone,
}

/// Cliente HTTP compartido por los transportes, con esperas acotadas.
///
/// Un envío colgado retendría su plaza de concurrencia y frenaría el reparto
/// completo justo cuando hay prisa, así que se corta y se reintenta.
pub(crate) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(5))
        .user_agent("territorio-preparado/0.1")
        .build()
        .unwrap_or_default()
}

/// Extremo aceptable para un transporte.
///
/// Producción exige https: tanto el aviso cifrado como el número de teléfono
/// viajan por ahí. En desarrollo se admite http en loopback, igual que ya hace
/// `config.rs` con `TP_ORIGIN`, para poder probar el canal contra un servidor
/// local sin montar certificados.
pub(crate) fn acceptable_endpoint(url: &str, production: bool) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    !production
        && (url.starts_with("http://127.0.0.1:")
            || url.starts_with("http://localhost:")
            || url == "http://127.0.0.1"
            || url == "http://localhost")
}

/// Transportes activos según la configuración.
pub struct Transports {
    pub push: Option<webpush::WebPush>,
    pub sms: Option<sms::Sms>,
}
impl Transports {
    pub fn from_config(config: &crate::config::Config) -> Self {
        let push = webpush::WebPush::from_config(config);
        if push.is_none() {
            tracing::warn!(
                "Web Push desactivado: define TP_VAPID_PRIVATE_KEY y TP_VAPID_CONTACT. \
                 Sin él no se entrega ningún aviso al plan gratuito"
            );
        }
        let sms = sms::Sms::from_config(config);
        if sms.is_none() {
            tracing::info!(
                "SMS desactivado: define TP_SMS_URL con tu proveedor. \
                 Los avisos de evacuación del plan de pago quedarán sin entregar"
            );
        }
        Self { push, sms }
    }
    pub fn names(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.push.is_some() {
            out.push("push");
        }
        if self.sms.is_some() {
            out.push("sms");
        }
        out
    }
}

#[derive(Default, Debug)]
pub struct Pass {
    pub claimed: u64,
    pub sent: u64,
    pub retried: u64,
    pub failed: u64,
    /// Dispositivos borrados porque el navegador canceló la suscripción.
    pub devices_removed: u64,
}

/// Espera creciente antes del siguiente intento.
///
/// Se duplica con cada intento desde la base configurada y se limita a una hora:
/// un aviso de hace más de una hora ya no sirve de mucho, y el tope evita que la
/// cola quede con reintentos agendados para mañana.
fn backoff_seconds(attempts: i32, base: u64) -> i64 {
    let factor = 1u64 << attempts.clamp(0, 10) as u32;
    (base.saturating_mul(factor)).min(3600) as i64
}

/// Reclama un lote de avisos pendientes.
///
/// Son tres viajes a la base de datos por lote, no uno por mensaje: se buscan
/// candidatos ordenados por prioridad, se marcan todos con el mismo testigo de
/// arrendamiento y se recuperan los que efectivamente se ganaron. El testigo es
/// lo que hace que dos procesos de reparto no manden el mismo aviso dos veces.
async fn claim(state: &AppState, token: &str) -> Result<Vec<Document>> {
    let collection = state.db.collection::<Document>("notifications");
    let ready = doc! {
        "status": {"$in": ["pending", "sending"]},
        "next_attempt": {"$lte": db::now()},
        "lease": {"$lte": db::now()},
        "attempts": {"$lt": state.config.delivery_max_attempts},
    };
    let candidates: Vec<Document> = collection
        .find(ready.clone())
        .sort(doc! {"level_rank": -1, "distance_km": 1})
        .limit(state.config.delivery_batch as i64)
        .projection(doc! {"_id": 1})
        .await?
        .try_collect()
        .await?;
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<&str> = candidates
        .iter()
        .filter_map(|d| d.get_str("_id").ok())
        .collect();
    let mut filter = ready;
    filter.insert("_id", doc! {"$in": &ids});
    collection
        .update_many(
            filter,
            doc! {
                "$set": {
                    "status": "sending",
                    "lease": db::after(120),
                    "lease_token": token,
                },
                "$inc": {"attempts": 1},
            },
        )
        .await?;
    Ok(collection
        .find(doc! {"lease_token": token, "status": "sending"})
        .sort(doc! {"level_rank": -1, "distance_km": 1})
        .await?
        .try_collect()
        .await?)
}

/// Dispositivos de push de una cuenta.
async fn devices(state: &AppState, owner: &str) -> Result<Vec<Document>> {
    Ok(state
        .db
        .collection::<Document>("push_devices")
        .find(doc! {"owner": owner})
        .limit(10)
        .await?
        .try_collect()
        .await?)
}

/// Amenaza del aviso, para que el cliente abra la lección que corresponde.
///
/// Se lee del propio aviso y no del evento porque el aviso es lo que siempre
/// está presente en este punto; si faltara, un sismo es el caso que el sistema
/// cubre hoy.
fn event_hazard(notification: &Document) -> crate::hazards::HazardType {
    match notification.get_str("hazard_type").unwrap_or("earthquake") {
        "flood" => crate::hazards::HazardType::Flood,
        "landslide" => crate::hazards::HazardType::Landslide,
        _ => crate::hazards::HazardType::Earthquake,
    }
}

/// Entrega un aviso por su canal.
async fn deliver(
    state: &AppState,
    transports: &Transports,
    notification: &Document,
    rendered: &message::Rendered,
    level: Level,
    subscription: &Document,
) -> (Outcome, Vec<String>) {
    let mut gone = Vec::new();
    let channel = notification.get_str("channel").unwrap_or("push");
    let owner = notification.get_str("owner").unwrap_or_default();

    match channel {
        "push" => {
            let Some(transport) = &transports.push else {
                return (Outcome::Retry("Web Push no configurado".into()), gone);
            };
            let registered = match devices(state, owner).await {
                Ok(d) => d,
                Err(e) => return (Outcome::Retry(format!("base de datos: {}", e.1)), gone),
            };
            if registered.is_empty() {
                // Pedir push sin navegador registrado no es un fallo del sistema:
                // esa persona aún no ha autorizado notificaciones.
                return (
                    Outcome::Permanent("sin dispositivo de push registrado".into()),
                    gone,
                );
            }
            let hazard = event_hazard(notification);
            let payload = message::push_payload(rendered, level, hazard);
            let mut delivered = false;
            let mut last = String::new();
            for d in registered {
                let device = webpush::Device {
                    endpoint: d.get_str("endpoint").unwrap_or_default().to_owned(),
                    p256dh: d.get_str("p256dh").unwrap_or_default().to_owned(),
                    auth: d.get_str("auth").unwrap_or_default().to_owned(),
                };
                match transport.send(&device, payload.clone()).await {
                    Outcome::Sent => delivered = true,
                    Outcome::Gone => {
                        gone.push(d.get_str("_id").unwrap_or_default().to_owned());
                        last = "suscripción cancelada".into();
                    }
                    Outcome::Retry(e) | Outcome::Permanent(e) => last = e,
                }
            }
            // Basta un navegador para considerar entregado: la gente tiene el
            // teléfono y el portátil, y no hace falta que llegue a los dos.
            if delivered {
                (Outcome::Sent, gone)
            } else if gone.is_empty() {
                (Outcome::Retry(last), gone)
            } else {
                (Outcome::Permanent(last), gone)
            }
        }
        "sms" => {
            let Some(transport) = &transports.sms else {
                return (Outcome::Retry("SMS no configurado".into()), gone);
            };
            let Ok(phone) = subscription.get_str("phone") else {
                return (
                    Outcome::Permanent("la afiliación ya no tiene teléfono".into()),
                    gone,
                );
            };
            (transport.send(phone, &rendered.sms).await, gone)
        }
        other => (
            Outcome::Permanent(format!("canal desconocido: {other}")),
            gone,
        ),
    }
}

/// Reparte un lote de la cola.
pub async fn run_once(state: &AppState, transports: &Transports) -> Result<Pass> {
    let token = security::token();
    let batch = claim(state, &token).await?;
    let mut pass = Pass {
        claimed: batch.len() as u64,
        ..Pass::default()
    };
    if batch.is_empty() {
        return Ok(pass);
    }

    // Los documentos compartidos se leen una vez por lote. Un sismo genera miles
    // de avisos del mismo evento y de los mismos pocos territorios.
    let mut events: HashMap<String, Option<Document>> = HashMap::new();
    let mut subscriptions: HashMap<String, Option<Document>> = HashMap::new();
    let mut territories: HashMap<String, Option<territory::Territory>> = HashMap::new();
    let mut destination_cache = HashMap::new();
    let notifications = state.db.collection::<Document>("notifications");

    for n in batch {
        let id = n.get_str("_id").unwrap_or_default().to_owned();
        let attempts = n.get_i32("attempts").unwrap_or(1);
        let level = Level::parse(n.get_str("level").unwrap_or("info")).unwrap_or(Level::Info);

        let event_id = n.get_str("event_id").unwrap_or_default().to_owned();
        let event = match events.entry(event_id.clone()) {
            std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
            std::collections::hash_map::Entry::Vacant(e) => e
                .insert(
                    state
                        .db
                        .collection::<Document>("events")
                        .find_one(doc! {"_id": &event_id})
                        .await?,
                )
                .clone(),
        };
        let subscription_id = n.get_str("subscription_id").unwrap_or_default().to_owned();
        let subscription = match subscriptions.entry(subscription_id.clone()) {
            std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
            std::collections::hash_map::Entry::Vacant(e) => e
                .insert(
                    state
                        .db
                        .collection::<Document>("subscriptions")
                        .find_one(doc! {"_id": &subscription_id})
                        .await?,
                )
                .clone(),
        };

        // Si la persona retiró su afiliación entre la decisión y el envío, el
        // aviso se descarta: ya no hay consentimiento para avisarle.
        let (Some(event), Some(subscription)) = (event, subscription) else {
            notifications
                .update_one(
                    doc! {"_id": &id},
                    doc! {"$set": {
                        "status": "skipped",
                        "last_error": "la afiliación o el evento ya no existen",
                        "updated": db::now(),
                    }, "$unset": {"lease_token": ""}},
                )
                .await?;
            pass.failed += 1;
            continue;
        };

        let territory_id = subscription
            .get_str("territory_id")
            .unwrap_or_default()
            .to_owned();
        let t = match territories.entry(territory_id.clone()) {
            std::collections::hash_map::Entry::Occupied(e) => e.get().clone(),
            std::collections::hash_map::Entry::Vacant(e) => e
                .insert(territory::get(&state.db, &territory_id).await.ok())
                .clone(),
        };
        let Some(t) = t else {
            notifications
                .update_one(
                    doc! {"_id": &id},
                    doc! {"$set": {"status": "skipped", "last_error": "territorio desconocido", "updated": db::now()}},
                )
                .await?;
            pass.failed += 1;
            continue;
        };

        let rendered =
            message::render(state, &n, &event, &subscription, &t, &mut destination_cache).await?;
        let (outcome, gone) = deliver(state, transports, &n, &rendered, level, &subscription).await;

        // Los dispositivos cancelados se borran en cuanto se sabe: conservarlos
        // gastaría un envío por emergencia para siempre.
        if !gone.is_empty() {
            let removed = state
                .db
                .collection::<Document>("push_devices")
                .delete_many(doc! {"_id": {"$in": &gone}})
                .await?;
            pass.devices_removed += removed.deleted_count;
        }

        let update = match outcome {
            Outcome::Sent => {
                pass.sent += 1;
                doc! {"$set": {"status": "sent", "sent_at": db::now(), "updated": db::now()},
                "$unset": {"lease_token": "", "last_error": ""}}
            }
            Outcome::Retry(reason) if attempts < state.config.delivery_max_attempts => {
                pass.retried += 1;
                let wait = backoff_seconds(attempts, state.config.delivery_backoff_seconds);
                doc! {"$set": {
                    "status": "pending",
                    "next_attempt": db::after(wait),
                    "lease": db::now(),
                    "last_error": reason,
                    "updated": db::now(),
                }, "$unset": {"lease_token": ""}}
            }
            Outcome::Retry(reason) => {
                // Agotados los intentos, un fallo temporal deja de ser temporal.
                pass.failed += 1;
                tracing::warn!(notification = %id, reason = %reason, "Aviso agotó los intentos");
                doc! {"$set": {"status": "failed", "last_error": reason, "updated": db::now()},
                "$unset": {"lease_token": ""}}
            }
            Outcome::Permanent(reason) => {
                pass.failed += 1;
                doc! {"$set": {"status": "failed", "last_error": reason, "updated": db::now()},
                "$unset": {"lease_token": ""}}
            }
            Outcome::Gone => {
                pass.failed += 1;
                doc! {"$set": {"status": "failed", "last_error": "suscripción cancelada", "updated": db::now()},
                "$unset": {"lease_token": ""}}
            }
        };
        notifications.update_one(doc! {"_id": &id}, update).await?;
    }
    Ok(pass)
}

/// Bucle de reparto. Vive en los modos `serve` y `worker`.
pub async fn worker(state: AppState) {
    let transports = Transports::from_config(&state.config);
    tracing::info!(
        channels = ?transports.names(),
        seconds = state.config.delivery_poll_seconds,
        "Reparto de avisos iniciado"
    );
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(
        state.config.delivery_poll_seconds,
    ));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        match run_once(&state, &transports).await {
            Ok(pass) if pass.claimed > 0 => tracing::info!(
                claimed = pass.claimed,
                sent = pass.sent,
                retried = pass.retried,
                failed = pass.failed,
                devices_removed = pass.devices_removed,
                "Lote repartido"
            ),
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e.1, "Fallo en el reparto"),
        }
    }
}

// --- API -------------------------------------------------------------------

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/push-public-key", get(public_key))
        .route("/push-devices", get(list_devices).post(register_device))
        .route("/push-devices/{id}", delete(remove_device))
}

/// Clave pública VAPID, que el navegador necesita para suscribirse.
///
/// Es pública por definición —va en la llamada a `pushManager.subscribe`— pero se
/// sirve con sesión como el resto de la API para que el límite por usuario siga
/// aplicando.
async fn public_key(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    auth::actor(&s, &h).await?;
    let transport = webpush::WebPush::from_config(&s.config).ok_or(error::Error(
        StatusCode::SERVICE_UNAVAILABLE,
        "Web Push no está configurado en este despliegue",
    ))?;
    Ok(Json(json!({"public_key": transport.public_key()})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewDevice {
    endpoint: String,
    p256dh: String,
    auth: String,
    #[serde(default)]
    label: Option<String>,
}

/// Registra la suscripción de push de un navegador.
///
/// El `endpoint` es único: volver a suscribirse desde el mismo navegador
/// actualiza el registro en lugar de crear otro, porque si no cada visita
/// añadiría un envío más por emergencia.
async fn register_device(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(i): Json<NewDevice>,
) -> Result<(StatusCode, Json<Value>)> {
    let a = auth::actor(&s, &h).await?;
    security::limit(&s, "push-devices", &a.id, 20, 3600).await?;
    let endpoint = i.endpoint.trim();
    if !acceptable_endpoint(endpoint, s.config.production) || endpoint.len() > 1000 {
        return Err(error::bad("El endpoint de push debe ser una URL https"));
    }
    // Se validan las claves aquí y no al enviar: un registro inválido sería un
    // fallo silencioso que solo se descubriría durante una emergencia.
    if i.p256dh.trim().len() < 80 || i.p256dh.len() > 200 {
        return Err(error::bad("La clave p256dh no tiene el tamaño esperado"));
    }
    if i.auth.trim().len() < 16 || i.auth.len() > 60 {
        return Err(error::bad("El secreto auth no tiene el tamaño esperado"));
    }
    let label = i
        .label
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty() && v.chars().count() <= 60)
        .unwrap_or("navegador")
        .to_owned();

    let owned =
        s.db.collection::<Document>("push_devices")
            .count_documents(doc! {"owner": &a.id})
            .await?;
    let existing =
        s.db.collection::<Document>("push_devices")
            .find_one(doc! {"endpoint": endpoint})
            .await?;
    if owned >= 10 && existing.is_none() {
        return Err(error::bad(
            "Se alcanzó el máximo de navegadores por cuenta; elimina uno antes de añadir otro",
        ));
    }

    let id = existing
        .as_ref()
        .and_then(|d| d.get_str("_id").ok())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    db::audit(&s.db, &a.id, "push_device_register", &id).await?;
    s.db.collection::<Document>("push_devices")
        .update_one(
            doc! {"_id": &id},
            doc! {"$set": {
                "owner": &a.id,
                "endpoint": endpoint,
                "p256dh": i.p256dh.trim(),
                "auth": i.auth.trim(),
                "label": &label,
                "updated": db::now(),
            }, "$setOnInsert": {"created": db::now()}},
        )
        .upsert(true)
        .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id, "label": label}))))
}

async fn list_devices(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    let items: Vec<Document> =
        s.db.collection::<Document>("push_devices")
            .find(doc! {"owner": &a.id})
            .sort(doc! {"created": 1})
            .limit(10)
            .await?
            .try_collect()
            .await?;
    Ok(Json(json!({
        "items": items.iter().map(|d| json!({
            "id": d.get_str("_id").unwrap_or_default(),
            "label": d.get_str("label").unwrap_or_default(),
            // El endpoint identifica al navegador y no se devuelve entero: no
            // aporta nada al titular y es un dato de seguimiento.
            "created": d.get_datetime("created").ok().map(ToString::to_string),
        })).collect::<Vec<_>>(),
    })))
}

async fn remove_device(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    db::audit(&s.db, &a.id, "push_device_delete", &id).await?;
    let r =
        s.db.collection::<Document>("push_devices")
            .delete_one(doc! {"_id": &id, "owner": &a.id})
            .await?;
    if r.deleted_count == 0 {
        return Err(error::Error(
            StatusCode::NOT_FOUND,
            "Dispositivo no encontrado",
        ));
    }
    Ok(Json(json!({"ok": true})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_then_stops_growing() {
        assert_eq!(backoff_seconds(1, 30), 60);
        assert_eq!(backoff_seconds(2, 30), 120);
        assert_eq!(backoff_seconds(3, 30), 240);
        // Un aviso de hace más de una hora ya no sirve de mucho, así que la
        // espera no crece indefinidamente.
        assert_eq!(backoff_seconds(20, 30), 3600);
        assert_eq!(backoff_seconds(0, 30), 30);
    }

    #[test]
    fn backoff_never_goes_backwards() {
        let mut previous = 0;
        for attempts in 0..12 {
            let wait = backoff_seconds(attempts, 15);
            assert!(
                wait >= previous,
                "intento {attempts} esperó menos que el anterior"
            );
            previous = wait;
        }
    }

    #[test]
    fn production_requires_https_and_development_allows_loopback() {
        assert!(acceptable_endpoint(
            "https://fcm.googleapis.com/fcm/send/abc",
            true
        ));
        assert!(acceptable_endpoint(
            "https://updates.push.services.mozilla.com/wpush/v2/x",
            true
        ));
        // En producción, nada en claro: el aviso cifrado y el teléfono viajan ahí.
        assert!(!acceptable_endpoint("http://127.0.0.1:4199/push", true));
        assert!(!acceptable_endpoint("http://proveedor.example/send", true));
        // En desarrollo, solo loopback, para poder probar el canal localmente.
        assert!(acceptable_endpoint("http://127.0.0.1:4199/push", false));
        assert!(acceptable_endpoint("http://localhost:4199/push", false));
        // Un dominio externo en claro no se admite ni en desarrollo: sería un
        // descuido fácil de dejar puesto al desplegar.
        assert!(!acceptable_endpoint("http://proveedor.example/send", false));
        assert!(!acceptable_endpoint(
            "http://127.0.0.1.evil.example/push",
            false
        ));
    }

    #[test]
    fn with_nothing_configured_no_channel_is_announced() {
        let transports = Transports::from_config(&crate::config::Config::for_tests());
        assert!(
            transports.names().is_empty(),
            "sin claves no debe anunciarse ningún canal"
        );
    }
}
