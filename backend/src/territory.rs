//! Territorios y nivel de cobertura de datos.
//!
//! Un territorio es la unidad de despliegue: un país y una ciudad con su propio
//! inventario de espacios y sus capas de amenaza. Hasta esta revisión el backend
//! cargaba un único inventario de Cali en memoria; ahora cada territorio vive en
//! MongoDB y se añade importando datos, sin recompilar ni redesplegar.
//!
//! `Layers` declara qué capas de amenaza existen para el territorio. El nivel de
//! cobertura se deriva de ellas y nunca se escribe a mano, porque la ausencia de
//! una capa no significa ausencia de riesgo: si falta la capa de inundación, las
//! recomendaciones del territorio deben decir que no se verificó inundación, no
//! omitir el tema.

use crate::error::{self, Result};
use mongodb::{
    Database,
    bson::{Document, doc},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Cuánto se puede afirmar sobre un territorio con los datos cargados.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// Inventario y las tres capas de amenaza: se puede filtrar un albergue por
    /// aptitud ante sismo y ante inundación.
    Complete,
    /// Inventario y parte de las capas: se filtra por lo disponible y se declara
    /// lo que falta.
    Partial,
    /// Solo inventario: distancia, área y capacidad. Sin filtro de amenaza.
    Basic,
}
impl Coverage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Basic => "basic",
        }
    }
}

/// Capas de amenaza cargadas para un territorio.
#[derive(Clone, Copy, Default, Debug, Serialize, Deserialize)]
pub struct Layers {
    /// Amenaza de inundación fluvial y pluvial (en Cali, polígonos del POT).
    #[serde(default)]
    pub flood: bool,
    /// Microzonificación sísmica.
    #[serde(default)]
    pub seismic_microzonation: bool,
    /// Suelos licuables.
    #[serde(default)]
    pub liquefaction: bool,
}
impl Layers {
    /// Cada capa con su nombre, en el orden en que se declaran. Añadir una capa
    /// nueva aquí es lo único que hace falta: el nivel de cobertura y la lista de
    /// faltantes se derivan de esta tabla.
    fn entries(self) -> [(&'static str, bool); 3] {
        [
            ("flood", self.flood),
            ("seismic_microzonation", self.seismic_microzonation),
            ("liquefaction", self.liquefaction),
        ]
    }
    /// Nivel derivado de las capas presentes. No se almacena: se recalcula para
    /// que no pueda contradecir a los datos.
    pub fn coverage(self) -> Coverage {
        let present = self.entries().iter().filter(|(_, has)| *has).count();
        match present {
            0 => Coverage::Basic,
            n if n == self.entries().len() => Coverage::Complete,
            _ => Coverage::Partial,
        }
    }
    /// Capas ausentes, para declararlas junto a cada recomendación.
    pub fn missing(self) -> Vec<&'static str> {
        self.entries()
            .into_iter()
            .filter(|(_, has)| !has)
            .map(|(name, _)| name)
            .collect()
    }
}

/// Rutas de los archivos de datos de un territorio.
///
/// Solo se usan al importar, y se resuelven desde el directorio en el que se
/// ejecuta el comando (`backend`). Declararlas aquí es lo que permite que añadir
/// una ciudad sea un archivo JSON y una orden, sin tocar el código.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Sources {
    /// Inventario de espacios aprovechables, en GeoJSON.
    #[serde(default)]
    pub spaces: Option<String>,
    /// Límites de barrios, veredas o sectores censales.
    #[serde(default)]
    pub neighborhoods: Option<String>,
    /// Límites de comunas, localidades o distritos.
    #[serde(default)]
    pub communes: Option<String>,
}

/// Un territorio con inventario cargado.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Territory {
    /// Identificador estable en minúsculas, por ejemplo `co-cali`. Prefija el
    /// `_id` de cada espacio para que dos ciudades puedan reutilizar el mismo
    /// identificador de origen sin colisionar.
    /// En MongoDB es `_id`; el alias permite escribir `id` en el archivo de
    /// declaración del territorio, que se edita a mano.
    #[serde(rename = "_id", alias = "id")]
    pub id: String,
    /// Código ISO 3166-1 alfa-2, por ejemplo `CO`.
    pub country_code: String,
    /// División administrativa mayor: departamento, estado o provincia.
    pub admin1: String,
    /// Nombre de la ciudad o municipio.
    pub name: String,
    /// Zona IANA, por ejemplo `America/Bogota`. Las alertas se fechan en la hora
    /// local de quien las recibe, no en la del servidor.
    pub timezone: String,
    /// Etiqueta BCP 47, por ejemplo `es-CO`.
    pub locale: String,
    #[serde(default)]
    pub layers: Layers,
    /// Archivos de datos a importar. No participa en las respuestas de la API.
    #[serde(default)]
    pub sources: Sources,
    /// SHA256 del archivo de inventario importado. Queda congelado en cada
    /// propuesta guardada, así que reimportar no altera propuestas anteriores.
    #[serde(default)]
    pub source_sha256: String,
    #[serde(default)]
    pub spaces_total: u64,
    #[serde(default)]
    pub measurable_total: u64,
}
impl Territory {
    /// Vista pública, con la cobertura y las capas faltantes explícitas.
    pub fn summary(&self) -> Value {
        json!({
            "id": self.id,
            "country_code": self.country_code,
            "admin1": self.admin1,
            "name": self.name,
            "timezone": self.timezone,
            "locale": self.locale,
            "coverage": self.layers.coverage().as_str(),
            "layers": self.layers,
            "missing_layers": self.layers.missing(),
            "spaces_total": self.spaces_total,
            "measurable_total": self.measurable_total,
            "source_sha256": self.source_sha256,
        })
    }
    /// Identificador válido: minúsculas, dígitos y guiones. Se usa como prefijo
    /// de los `_id` de espacios, así que no puede contener dos puntos.
    pub fn valid_id(id: &str) -> bool {
        !id.is_empty()
            && id.len() <= 40
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    }
}

pub async fn get(db: &Database, id: &str) -> Result<Territory> {
    let found = db
        .collection::<Territory>("territories")
        .find_one(doc! {"_id": id})
        .await?;
    found.ok_or(error::Error(
        axum::http::StatusCode::NOT_FOUND,
        "Territorio no encontrado",
    ))
}

/// Territorios disponibles, ordenados por país y nombre. La lista es pequeña
/// (una entrada por ciudad) y se consulta sin paginar.
pub async fn list(db: &Database) -> Result<Vec<Territory>> {
    use futures_util::TryStreamExt;
    Ok(db
        .collection::<Territory>("territories")
        .find(doc! {})
        .sort(doc! {"country_code": 1, "name": 1})
        .await?
        .try_collect()
        .await?)
}

/// Crea o actualiza el territorio conservando los totales que calcula la
/// importación del inventario.
pub async fn upsert(db: &Database, t: &Territory) -> anyhow::Result<()> {
    db.collection::<Document>("territories")
        .update_one(
            doc! {"_id": &t.id},
            doc! {"$set": {
                "country_code": &t.country_code,
                "admin1": &t.admin1,
                "name": &t.name,
                "timezone": &t.timezone,
                "locale": &t.locale,
                "layers": mongodb::bson::to_bson(&t.layers)?,
            }},
        )
        .upsert(true)
        .await?;
    Ok(())
}
