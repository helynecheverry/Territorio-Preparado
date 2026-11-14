//! A dónde ir: espacios candidatos según la amenaza y la ubicación.
//!
//! # El problema que resuelve
//!
//! `docs/DATASETS.md` documenta el hallazgo que da sentido a todo el proyecto:
//! tras el sismo del 10 de agosto de 2026, en los parques de Chiminangos y
//! Calimio se montaron albergues autogestionados. Esos parques tienen amenaza
//! **alta** de inundación, están sobre suelo **licuable** y quedan a entre 72 y
//! 152 m del dique del río Cauca. La gente huyó del sismo hacia el lugar de mayor
//! riesgo de inundación, porque era el espacio abierto que tenía al lado.
//!
//! Este módulo convierte ese hallazgo en una respuesta: no «el espacio abierto
//! más cercano», sino «el más cercano que además sirve para esta amenaza». Y
//! cuando descarta algo cercano, **lo dice y explica por qué**, porque si alguien
//! va a caminar 1,8 km en vez de 400 m necesita saber la razón.
//!
//! # Ausencia de dato no es ausencia de riesgo
//!
//! Las propiedades `flood` y `seismic` del inventario son intersecciones con las
//! capas cargadas. Un arreglo vacío significa «no intersecta con lo que tenemos»,
//! nunca «es seguro». `PLANNING.md` ya fija ese principio y aquí se respeta en dos
//! niveles:
//!
//! - Por espacio: la aptitud se declara como `sin_amenaza_declarada`, no como
//!   segura, y se nombra qué se comprobó.
//! - Por territorio: si a la ciudad le falta la capa que esta amenaza necesita,
//!   la respuesta marca esa dimensión como **no verificada**. Sin eso, una ciudad
//!   sin capa de inundación daría arreglos vacíos indistinguibles de los de una
//!   ciudad donde sí se comprobó, y la recomendación mentiría sin que nadie lo
//!   notara.
//!
//! # Lo que no hace
//!
//! La distancia es en línea recta. No es una ruta a pie: no sabe de puentes, del
//! dique, de calles bloqueadas ni de un paso elevado caído, que es precisamente
//! lo que cambia después de un sismo. Y la capacidad que informa es una cota
//! superior por área según la referencia de ACNUR que ya usa el proyecto, no una
//! propuesta del planificador: para eso está `/api/v1/planning/preview`, que sí
//! calcula distribución, exclusiones y recursos.

use crate::{
    AppState, auth,
    error::{self, Result},
    hazards::HazardType,
    planning::geometry::{Projection, Shape},
    territory::{self, Territory},
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use futures_util::TryStreamExt;
use geo::Area;
use mongodb::bson::{Document, doc};
use serde::Deserialize;
use serde_json::{Value, json};

/// Metros cuadrados por persona del perfil de asentamiento de ACNUR, el mismo
/// valor que usa el planificador. Incluye circulación y servicios, no es
/// superficie cubierta por persona.
const AREA_PER_PERSON_M2: f64 = 45.0;

/// Capa de amenaza que una decisión necesita tener cargada.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Requirement {
    Flood,
    Liquefaction,
}
impl Requirement {
    fn as_str(self) -> &'static str {
        match self {
            Self::Flood => "flood",
            Self::Liquefaction => "liquefaction",
        }
    }
    /// Si el territorio tiene cargada la capa que esta comprobación necesita.
    fn available(self, t: &Territory) -> bool {
        match self {
            Self::Flood => t.layers.flood,
            // La susceptibilidad a licuación viene de la microzonificación, así
            // que hacen falta las dos capas para poder afirmar algo.
            Self::Liquefaction => t.layers.liquefaction && t.layers.seismic_microzonation,
        }
    }
}

