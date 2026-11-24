//! Ingesta de eventos de amenaza y clasificación del nivel de aviso.
//!
//! # Lo que esto es y lo que no es
//!
//! Esto **no es alerta temprana**. Los catálogos sísmicos oficiales publican un
//! evento entre uno y cinco minutos después de que ocurre, así que lo que llega
//! por aquí es siempre posterior al sismo. El valor del sistema está en la
//! decisión que viene después —qué hacer y a qué albergue ir, evitando los que
//! están en zona inundable o de suelo licuable—, no en la velocidad del aviso.
//! La detección en segundos requiere instrumentación de ondas P, y en los
//! teléfonos Android el sistema de alertas sísmicas de Google ya cubre ese hueco.
//!
//! # Fuentes
//!
//! `EventSource` abstrae el catálogo para que añadir un país sea añadir una
//! implementación, no tocar el motor. Hoy hay dos:
//!
//! - **USGS** (`usgs.rs`): servicio FDSN documentado y estable. Es el piso
//!   universal y funciona para cualquier país.
//! - **SGC** (`sgc.rs`): el Servicio Geológico Colombiano es la autoridad
//!   nacional y tiene precedencia sobre USGS, pero **no publica una API**. Su
//!   portal usa un endpoint AJAX de Django protegido por CSRF cuyo contrato no
//!   está documentado. La fuente queda implementada y conectada, pero desactivada
//!   hasta que `TP_SGC_URL` apunte a un servicio compatible con FDSN; el camino
//!   correcto es solicitar acceso al SGC, no raspar una página que puede cambiar
//!   sin aviso en medio de una emergencia.
//!
//! # Revisiones
//!
//! Un mismo sismo se publica varias veces: el evento de Panamá que ya estaba en
//! los datos del proyecto pasó de M8.0 a M7.7 al revisarse. Por eso cada
//! observación se guarda por `(fuente, identificador)` y las observaciones del
//! mismo sismo se agrupan bajo un `canonical_id`. Al insertar o revisar se marca
//! `alert_pending`, y es el motor de alertas quien decide si eso merece avisar:
//! una revisión **nunca** vuelve a notificar salvo que suba de nivel.

pub mod sgc;
pub mod usgs;

use crate::{
    AppState, auth, db,
    error::{self, Result},
};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::HeaderMap,
    routing::get,
};
use futures_util::TryStreamExt;
use mongodb::bson::{DateTime, Document, doc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Radio medio de la Tierra, el mismo valor que usa `planning::geometry`.
/// Público porque las consultas `$centerSphere` de MongoDB piden radianes.
pub const EARTH_RADIUS_KM: f64 = 6371.0088;

/// Tipo de amenaza. Es abierto a propósito: Centroamérica y el Caribe tienen
/// huracanes, los Andes deslizamientos y Ecuador o Guatemala volcanes. Añadir un
/// tipo no debe obligar a rediseñar el almacenamiento.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HazardType {
    Earthquake,
    Flood,
    Landslide,
}
impl HazardType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Earthquake => "earthquake",
            Self::Flood => "flood",
            Self::Landslide => "landslide",
        }
    }
}

/// Nivel de aviso para una persona concreta, que depende del evento **y** de su
/// distancia a él.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// No se avisa.
    None,
    /// Ocurrió algo cerca; conviene saberlo.
    Info,
    /// Conviene prepararse y revisar el entorno.
    Prepare,
    /// Conviene salir hacia un lugar seguro.
    Evacuate,
}
impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Info => "info",
            Self::Prepare => "prepare",
            Self::Evacuate => "evacuate",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "info" => Some(Self::Info),
            "prepare" => Some(Self::Prepare),
            "evacuate" => Some(Self::Evacuate),
            _ => None,
        }
    }
}

