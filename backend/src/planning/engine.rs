use super::{
    catalog::Space,
    geometry::{Projection, Shape, rect, reserve_south},
};
use anyhow::{Result, ensure};
use geo::{Area, BooleanOps, BoundingRect, Contains, MultiPolygon, Polygon};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const VERSION: &str = "territorio-geometric-planner/1.0.0";
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Camp,
    Covered,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanInput {
    pub space_id: String,
    pub profile: Profile,
    pub days: u32,
    pub target_people: Option<u32>,
    pub service_reserve_percent: f64,
    pub water_liters_per_day: Option<f64>,
    pub toilets_available: Option<u32>,
    pub kits_available: Option<u32>,
    pub evacuation_capacity: Option<u32>,
    pub covered_footprint_confirmed: bool,
    #[serde(default)]
    pub exclusions: Vec<Shape>,
    pub request_key: Option<String>,
}
pub fn validate(i: &PlanInput) -> Result<()> {
    ensure!(
        (1..=30).contains(&i.days),
        "Duración permitida: 1 a 30 días"
    );
    ensure!(
        i.target_people.is_none_or(|n| n <= 20_000),
        "El objetivo supera 20.000 personas"
    );
    ensure!(
        i.service_reserve_percent.is_finite() && (0.0..=70.0).contains(&i.service_reserve_percent),
        "Reserva de servicios: 0 a 70 %"
    );
    ensure!(
        i.water_liters_per_day
            .is_none_or(|v| v.is_finite() && (0.0..=10_000_000.0).contains(&v)),
        "Agua disponible inválida"
    );
    ensure!(
        [i.toilets_available, i.kits_available, i.evacuation_capacity]
            .iter()
            .all(|v| v.is_none_or(|n| n <= 100_000)),
        "Disponibilidad de recursos inválida"
    );
    ensure!(
        i.exclusions.len() <= 20 && i.exclusions.iter().map(Shape::vertices).sum::<usize>() <= 500,
        "Exclusiones: máximo 20 polígonos y 500 vértices"
    );
    ensure!(
        !matches!(i.profile, Profile::Covered) || i.covered_footprint_confirmed,
        "Confirma que la huella representa superficie cubierta medida; un parque no equivale a un recinto cubierto"
    );
    Ok(())
}
fn pack(
    placement: &MultiPolygon<f64>,
    width: f64,
    depth: f64,
    max: usize,
) -> Result<Vec<(Polygon<f64>, Polygon<f64>)>> {
    let Some(b) = placement.bounding_rect() else {
        return Ok(vec![]);
    };
    let pitch_x = width + 3.0;
    let pitch_y = depth + 3.0;
    let nx = (b.width() / pitch_x).floor() as usize;
    let ny = (b.height() / pitch_y).floor() as usize;
    ensure!(
        nx.saturating_mul(ny) <= 40_000,
        "La huella requiere una partición territorial antes de distribuir módulos"
    );
    let mut modules = Vec::new();
    for row in 0..ny {
        for col in 0..nx {
            let x = b.min().x + col as f64 * pitch_x;
            let y = b.min().y + row as f64 * pitch_y;
            let envelope = rect(x, y, pitch_x, pitch_y);
            if placement.contains(&envelope) {
                modules.push((rect(x + 1.5, y + 1.5, width, depth), envelope));
                if modules.len() >= max {
                    return Ok(modules);
                }
            }
        }
    }
    Ok(modules)
}
fn requirements(people: u32, days: u32) -> Value {
    json!({"people":people,"days":days,"shelter_kits_20_people":people.div_ceil(20),"toilets":people.div_ceil(20),"water_liters_per_day":people as u64*15,"water_liters_total":people as u64*15*days as u64,"sleeping_places":people,"meal_servings_total":people as u64*3*days as u64})
}
fn feature(projection: &Projection, g: &MultiPolygon<f64>, properties: Value) -> Value {
    json!({"type":"Feature","geometry":projection.json(g),"properties":properties})
}
fn round(n: f64) -> f64 {
    (n * 100.0).round() / 100.0
}
pub fn calculate(space: &Space, input: &PlanInput, source_hash: &str) -> Result<Value> {
    validate(input)?;
    ensure!(
        space.measurable,
        "{}",
        space.reason.as_deref().unwrap_or("Falta geometría medible")
    );
    let p = Projection::for_shape(&space.shape)?;
    let site = p.polygon(&space.shape)?;
    let mut usable = site.clone();
    for exclusion in &input.exclusions {
        usable = usable.difference(&p.polygon(exclusion)?);
    }
    let total = site.unsigned_area();
    let useful = usable.unsigned_area();
    let excluded = site.difference(&usable);
    let (reserved, placement) = reserve_south(&usable, input.service_reserve_percent / 100.0);
    let area_limit = match input.profile {
        Profile::Camp => (useful / 45.0).floor() as u32,
        Profile::Covered => (placement.unsigned_area() / 3.5).floor() as u32,
    };
    let a = pack(&placement, 10.0, 7.0, 1500)?;
    let b = pack(&placement, 7.0, 10.0, 1500)?;
    let (packing, rotation) = if b.len() > a.len() { (b, 90) } else { (a, 0) };
    let geometric_capacity = area_limit.min(packing.len() as u32 * 20);
    let mut limits = vec![
        json!({"factor":"area","people":area_limit}),
        json!({"factor":"layout","people":packing.len() as u32*20}),
    ];
    let mut pending = vec![
        "Inspección y autorización del espacio",
        "Pendientes, drenaje y estabilidad del suelo",
        "Accesos y rutas de evacuación verificados",
        "Accesibilidad y distribución de servicios",
        "Estado actual y fecha de las capas territoriales",
    ];
    for (name, value) in [
        (
            "water",
            input
                .water_liters_per_day
                .map(|v| (v / 15.0).floor() as u32),
        ),
        ("toilets", input.toilets_available.map(|n| n * 20)),
        ("kits", input.kits_available.map(|n| n * 20)),
        ("evacuation", input.evacuation_capacity),
    ] {
        if let Some(n) = value {
            limits.push(json!({"factor":name,"people":n}));
        } else {
            pending.push(match name {
                "water" => "Agua disponible por día sin verificar",
                "toilets" => "Baños disponibles sin verificar",
                "kits" => "Módulos disponibles sin verificar",
                _ => "Capacidad de evacuación sin verificar",
            });
        }
    }
    let constrained = limits
        .iter()
        .filter_map(|l| l["people"].as_u64())
        .min()
        .unwrap_or(0) as u32;
    let target = input.target_people.unwrap_or(geometric_capacity);
    let proposed = target.min(constrained);
    let bottlenecks: Vec<_> = limits
        .iter()
        .filter(|l| l["people"].as_u64() == Some(constrained as u64))
        .map(|l| l["factor"].clone())
        .collect();
    let mut layout = Vec::new();
    for (geometry, role) in [(&reserved, "services_reserve"), (&excluded, "excluded")] {
        if geometry.unsigned_area() > 0.001 {
            layout.push(feature(&p, geometry, json!({"role":role})));
        }
    }
    for (index, (kit, envelope)) in packing
        .iter()
        .take(proposed.div_ceil(20) as usize)
        .enumerate()
    {
        let people = (proposed - index as u32 * 20).min(20);
        layout.push(feature(
            &p,
            &MultiPolygon(vec![envelope.clone()]),
            json!({"role":"clearance","module":index+1}),
        ));
        layout.push(feature(&p,&MultiPolygon(vec![kit.clone()]),json!({"role":"shelter_module","module":index+1,"people":people,"height_m":3,"rotation_degrees":rotation})));
    }
    let alerts = json!({"flood":space.properties["flood"],"seismic":space.properties["seismic"],"method":space.properties["assessmentMethod"],"meaning":"Intersecciones del inventario; no se descuentan como polígonos de amenaza ni confirman seguridad"});
    let declared = space.properties["areaM2"].as_f64();
    Ok(json!({
        "schema_version":1,"engine":VERSION,"status":"proposal_requires_review","operational_capacity":null,
        "space":{"id":space.id,"name":space.properties["name"],"commune":space.properties["commune"],"neighborhood":space.properties["neighborhood"],"geometry":space.shape,"inventory_area_m2":declared},
        "source":{"file":"data/spaces.json","sha256":source_hash},
        "areas":{"footprint_m2":round(total),"excluded_union_m2":round(total-useful),"usable_m2":round(useful),"services_reserve_m2":round(reserved.unsigned_area()),"placement_m2":round(placement.unsigned_area()),"inventory_difference_percent":declared.filter(|v|*v>0.0).map(|v|round((total-v)/v*100.0))},
        "capacity":{"geometry_people":geometric_capacity,"known_resource_limit_people":constrained,"target_people":input.target_people,"target_basis":if input.target_people.is_some(){"operator_scenario"}else{"geometric_capacity_scenario"},"proposed_people":proposed,"unserved_target_people":target.saturating_sub(proposed),"limits":limits,"bottlenecks":bottlenecks,"layout_slots_found":packing.len(),"placed_modules":proposed.div_ceil(20)},
        "requirements_for_target":requirements(target,input.days),"requirements_for_proposal":requirements(proposed,input.days),
        "shortages_for_target":{"water_liters_per_day":input.water_liters_per_day.map(|v|(target as f64*15.0-v).max(0.0)),"toilets":input.toilets_available.map(|n|target.div_ceil(20).saturating_sub(n)),"kits":input.kits_available.map(|n|target.div_ceil(20).saturating_sub(n)),"evacuation_places":input.evacuation_capacity.map(|n|target.saturating_sub(n))},
        "verification_pending":pending,"catalogue_alerts":alerts,
        "assumptions":{"profile":input.profile,"area_per_person_m2":if matches!(input.profile,Profile::Camp){45.0}else{3.5},"camp_area_includes_services":true,"service_reserve_method":"south_strip_by_area","module_width_m":10,"module_depth_m":7,"module_people":20,"module_covered_area_m2":70,"clearance_between_modules_m":3,"packing_method":"bounded_grid_best_of_0_and_90_degrees_not_global_optimum","projection":"local equirectangular, R=6371008.8 m; aproximación de planeación, no levantamiento topográfico","projection_origin":[p.lon,p.lat],"toilet_ratio":20,"water_liters_per_person_day":15,"meal_servings_per_person_day":3,"meals_are_project_assumption":true},
        "limitations":["No es una certificación de aforo ni una autorización de albergue","La población presente y la población damnificada no se infieren del área","No se han detectado obstáculos automáticamente: solo se descuentan las exclusiones declaradas","Los corredores geométricos no acreditan conexión con una salida ni ruta segura","La reserva de servicios no diseña por sí sola baños, accesibilidad, salud o protección","Las referencias de recursos requieren adaptación al contexto y protocolo de la entidad"],
        "layout":{"type":"FeatureCollection","features":layout},"visual_analysis":{"performed":false,"basis":"inventory_polygon_and_operator_exclusions"}
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::{Coord, Intersects, polygon};
    fn shape(rings: Vec<Vec<(f64, f64)>>) -> Shape {
        let p = Projection {
            lon: -76.5,
            lat: 3.45,
        };
        Shape::Polygon(
            rings
                .into_iter()
                .map(|r| {
                    r.into_iter()
                        .map(|(x, y)| p.unproject(Coord { x, y }))
                        .collect()
                })
                .collect(),
        )
    }
    fn rectangle(x: f64, y: f64, w: f64, h: f64) -> Shape {
        shape(vec![vec![
            (x, y),
            (x + w, y),
            (x + w, y + h),
            (x, y + h),
            (x, y),
        ]])
    }
    fn fixture() -> (Space, PlanInput) {
        (
            Space {
                id: "test".into(),
                properties: json!({"name":"Test","areaM2":10000,"flood":[],"seismic":[]}),
                shape: rectangle(0., 0., 100., 100.),
                measurable: true,
                reason: None,
            },
            PlanInput {
                space_id: "test".into(),
                profile: Profile::Camp,
                days: 3,
                target_people: Some(300),
                service_reserve_percent: 30.,
                water_liters_per_day: None,
                toilets_available: None,
                kits_available: None,
                evacuation_capacity: None,
                covered_footprint_confirmed: false,
                exclusions: vec![],
                request_key: None,
            },
        )
    }
    #[test]
    fn resources_limit_capacity_and_unknown_is_not_zero() {
        let (s, mut i) = fixture();
        let unbounded = calculate(&s, &i, "hash").unwrap();
        assert!(unbounded["capacity"]["proposed_people"].as_u64().unwrap() > 80);
        assert!(unbounded["operational_capacity"].is_null());
        i.water_liters_per_day = Some(1500.);
        i.toilets_available = Some(4);
        i.kits_available = Some(10);
        i.evacuation_capacity = Some(90);
        let plan = calculate(&s, &i, "hash").unwrap();
        assert_eq!(plan["capacity"]["proposed_people"], 80);
        assert_eq!(plan["capacity"]["bottlenecks"], json!(["toilets"]));
        assert_eq!(plan["capacity"]["unserved_target_people"], 220);
        assert_eq!(
            plan["requirements_for_proposal"]["water_liters_total"],
            3600
        );
        i.water_liters_per_day = Some(0.);
        let plan = calculate(&s, &i, "hash").unwrap();
        assert_eq!(plan["capacity"]["proposed_people"], 0);
        assert_eq!(plan["capacity"]["placed_modules"], 0);
    }
    #[test]
    fn overlapping_and_external_exclusions_are_not_double_counted() {
        let (s, mut i) = fixture();
        i.exclusions = vec![
            rectangle(0., 0., 20., 20.),
            rectangle(10., 0., 20., 20.),
            rectangle(150., 150., 20., 20.),
        ];
        let p = calculate(&s, &i, "hash").unwrap();
        assert!((p["areas"]["excluded_union_m2"].as_f64().unwrap() - 600.).abs() < 1.);
        assert!((p["areas"]["services_reserve_m2"].as_f64().unwrap() - 2820.).abs() < 1.);
    }
    #[test]
    fn all_modules_and_clearances_stay_inside_with_holes() {
        let (mut s, i) = fixture();
        s.shape = shape(vec![
            vec![(0., 0.), (100., 0.), (100., 100.), (0., 100.), (0., 0.)],
            vec![(35., 35.), (35., 65.), (65., 65.), (65., 35.), (35., 35.)],
        ]);
        let p = calculate(&s, &i, "hash").unwrap();
        let projection = Projection::for_shape(&s.shape).unwrap();
        let boundary = projection.polygon(&s.shape).unwrap();
        let mut envelopes = vec![];
        for f in p["layout"]["features"].as_array().unwrap() {
            let role = f["properties"]["role"].as_str().unwrap();
            if role == "clearance" || role == "shelter_module" {
                let shape: Shape = serde_json::from_value(f["geometry"].clone()).unwrap();
                let poly = projection.polygon(&shape).unwrap();
                assert!(boundary.contains(&poly));
                if role == "clearance" {
                    envelopes.push(poly);
                }
            }
        }
        assert!(!envelopes.is_empty());
        for a in 0..envelopes.len() {
            for b in a + 1..envelopes.len() {
                assert!(envelopes[a].intersection(&envelopes[b]).unsigned_area() < 0.001);
            }
        }
        let placed: u64 = p["layout"]["features"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|f| f["properties"]["people"].as_u64())
            .sum();
        assert_eq!(placed, p["capacity"]["proposed_people"].as_u64().unwrap());
    }
    #[test]
    fn concave_boundary_is_checked_beyond_corners() {
        let boundary = MultiPolygon(vec![
            geo::polygon![(x:0.,y:0.),(x:20.,y:0.),(x:20.,y:20.),(x:12.,y:20.),(x:12.,y:7.),(x:8.,y:7.),(x:8.,y:20.),(x:0.,y:20.),(x:0.,y:0.)],
        ]);
        let modules = pack(&boundary, 10., 7., 1500).unwrap();
        for (_, envelope) in modules {
            assert!(boundary.contains(&envelope));
        }
    }
    #[test]
    fn point_and_self_intersection_are_rejected() {
        let (mut s, i) = fixture();
        s.shape = Shape::Point([-76.5, 3.45]);
        assert!(calculate(&s, &i, "hash").is_err());
        s.shape = shape(vec![vec![
            (0., 0.),
            (20., 20.),
            (0., 20.),
            (20., 0.),
            (0., 0.),
        ]]);
        assert!(calculate(&s, &i, "hash").is_err());
    }
    #[test]
    fn profiles_require_explicit_confirmation_and_differ() {
        let (s, mut i) = fixture();
        i.profile = Profile::Covered;
        assert!(calculate(&s, &i, "hash").is_err());
        i.covered_footprint_confirmed = true;
        let covered = calculate(&s, &i, "hash").unwrap();
        i.profile = Profile::Camp;
        let camp = calculate(&s, &i, "hash").unwrap();
        assert!(
            covered["capacity"]["geometry_people"].as_u64()
                > camp["capacity"]["geometry_people"].as_u64()
        );
    }
    #[test]
    fn zero_target_and_fully_excluded_site_have_no_modules() {
        let (s, mut i) = fixture();
        i.target_people = Some(0);
        let p = calculate(&s, &i, "hash").unwrap();
        assert_eq!(p["capacity"]["proposed_people"], 0);
        i.target_people = Some(100);
        i.exclusions = vec![s.shape.clone()];
        let p = calculate(&s, &i, "hash").unwrap();
        assert_eq!(p["capacity"]["proposed_people"], 0);
    }
    #[test]
    fn deterministic_and_invalid_parameters() {
        let (s, mut i) = fixture();
        assert_eq!(
            calculate(&s, &i, "hash").unwrap(),
            calculate(&s, &i, "hash").unwrap()
        );
        i.days = 0;
        assert!(validate(&i).is_err());
        i.days = 3;
        i.water_liters_per_day = Some(-1.);
        assert!(validate(&i).is_err());
    }
    #[test]
    fn disconnected_components_are_not_bridged() {
        let a = rectangle(0., 0., 30., 40.);
        let b = rectangle(100., 100., 30., 40.);
        let polygons = vec![
            a.polygons().unwrap().remove(0),
            b.polygons().unwrap().remove(0),
        ];
        let shape = Shape::MultiPolygon(polygons);
        let projection = Projection::for_shape(&shape).unwrap();
        let boundary = projection.polygon(&shape).unwrap();
        for (_, p) in pack(&boundary, 10., 7., 1500).unwrap() {
            assert!(boundary.contains(&p));
            assert!(!p.intersects(&rect(40., 40., 40., 40.)));
        }
    }
}