/// Qué hay que comprobar para cada amenaza.
///
/// Tras un sismo la gente se queda en el destino durante días, así que un
/// destino sísmico también tiene que aguantar la lluvia: se excluye la amenaza
/// alta de inundación además del suelo licuable. Eso es exactamente lo que falló
/// en Chiminangos.
fn requirements(hazard: HazardType) -> &'static [Requirement] {
    match hazard {
        HazardType::Earthquake => &[Requirement::Liquefaction, Requirement::Flood],
        HazardType::Flood => &[Requirement::Flood],
        // No hay capa de deslizamientos en el proyecto. No se finge comprobarlo.
        HazardType::Landslide => &[],
    }
}

/// Motivo por el que un espacio queda descartado, en el lenguaje de los datos.
struct Rejection {
    requirement: Requirement,
    detail: String,
}

/// Intersecciones declaradas del inventario para una propiedad.
fn declared(properties: &Value, key: &str) -> Vec<String> {
    properties[key]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Evalúa un espacio contra una comprobación.
///
/// Devuelve `Some` solo cuando el inventario declara una intersección que
/// desaconseja el espacio. Las cadenas provienen del POT y de la
/// microzonificación de Cali, así que se buscan por su contenido en lugar de
/// compararlas enteras: la redacción varía entre polígonos de la misma capa
/// —«Profundidad > 0,90 m» y «Profundidad > 0,90m o Velocidad > 0,8 m/s»— y una
/// comparación exacta dejaría pasar amenaza alta por una diferencia de formato.
fn evaluate(properties: &Value, requirement: Requirement, hazard: HazardType) -> Option<Rejection> {
    match requirement {
        Requirement::Flood => {
            let values = declared(properties, "flood");
            // Para una inundación se descarta también la amenaza media: el
            // destino tiene que seguir siendo utilizable mientras sube el agua.
            let unacceptable: Vec<&String> = values
                .iter()
                .filter(|v| {
                    let v = v.to_lowercase();
                    v.contains("amenaza alta")
                        || (hazard == HazardType::Flood && v.contains("amenaza media"))
                })
                .collect();
            (!unacceptable.is_empty()).then(|| Rejection {
                requirement,
                detail: unacceptable
                    .iter()
                    .map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join("; "),
            })
        }
        Requirement::Liquefaction => {
            // Cualquier intersección con la capa sísmica del inventario indica
            // susceptibilidad a licuación o corrimiento. No se intenta graduarla:
            // la capa no publica un nivel y graduarla sería inventarlo.
            let values = declared(properties, "seismic");
            (!values.is_empty()).then(|| Rejection {
                requirement,
                detail: values.join("; "),
            })
        }
    }
}

/// Área real de la huella, recalculada desde la geometría.
///
/// No se usa `areaM2` del inventario: el planificador ya recalcula el área por la
/// misma razón, que el valor declarado y el que da la geometría no siempre
/// coinciden. Es la aproximación equirectangular de `geometry.rs`, de planeación
/// y no topográfica.
fn area_m2(shape: &Shape) -> Option<f64> {
    let projection = Projection::for_shape(shape).ok()?;
    let polygon = projection.polygon(shape).ok()?;
    Some(polygon.unsigned_area())
}

#[derive(Deserialize)]
struct Stored {
    #[serde(rename = "_id")]
    id: String,
    properties: Value,
    geometry: Shape,
    distance_m: f64,
}

pub struct Options {
    pub hazard: HazardType,
    pub radius_km: f64,
    pub min_area_m2: f64,
    pub results: usize,
}

/// Busca destinos candidatos alrededor de un punto.
///
/// Recorre el inventario del más cercano al más lejano con `$geoNear` sobre el
/// índice `2dsphere` y se detiene al reunir los resultados pedidos o al agotar el
/// presupuesto de candidatos. Los espacios descartados que están **más cerca** que
/// el último aceptado se devuelven con su motivo: son la explicación de por qué
/// la recomendación manda a alguien más lejos.
pub async fn search(
    state: &AppState,
    t: &Territory,
    point: [f64; 2],
    options: &Options,
) -> Result<Value> {
    let needed = requirements(options.hazard);
    let unverifiable: Vec<&'static str> = needed
        .iter()
        .filter(|r| !r.available(t))
        .map(|r| r.as_str())
        .collect();

    let mut cursor = state
        .db
        .collection::<Stored>("spaces")
        .aggregate(vec![doc! {"$geoNear": {
            "near": {"type": "Point", "coordinates": [point[0], point[1]]},
            "distanceField": "distance_m",
            "maxDistance": options.radius_km * 1000.0,
            "spherical": true,
            // Solo huellas poligonales: de un punto no se puede informar área ni
            // capacidad, y la recomendación vale por poder decir cuánta gente cabe.
            "query": {"territory_id": &t.id, "measurable": true},
        }}])
        .with_type::<Stored>()
        .await?;

    let mut items = Vec::new();
    let mut rejected = Vec::new();
    let mut examined = 0usize;
    while let Some(space) = cursor.try_next().await? {
        if items.len() >= options.results || examined >= state.config.destination_candidates {
            break;
        }
        examined += 1;
        let distance_km = space.distance_m / 1000.0;
        let Some(area) = area_m2(&space.geometry) else {
            continue;
        };
        let name = space.properties["name"]
            .as_str()
            .unwrap_or_default()
            .to_owned();

        if area < options.min_area_m2 {
            // No se reporta como descarte por amenaza: es demasiado pequeño, que
            // es un motivo distinto y menos informativo para quien decide.
            continue;
        }
        // Solo se comprueba lo que el territorio permite comprobar. Si la capa no
        // está, no se descarta por ella ni se afirma que esté bien: queda en
        // `unverified` de la respuesta.
        let problems: Vec<Rejection> = needed
            .iter()
            .filter(|r| r.available(t))
            .filter_map(|r| evaluate(&space.properties, *r, options.hazard))
            .collect();

        if problems.is_empty() {
            items.push(json!({
                "id": space.id,
                "name": name,
                "type": space.properties["type"],
                "neighborhood": space.properties["neighborhood"],
                "commune": space.properties["commune"],
                "straight_line_km": (distance_km * 100.0).round() / 100.0,
                "area_m2": area.round(),
                // Cota superior por área, no una propuesta del planificador.
                "area_upper_bound_people": (area / AREA_PER_PERSON_M2).floor(),
                "declared_flood": declared(&space.properties, "flood"),
                "declared_seismic": declared(&space.properties, "seismic"),
                "checked": needed
                    .iter()
                    .filter(|r| r.available(t))
                    .map(|r| r.as_str())
                    .collect::<Vec<_>>(),
                "unverified": unverifiable,
                "suitability": "sin_amenaza_declarada",
                "availability": space.properties["availability"],
                "condition": space.properties["condition"],
            }));
        } else {
            rejected.push(json!({
                "id": space.id,
                "name": name,
                "neighborhood": space.properties["neighborhood"],
                "straight_line_km": (distance_km * 100.0).round() / 100.0,
                "area_m2": area.round(),
                "reasons": problems
                    .iter()
                    .map(|p| json!({"requirement": p.requirement.as_str(), "declared": p.detail}))
                    .collect::<Vec<_>>(),
            }));
        }
    }

    // Los descartes que importan son los que están más cerca que el destino
    // recomendado: son los que alguien elegiría por instinto.
    let nearest_accepted = items
        .first()
        .and_then(|i| i["straight_line_km"].as_f64())
        .unwrap_or(f64::MAX);
    let closer_but_unsuitable: Vec<Value> = rejected
        .into_iter()
        .filter(|r| r["straight_line_km"].as_f64().unwrap_or(f64::MAX) <= nearest_accepted)
        .collect();

    Ok(json!({
        "hazard": options.hazard.as_str(),
        "territory": t.summary(),
        "radius_km": options.radius_km,
        "min_area_m2": options.min_area_m2,
        "examined": examined,
        "items": items,
        // El diferenciador, explícito: por qué no el de al lado.
        "closer_but_unsuitable": closer_but_unsuitable,
        "checked": needed
            .iter()
            .filter(|r| r.available(t))
            .map(|r| r.as_str())
            .collect::<Vec<_>>(),
        "unverified": unverifiable,
        "meaning": "Candidatos del inventario, no albergues habilitados. La aptitud es \
                    «sin amenaza declarada» según las capas cargadas: la ausencia de dato no \
                    acredita seguridad. La distancia es en línea recta y no una ruta a pie: no \
                    considera calles bloqueadas, puentes ni el dique. La capacidad es una cota \
                    superior por área según la referencia de ACNUR, no una propuesta del \
                    planificador. Requiere revisión humana y confirmación con la entidad \
                    responsable.",
    }))
}

// --- API -------------------------------------------------------------------

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/destinations", get(by_point))
        .route("/subscriptions/{id}/destinations", get(by_subscription))
}

