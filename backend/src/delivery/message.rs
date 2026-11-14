//! Redacción del aviso.
//!
//! Es el único texto que una persona va a leer, posiblemente de madrugada y
//! asustada, así que tiene tres obligaciones:
//!
//! 1. **Decir que ya pasó.** «Sismo M6.8 hace 3 min» y no «alerta de sismo». El
//!    aviso llega entre uno y cinco minutos después del evento porque así
//!    publican los catálogos, y presentarlo como una advertencia previa sería
//!    mentir sobre lo único que importa: cuánto tiempo queda.
//! 2. **Decir a dónde ir, y por qué no al de al lado.** Es el hallazgo de
//!    `docs/DATASETS.md` convertido en una frase. Sin el motivo, alguien que ve
//!    un parque a 100 m no va a caminar 900.
//! 3. **No dar una orden que no se puede respaldar.** El sistema opera en modo
//!    informar y recomendar, sin convenio con la autoridad, así que el texto
//!    recomienda y nombra su fuente.

use crate::{AppState, destinations, error::Result, hazards::Level, territory::Territory};
use mongodb::bson::{DateTime, Document};
use serde_json::{Value, json};

/// Longitud a la que se recorta el SMS.
///
/// Un SMS de más de 160 caracteres GSM-7 se parte en varios y se cobra por cada
/// parte, así que el límite es también una decisión de coste. Se permite algo más
/// de un mensaje porque el destino y el motivo no caben en uno solo, pero no se
/// deja crecer sin control.
const SMS_LIMIT: usize = 300;

pub struct Rendered {
    pub title: String,
    pub body: String,
    pub url: String,
    pub sms: String,
}

/// Recorta por frontera de carácter y no por byte, para no partir una tilde por
/// la mitad y dejar el mensaje ilegible.
fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Minutos transcurridos desde el origen del evento.
fn minutes_since(origin: DateTime) -> i64 {
    ((DateTime::now().timestamp_millis() - origin.timestamp_millis()) / 60_000).max(0)
}

/// Cómo se nombra cada nivel de cara a la persona.
fn headline(level: Level) -> &'static str {
    match level {
        Level::Evacuate => "Evacúa",
        Level::Prepare => "Prepárate",
        Level::Info => "Información",
        Level::None => "Información",
    }
}

/// Qué se espera que haga la persona en cada nivel.
fn action(level: Level) -> &'static str {
    match level {
        Level::Evacuate => "Sal hacia un lugar seguro",
        Level::Prepare => "Revisa tu entorno y ten el kit a mano",
        _ => "Sin acción necesaria",
    }
}

/// Clave de caché para la consulta de destinos.
///
/// Redondea a tres decimales, unos 100 m. Dos personas de la misma manzana
/// comparten destino recomendado, así que un sismo que alcanza a un barrio
/// completo hace unas pocas consultas en lugar de una por afiliado.
pub fn area_key(point: [f64; 2]) -> String {
    format!("{:.3},{:.3}", point[0], point[1])
}

/// Destino recomendado más cercano y, si lo hay, el descarte más cercano con su
/// motivo.
async fn destination(
    state: &AppState,
    t: &Territory,
    point: [f64; 2],
    hazard: crate::hazards::HazardType,
) -> Result<(Option<Value>, Option<Value>)> {
    let found = destinations::search(
        state,
        t,
        point,
        &destinations::Options {
            hazard,
            radius_km: state.config.destination_radius_km,
            min_area_m2: state.config.destination_min_area_m2,
            results: 1,
        },
    )
    .await?;
    let best = found["items"].as_array().and_then(|v| v.first()).cloned();
    let rejected = found["closer_but_unsuitable"]
        .as_array()
        .and_then(|v| v.first())
        .cloned();
    Ok((best, rejected))
}

/// Traduce el motivo técnico del descarte a algo que se entienda sin conocer el
/// POT ni la microzonificación sísmica.
fn plain_reason(rejected: &Value) -> Option<String> {
    let reasons: Vec<&str> = rejected["reasons"]
        .as_array()?
        .iter()
        .filter_map(|r| r["requirement"].as_str())
        .collect();
    let liquefaction = reasons.contains(&"liquefaction");
    let flood = reasons.contains(&"flood");
    Some(match (liquefaction, flood) {
        (true, true) => "suelo licuable y riesgo de inundación".into(),
        (true, false) => "suelo licuable".into(),
        (false, true) => "riesgo alto de inundación".into(),
        (false, false) => return None,
    })
}