/// Umbrales de aviso: `(magnitud mínima, distancia máxima en km, nivel)`.
///
/// **Estas bandas no son un modelo de intensidad macrosísmica.** Son umbrales
/// operativos provisionales, escritos de forma que un especialista pueda leerlos
/// y corregirlos de un vistazo. No incorporan tipo de suelo, profundidad,
/// mecanismo focal ni atenuación regional, que es exactamente lo que haría falta
/// para estimar sacudida real. Por eso cada evento conserva
/// `validation_level: "automatic"` y el sistema opera en modo informar y
/// recomendar, sin convenio con la autoridad.
///
/// Se evalúan en orden y gana la primera que coincida, así que las bandas van de
/// mayor a menor magnitud y, dentro de cada una, del nivel más alto al más bajo.
const RULES: &[(f64, f64, Level)] = &[
    (6.5, 200.0, Level::Evacuate),
    (6.5, 600.0, Level::Prepare),
    (6.5, 1200.0, Level::Info),
    (5.5, 100.0, Level::Evacuate),
    (5.5, 300.0, Level::Prepare),
    (5.5, 700.0, Level::Info),
    (4.5, 50.0, Level::Prepare),
    (4.5, 200.0, Level::Info),
    (3.5, 80.0, Level::Info),
];

/// Nivel de aviso para alguien a `distance_km` del epicentro de un sismo de
/// magnitud `magnitude`.
pub fn classify(magnitude: f64, distance_km: f64) -> Level {
    if !magnitude.is_finite() || !distance_km.is_finite() || distance_km < 0.0 {
        return Level::None;
    }
    for (min_magnitude, max_distance, level) in RULES {
        if magnitude >= *min_magnitude && distance_km <= *max_distance {
            return *level;
        }
    }
    Level::None
}

/// Distancia máxima a la que un sismo de esta magnitud produce algún aviso.
///
/// Se guarda con el evento para que el motor de alertas pueda recuperar a los
/// suscriptores afectados con una sola consulta geoespacial indexada, en lugar de
/// evaluar la magnitud contra el millón de afiliados.
pub fn reach_km(magnitude: f64) -> f64 {
    if !magnitude.is_finite() {
        return 0.0;
    }
    RULES
        .iter()
        .filter(|(min_magnitude, _, _)| magnitude >= *min_magnitude)
        .map(|(_, max_distance, _)| *max_distance)
        .fold(0.0, f64::max)
}

/// Alcance máximo de cualquier evento posible.
///
/// Permite acotar con una sola consulta geoespacial qué eventos pueden haber
/// alcanzado un punto, antes de calcular la distancia exacta de cada uno. Se
/// deriva de las reglas para que no pueda quedar descoordinado con ellas.
pub fn max_reach_km() -> f64 {
    RULES
        .iter()
        .map(|(_, max_distance, _)| *max_distance)
        .fold(0.0, f64::max)
}

/// Distancia de círculo máximo entre dos puntos WGS84, en kilómetros.
///
/// Es la distancia epicentral sobre la superficie; no la hipocentral, que
/// incluiría la profundidad. Para las bandas de aviso de arriba la diferencia es
/// menor que su propia incertidumbre.
pub fn distance_km(a: [f64; 2], b: [f64; 2]) -> f64 {
    let (lon1, lat1) = (a[0].to_radians(), a[1].to_radians());
    let (lon2, lat2) = (b[0].to_radians(), b[1].to_radians());
    let (dlon, dlat) = (lon2 - lon1, lat2 - lat1);
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * h.sqrt().clamp(0.0, 1.0).asin()
}