#[derive(Deserialize)]
struct Ask {
    latitude: f64,
    longitude: f64,
    hazard: String,
    territory: Option<String>,
    radius_km: Option<f64>,
    min_area_m2: Option<f64>,
    results: Option<usize>,
}

fn parse_hazard(value: &str) -> Result<HazardType> {
    match value {
        "earthquake" => Ok(HazardType::Earthquake),
        "flood" => Ok(HazardType::Flood),
        "landslide" => Ok(HazardType::Landslide),
        _ => Err(error::bad(
            "La amenaza debe ser earthquake, flood o landslide",
        )),
    }
}

fn options(state: &AppState, hazard: HazardType, ask: &Ask) -> Result<Options> {
    let radius_km = ask
        .radius_km
        .unwrap_or(state.config.destination_radius_km)
        .clamp(0.1, 50.0);
    let min_area_m2 = ask
        .min_area_m2
        .unwrap_or(state.config.destination_min_area_m2)
        .clamp(0.0, 1_000_000.0);
    if !radius_km.is_finite() || !min_area_m2.is_finite() {
        return Err(error::bad("Radio o área mínima inválidos"));
    }
    Ok(Options {
        hazard,
        radius_km,
        min_area_m2,
        results: ask.results.unwrap_or(10).clamp(1, 50),
    })
}

