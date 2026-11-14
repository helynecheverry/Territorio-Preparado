//! Afiliación: a qué sectores quiere una persona recibir avisos.
//!
//! Una afiliación es un punto del territorio —la casa, el trabajo, la escuela de
//! los hijos— con el sector resuelto, los canales por los que avisar y el nivel
//! mínimo a partir del cual se avisa. Una persona puede tener varias, porque un
//! sismo a media tarde la encuentra en un sitio distinto que de madrugada.
//!
//! # Datos personales
//!
//! La ubicación de la vivienda de alguien es dato sensible, así que:
//!
//! - El consentimiento es explícito, obligatorio y queda registrado con la
//!   versión del texto aceptado. Sin él la creación se rechaza.
//! - Las coordenadas **nunca** se escriben en el registro de la aplicación. En el
//!   `audit` queda el identificador de la afiliación, no dónde vive nadie.
//! - La afiliación se puede borrar, y borrarla elimina el documento en lugar de
//!   marcarlo como inactivo.
//! - Un operador no puede listar las afiliaciones de otra persona por esta API.
//!   El motor de alertas consulta la colección por geometría, no por titular.
//!
//! # Por qué se guarda el punto y además el sector
//!
//! El sector sirve para hablar con la gente —«el oriente de Cali»— y para cruzar
//! con las capas de amenaza, que son polígonos administrativos. El punto sirve
//! para calcular la distancia al epicentro, que es lo que decide el nivel de
//! aviso de cada persona, y para encontrar el albergue apto más cercano. Ninguno
//! de los dos sustituye al otro.

use crate::{
    AppState, auth, db,
    error::{self, Result},
    hazards::Level,
    sectors, security, territory,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get},
};
use futures_util::TryStreamExt;
use mongodb::bson::{Document, doc};
use serde::Deserialize;
use serde_json::{Value, json};

/// Versión del texto de consentimiento aceptado. Cambiarla obliga a volver a
/// pedirlo, lo que permite demostrar qué aceptó cada persona y cuándo.
const CONSENT_VERSION: &str = "2026-11-v1";

/// Afiliaciones por persona. Cubre casa, trabajo, estudio y un par de familiares;
/// más que eso no describe a una persona, describe una lista de distribución.
const MAX_PER_USER: u64 = 8;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/subscriptions", get(list).post(create))
        .route("/subscriptions/{id}", delete(remove).patch(update))
        .route("/subscriptions/{id}/events", get(events))
}