/// Redacta el aviso de una notificación concreta.
///
/// `destination_cache` evita repetir la consulta geoespacial para gente de la
/// misma manzana durante un mismo reparto.
pub async fn render(
    state: &AppState,
    notification: &Document,
    event: &Document,
    subscription: &Document,
    t: &Territory,
    destination_cache: &mut std::collections::HashMap<String, (Option<Value>, Option<Value>)>,
) -> Result<Rendered> {
    let level =
        Level::parse(notification.get_str("level").unwrap_or("info")).unwrap_or(Level::Info);
    let magnitude = event.get_f64("magnitude").ok();
    let distance_km = notification.get_f64("distance_km").unwrap_or_default();
    let label = subscription.get_str("label").unwrap_or("tu ubicación");
    let minutes = event
        .get_datetime("origin_utc")
        .map(|d| minutes_since(*d))
        .unwrap_or(0);

    // Siempre en pasado: el aviso llega después del evento y el texto no puede
    // insinuar lo contrario.
    let what = match magnitude {
        Some(m) => format!("Sismo M{m:.1} hace {minutes} min, a {distance_km:.0} km de {label}"),
        None => format!("Evento a {distance_km:.0} km de {label}"),
    };

    let mut body = format!("{what}. {}.", action(level));
    let mut sms = format!("{}: {what}. ", headline(level).to_uppercase());
    let url = format!("{}/app/", state.config.origin);

    // El destino solo se busca para una evacuación. Para `info` o `prepare` nadie
    // se mueve, así que la consulta geoespacial no se hace.
    if level == Level::Evacuate {
        let point = subscription
            .get_document("point")
            .ok()
            .and_then(|p| p.get_array("coordinates").ok())
            .and_then(|c| Some([c.first()?.as_f64()?, c.get(1)?.as_f64()?]));
        if let Some(point) = point {
            let hazard = crate::hazards::HazardType::Earthquake;
            let key = area_key(point);
            let found = match destination_cache.get(&key) {
                Some(v) => v.clone(),
                None => {
                    let v = destination(state, t, point, hazard).await?;
                    destination_cache.insert(key, v.clone());
                    v
                }
            };
            match &found.0 {
                Some(best) => {
                    let name = best["name"].as_str().unwrap_or("un espacio del inventario");
                    let metres =
                        (best["straight_line_km"].as_f64().unwrap_or(0.0) * 1000.0).round();
                    body.push_str(&format!(
                        " Destino recomendado: {name}, a unos {metres:.0} m en línea recta."
                    ));
                    sms.push_str(&format!("Ve a: {name} ({metres:.0} m). "));
                    // El motivo del descarte es lo que convence a alguien de no
                    // quedarse en el parque de al lado.
                    if let Some(reason) = found.1.as_ref().and_then(plain_reason) {
                        let near = found
                            .1
                            .as_ref()
                            .and_then(|r| r["name"].as_str())
                            .unwrap_or("");
                        body.push_str(&format!(
                            " No uses {near}, aunque esté más cerca: {reason}."
                        ));
                        sms.push_str(&format!("NO {near}: {reason}. "));
                    }
                }
                None => {
                    // No encontrar destino apto es información, no un hueco que
                    // convenga rellenar con el espacio más cercano sin comprobar.
                    body.push_str(
                        " No se encontró un espacio apto cerca; busca una zona despejada, lejos de taludes y del río.",
                    );
                    sms.push_str("Sin espacio apto cerca; busca zona despejada. ");
                }
            }
        }
    }

    let source = event.get_str("source").unwrap_or("catálogo sísmico");
    body.push_str(&format!(" Fuente: {source}. Recomendación automática sujeta a revisión; no sustituye a la autoridad de gestión del riesgo."));
    sms.push_str(&url);

    Ok(Rendered {
        title: format!(
            "{}: {}",
            headline(level),
            magnitude
                .map(|m| format!("sismo M{m:.1}"))
                .unwrap_or_else(|| "evento".into())
        ),
        body,
        url,
        sms: clip(&sms, SMS_LIMIT),
    })
}