/// Destinos para una ubicación indicada.
///
/// Existe además de la variante por afiliación porque una emergencia encuentra a
/// la gente donde esté, no donde se afilió.
async fn by_point(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Ask>,
) -> Result<Json<Value>> {
    auth::actor(&s, &h).await?;
    if !q.latitude.is_finite()
        || !(-90.0..=90.0).contains(&q.latitude)
        || !q.longitude.is_finite()
        || !(-180.0..=180.0).contains(&q.longitude)
    {
        return Err(error::bad("Coordenadas inválidas"));
    }
    let hazard = parse_hazard(&q.hazard)?;
    let t = match q.territory.as_deref() {
        Some(id) if !Territory::valid_id(id) => {
            return Err(error::bad("Identificador de territorio inválido"));
        }
        Some(id) => territory::get(&s.db, id).await?,
        None => {
            let mut all = territory::list(&s.db).await?;
            if all.len() == 1 {
                all.remove(0)
            } else {
                return Err(error::bad("Indica el territorio"));
            }
        }
    };
    let options = options(&s, hazard, &q)?;
    Ok(Json(
        search(&s, &t, [q.longitude, q.latitude], &options).await?,
    ))
}

#[derive(Deserialize)]
struct AskForSubscription {
    hazard: String,
    radius_km: Option<f64>,
    min_area_m2: Option<f64>,
    results: Option<usize>,
}

