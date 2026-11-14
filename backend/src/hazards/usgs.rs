//! Fuente USGS, sobre su servicio FDSN.
//!
//! Es el piso universal del sistema: está documentado, es estable, cubre
//! cualquier país y no exige credenciales. No es la autoridad local de ningún
//! territorio, así que tiene el rango más bajo y cualquier servicio nacional lo
//! sobreescribe cuando publica el mismo sismo.

use super::{EventSource, HazardType, Observation};
use crate::config::Config;
use mongodb::bson::DateTime;
use serde::Deserialize;

pub struct Usgs {
    url: String,
    bbox: [f64; 4],
    min_magnitude: f64,
    lookback_minutes: i64,
    client: reqwest::Client,
}

impl Usgs {
    pub fn from_config(config: &Config) -> Self {
        Self {
            url: config.usgs_url.clone(),
            bbox: config.hazard_bbox,
            min_magnitude: config.hazard_min_magnitude,
            lookback_minutes: config.hazard_lookback_minutes,
            client: client(),
        }
    }
}

/// Cliente con tiempos de espera acotados.
///
/// Un `fetch` que se quede colgado bloquearía el bucle de ingesta justo cuando
/// más hace falta, así que la petición se corta antes de que eso ocurra y se
/// reintenta en el siguiente ciclo.
pub(super) fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .connect_timeout(std::time::Duration::from_secs(8))
        .user_agent(
            "territorio-preparado/0.1 (+https://github.com/helynecheverry/Territorio-Preparado)",
        )
        .build()
        .unwrap_or_default()
}

/// Respuesta GeoJSON del servicio FDSN. Solo se declaran los campos que se usan;
/// el servicio publica muchos más y añadirlos no debe romper la lectura.
#[derive(Deserialize)]
pub(super) struct FeatureCollection {
    #[serde(default)]
    pub features: Vec<Feature>,
}
#[derive(Deserialize)]
pub(super) struct Feature {
    pub id: String,
    pub properties: Properties,
    pub geometry: Geometry,
}
#[derive(Deserialize)]
pub(super) struct Properties {
    #[serde(default)]
    pub mag: Option<f64>,
    #[serde(default)]
    pub place: Option<String>,
    /// Milisegundos UTC del origen.
    #[serde(default)]
    pub time: Option<i64>,
    #[serde(default)]
    pub updated: Option<i64>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}
#[derive(Deserialize)]
pub(super) struct Geometry {
    /// `[longitud, latitud, profundidad_km]`.
    #[serde(default)]
    pub coordinates: Vec<f64>,
}

/// Convierte la respuesta FDSN en observaciones.
///
/// Se comparte con la fuente nacional: cualquier servicio compatible con FDSN
/// devuelve este mismo GeoJSON, y es la razón por la que añadir un país debería
/// ser configuración y no código nuevo.
pub(super) fn parse(body: FeatureCollection) -> Vec<Observation> {
    body.features
        .into_iter()
        .filter_map(|f| {
            let origin_utc = f.properties.time?;
            let longitude = *f.geometry.coordinates.first()?;
            let latitude = *f.geometry.coordinates.get(1)?;
            Some(Observation {
                external_id: f.id,
                hazard_type: HazardType::Earthquake,
                magnitude: f.properties.mag,
                // FDSN publica la profundidad como tercer valor de la geometría.
                // Puede faltar, y faltar no es cero.
                depth_km: f.geometry.coordinates.get(2).copied(),
                longitude,
                latitude,
                origin_utc,
                updated_utc: f.properties.updated,
                status: f.properties.status,
                place: f.properties.place,
                url: f.properties.url,
            })
        })
        .collect()
}

