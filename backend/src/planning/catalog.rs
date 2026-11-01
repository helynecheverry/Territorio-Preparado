use super::geometry::{Projection, Shape};
use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::Digest;
use std::{collections::BTreeMap, path::Path};
#[derive(Clone)]
pub struct Space {
    pub id: String,
    pub properties: Value,
    pub shape: Shape,
    pub measurable: bool,
    pub reason: Option<String>,
}
pub struct Catalog {
    pub spaces: BTreeMap<String, Space>,
    pub sha256: String,
}
#[derive(Deserialize)]
struct Collection {
    features: Vec<Feature>,
}
#[derive(Deserialize)]
struct Feature {
    geometry: Shape,
    properties: Value,
}
impl Catalog {
    pub fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read(path)?;
        let sha256 = hex::encode(sha2::Sha256::digest(&data));
        let parsed: Collection = serde_json::from_slice(&data)?;
        let mut spaces = BTreeMap::new();
        for f in parsed.features {
            let id = f.properties["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Space without ID"))?
                .to_owned();
            let validation =
                Projection::for_shape(&f.geometry).and_then(|p| p.polygon(&f.geometry));
            let reason = validation.as_ref().err().map(ToString::to_string);
            let item = Space {
                id: id.clone(),
                properties: f.properties,
                shape: f.geometry,
                measurable: validation.is_ok(),
                reason,
            };
            anyhow::ensure!(spaces.insert(id, item).is_none(), "Duplicate space ID");
        }
        Ok(Self { spaces, sha256 })
    }
    pub fn summary(&self, s: &Space) -> Value {
        json!({"id":s.id,"name":s.properties["name"],"neighborhood":s.properties["neighborhood"],"commune":s.properties["commune"],"type":s.properties["type"],"inventory_area_m2":s.properties["areaM2"],"measurable":s.measurable,"reason":s.reason})
    }
}
