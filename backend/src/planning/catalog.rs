//! Inventario de espacios, respaldado por MongoDB.
//!
//! La revisión anterior cargaba `frontend/dist/data/spaces.json` completo en la
//! memoria de cada instancia: 2.991 espacios de Cali y un único SHA256 global.
//! Eso no sobrevive al alcance nacional, así que el inventario vive ahora en la
//! colección `spaces`, se consulta por territorio y se amplía importando datos.
//!
//! Cada espacio guarda su geometría original **sin índice geoespacial** y, por
//! separado, un punto representativo que sí está indexado con `2dsphere`. La
//! razón es que MongoDB rechaza al insertar los polígonos que considera
//! inválidos, y el inventario de Cali contiene tres; un punto nunca es inválido.
//! Así la validación geométrica sigue siendo la de `geometry.rs`, que es la que
//! `PLANNING.md` documenta, y las consultas por cercanía siguen usando el índice.

use super::geometry::{Projection, Shape};
use crate::{
    error::{self, Result},
    territory::Territory,
};
use anyhow::Context;
use axum::http::StatusCode;
use futures_util::TryStreamExt;
use mongodb::{
    Database,
    bson::{Document, doc},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Digest;
use std::path::Path;

/// Un espacio del inventario, tal como lo consume el motor de planeación.
#[derive(Clone)]
pub struct Space {
    /// `_id` completo, con el territorio como prefijo: `co-cali:epou-8413`.
    pub id: String,
    pub territory_id: String,
    pub properties: Value,
    pub shape: Shape,
    pub measurable: bool,
    pub reason: Option<String>,
}

/// Documento almacenado. Se mantiene separado de `Space` porque incluye campos
/// que solo sirven para consultar (`search`, `point`) o para limpiar
/// importaciones anteriores (`source_sha256`).
#[derive(Deserialize)]
struct Stored {
    #[serde(rename = "_id")]
    id: String,
    territory_id: String,
    properties: Value,
    geometry: Shape,
    measurable: bool,
    #[serde(default)]
    reason: Option<String>,
}
impl From<Stored> for Space {
    fn from(d: Stored) -> Self {
        Self {
            id: d.id,
            territory_id: d.territory_id,
            properties: d.properties,
            shape: d.geometry,
            measurable: d.measurable,
            reason: d.reason,
        }
    }
}

/// Vista corta de un espacio para listados.
pub fn summary(s: &Space) -> Value {
    json!({
        "id": s.id,
        "territory_id": s.territory_id,
        "name": s.properties["name"],
        "neighborhood": s.properties["neighborhood"],
        "commune": s.properties["commune"],
        "type": s.properties["type"],
        "inventory_area_m2": s.properties["areaM2"],
        "measurable": s.measurable,
        "reason": s.reason,
    })
}

/// Un `_id` de espacio aceptable. Se valida antes de llegar a la consulta para
/// que un identificador arbitrario no se convierta en una clave enorme ni en
/// metacaracteres de expresión regular.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
}

pub async fn get(db: &Database, id: &str) -> Result<Space> {
    if !valid_id(id) {
        return Err(error::bad("Identificador de espacio inválido"));
    }
    let found = db
        .collection::<Stored>("spaces")
        .find_one(doc! {"_id": id})
        .await?;
    found
        .map(Space::from)
        .ok_or(error::Error(StatusCode::NOT_FOUND, "Espacio no encontrado"))
}