/// Consulta FDSN para una ventana reciente y un recuadro geográfico.
///
/// El recuadro es generoso a propósito: un sismo con epicentro en Panamá o
/// Ecuador sacude Colombia, y recortar la consulta al contorno del país dejaría
/// fuera eventos que sí hay que avisar. Quién recibe el aviso lo decide después
/// la distancia de cada persona, no el recuadro de la consulta.
pub(super) async fn fetch_fdsn(
    client: &reqwest::Client,
    url: &str,
    source: &'static str,
    bbox: [f64; 4],
    min_magnitude: f64,
    lookback_minutes: i64,
) -> anyhow::Result<Vec<Observation>> {
    let since =
        DateTime::from_millis(DateTime::now().timestamp_millis() - lookback_minutes * 60_000)
            .try_to_rfc3339_string()?;
    let response = client
        .get(url)
        .query(&[
            ("format", "geojson"),
            ("starttime", &since),
            ("minlongitude", &bbox[0].to_string()),
            ("minlatitude", &bbox[1].to_string()),
            ("maxlongitude", &bbox[2].to_string()),
            ("maxlatitude", &bbox[3].to_string()),
            ("minmagnitude", &min_magnitude.to_string()),
            ("orderby", "time"),
        ])
        .send()
        .await?;
    let status = response.status();
    anyhow::ensure!(status.is_success(), "{source} respondió {status}");
    // El cuerpo se limita antes de deserializar: una respuesta inesperadamente
    // grande no debe poder agotar la memoria del proceso.
    let body = response.bytes().await?;
    anyhow::ensure!(
        body.len() <= 8 * 1024 * 1024,
        "{source} devolvió una respuesta de {} bytes",
        body.len()
    );
    Ok(parse(serde_json::from_slice(&body)?))
}

impl EventSource for Usgs {
    fn name(&self) -> &'static str {
        "USGS"
    }
    /// El rango más bajo: cualquier autoridad nacional lo sobreescribe.
    fn authority_rank(&self) -> i32 {
        10
    }
    fn fetch(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<Vec<Observation>>> + Send + '_>,
    > {
        Box::pin(fetch_fdsn(
            &self.client,
            &self.url,
            "USGS",
            self.bbox,
            self.min_magnitude,
            self.lookback_minutes,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorte real de la respuesta del servicio, con el sismo de Panamá que ya
    /// estaba en los datos del proyecto.
    const SAMPLE: &str = r#"{
      "type": "FeatureCollection",
      "features": [
        {
          "type": "Feature",
          "id": "us6000u18k",
          "properties": {
            "mag": 7.7,
            "place": "12 km WSW of Pitaloza Arriba, Panama",
            "time": 1760032566036,
            "updated": 1760037634239,
            "status": "reviewed",
            "url": "https://earthquake.usgs.gov/earthquakes/eventpage/us6000u18k",
            "tsunami": 1
          },
          "geometry": {"type": "Point", "coordinates": [-80.769, 7.5868, 12.647]}
        },
        {
          "type": "Feature",
          "id": "sin-hora",
          "properties": {"mag": 4.1},
          "geometry": {"type": "Point", "coordinates": [-76.5, 3.4, 30.0]}
        },
        {
          "type": "Feature",
          "id": "sin-geometria",
          "properties": {"mag": 4.1, "time": 1760032566036},
          "geometry": {"type": "Point", "coordinates": []}
        }
      ]
    }"#;

    #[test]
    fn parses_an_event_and_skips_the_unusable_ones() {
        let body: FeatureCollection = serde_json::from_str(SAMPLE).unwrap();
        let events = parse(body);
        // Los dos registros sin hora o sin coordenadas no se inventan: se omiten.
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.external_id, "us6000u18k");
        assert_eq!(e.magnitude, Some(7.7));
        assert_eq!(e.depth_km, Some(12.647));
        assert_eq!(e.longitude, -80.769);
        assert_eq!(e.latitude, 7.5868);
        assert_eq!(e.origin_utc, 1760032566036);
        assert_eq!(e.updated_utc, Some(1760037634239));
        assert_eq!(e.status.as_deref(), Some("reviewed"));
        assert_eq!(e.hazard_type, HazardType::Earthquake);
    }

    #[test]
    fn unknown_fields_do_not_break_the_reader() {
        // `tsunami` no está declarado en `Properties` y la lectura funciona, que
        // es lo que permite que el servicio añada campos sin romper la ingesta.
        let body: FeatureCollection = serde_json::from_str(SAMPLE).unwrap();
        assert!(!body.features.is_empty());
    }

    #[test]
    fn an_empty_catalogue_is_not_an_error() {
        let body: FeatureCollection = serde_json::from_str(r#"{"features": []}"#).unwrap();
        assert!(parse(body).is_empty());
        let body: FeatureCollection = serde_json::from_str(r#"{}"#).unwrap();
        assert!(parse(body).is_empty());
    }

    #[test]
    fn missing_depth_is_absent_and_not_zero() {
        let body: FeatureCollection = serde_json::from_str(
            r#"{"features":[{"id":"x","properties":{"mag":5.0,"time":1},
                "geometry":{"coordinates":[-76.5,3.4]}}]}"#,
        )
        .unwrap();
        let events = parse(body);
        assert_eq!(events[0].depth_km, None);
    }
}