/// Canales por los que avisar.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Channels {
    /// Web Push. No tiene coste por mensaje, así que está disponible para todos.
    #[serde(default)]
    push: bool,
    /// SMS. Tiene coste por mensaje, así que el motor de alertas lo reservará
    /// para el nivel más alto y para quien tenga plan activo. Declararlo aquí no
    /// otorga el derecho: eso lo comprueba el motor contra el plan.
    #[serde(default)]
    sms: bool,
}
impl Channels {
    fn any(self) -> bool {
        self.push || self.sms
    }
    fn to_bson(self) -> Document {
        doc! {"push": self.push, "sms": self.sms}
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct New {
    /// Territorio; si se omite y solo hay uno cargado, se usa ese.
    territory: Option<String>,
    /// Nombre que le da la persona: «Casa», «Trabajo».
    label: String,
    latitude: f64,
    longitude: f64,
    channels: Channels,
    /// Teléfono en formato E.164. Obligatorio si se pide SMS.
    #[serde(default)]
    phone: Option<String>,
    /// Nivel mínimo que se quiere recibir. `evacuate` solo avisa de lo grave.
    #[serde(default)]
    min_level: Option<String>,
    /// Aceptación explícita del tratamiento de la ubicación.
    consent: bool,
}

/// Teléfono en E.164: un `+`, un dígito distinto de cero y hasta catorce más.
///
/// Se valida el formato, no la existencia de la línea. El SMS de un número
/// inexistente se descubre al enviarlo, y el motor de alertas tendrá que marcar
/// el canal como fallido en ese momento.
fn valid_phone(value: &str) -> bool {
    let digits = value.strip_prefix('+').unwrap_or("");
    (8..=15).contains(&digits.len())
        && digits.starts_with(|c: char| ('1'..='9').contains(&c))
        && digits.chars().all(|c| c.is_ascii_digit())
}

fn valid_label(value: &str) -> Option<&str> {
    let v = value.trim();
    (!v.is_empty() && v.chars().count() <= 60).then_some(v)
}

/// Vista pública de una afiliación. Devuelve las coordenadas a su propio titular
/// —las necesita para mostrar el punto en un mapa— pero nunca el teléfono
/// completo, para que una sesión robada no sirva para cosechar números.
fn summary(d: &Document) -> Value {
    let phone = d.get_str("phone").ok();
    json!({
        "id": d.get_str("_id").unwrap_or_default(),
        "territory_id": d.get_str("territory_id").unwrap_or_default(),
        "label": d.get_str("label").unwrap_or_default(),
        "latitude": d.get_document("point").ok()
            .and_then(|p| p.get_array("coordinates").ok())
            .and_then(|c| c.get(1))
            .and_then(mongodb::bson::Bson::as_f64),
        "longitude": d.get_document("point").ok()
            .and_then(|p| p.get_array("coordinates").ok())
            .and_then(|c| c.first())
            .and_then(mongodb::bson::Bson::as_f64),
        "neighborhood": d.get_str("neighborhood").ok(),
        "neighborhood_code": d.get_str("neighborhood_code").ok(),
        "commune": d.get_str("commune").ok(),
        "commune_code": d.get_str("commune_code").ok(),
        "channels": d.get_document("channels").ok(),
        "phone_last4": phone.and_then(|p| p.len().checked_sub(4).map(|i| &p[i..])),
        "min_level": d.get_str("min_level").unwrap_or("info"),
        "consent_version": d.get_str("consent_version").ok(),
        "created": d.get_datetime("created").ok().map(ToString::to_string),
    })
}

async fn create(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(i): Json<New>,
) -> Result<(StatusCode, Json<Value>)> {
    let a = auth::actor(&s, &h).await?;
    security::limit(&s, "subscriptions", &a.id, 20, 3600).await?;

    if !i.consent {
        return Err(error::bad(
            "Se requiere aceptar el tratamiento de la ubicación para afiliarse",
        ));
    }
    let label = valid_label(&i.label)
        .ok_or(error::bad(
            "El nombre de la afiliación está vacío o es demasiado largo",
        ))?
        .to_owned();
    if !i.latitude.is_finite()
        || !(-90.0..=90.0).contains(&i.latitude)
        || !i.longitude.is_finite()
        || !(-180.0..=180.0).contains(&i.longitude)
    {
        return Err(error::bad("Coordenadas inválidas"));
    }
    if !i.channels.any() {
        return Err(error::bad("Elige al menos un canal de aviso"));
    }
    // Un canal sin proveedor configurado no se acepta: la afiliación quedaría
    // prometiendo un aviso que nadie va a entregar, y la persona no tendría cómo
    // saberlo hasta la emergencia.
    if i.channels.sms && s.config.sms_url.trim().is_empty() {
        return Err(error::bad(
            "El canal SMS no está disponible en este despliegue todavía",
        ));
    }
    if i.channels.push && s.config.vapid_private_key.trim().is_empty() {
        return Err(error::bad(
            "Las notificaciones no están disponibles en este despliegue todavía",
        ));
    }
    let phone = match (&i.phone, i.channels.sms) {
        (Some(p), _) if !valid_phone(p.trim()) => {
            return Err(error::bad(
                "El teléfono debe estar en formato internacional, por ejemplo +573001234567",
            ));
        }
        (Some(p), _) => Some(p.trim().to_owned()),
        (None, true) => {
            return Err(error::bad("El canal SMS requiere un teléfono"));
        }
        (None, false) => None,
    };
    let min_level = match i.min_level.as_deref() {
        None => Level::Info,
        Some(v) => Level::parse(v)
            .filter(|l| *l != Level::None)
            .ok_or(error::bad(
                "El nivel mínimo debe ser info, prepare o evacuate",
            ))?,
    };

    let t = match i.territory.as_deref() {
        Some(id) if !territory::Territory::valid_id(id) => {
            return Err(error::bad("Identificador de territorio inválido"));
        }
        Some(id) => territory::get(&s.db, id).await?,
        None => {
            let mut all = territory::list(&s.db).await?;
            if all.len() == 1 {
                all.remove(0)
            } else {
                return Err(error::bad("Indica el territorio de la afiliación"));
            }
        }
    };
    sectors::ready(&s.db, &t.id).await?;

    let resolved = sectors::resolve(&s.db, &t.id, [i.longitude, i.latitude]).await?;
    if !resolved.inside() {
        // Afiliar un punto fuera de la ciudad prometería una cobertura que no
        // existe: no hay capas de amenaza ni inventario de albergues para él.
        return Err(error::bad(
            "El punto está fuera del territorio; elige una ubicación dentro de la ciudad cubierta",
        ));
    }

    let owned =
        s.db.collection::<Document>("subscriptions")
            .count_documents(doc! {"owner": &a.id})
            .await?;
    if owned >= MAX_PER_USER {
        return Err(error::bad(
            "Se alcanzó el máximo de afiliaciones por cuenta; elimina una antes de crear otra",
        ));
    }

    let id = uuid::Uuid::new_v4().to_string();
    // La auditoría registra el identificador, nunca las coordenadas.
    db::audit(&s.db, &a.id, "subscription_create", &id).await?;
    let document = doc! {
        "_id": &id,
        "owner": &a.id,
        "territory_id": &t.id,
        "label": &label,
        "point": {"type": "Point", "coordinates": [i.longitude, i.latitude]},
        "neighborhood": resolved.neighborhood.as_deref(),
        "neighborhood_code": resolved.neighborhood_code.as_deref(),
        "commune": resolved.commune.as_deref(),
        "commune_code": resolved.commune_code.as_deref(),
        "channels": i.channels.to_bson(),
        "phone": phone.as_deref(),
        "min_level": min_level.as_str(),
        "consent_version": CONSENT_VERSION,
        "consent_at": db::now(),
        "created": db::now(),
        "updated": db::now(),
    };
    s.db.collection::<Document>("subscriptions")
        .insert_one(&document)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "subscription": summary(&document),
            "territory": t.summary(),
            // Lo que el territorio no puede verificar se declara aquí, no se
            // omite: si falta la capa de inundación, la recomendación de albergue
            // de esta afiliación no habrá comprobado inundación.
            "missing_layers": t.layers.missing(),
            "meaning": "Avisos posteriores al evento con recomendación de destino. \
                        No es alerta sísmica temprana ni sustituye a la autoridad de gestión del riesgo.",
        })),
    ))
}