/// Lo que una fuente reporta de un evento.
#[derive(Clone, Debug)]
pub struct Observation {
    /// Identificador del evento en la fuente que lo publicó. Quién lo publicó no
    /// se guarda aquí: lo aporta el `EventSource` que produjo la observación.
    pub external_id: String,
    pub hazard_type: HazardType,
    pub magnitude: Option<f64>,
    pub depth_km: Option<f64>,
    pub longitude: f64,
    pub latitude: f64,
    /// Instante del origen, en milisegundos desde la época UTC.
    pub origin_utc: i64,
    pub updated_utc: Option<i64>,
    /// Estado que declara la fuente: `automatic`, `reviewed`…
    pub status: Option<String>,
    pub place: Option<String>,
    pub url: Option<String>,
}
impl Observation {
    /// Comprobaciones mínimas antes de almacenar. Una fuente puede publicar un
    /// registro incompleto y eso no debe propagarse al motor de alertas.
    fn valid(&self) -> bool {
        !self.external_id.is_empty()
            && self.external_id.len() <= 80
            && self.longitude.is_finite()
            && (-180.0..=180.0).contains(&self.longitude)
            && self.latitude.is_finite()
            && (-90.0..=90.0).contains(&self.latitude)
            && self.origin_utc > 0
            && self
                .magnitude
                .is_none_or(|m| m.is_finite() && m > -2.0 && m < 11.0)
            && self.depth_km.is_none_or(|d| d.is_finite() && d >= 0.0)
    }
}