/// Cuerpo que recibe el trabajador de servicio del navegador.
pub fn push_payload(r: &Rendered, level: Level, hazard: crate::hazards::HazardType) -> Vec<u8> {
    json!({
        "title": r.title,
        "body": r.body,
        "url": r.url,
        "level": level.as_str(),
        // Para que el cliente abra la lección correcta del paquete que ya tiene
        // guardado, sin depender de la red en ese momento.
        "hazard_type": hazard.as_str(),
        // Web Push no garantiza entrega ni orden, así que el cliente necesita
        // poder descartar un aviso viejo que llega tarde.
        "sent_at": DateTime::now().to_string(),
    })
    .to_string()
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_respects_character_boundaries() {
        // Con tildes, recortar por bytes partiría un carácter y dejaría basura.
        let text = "á".repeat(400);
        let out = clip(&text, 300);
        assert_eq!(out.chars().count(), 300);
        assert!(out.ends_with('…'));
        assert_eq!(clip("corto", 300), "corto");
        // El límite exacto no se recorta.
        assert_eq!(clip(&"a".repeat(300), 300).chars().count(), 300);
    }

    #[test]
    fn nearby_blocks_are_a_single_cache_entry() {
        // Dos puntos de la misma manzana comparten clave, que es lo que evita una
        // consulta geoespacial por afiliado durante un sismo grande.
        assert_eq!(
            area_key([-76.520_1, 3.450_2]),
            area_key([-76.520_4, 3.450_1])
        );
        // A un barrio de distancia ya no.
        assert_ne!(area_key([-76.520, 3.450]), area_key([-76.530, 3.450]));
    }

    #[test]
    fn levels_name_what_to_do() {
        assert_eq!(headline(Level::Evacuate), "Evacúa");
        assert_eq!(action(Level::Evacuate), "Sal hacia un lugar seguro");
        assert_eq!(action(Level::Info), "Sin acción necesaria");
        // Un nivel que no avisa nunca llega aquí, pero si llegara no debe
        // presentarse como una evacuación.
        assert_eq!(action(Level::None), "Sin acción necesaria");
    }

    #[test]
    fn rejection_reasons_are_said_in_plain_words() {
        let both = json!({"reasons": [
            {"requirement": "liquefaction"}, {"requirement": "flood"}
        ]});
        assert_eq!(
            plain_reason(&both).unwrap(),
            "suelo licuable y riesgo de inundación"
        );
        let only_soil = json!({"reasons": [{"requirement": "liquefaction"}]});
        assert_eq!(plain_reason(&only_soil).unwrap(), "suelo licuable");
        let only_water = json!({"reasons": [{"requirement": "flood"}]});
        assert_eq!(
            plain_reason(&only_water).unwrap(),
            "riesgo alto de inundación"
        );
        // Sin motivos reconocibles no se inventa una explicación.
        assert!(plain_reason(&json!({"reasons": []})).is_none());
        assert!(plain_reason(&json!({})).is_none());
    }

    #[test]
    fn elapsed_time_never_goes_negative() {
        // Un catálogo puede publicar un origen con unos segundos de deriva
        // respecto al reloj del servidor, y «hace -1 min» no se puede mandar.
        let future = DateTime::from_millis(DateTime::now().timestamp_millis() + 120_000);
        assert_eq!(minutes_since(future), 0);
    }

    #[test]
    fn the_push_payload_carries_what_the_client_needs() {
        let r = Rendered {
            title: "Evacúa: sismo M6.8".into(),
            body: "cuerpo".into(),
            url: "http://127.0.0.1:4180/app/".into(),
            sms: "sms".into(),
        };
        let payload = push_payload(&r, Level::Evacuate, crate::hazards::HazardType::Earthquake);
        let parsed: Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(parsed["title"], "Evacúa: sismo M6.8");
        assert_eq!(parsed["level"], "evacuate");
        assert_eq!(
            parsed["hazard_type"], "earthquake",
            "el cliente necesita saber qué lección abrir sin red"
        );
        assert!(
            parsed["sent_at"].is_string(),
            "el cliente necesita descartar avisos viejos"
        );
    }
}
