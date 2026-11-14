//! Sectores administrativos y resolución de un punto a su sector.
//!
//! Una afiliación se declara con una coordenada —la casa, el trabajo, la escuela
//! de los hijos— y el sistema necesita saber a qué barrio y a qué comuna
//! pertenece, porque las amenazas y los albergues se razonan por sector y porque
//! una alerta se dirige a «el oriente de Cali», no a una lista de coordenadas.
//!
//! Los límites viven en la colección `sectors` con índice `2dsphere`, igual que
//! el inventario de espacios: resolver un punto es una consulta `$geoIntersects`
//! y no un recorrido de 342 barrios en memoria de cada proceso.
//!
//! A diferencia del inventario, aquí la geometría **sí** se indexa, porque hay
//! que consultar contención y no solo cercanía. Eso significa que MongoDB aplica
//! su propia validación al insertar y puede rechazar un límite que la fuente
//! publique mal cerrado o autocruzado. La importación no esconde esos casos: los
//! cuenta y los nombra, para que el sector faltante se corrija en el origen en
//! lugar de desaparecer en silencio.

use crate::{
    error::{self, Result},
    planning::geometry::Shape,
    territory::Territory,
};
use anyhow::Context;
use mongodb::{
    Database,
    bson::{Document, doc},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Tipo de sector. Son los dos niveles que el inventario de Cali distingue; al
/// añadir países habrá que mapear cada nomenclatura local a estos dos niveles o
/// ampliar la enumeración.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Barrio, vereda o sector censal.
    Neighborhood,
    /// Comuna, localidad o distrito.
    Commune,
}
impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Neighborhood => "neighborhood",
            Self::Commune => "commune",
        }
    }
}

/// Sector al que pertenece un punto.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Resolved {
    /// Código oficial del barrio, por ejemplo `1401`.
    pub neighborhood_code: Option<String>,
    pub neighborhood: Option<String>,
    /// Código de comuna de dos dígitos, por ejemplo `14`.
    pub commune_code: Option<String>,
    pub commune: Option<String>,
}
impl Resolved {
    /// Un punto resuelto al menos a nivel de comuna. Un punto que no cae en
    /// ningún sector del territorio está fuera de la ciudad, y afiliarlo sería
    /// prometer una cobertura que no existe.
    pub fn inside(&self) -> bool {
        self.commune_code.is_some() || self.neighborhood_code.is_some()
    }
}

#[derive(Deserialize)]
struct Stored {
    name: String,
    code: String,
    #[serde(default)]
    commune: Option<String>,
}

/// Resuelve un punto WGS84 (`[lon, lat]`) a su barrio y comuna dentro del
/// territorio.
///
/// Son dos consultas indexadas en lugar de una porque los dos niveles se
/// publican como capas independientes y un punto puede caer en una comuna sin
/// caer en ningún barrio —los límites no coinciden exactamente en los bordes del
/// perímetro urbano—. Resolver cada nivel por separado conserva esa diferencia en
/// vez de descartar el punto.
pub async fn resolve(db: &Database, territory_id: &str, point: [f64; 2]) -> Result<Resolved> {
    let collection = db.collection::<Stored>("sectors");
    let mut out = Resolved::default();
    for kind in [Kind::Neighborhood, Kind::Commune] {
        let found = collection
            .find_one(doc! {
                "territory_id": territory_id,
                "kind": kind.as_str(),
                "geometry": {"$geoIntersects": {"$geometry": {
                    "type": "Point",
                    "coordinates": [point[0], point[1]],
                }}},
            })
            .await?;
        if let Some(s) = found {
            match kind {
                Kind::Neighborhood => {
                    out.neighborhood_code = Some(s.code);
                    out.neighborhood = Some(s.name);
                    // El barrio declara su comuna; si la capa de comunas no
                    // cubriera el punto, este valor la conserva igualmente.
                    out.commune_code = s.commune;
                }
                Kind::Commune => {
                    out.commune_code = Some(s.code);
                    out.commune = Some(s.name);
                }
            }
        }
    }
    Ok(out)
}

/// Sectores de un territorio, para que la interfaz ofrezca una lista en lugar de
/// pedir coordenadas a mano. No devuelve geometrías: son megabytes que el
/// selector no necesita.
pub async fn list(db: &Database, territory_id: &str, kind: Kind) -> Result<Vec<Value>> {
    use futures_util::TryStreamExt;
    let items: Vec<Stored> = db
        .collection::<Stored>("sectors")
        .find(doc! {"territory_id": territory_id, "kind": kind.as_str()})
        .sort(doc! {"code": 1})
        .await?
        .try_collect()
        .await?;
    Ok(items
        .into_iter()
        .map(|s| json!({"code": s.code, "name": s.name, "commune": s.commune}))
        .collect())
}

/// Comprueba que el territorio tiene sectores cargados. Afiliar sin ellos
/// dejaría a todos los suscriptores sin sector y el motor de alertas no podría
/// dirigir nada.
pub async fn ready(db: &Database, territory_id: &str) -> Result<()> {
    let count = db
        .collection::<Document>("sectors")
        .count_documents(doc! {"territory_id": territory_id})
        .await?;
    if count == 0 {
        return Err(error::Error(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "El territorio no tiene sectores cargados",
        ));
    }
    Ok(())
}

// --- Importación -----------------------------------------------------------

#[derive(Deserialize)]
struct Collection {
    features: Vec<Feature>,
}
#[derive(Deserialize)]
struct Feature {
    geometry: Shape,
    properties: Value,
}

pub struct Report {
    pub imported: u64,
    /// Sectores que MongoDB rechazó, con el motivo. Se nombran uno por uno: cada
    /// uno es un trozo de ciudad que quedaría sin cobertura.
    pub rejected: Vec<(String, String)>,
}

/// Importa una capa de límites administrativos para un territorio.
///
/// Igual que el inventario de espacios, es una operación fuera de línea que
/// reemplaza la capa completa del territorio. Los sectores se insertan uno a uno
/// y no en lote porque un único polígono que MongoDB considere inválido abortaría
/// el lote entero y perderíamos los 341 restantes; de este modo el rechazo queda
/// aislado y registrado.
pub async fn import(
    db: &Database,
    territory: &Territory,
    kind: Kind,
    path: &std::path::Path,
) -> anyhow::Result<Report> {
    let data = std::fs::read(path)
        .with_context(|| format!("No se pudo leer la capa en {}", path.display()))?;
    let parsed: Collection =
        serde_json::from_slice(&data).context("La capa no es un GeoJSON válido")?;

    let collection = db.collection::<Document>("sectors");
    collection
        .delete_many(doc! {"territory_id": &territory.id, "kind": kind.as_str()})
        .await?;

    let mut imported = 0u64;
    let mut rejected = Vec::new();
    for f in parsed.features {
        let code = f.properties["code"]
            .as_str()
            .context("Sector sin código")?
            .to_owned();
        let name = f.properties["name"].as_str().unwrap_or(&code).to_owned();
        let id = format!("{}:{}:{code}", territory.id, kind.as_str());
        let document = doc! {
            "_id": &id,
            "territory_id": &territory.id,
            "kind": kind.as_str(),
            "code": &code,
            "name": &name,
            "commune": f.properties["commune"].as_str(),
            "geometry": mongodb::bson::to_bson(&f.geometry)?,
        };
        match collection.insert_one(document).await {
            Ok(_) => imported += 1,
            Err(e) => rejected.push((format!("{name} ({code})"), e.to_string())),
        }
    }
    Ok(Report { imported, rejected })
}