/// Una fuente de eventos.
///
/// `authority_rank` resuelve los desacuerdos: cuando dos catálogos publican el
/// mismo sismo con magnitudes distintas, manda el de mayor rango. El servicio
/// nacional conoce mejor su territorio que el global, pero suele publicar más
/// tarde, así que el global sigue sirviendo para no quedarse esperando.
pub trait EventSource: Send + Sync {
    fn name(&self) -> &'static str;
    fn authority_rank(&self) -> i32;
    fn fetch(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<Vec<Observation>>> + Send + '_>,
    >;
}

/// Ventana temporal y radio dentro de los cuales dos observaciones se consideran
/// el mismo sismo. Un sismo publicado por dos catálogos difiere en segundos y en
/// decenas de kilómetros según cómo cada red resuelva el hipocentro.
const SAME_EVENT_MS: i64 = 90_000;
const SAME_EVENT_KM: f64 = 100.0;

/// Agrupa la observación con un evento ya conocido, o le asigna un grupo nuevo.
async fn canonical_id(
    db: &mongodb::Database,
    o: &Observation,
) -> Result<(String, Option<Document>)> {
    let window = doc! {
        "hazard_type": o.hazard_type.as_str(),
        "origin_utc": {
            "$gte": DateTime::from_millis(o.origin_utc - SAME_EVENT_MS),
            "$lte": DateTime::from_millis(o.origin_utc + SAME_EVENT_MS),
        },
        "point": {"$geoWithin": {"$centerSphere": [
            [o.longitude, o.latitude],
            SAME_EVENT_KM / EARTH_RADIUS_KM,
        ]}},
    };
    let existing = db.collection::<Document>("events").find_one(window).await?;
    match existing {
        Some(d) => {
            let id = d.get_str("canonical_id").unwrap_or_default().to_owned();
            if id.is_empty() {
                Ok((uuid::Uuid::new_v4().to_string(), Some(d)))
            } else {
                Ok((id, Some(d)))
            }
        }
        None => Ok((uuid::Uuid::new_v4().to_string(), None)),
    }
}

pub struct Ingested {
    pub inserted: u64,
    pub revised: u64,
    pub unchanged: u64,
    pub rejected: u64,
}

/// Guarda las observaciones de una fuente.
///
/// Cada observación es un documento propio con `_id` igual a
/// `fuente:identificador`, así que reingerir el mismo feed no duplica nada. Si la
/// fuente revisó el evento, se actualiza y se cuenta como revisión; `notified`
/// **no** se toca, porque decidir si una revisión merece volver a avisar es del
/// motor de alertas y depende de que el nivel haya subido.
pub async fn ingest(
    db: &mongodb::Database,
    source: &dyn EventSource,
    observations: Vec<Observation>,
) -> Result<Ingested> {
    let collection = db.collection::<Document>("events");
    let mut out = Ingested {
        inserted: 0,
        revised: 0,
        unchanged: 0,
        rejected: 0,
    };
    for o in observations {
        if !o.valid() {
            tracing::warn!(
                source = source.name(),
                external_id = %o.external_id,
                "Observación descartada por datos incompletos o fuera de rango"
            );
            out.rejected += 1;
            continue;
        }
        let id = format!("{}:{}", source.name().to_lowercase(), o.external_id);
        let reach = o.magnitude.map(reach_km).unwrap_or(0.0);
        let previous = collection.find_one(doc! {"_id": &id}).await?;
        let changed = previous.as_ref().is_none_or(|d| {
            d.get_f64("magnitude").ok() != o.magnitude
                || d.get_i64("updated_utc").ok() != o.updated_utc
                || d.get_str("status").ok() != o.status.as_deref()
        });
        if !changed {
            out.unchanged += 1;
            continue;
        }
        let (canonical, _) = canonical_id(db, &o).await?;
        let fields = doc! {
            "canonical_id": &canonical,
            "source": source.name(),
            "authority_rank": source.authority_rank(),
            "external_id": &o.external_id,
            "hazard_type": o.hazard_type.as_str(),
            "magnitude": o.magnitude,
            "depth_km": o.depth_km,
            "point": {"type": "Point", "coordinates": [o.longitude, o.latitude]},
            "reach_km": reach,
            "origin_utc": DateTime::from_millis(o.origin_utc),
            "updated_utc": o.updated_utc,
            "status": o.status.as_deref(),
            "place": o.place.as_deref(),
            "url": o.url.as_deref(),
            // Sin convenio con la autoridad, ningún evento se presenta como
            // validado institucionalmente. El campo existe para poder activar ese
            // modo sin rediseñar nada.
            "validation_level": "automatic",
            // El motor de alertas reparte lo que esté pendiente y limpia la
            // bandera. Una revisión la vuelve a levantar, y es el motor quien
            // decide si eso merece un aviso nuevo: solo si sube de nivel.
            "alert_pending": true,
            "updated": db::now(),
        };
        if previous.is_some() {
            collection
                .update_one(doc! {"_id": &id}, doc! {"$set": fields})
                .await?;
            out.revised += 1;
            tracing::info!(
                event = %id,
                magnitude = ?o.magnitude,
                "Evento revisado por la fuente"
            );
        } else {
            let mut document = fields;
            document.insert("_id", &id);
            document.insert("created", db::now());
            collection.insert_one(document).await?;
            out.inserted += 1;
        }
    }
    Ok(out)
}

/// Fuentes activas según la configuración.
pub fn sources(config: &crate::config::Config) -> Vec<Box<dyn EventSource>> {
    let mut all: Vec<Box<dyn EventSource>> = Vec::new();
    if let Some(source) = sgc::Sgc::from_config(config) {
        all.push(Box::new(source));
    } else {
        tracing::info!(
            "SGC desactivado: define TP_SGC_URL con un servicio compatible con FDSN. \
             El SGC no publica una API documentada; USGS cubre el territorio mientras tanto"
        );
    }
    all.push(Box::new(usgs::Usgs::from_config(config)));
    all
}

/// Consulta todas las fuentes una vez.
///
/// El fallo de una fuente no detiene a las demás: durante un sismo fuerte es
/// justo cuando un portal nacional se cae por la carga, y es justo cuando el
/// respaldo global tiene que seguir funcionando.
pub async fn poll_once(state: &AppState, all: &[Box<dyn EventSource>]) -> Ingested {
    let mut total = Ingested {
        inserted: 0,
        revised: 0,
        unchanged: 0,
        rejected: 0,
    };
    for source in all {
        match source.fetch().await {
            Ok(observations) => {
                let count = observations.len();
                match ingest(&state.db, source.as_ref(), observations).await {
                    Ok(r) => {
                        total.inserted += r.inserted;
                        total.revised += r.revised;
                        total.unchanged += r.unchanged;
                        total.rejected += r.rejected;
                        if r.inserted > 0 || r.revised > 0 {
                            tracing::info!(
                                source = source.name(),
                                fetched = count,
                                inserted = r.inserted,
                                revised = r.revised,
                                "Eventos ingeridos"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::error!(source = source.name(), error = ?e, "Fallo al almacenar eventos")
                    }
                }
            }
            Err(e) => {
                tracing::warn!(source = source.name(), error = %e, "Fuente no disponible")
            }
        }
    }
    total
}

/// Bucle de ingesta. Vive en los modos `serve` y `worker`.
pub async fn worker(state: AppState) {
    let all = sources(&state.config);
    let period = std::time::Duration::from_secs(state.config.hazard_poll_seconds);
    tracing::info!(
        sources = ?all.iter().map(|s| s.name()).collect::<Vec<_>>(),
        seconds = state.config.hazard_poll_seconds,
        "Ingesta de amenazas iniciada"
    );
    let mut ticker = tokio::time::interval(period);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        poll_once(&state, &all).await;
    }
}

// --- API -------------------------------------------------------------------

pub fn routes() -> Router<AppState> {
    Router::new().route("/events", get(list))
}

#[derive(Deserialize)]
struct Filter {
    /// Magnitud mínima a devolver.
    magnitude: Option<f64>,
    page: Option<u64>,
}

/// Eventos recientes, del más nuevo al más antiguo.
///
/// Es información pública de catálogos públicos, pero exige sesión como el resto
/// de la API para que el límite por usuario siga aplicando.
async fn list(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Json<Value>> {
    auth::actor(&s, &h).await?;
    let mut filter = doc! {};
    if let Some(magnitude) = q.magnitude {
        if !magnitude.is_finite() {
            return Err(error::bad("Magnitud inválida"));
        }
        filter.insert("magnitude", doc! {"$gte": magnitude});
    }
    let page = q.page.unwrap_or(0).min(1000);
    let items: Vec<Document> =
        s.db.collection::<Document>("events")
            .find(filter)
            .sort(doc! {"origin_utc": -1, "_id": -1})
            .skip(page * 50)
            .limit(50)
            .await?
            .try_collect()
            .await?;
    Ok(Json(json!({
        "items": items,
        "page": page,
        "page_size": 50,
        "meaning": "Catálogo sísmico posterior al evento, no alerta temprana. \
                    Los niveles de aviso son umbrales operativos sin validación de especialista.",
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strong_and_close_means_evacuate() {
        assert_eq!(classify(7.4, 20.0), Level::Evacuate);
        assert_eq!(classify(6.5, 200.0), Level::Evacuate);
    }

    #[test]
    fn the_same_quake_de_escalates_with_distance() {
        // Un M7 avisa de forma distinta según dónde esté cada persona; el nivel
        // no es una propiedad del sismo.
        assert_eq!(classify(7.0, 50.0), Level::Evacuate);
        assert_eq!(classify(7.0, 400.0), Level::Prepare);
        assert_eq!(classify(7.0, 900.0), Level::Info);
        assert_eq!(classify(7.0, 2000.0), Level::None);
    }

    #[test]
    fn small_and_far_means_nothing() {
        assert_eq!(classify(3.6, 500.0), Level::None);
        assert_eq!(classify(2.0, 1.0), Level::None);
    }

    #[test]
    fn levels_are_ordered_so_a_revision_can_be_compared() {
        // El motor de alertas decide si una revisión vuelve a notificar
        // comparando niveles, así que el orden importa.
        assert!(Level::Evacuate > Level::Prepare);
        assert!(Level::Prepare > Level::Info);
        assert!(Level::Info > Level::None);
    }

    #[test]
    fn reach_covers_every_level_of_its_magnitude() {
        assert_eq!(reach_km(7.0), 1200.0);
        assert_eq!(reach_km(5.6), 700.0);
        assert_eq!(reach_km(4.5), 200.0);
        assert_eq!(reach_km(3.5), 80.0);
        assert_eq!(reach_km(2.0), 0.0);
        // Nadie fuera del alcance puede recibir aviso: es la condición que
        // permite prefiltrar suscriptores con una sola consulta geoespacial.
        for magnitude in [3.0, 4.0, 5.0, 6.0, 7.0, 8.0] {
            let reach = reach_km(magnitude);
            assert_eq!(
                classify(magnitude, reach + 1.0),
                Level::None,
                "M{magnitude}"
            );
        }
    }

    #[test]
    fn the_maximum_reach_bounds_every_rule() {
        let max = max_reach_km();
        assert_eq!(max, 1200.0);
        for (_, distance, _) in RULES {
            assert!(*distance <= max, "una regla supera el alcance máximo");
        }
        // Nadie más allá del alcance máximo puede recibir aviso de nada, que es
        // lo que permite acotar la consulta geoespacial.
        for magnitude in [3.0, 5.0, 7.0, 9.0] {
            assert_eq!(classify(magnitude, max + 1.0), Level::None, "M{magnitude}");
        }
    }

    #[test]
    fn classification_rejects_nonsense() {
        assert_eq!(classify(f64::NAN, 10.0), Level::None);
        assert_eq!(classify(7.0, f64::INFINITY), Level::None);
        assert_eq!(classify(7.0, -5.0), Level::None);
        assert_eq!(reach_km(f64::NAN), 0.0);
    }

    #[test]
    fn distance_matches_known_separations() {
        // Cali y Bogotá: unos 315 km en línea recta.
        let cali = [-76.5320, 3.4516];
        let bogota = [-74.0721, 4.7110];
        let d = distance_km(cali, bogota);
        assert!((300.0..330.0).contains(&d), "distancia {d}");
        assert_eq!(distance_km(cali, cali), 0.0);
    }

    #[test]
    fn the_panama_quake_would_have_reached_cali() {
        // Caso real que ya estaba en los datos del proyecto: M7.7 frente a la
        // costa de Panamá. Sirve para comprobar que un sismo lejano y fuerte
        // produce aviso, no silencio.
        let panama = [-80.769, 7.5868];
        let cali = [-76.5320, 3.4516];
        let d = distance_km(panama, cali);
        assert!(d > 500.0 && d < 700.0, "distancia {d}");
        assert_eq!(classify(7.7, d), Level::Info);
        // Y con la magnitud del reporte inicial, el nivel no debía ser mayor:
        // revisar a la baja no puede dejar un aviso inflado sin corregir.
        assert_eq!(classify(8.0, d), Level::Info);
    }

    fn sample(magnitude: Option<f64>, lon: f64, lat: f64) -> Observation {
        Observation {
            external_id: "test".into(),
            hazard_type: HazardType::Earthquake,
            magnitude,
            depth_km: Some(10.0),
            longitude: lon,
            latitude: lat,
            origin_utc: 1_700_000_000_000,
            updated_utc: None,
            status: Some("reviewed".into()),
            place: None,
            url: None,
        }
    }

    #[test]
    fn observations_outside_the_world_are_invalid() {
        assert!(sample(Some(5.0), -76.5, 3.45).valid());
        assert!(!sample(Some(5.0), -200.0, 3.45).valid());
        assert!(!sample(Some(5.0), -76.5, 100.0).valid());
        assert!(!sample(Some(99.0), -76.5, 3.45).valid());
        assert!(!sample(Some(f64::NAN), -76.5, 3.45).valid());
        // Una amenaza sin magnitud es válida: una inundación no tiene magnitud.
        assert!(sample(None, -76.5, 3.45).valid());
        let mut no_id = sample(Some(5.0), -76.5, 3.45);
        no_id.external_id = String::new();
        assert!(!no_id.valid());
        let mut no_time = sample(Some(5.0), -76.5, 3.45);
        no_time.origin_utc = 0;
        assert!(!no_time.valid());
    }

    #[test]
    fn level_round_trips_through_its_text() {
        for level in [Level::None, Level::Info, Level::Prepare, Level::Evacuate] {
            assert_eq!(Level::parse(level.as_str()), Some(level));
        }
        assert_eq!(Level::parse("urgente"), None);
    }
}