/// Destinos para una afiliación propia.
///
/// Evita que la ubicación de la casa de alguien viaje en la cadena de consulta,
/// donde terminaría en los registros de cualquier intermediario.
async fn by_subscription(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<AskForSubscription>,
) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    let hazard = parse_hazard(&q.hazard)?;
    let sub =
        s.db.collection::<Document>("subscriptions")
            .find_one(doc! {"_id": &id, "owner": &a.id})
            .await?
            .ok_or(error::Error(
                StatusCode::NOT_FOUND,
                "Afiliación no encontrada",
            ))?;
    let coordinates = sub
        .get_document("point")
        .ok()
        .and_then(|p| p.get_array("coordinates").ok())
        .and_then(|c| Some([c.first()?.as_f64()?, c.get(1)?.as_f64()?]))
        .ok_or_else(|| error::internal("afiliación sin punto"))?;
    let t = territory::get(&s.db, sub.get_str("territory_id").unwrap_or_default()).await?;
    let ask = Ask {
        latitude: coordinates[1],
        longitude: coordinates[0],
        hazard: q.hazard,
        territory: None,
        radius_km: q.radius_km,
        min_area_m2: q.min_area_m2,
        results: q.results,
    };
    let options = options(&s, hazard, &ask)?;
    let mut out = search(&s, &t, coordinates, &options).await?;
    out["subscription"] = json!({
        "id": id,
        "label": sub.get_str("label").unwrap_or_default(),
        "neighborhood": sub.get_str("neighborhood").ok(),
        "commune": sub.get_str("commune").ok(),
    });
    Ok(Json(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::territory::Layers;

    fn properties(flood: &[&str], seismic: &[&str]) -> Value {
        json!({"flood": flood, "seismic": seismic})
    }

    fn cali() -> Territory {
        Territory {
            id: "co-cali".into(),
            country_code: "CO".into(),
            admin1: "Valle del Cauca".into(),
            name: "Cali".into(),
            timezone: "America/Bogota".into(),
            locale: "es-CO".into(),
            layers: Layers {
                flood: true,
                seismic_microzonation: true,
                liquefaction: true,
            },
            sources: Default::default(),
            source_sha256: String::new(),
            spaces_total: 0,
            measurable_total: 0,
        }
    }

    #[test]
    fn an_earthquake_destination_must_survive_both_hazards() {
        // La lección de Chiminangos, escrita como prueba: tras un sismo la gente
        // se queda días, así que el destino también se comprueba contra
        // inundación y no solo contra licuación.
        assert_eq!(
            requirements(HazardType::Earthquake),
            &[Requirement::Liquefaction, Requirement::Flood]
        );
        assert_eq!(requirements(HazardType::Flood), &[Requirement::Flood]);
        // Sin capa de deslizamientos no se finge comprobar nada.
        assert!(requirements(HazardType::Landslide).is_empty());
    }

    #[test]
    fn a_liquefiable_park_is_rejected_for_an_earthquake() {
        // Cadenas reales de la microzonificación de Cali.
        let p = properties(
            &[],
            &[
                "Zona 6 · susceptibilidad a licuación / corrimiento",
                "Zona susceptible a licuacion (Áreas estudiadas)",
            ],
        );
        let r = evaluate(&p, Requirement::Liquefaction, HazardType::Earthquake)
            .expect("debía descartarse");
        assert!(r.detail.contains("Zona 6"));
    }

    #[test]
    fn the_dike_corridor_is_rejected() {
        // El caso de Calimio: parques a menos de 152 m del dique del río Cauca.
        let p = properties(&[], &["Corrimiento lateral (80m del Dique Río Cauca)"]);
        assert!(evaluate(&p, Requirement::Liquefaction, HazardType::Earthquake).is_some());
    }

    #[test]
    fn high_flood_hazard_is_rejected_for_every_hazard() {
        let p = properties(
            &["Amenaza Alta mitigable (Profundidad > 0,90m o Velocidad > 0,8 m/s)"],
            &[],
        );
        assert!(evaluate(&p, Requirement::Flood, HazardType::Earthquake).is_some());
        assert!(evaluate(&p, Requirement::Flood, HazardType::Flood).is_some());
        // La redacción varía entre polígonos de la misma capa, así que la
        // comparación es por contenido y no por igualdad exacta.
        let other = properties(&["Amenaza Alta mitigable (Profundidad > 0,90 m)"], &[]);
        assert!(evaluate(&other, Requirement::Flood, HazardType::Flood).is_some());
    }

    #[test]
    fn medium_flood_only_blocks_a_flood_destination() {
        let p = properties(
            &[
                "Amenaza Media mitigable (Profundidad 0.45 m - 0.90 m o Velocidad entre 0.5 - 0.8 m/s",
            ],
            &[],
        );
        // Para una inundación el agua va a subir ahí: no sirve.
        assert!(evaluate(&p, Requirement::Flood, HazardType::Flood).is_some());
        // Tras un sismo sin lluvia, la amenaza media no es motivo para mandar a
        // alguien más lejos; la alta sí.
        assert!(evaluate(&p, Requirement::Flood, HazardType::Earthquake).is_none());
    }

    #[test]
    fn low_flood_hazard_does_not_reject() {
        let p = properties(
            &["Amenaza Baja mitigable (Profundidad < 0.45 m o Velocidad < 0.5 m/s"],
            &[],
        );
        assert!(evaluate(&p, Requirement::Flood, HazardType::Flood).is_none());
    }

    #[test]
    fn no_declared_hazard_is_not_a_rejection_but_is_not_safety_either() {
        // Un arreglo vacío no descarta el espacio, y la respuesta lo llama
        // «sin_amenaza_declarada» en lugar de «seguro». La distinción es la que
        // `PLANNING.md` exige y la que evita afirmar seguridad sin datos.
        let p = properties(&[], &[]);
        assert!(evaluate(&p, Requirement::Flood, HazardType::Flood).is_none());
        assert!(evaluate(&p, Requirement::Liquefaction, HazardType::Earthquake).is_none());
    }

    #[test]
    fn a_missing_layer_makes_the_check_unverifiable() {
        let mut t = cali();
        assert!(Requirement::Flood.available(&t));
        assert!(Requirement::Liquefaction.available(&t));

        t.layers.flood = false;
        assert!(!Requirement::Flood.available(&t));
        // Sin microzonificación no se puede afirmar nada sobre licuación, aunque
        // la bandera de licuación siga puesta: hacen falta las dos capas.
        t.layers.seismic_microzonation = false;
        assert!(!Requirement::Liquefaction.available(&t));
    }

    #[test]
    fn area_is_recomputed_from_the_footprint() {
        // Un cuadrado de aproximadamente 0,01° de lado cerca de Cali.
        let square = Shape::Polygon(vec![vec![
            [-76.52, 3.45],
            [-76.51, 3.45],
            [-76.51, 3.46],
            [-76.52, 3.46],
            [-76.52, 3.45],
        ]]);
        let area = area_m2(&square).expect("el cuadrado es medible");
        // Un grado de latitud son unos 111 km, así que 0,01° de lado da del
        // orden de 1,1 km por lado y algo más de un kilómetro cuadrado.
        assert!((1.1e6..1.4e6).contains(&area), "área inesperada: {area}");
        assert!((area / AREA_PER_PERSON_M2).floor() > 0.0);
        // De un punto no se puede obtener área, y por eso no se recomienda.
        assert!(area_m2(&Shape::Point([-76.52, 3.45])).is_none());
    }

    #[test]
    fn hazard_names_are_validated() {
        assert!(parse_hazard("earthquake").is_ok());
        assert!(parse_hazard("flood").is_ok());
        assert!(parse_hazard("landslide").is_ok());
        assert!(parse_hazard("terremoto").is_err());
        assert!(parse_hazard("").is_err());
    }
}
