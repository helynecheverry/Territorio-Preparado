use anyhow::{Result, ensure};
use geo::{
    Area, BooleanOps, BoundingRect, Coord, LineString, MultiPolygon, Polygon, Rect, Validation,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const R: f64 = 6_371_008.8;
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", content = "coordinates")]
pub enum Shape {
    Polygon(Vec<Vec<[f64; 2]>>),
    MultiPolygon(Vec<Vec<Vec<[f64; 2]>>>),
    Point([f64; 2]),
}
impl Shape {
    pub fn polygons(&self) -> Result<Vec<Vec<Vec<[f64; 2]>>>> {
        match self {
            Self::Polygon(p) => Ok(vec![p.clone()]),
            Self::MultiPolygon(p) => Ok(p.clone()),
            Self::Point(_) => {
                anyhow::bail!("El inventario solo tiene un punto: falta una huella medida")
            }
        }
    }
    pub fn vertices(&self) -> usize {
        match self {
            Self::Polygon(p) => p.iter().map(Vec::len).sum(),
            Self::MultiPolygon(p) => p.iter().flatten().map(Vec::len).sum(),
            Self::Point(_) => 1,
        }
    }
}
#[derive(Clone, Copy)]
pub struct Projection {
    pub lon: f64,
    pub lat: f64,
}
impl Projection {
    pub fn for_shape(shape: &Shape) -> Result<Self> {
        let p = shape.polygons()?;
        let point = p
            .first()
            .and_then(|p| p.first())
            .and_then(|r| r.first())
            .ok_or_else(|| anyhow::anyhow!("Huella vacía"))?;
        Ok(Self {
            lon: point[0],
            lat: point[1],
        })
    }
    pub fn project(&self, p: [f64; 2]) -> Result<Coord<f64>> {
        ensure!(
            p[0].is_finite()
                && p[1].is_finite()
                && (-77.0..=-76.0).contains(&p[0])
                && (3.0..=4.0).contains(&p[1]),
            "Coordenadas fuera del ámbito de Cali"
        );
        Ok(Coord {
            x: R * (p[0] - self.lon).to_radians() * self.lat.to_radians().cos(),
            y: R * (p[1] - self.lat).to_radians(),
        })
    }
    pub fn unproject(&self, c: Coord<f64>) -> [f64; 2] {
        [
            self.lon + (c.x / (R * self.lat.to_radians().cos())).to_degrees(),
            self.lat + (c.y / R).to_degrees(),
        ]
    }
    pub fn polygon(&self, shape: &Shape) -> Result<MultiPolygon<f64>> {
        ensure!(shape.vertices() <= 12_000, "Demasiados vértices");
        let mut out = Vec::new();
        for rings in shape.polygons()? {
            ensure!(!rings.is_empty() && rings.len() <= 128, "Anillos inválidos");
            let mut converted = Vec::new();
            for ring in rings {
                ensure!(
                    ring.len() >= 4 && ring.first() == ring.last(),
                    "Cada anillo necesita al menos cuatro coordenadas y debe estar cerrado"
                );
                converted.push(LineString::new(
                    ring.into_iter()
                        .map(|p| self.project(p))
                        .collect::<Result<_>>()?,
                ));
            }
            let exterior = converted.remove(0);
            out.push(Polygon::new(exterior, converted));
        }
        let result = MultiPolygon(out);
        ensure!(
            result.is_valid(),
            "La huella se cruza, solapa o tiene anillos inválidos"
        );
        ensure!(
            result.unsigned_area() > 0.01,
            "La huella no tiene área medible"
        );
        Ok(result)
    }
    pub fn json(&self, geometry: &MultiPolygon<f64>) -> Value {
        json!({"type":"MultiPolygon","coordinates":geometry.0.iter().map(|p|std::iter::once(p.exterior()).chain(p.interiors().iter()).map(|r|r.0.iter().map(|&c|self.unproject(c)).collect::<Vec<_>>()).collect::<Vec<_>>()).collect::<Vec<_>>()})
    }
}
pub fn rect(x: f64, y: f64, w: f64, h: f64) -> Polygon<f64> {
    Rect::new(Coord { x, y }, Coord { x: x + w, y: y + h }).to_polygon()
}
pub fn reserve_south(
    usable: &MultiPolygon<f64>,
    fraction: f64,
) -> (MultiPolygon<f64>, MultiPolygon<f64>) {
    if fraction == 0.0 || usable.unsigned_area() < 0.01 {
        return (MultiPolygon(vec![]), usable.clone());
    }
    let bounds = usable.bounding_rect().expect("nonempty area has bounds");
    let (mut low, mut high) = (bounds.min().y, bounds.max().y);
    let target = usable.unsigned_area() * fraction;
    for _ in 0..28 {
        let mid = (low + high) / 2.0;
        let cut = rect(
            bounds.min().x - 1.0,
            bounds.min().y - 1.0,
            bounds.width() + 2.0,
            mid - bounds.min().y + 1.0,
        );
        if usable.intersection(&cut).unsigned_area() < target {
            low = mid;
        } else {
            high = mid;
        }
    }
    let cut = rect(
        bounds.min().x - 1.0,
        bounds.min().y - 1.0,
        bounds.width() + 2.0,
        high - bounds.min().y + 1.0,
    );
    let reserved = usable.intersection(&cut);
    let placement = usable.difference(&reserved);
    (reserved, placement)
}