/// Escapa los metacaracteres de expresión regular de una búsqueda del usuario.
fn escape(needle: &str) -> String {
    let mut out = String::with_capacity(needle.len() * 2);
    for c in needle.chars() {
        if "\\^$.|?*+()[]{}".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub struct Query<'a> {
    pub territory_id: &'a str,
    pub needle: &'a str,
    pub commune: Option<&'a str>,
    pub measurable: Option<bool>,
    pub page: u64,
}

/// Búsqueda por subcadena dentro de un territorio, paginada de 40 en 40.
///
/// El filtro por `territory_id` sí usa índice; la subcadena se evalúa con una
/// expresión regular no anclada, que MongoDB no puede indexar y resuelve
/// recorriendo la partición del territorio. Con unos miles de espacios por
/// ciudad es asumible y conserva exactamente la semántica que ya esperaba el
/// frontend. Cuando una ciudad crezca lo suficiente para que pese, el reemplazo
/// es un índice de texto o Atlas Search, no un cambio de este contrato.
pub async fn search(db: &Database, q: Query<'_>) -> Result<(Vec<Value>, u64)> {
    let mut filter = doc! {"territory_id": q.territory_id};
    if !q.needle.is_empty() {
        filter.insert(
            "search",
            doc! {"$regex": escape(&q.needle.to_lowercase()), "$options": "i"},
        );
    }
    if let Some(commune) = q.commune {
        filter.insert("properties.commune", commune);
    }
    if let Some(measurable) = q.measurable {
        filter.insert("measurable", measurable);
    }
    let collection = db.collection::<Stored>("spaces");
    let total = collection.count_documents(filter.clone()).await?;
    let items: Vec<Stored> = collection
        .find(filter)
        .sort(doc! {"_id": 1})
        .skip(q.page * 40)
        .limit(40)
        .await?
        .try_collect()
        .await?;
    Ok((
        items
            .into_iter()
            .map(|d| summary(&Space::from(d)))
            .collect(),
        total,
    ))
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
    pub sha256: String,
    pub total: u64,
    pub measurable: u64,
    /// Registros que el inventario publica como punto y no como huella. No son
    /// un error: el inventario no midió ese espacio. Se cuentan pero no se
    /// nombran uno por uno, porque son más de mil en Cali.
    pub points: u64,
    /// Huellas poligonales que no pasan la validación geométrica. Estos **sí**
    /// se nombran: cada uno es un dato que alguien debe corregir en la fuente, y
    /// mezclarlos con los puntos los haría invisibles.
    pub rejected: Vec<(String, String)>,
}

/// Punto representativo para ordenar por cercanía: el centro de la caja
/// envolvente de todos los vértices.
///
/// No es el centroide geométrico. Se elige porque no puede fallar sobre una
/// geometría inválida —y hay tres en el inventario de Cali— y porque a escala
/// urbana la diferencia con el centroide es de decenas de metros, irrelevante
/// para decidir a qué albergue caminar. Las medidas de área y la distribución de
/// módulos siguen calculándose con la geometría completa en `engine.rs`.
fn representative_point(shape: &Shape) -> [f64; 2] {
    let mut min = [f64::MAX, f64::MAX];
    let mut max = [f64::MIN, f64::MIN];
    let mut visit = |p: &[f64; 2]| {
        for axis in 0..2 {
            min[axis] = min[axis].min(p[axis]);
            max[axis] = max[axis].max(p[axis]);
        }
    };
    match shape {
        Shape::Point(p) => visit(p),
        Shape::Polygon(rings) => rings.iter().flatten().for_each(&mut visit),
        Shape::MultiPolygon(polygons) => {
            polygons.iter().flatten().flatten().for_each(&mut visit);
        }
    }
    [(min[0] + max[0]) / 2.0, (min[1] + max[1]) / 2.0]
}

/// Importa un inventario GeoJSON para un territorio.
///
/// Es una operación fuera de línea: borra los espacios del territorio y vuelve a
/// insertarlos, así que durante la importación ese territorio queda sin
/// inventario. Reimportar el mismo archivo no cambia nada, porque el SHA256
/// coincide y la función sale antes de tocar la colección. Las propuestas ya
/// guardadas conservan su propia instantánea y su hash de origen, de modo que
/// reimportar nunca las recalcula.
pub async fn import(db: &Database, territory: &Territory, path: &Path) -> anyhow::Result<Report> {
    let data = std::fs::read(path)
        .with_context(|| format!("No se pudo leer el inventario en {}", path.display()))?;
    let sha256 = hex::encode(sha2::Sha256::digest(&data));
    let parsed: Collection =
        serde_json::from_slice(&data).context("El inventario no es un GeoJSON válido")?;

    let mut documents = Vec::with_capacity(parsed.features.len());
    let mut seen = std::collections::HashSet::new();
    let mut rejected = Vec::new();
    let mut measurable_total = 0u64;
    let mut points_total = 0u64;
    for f in parsed.features {
        let source_id = f.properties["id"]
            .as_str()
            .context("Espacio sin identificador")?
            .to_owned();
        anyhow::ensure!(
            seen.insert(source_id.clone()),
            "Identificador de espacio repetido: {source_id}"
        );
        let id = format!("{}:{source_id}", territory.id);
        anyhow::ensure!(valid_id(&id), "Identificador de espacio inválido: {id}");

        // Misma validación que antes: solo las huellas poligonales que proyectan
        // y pasan las comprobaciones de `geometry.rs` son calculables. No se
        // inventa un polígono a partir de un punto.
        let validation = Projection::for_shape(&f.geometry).and_then(|p| p.polygon(&f.geometry));
        let reason = validation.as_ref().err().map(ToString::to_string);
        let measurable = validation.is_ok();
        if measurable {
            measurable_total += 1;
        } else if matches!(f.geometry, Shape::Point(_)) {
            points_total += 1;
        } else if let Some(reason) = &reason {
            rejected.push((id.clone(), reason.clone()));
        }

        let point = representative_point(&f.geometry);
        let search = format!(
            "{source_id} {} {}",
            f.properties["name"].as_str().unwrap_or_default(),
            f.properties["neighborhood"].as_str().unwrap_or_default()
        )
        .to_lowercase();

        documents.push(doc! {
            "_id": id,
            "territory_id": &territory.id,
            "source_id": source_id,
            "properties": mongodb::bson::to_bson(&f.properties)?,
            "geometry": mongodb::bson::to_bson(&f.geometry)?,
            "point": {"type": "Point", "coordinates": [point[0], point[1]]},
            "measurable": measurable,
            "reason": reason,
            "search": search,
            "source_sha256": &sha256,
        });
    }
    let total = documents.len() as u64;

    let collection = db.collection::<Document>("spaces");
    let current = db
        .collection::<Territory>("territories")
        .find_one(doc! {"_id": &territory.id})
        .await?;
    if current.as_ref().is_some_and(|t| t.source_sha256 == sha256)
        && collection
            .count_documents(doc! {"territory_id": &territory.id})
            .await?
            == total
    {
        tracing::info!(territory = %territory.id, "Inventario sin cambios; no se reimporta");
        return Ok(Report {
            sha256,
            total,
            measurable: measurable_total,
            points: points_total,
            rejected,
        });
    }

    collection
        .delete_many(doc! {"territory_id": &territory.id})
        .await?;
    for chunk in documents.chunks(1000) {
        collection.insert_many(chunk.to_vec()).await?;
    }
    db.collection::<Document>("territories")
        .update_one(
            doc! {"_id": &territory.id},
            doc! {"$set": {
                "source_sha256": &sha256,
                "spaces_total": total as i64,
                "measurable_total": measurable_total as i64,
            }},
        )
        .await?;
    Ok(Report {
        sha256,
        total,
        measurable: measurable_total,
        points: points_total,
        rejected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::territory::{Coverage, Layers};

    #[test]
    fn representative_point_of_a_point_is_itself() {
        let p = representative_point(&Shape::Point([-76.5, 3.45]));
        assert_eq!(p, [-76.5, 3.45]);
    }

    #[test]
    fn representative_point_centres_the_bounding_box() {
        let square = Shape::Polygon(vec![vec![
            [-76.6, 3.4],
            [-76.4, 3.4],
            [-76.4, 3.5],
            [-76.6, 3.5],
            [-76.6, 3.4],
        ]]);
        let [lon, lat] = representative_point(&square);
        assert!((lon - -76.5).abs() < 1e-12, "longitud {lon}");
        assert!((lat - 3.45).abs() < 1e-12, "latitud {lat}");
    }

    #[test]
    fn representative_point_spans_every_component() {
        // Dos componentes separadas: el punto representativo cae entre ambas, no
        // dentro de la primera.
        let ring = |x: f64| {
            vec![vec![
                [x, 3.4],
                [x + 0.1, 3.4],
                [x + 0.1, 3.5],
                [x, 3.5],
                [x, 3.4],
            ]]
        };
        let multi = Shape::MultiPolygon(vec![ring(-76.8), ring(-76.4)]);
        let [lon, _] = representative_point(&multi);
        assert!((lon - -76.55).abs() < 1e-12, "longitud {lon}");
    }

    #[test]
    fn representative_point_survives_an_invalid_footprint() {
        // Un anillo que se autocruza no proyecta, pero ordenar por cercanía no
        // debe depender de que la huella sea calculable.
        let bowtie = Shape::Polygon(vec![vec![
            [-76.6, 3.4],
            [-76.4, 3.5],
            [-76.4, 3.4],
            [-76.6, 3.5],
            [-76.6, 3.4],
        ]]);
        assert!(
            Projection::for_shape(&bowtie)
                .and_then(|p| p.polygon(&bowtie))
                .is_err()
        );
        let [lon, lat] = representative_point(&bowtie);
        assert!((lon - -76.5).abs() < 1e-12 && (lat - 3.45).abs() < 1e-12);
    }

    #[test]
    fn search_metacharacters_are_escaped() {
        // Sin escapar, `.*` recuperaría todo el inventario del territorio.
        assert_eq!(escape(".*"), "\\.\\*");
        assert_eq!(escape("vipasa"), "vipasa");
        assert_eq!(escape("a(b)[c]"), "a\\(b\\)\\[c\\]");
    }

    #[test]
    fn space_ids_are_bounded_and_prefixed() {
        assert!(valid_id("co-cali:epou-8413"));
        assert!(!valid_id(""));
        assert!(!valid_id("co-cali:epou 8413"));
        assert!(!valid_id("co-cali:.*"));
        assert!(!valid_id(&"x".repeat(81)));
    }

    #[test]
    fn coverage_follows_the_layers_actually_loaded() {
        let all = Layers {
            flood: true,
            seismic_microzonation: true,
            liquefaction: true,
        };
        assert_eq!(all.coverage(), Coverage::Complete);
        assert!(all.missing().is_empty());

        let some = Layers {
            flood: true,
            ..Layers::default()
        };
        assert_eq!(some.coverage(), Coverage::Partial);
        assert_eq!(
            some.missing(),
            vec!["seismic_microzonation", "liquefaction"]
        );

        let none = Layers::default();
        assert_eq!(none.coverage(), Coverage::Basic);
        assert_eq!(none.missing().len(), 3);
    }
}