/// Eventos recientes que alcanzaron esta afiliación, con el nivel que le
/// corresponde a **esa** ubicación.
///
/// Es la pieza que cierra el circuito entre la afiliación y la ingesta antes de
/// que exista el canal de envío: permite comprobar con datos reales a quién
/// habría avisado el sistema y con qué nivel, en lugar de confiar en que el
/// motor de alertas haga lo correcto cuando se escriba.
///
/// La consulta se acota primero al alcance máximo posible con el índice
/// geoespacial y solo después calcula la distancia exacta de cada evento. Con un
/// millón de afiliaciones, recorrer el catálogo por cada una sería inviable; con
/// este orden, el índice descarta de entrada todo lo que no pudo alcanzarla.
async fn events(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
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
        .ok_or_else(|| error::internal("afiliación sin punto"))?;
    let longitude = coordinates
        .first()
        .and_then(mongodb::bson::Bson::as_f64)
        .ok_or_else(|| error::internal("afiliación sin longitud"))?;
    let latitude = coordinates
        .get(1)
        .and_then(mongodb::bson::Bson::as_f64)
        .ok_or_else(|| error::internal("afiliación sin latitud"))?;
    let point = [longitude, latitude];
    let wanted = Level::parse(sub.get_str("min_level").unwrap_or("info")).unwrap_or(Level::Info);

    let nearby: Vec<Document> =
        s.db.collection::<Document>("events")
            .find(doc! {"point": {"$geoWithin": {"$centerSphere": [
                [longitude, latitude],
                crate::hazards::max_reach_km() / crate::hazards::EARTH_RADIUS_KM,
            ]}}})
            .sort(doc! {"origin_utc": -1})
            .limit(200)
            .await?
            .try_collect()
            .await?;

    let mut items = Vec::new();
    for e in &nearby {
        let Some(magnitude) = e.get_f64("magnitude").ok() else {
            // Una amenaza sin magnitud —una inundación— no se clasifica con las
            // bandas sísmicas. Queda para cuando exista su propia fuente.
            continue;
        };
        let Some(epicentre) = e
            .get_document("point")
            .ok()
            .and_then(|p| p.get_array("coordinates").ok())
            .and_then(|c| Some([c.first()?.as_f64()?, c.get(1)?.as_f64()?]))
        else {
            continue;
        };
        let distance = crate::hazards::distance_km(epicentre, point);
        let level = crate::hazards::classify(magnitude, distance);
        if level == Level::None || level < wanted {
            continue;
        }
        items.push(json!({
            "event_id": e.get_str("_id").unwrap_or_default(),
            "canonical_id": e.get_str("canonical_id").ok(),
            "source": e.get_str("source").ok(),
            "hazard_type": e.get_str("hazard_type").ok(),
            "magnitude": magnitude,
            "depth_km": e.get_f64("depth_km").ok(),
            "place": e.get_str("place").ok(),
            "url": e.get_str("url").ok(),
            "origin_utc": e.get_datetime("origin_utc").ok().map(ToString::to_string),
            "status": e.get_str("status").ok(),
            "validation_level": e.get_str("validation_level").unwrap_or("automatic"),
            "distance_km": (distance * 10.0).round() / 10.0,
            "level": level.as_str(),
        }));
        if items.len() >= 50 {
            break;
        }
    }
    Ok(Json(json!({
        "subscription": summary(&sub),
        "items": items,
        "min_level": wanted.as_str(),
        "meaning": "Avisos posteriores al evento. Los niveles son umbrales operativos por \
                    magnitud y distancia, sin validación de especialista y sin considerar tipo \
                    de suelo ni profundidad. No es alerta sísmica temprana.",
    })))
}
async fn list(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    let items: Vec<Document> =
        s.db.collection::<Document>("subscriptions")
            .find(doc! {"owner": &a.id})
            .sort(doc! {"created": 1})
            .limit(MAX_PER_USER as i64)
            .await?
            .try_collect()
            .await?;
    Ok(Json(json!({
        "items": items.iter().map(summary).collect::<Vec<_>>(),
        "max_per_user": MAX_PER_USER,
        "consent_version": CONSENT_VERSION,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    label: Option<String>,
    channels: Option<Channels>,
    phone: Option<String>,
    min_level: Option<String>,
}

/// Cambia preferencias de una afiliación propia.
///
/// No se puede mover el punto: cambiar la ubicación cambia el sector y las
/// amenazas aplicables, así que es una afiliación distinta y debe crearse con su
/// propio consentimiento registrado.
async fn update(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(i): Json<Update>,
) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    let collection = s.db.collection::<Document>("subscriptions");
    let current = collection
        .find_one(doc! {"_id": &id, "owner": &a.id})
        .await?
        .ok_or(error::Error(
            StatusCode::NOT_FOUND,
            "Afiliación no encontrada",
        ))?;

    let mut set = doc! {"updated": db::now()};
    if let Some(label) = &i.label {
        set.insert(
            "label",
            valid_label(label).ok_or(error::bad(
                "El nombre de la afiliación está vacío o es demasiado largo",
            ))?,
        );
    }
    if let Some(level) = &i.min_level {
        let parsed = Level::parse(level)
            .filter(|l| *l != Level::None)
            .ok_or(error::bad(
                "El nivel mínimo debe ser info, prepare o evacuate",
            ))?;
        set.insert("min_level", parsed.as_str());
    }
    // El teléfono se resuelve antes que los canales para poder comprobar que
    // activar SMS deja efectivamente un número con el que avisar.
    let phone = match &i.phone {
        Some(p) if p.trim().is_empty() => None,
        Some(p) if !valid_phone(p.trim()) => {
            return Err(error::bad(
                "El teléfono debe estar en formato internacional, por ejemplo +573001234567",
            ));
        }
        Some(p) => Some(p.trim().to_owned()),
        None => current.get_str("phone").ok().map(ToOwned::to_owned),
    };
    if i.phone.is_some() {
        set.insert("phone", phone.as_deref());
    }
    if let Some(channels) = i.channels {
        if !channels.any() {
            return Err(error::bad("Elige al menos un canal de aviso"));
        }
        if channels.sms && phone.is_none() {
            return Err(error::bad("El canal SMS requiere un teléfono"));
        }
        if channels.sms && s.config.sms_url.trim().is_empty() {
            return Err(error::bad(
                "El canal SMS no está disponible en este despliegue todavía",
            ));
        }
        set.insert("channels", channels.to_bson());
    }

    db::audit(&s.db, &a.id, "subscription_update", &id).await?;
    collection
        .update_one(doc! {"_id": &id, "owner": &a.id}, doc! {"$set": set})
        .await?;
    let updated = collection
        .find_one(doc! {"_id": &id, "owner": &a.id})
        .await?
        .ok_or(error::Error(
            StatusCode::NOT_FOUND,
            "Afiliación no encontrada",
        ))?;
    Ok(Json(json!({"subscription": summary(&updated)})))
}

/// Elimina una afiliación propia.
///
/// Borra el documento en lugar de marcarlo como inactivo: quien retira el
/// consentimiento sobre la ubicación de su casa tiene derecho a que el dato
/// desaparezca, no a que se conserve con una bandera.
async fn remove(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    db::audit(&s.db, &a.id, "subscription_delete", &id).await?;
    let r =
        s.db.collection::<Document>("subscriptions")
            .delete_one(doc! {"_id": &id, "owner": &a.id})
            .await?;
    if r.deleted_count == 0 {
        return Err(error::Error(
            StatusCode::NOT_FOUND,
            "Afiliación no encontrada",
        ));
    }
    Ok(Json(json!({"ok": true})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_international_numbers() {
        assert!(valid_phone("+573001234567"));
        assert!(valid_phone("+5215512345678"));
        assert!(valid_phone("+12025550123"));
    }

    #[test]
    fn rejects_numbers_that_cannot_receive_an_sms() {
        assert!(!valid_phone("3001234567"), "sin prefijo internacional");
        // Siete dígitos no alcanzan el mínimo de E.164; ocho sí, así que el
        // caso corto tiene que estar por debajo de ocho.
        assert!(!valid_phone("+5730012"), "demasiado corto");
        assert!(
            valid_phone("+57300123"),
            "ocho dígitos es una longitud E.164 válida"
        );
        assert!(!valid_phone("+5730012345678901"), "demasiado largo");
        assert!(
            !valid_phone("+0573001234567"),
            "país no puede empezar por cero"
        );
        assert!(!valid_phone("+57 300 123 4567"), "con espacios");
        assert!(!valid_phone("+57300123456a"), "con letras");
        assert!(!valid_phone(""), "vacío");
    }

    #[test]
    fn labels_are_trimmed_and_bounded() {
        assert_eq!(valid_label("  Casa  "), Some("Casa"));
        assert_eq!(valid_label(""), None);
        assert_eq!(valid_label("   "), None);
        assert_eq!(valid_label(&"a".repeat(60)).map(str::len), Some(60));
        assert_eq!(valid_label(&"a".repeat(61)), None);
        // Se cuentan caracteres y no bytes: un nombre con acentos de 60 letras
        // ocupa más de 60 bytes y debe seguir siendo válido.
        assert!(valid_label(&"á".repeat(60)).is_some());
    }

    #[test]
    fn a_channel_selection_must_reach_someone() {
        assert!(
            Channels {
                push: true,
                sms: false
            }
            .any()
        );
        assert!(
            Channels {
                push: false,
                sms: true
            }
            .any()
        );
        assert!(
            !Channels {
                push: false,
                sms: false
            }
            .any()
        );
    }

    #[test]
    fn the_summary_never_leaks_a_whole_phone_number() {
        let d = doc! {
            "_id": "s1",
            "owner": "u1",
            "territory_id": "co-cali",
            "label": "Casa",
            "point": {"type": "Point", "coordinates": [-76.52, 3.45]},
            "channels": {"push": true, "sms": true},
            "phone": "+573001234567",
            "min_level": "info",
        };
        let v = summary(&d);
        let text = v.to_string();
        assert!(
            !text.contains("+573001234567"),
            "el teléfono completo salió en la respuesta"
        );
        assert_eq!(v["phone_last4"], "4567");
        assert_eq!(v["latitude"], -0.0 + 3.45);
        assert_eq!(v["longitude"], -76.52);
    }

    #[test]
    fn a_subscription_without_a_phone_has_no_last_digits() {
        let d = doc! {
            "_id": "s1",
            "label": "Casa",
            "point": {"type": "Point", "coordinates": [-76.52, 3.45]},
            "channels": {"push": true, "sms": false},
            "min_level": "evacuate",
        };
        let v = summary(&d);
        assert!(v["phone_last4"].is_null());
        assert_eq!(v["min_level"], "evacuate");
    }
}
