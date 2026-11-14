//! Contenido educativo: qué hacer ante cada amenaza.
//!
//! # Por qué el texto va primero y el video después
//!
//! En una emergencia real la red celular se degrada: se cae la capacidad de
//! datos mucho antes que la de voz o SMS. Un contenido cuyo único soporte sea un
//! video de decenas de megabytes es inútil exactamente cuando hace falta.
//!
//! Por eso cada lección tiene **pasos en texto** como contenido obligatorio y el
//! video como complemento. Publicar una lección con video y sin pasos se rechaza.
//! Y por eso existe `GET /api/v1/lessons/bundle`: devuelve todo el contenido
//! publicado en una sola respuesta con `ETag`, para que la aplicación lo guarde
//! una vez y lo sirva sin red. Revalidar cuesta un 304.
//!
//! # Por qué nada se publica sin revisión
//!
//! Estas instrucciones pueden determinar si alguien se queda bajo un marco de
//! puerta o corre hacia la calle. No es contenido que pueda generar un sistema
//! automático ni redactar quien programa el backend. Así que publicar exige tres
//! cosas, comprobadas por la API y no por costumbre:
//!
//! 1. Pasos para la fase `during`, que es la que decide.
//! 2. Al menos una fuente citada.
//! 3. Un revisor identificado en `reviewed_by`.
//!
//! El contenido se importa con `territorio-api import-lessons`, llega como
//! borrador y solo un operador puede publicarlo después de revisarlo.

use crate::{
    AppState, auth, db,
    error::{self, Result},
    hazards::HazardType,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, patch},
};
use futures_util::TryStreamExt;
use mongodb::bson::{Document, doc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest;

/// Momento de la emergencia al que corresponde un paso.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Preparación, antes de que pase nada.
    Before,
    /// Durante el evento. Es la fase que decide y la única obligatoria.
    During,
    /// Después: evacuación, reencuentro, reporte de daños.
    After,
}
impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::During => "during",
            Self::After => "after",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Step {
    pub phase: Phase,
    pub text: String,
}

/// Video de apoyo. Nunca es el único soporte de las instrucciones.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Video {
    pub url: String,
    #[serde(default)]
    pub duration_seconds: Option<u32>,
    /// Transcripción. Hace el video accesible y, sobre todo, utilizable cuando no
    /// hay ancho de banda para reproducirlo.
    #[serde(default)]
    pub transcript: Option<String>,
}

/// Fuente de la que provienen las instrucciones.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Source {
    /// Entidad que publica la guía, por ejemplo una autoridad de gestión del
    /// riesgo o una sociedad de la Cruz Roja.
    pub name: String,
    #[serde(default)]
    pub reference: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Lesson {
    #[serde(rename = "_id", alias = "id")]
    pub id: String,
    pub hazard_type: String,
    pub locale: String,
    pub title: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub video: Option<Video>,
    #[serde(default)]
    pub sources: Vec<Source>,
    /// Quién revisó el contenido. Sin esto no se puede publicar.
    #[serde(default)]
    pub reviewed_by: Option<String>,
    #[serde(default)]
    pub published: bool,
}

/// Tipos de amenaza admitidos, más `general` para lo que aplica a todas.
fn valid_hazard(value: &str) -> bool {
    value == "general"
        || [
            HazardType::Earthquake,
            HazardType::Flood,
            HazardType::Landslide,
        ]
        .iter()
        .any(|h| h.as_str() == value)
}

/// Identificador estable, usado también como clave de caché en el cliente.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
}

/// Por qué una lección **no** se puede publicar, o `None` si sí se puede.
///
/// Es una función aparte y probada porque es la única barrera entre un borrador y
/// una instrucción que alguien va a seguir en una emergencia.
pub fn blocking_reason(l: &Lesson) -> Option<&'static str> {
    if l.title.trim().is_empty() {
        return Some("La lección necesita un título");
    }
    if !l.steps.iter().any(|s| s.phase == Phase::During) {
        // Es la fase que decide. Una lección que solo explica cómo prepararse no
        // sirve cuando el suelo ya se está moviendo.
        return Some("Faltan los pasos de la fase «durante», que es la que decide");
    }
    if l.steps.iter().any(|s| s.text.trim().is_empty()) {
        return Some("Hay pasos vacíos");
    }
    if l.sources.is_empty() || l.sources.iter().any(|s| s.name.trim().is_empty()) {
        return Some("Las instrucciones de emergencia deben citar su fuente");
    }
    if l.reviewed_by
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        return Some("Falta la persona que revisó el contenido");
    }
    if l.video.is_some() && l.steps.is_empty() {
        // Un video no puede ser el único soporte: en una emergencia la red de
        // datos es lo primero que se degrada.
        return Some("Un video no puede ser el único soporte de las instrucciones");
    }
    if let Some(v) = &l.video
        && !v.url.starts_with("https://")
    {
        // Un video en claro expone qué está consultando la persona y permite
        // alterarlo por el camino.
        return Some("La dirección del video debe ser https");
    }
    None
}

/// Comprueba que cada video declarado responde.
///
/// Lo que esta comprobación **sí** detecta: un dominio caído, un archivo borrado
/// del propio almacenamiento y una dirección mal escrita. Lo que **no** detecta:
/// que una plataforma haya retirado el video, porque sigue devolviendo una página
/// con código 200 diciendo que no está disponible. Esa es una razón de peso para
/// alojar los videos propios en lugar de enlazar a una plataforma: un enlace que
/// otro puede retirar no es contenido de emergencia fiable.
pub async fn check_videos(lessons: &[Lesson]) -> Vec<(String, String)> {
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => return vec![("cliente".into(), e.to_string())],
    };
    let mut problems = Vec::new();
    for l in lessons {
        let Some(video) = &l.video else { continue };
        match client.get(&video.url).send().await {
            Ok(r) if r.status().is_success() => {}
            Ok(r) => problems.push((l.id.clone(), format!("respondió {}", r.status()))),
            Err(e) => problems.push((l.id.clone(), format!("no respondió: {e}"))),
        }
    }
    problems
}

/// Vista pública de una lección.
fn view(l: &Lesson) -> Value {
    json!({
        "id": l.id,
        "hazard_type": l.hazard_type,
        "locale": l.locale,
        "title": l.title,
        "summary": l.summary,
        "steps": l.steps.iter().map(|s| json!({"phase": s.phase.as_str(), "text": s.text})).collect::<Vec<_>>(),
        "video": l.video,
        "sources": l.sources,
        "reviewed_by": l.reviewed_by,
    })
}

// --- API -------------------------------------------------------------------

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/lessons", get(list))
        .route("/lessons/bundle", get(bundle))
        .route("/lessons/{id}", patch(update))
}

#[derive(Deserialize)]
struct Filter {
    hazard: Option<String>,
    locale: Option<String>,
    /// Solo para operadores: incluye los borradores sin publicar.
    #[serde(default)]
    drafts: bool,
}

/// Lecciones publicadas, o también los borradores si lo pide un operador.
async fn list(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<Filter>,
) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    let mut filter = doc! {};
    if q.drafts {
        a.operator()?;
    } else {
        filter.insert("published", true);
    }
    if let Some(hazard) = &q.hazard {
        if !valid_hazard(hazard) {
            return Err(error::bad("Tipo de amenaza inválido"));
        }
        filter.insert("hazard_type", hazard);
    }
    if let Some(locale) = &q.locale {
        if locale.len() > 20 {
            return Err(error::bad("Idioma inválido"));
        }
        filter.insert("locale", locale);
    }
    let items: Vec<Lesson> =
        s.db.collection::<Lesson>("lessons")
            .find(filter)
            .sort(doc! {"hazard_type": 1, "_id": 1})
            .limit(200)
            .await?
            .try_collect()
            .await?;
    Ok(Json(json!({
        "items": items.iter().map(view).collect::<Vec<_>>(),
        "pending_review": items
            .iter()
            .filter(|l| !l.published)
            .map(|l| json!({"id": l.id, "reason": blocking_reason(l)}))
            .collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct BundleQuery {
    locale: Option<String>,
}

/// Todo el contenido publicado en una sola respuesta, para guardar sin red.
///
/// Devuelve `ETag` con el resumen del contenido. El cliente lo conserva y
/// revalida con `If-None-Match`, que cuesta un 304 sin cuerpo. Es lo que permite
/// que las instrucciones estén en el dispositivo **antes** de la emergencia, que
/// es el único momento en el que se pueden descargar con garantías.
async fn bundle(
    State(s): State<AppState>,
    h: HeaderMap,
    Query(q): Query<BundleQuery>,
) -> Result<Response> {
    auth::actor(&s, &h).await?;
    let mut filter = doc! {"published": true};
    if let Some(locale) = &q.locale {
        if locale.len() > 20 {
            return Err(error::bad("Idioma inválido"));
        }
        filter.insert("locale", locale);
    }
    let items: Vec<Lesson> =
        s.db.collection::<Lesson>("lessons")
            .find(filter)
            .sort(doc! {"hazard_type": 1, "_id": 1})
            .limit(500)
            .await?
            .try_collect()
            .await?;

    let payload = json!({
        "items": items.iter().map(view).collect::<Vec<_>>(),
        "count": items.len(),
        "meaning": "Contenido para guardar en el dispositivo antes de la emergencia. \
                    En una emergencia la red de datos se degrada, así que esto no debe \
                    descargarse en el momento. Revisado por personas, no generado \
                    automáticamente; no sustituye a la autoridad de gestión del riesgo.",
    });
    let body = serde_json::to_vec(&payload).map_err(error::internal)?;
    // El resumen se calcula sobre el cuerpo servido, así que no puede quedar
    // descoordinado con lo que el cliente guardó.
    let etag = format!("\"{}\"", hex::encode(sha2::Sha256::digest(&body)));

    if h.get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|candidate| candidate.trim() == etag))
    {
        return Ok((StatusCode::NOT_MODIFIED, [(header::ETAG, etag)]).into_response());
    }
    Ok((
        StatusCode::OK,
        [
            (header::ETAG, etag),
            (header::CONTENT_TYPE, "application/json".to_owned()),
        ],
        body,
    )
        .into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    /// Quién revisó el contenido.
    reviewed_by: Option<String>,
    /// Publicar o retirar.
    published: Option<bool>,
}

/// Revisa y publica, o retira, una lección.
///
/// Publicar comprueba las condiciones de `blocking_reason` sobre el documento tal
/// como quedaría, no sobre el que había antes: así no se puede publicar algo y
/// completar la revisión después.
async fn update(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(i): Json<Review>,
) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    a.operator()?;
    if !valid_id(&id) {
        return Err(error::bad("Identificador de lección inválido"));
    }
    let collection = s.db.collection::<Lesson>("lessons");
    let mut lesson = collection
        .find_one(doc! {"_id": &id})
        .await?
        .ok_or(error::Error(StatusCode::NOT_FOUND, "Lección no encontrada"))?;

    if let Some(reviewer) = &i.reviewed_by {
        let trimmed = reviewer.trim();
        if trimmed.is_empty() || trimmed.chars().count() > 120 {
            return Err(error::bad("El revisor está vacío o es demasiado largo"));
        }
        lesson.reviewed_by = Some(trimmed.to_owned());
    }
    if let Some(published) = i.published {
        if published && let Some(reason) = blocking_reason(&lesson) {
            return Err(error::Error(StatusCode::CONFLICT, reason));
        }
        lesson.published = published;
    }

    db::audit(
        &s.db,
        &a.id,
        if lesson.published {
            "lesson_publish"
        } else {
            "lesson_unpublish"
        },
        &id,
    )
    .await?;
    collection
        .update_one(
            doc! {"_id": &id},
            doc! {"$set": {
                "reviewed_by": lesson.reviewed_by.as_deref(),
                "published": lesson.published,
                "updated": db::now(),
            }},
        )
        .await?;
    Ok(Json(json!({
        "lesson": view(&lesson),
        "published": lesson.published,
    })))
}

// --- Importación -----------------------------------------------------------

#[derive(Deserialize)]
struct Collection {
    lessons: Vec<Lesson>,
}

pub struct Report {
    pub imported: u64,
    /// Lecciones que no cumplen las condiciones para publicarse, con el motivo.
    pub blocked: Vec<(String, &'static str)>,
    /// Las lecciones tal como quedaron, para comprobar sus videos sin volver a
    /// leer el archivo.
    pub lessons: Vec<Lesson>,
}

/// Importa lecciones desde un archivo.
///
/// **Todo entra como borrador**, sin excepción y sin importar lo que diga el
/// archivo: publicar es una decisión de una persona que revisó el contenido, no
/// una propiedad de un archivo que alguien puede copiar. El informe nombra qué le
/// falta a cada lección para poder publicarse.
///
/// Reimportar actualiza el contenido y **no** republica: si una lección estaba
/// publicada y su texto cambia, vuelve a borrador y hay que revisarla otra vez.
/// Es deliberado: un cambio de redacción en una instrucción de emergencia merece
/// la misma revisión que la original.
pub async fn import(db: &mongodb::Database, path: &std::path::Path) -> anyhow::Result<Report> {
    use anyhow::Context;
    let data = std::fs::read(path)
        .with_context(|| format!("No se pudo leer las lecciones en {}", path.display()))?;
    let parsed: Collection =
        serde_json::from_slice(&data).context("El archivo de lecciones no es un JSON válido")?;

    let collection = db.collection::<Document>("lessons");
    let mut imported = 0u64;
    let mut blocked = Vec::new();
    let mut stored = Vec::new();
    for lesson in parsed.lessons {
        anyhow::ensure!(
            valid_id(&lesson.id),
            "Identificador de lección inválido: {}",
            lesson.id
        );
        anyhow::ensure!(
            valid_hazard(&lesson.hazard_type),
            "Tipo de amenaza inválido en {}: {}",
            lesson.id,
            lesson.hazard_type
        );
        if let Some(reason) = blocking_reason(&lesson) {
            blocked.push((lesson.id.clone(), reason));
        }
        collection
            .update_one(
                doc! {"_id": &lesson.id},
                doc! {"$set": {
                    "hazard_type": &lesson.hazard_type,
                    "locale": &lesson.locale,
                    "title": &lesson.title,
                    "summary": lesson.summary.as_deref(),
                    "steps": mongodb::bson::to_bson(&lesson.steps)?,
                    "video": mongodb::bson::to_bson(&lesson.video)?,
                    "sources": mongodb::bson::to_bson(&lesson.sources)?,
                    "reviewed_by": lesson.reviewed_by.as_deref(),
                    // Importar nunca publica, y reimportar despublica.
                    "published": false,
                    "updated": db::now(),
                }, "$setOnInsert": {"created": db::now()}},
            )
            .upsert(true)
            .await?;
        imported += 1;
        stored.push(lesson);
    }
    Ok(Report {
        imported,
        blocked,
        lessons: stored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete() -> Lesson {
        Lesson {
            id: "sismo-durante".into(),
            hazard_type: "earthquake".into(),
            locale: "es-CO".into(),
            title: "Durante un sismo".into(),
            summary: None,
            steps: vec![Step {
                phase: Phase::During,
                text: "Texto revisado por la entidad competente.".into(),
            }],
            video: None,
            sources: vec![Source {
                name: "Entidad de gestión del riesgo".into(),
                reference: None,
            }],
            reviewed_by: Some("Especialista en gestión del riesgo".into()),
            published: false,
        }
    }

    #[test]
    fn a_complete_reviewed_lesson_can_be_published() {
        assert_eq!(blocking_reason(&complete()), None);
    }

    #[test]
    fn the_during_phase_is_mandatory() {
        // Una lección que solo explica cómo prepararse no sirve cuando el suelo
        // ya se está moviendo.
        let mut l = complete();
        l.steps = vec![Step {
            phase: Phase::Before,
            text: "Prepara un kit.".into(),
        }];
        assert_eq!(
            blocking_reason(&l),
            Some("Faltan los pasos de la fase «durante», que es la que decide")
        );
        l.steps.push(Step {
            phase: Phase::During,
            text: "Texto revisado.".into(),
        });
        assert_eq!(blocking_reason(&l), None);
    }

    #[test]
    fn emergency_instructions_must_cite_a_source() {
        let mut l = complete();
        l.sources.clear();
        assert_eq!(
            blocking_reason(&l),
            Some("Las instrucciones de emergencia deben citar su fuente")
        );
        // Y una fuente en blanco no cuenta como fuente.
        l.sources = vec![Source {
            name: "   ".into(),
            reference: None,
        }];
        assert_eq!(
            blocking_reason(&l),
            Some("Las instrucciones de emergencia deben citar su fuente")
        );
    }

    #[test]
    fn nothing_is_published_without_a_named_reviewer() {
        let mut l = complete();
        l.reviewed_by = None;
        assert_eq!(
            blocking_reason(&l),
            Some("Falta la persona que revisó el contenido")
        );
        l.reviewed_by = Some("  ".into());
        assert_eq!(
            blocking_reason(&l),
            Some("Falta la persona que revisó el contenido")
        );
    }

    #[test]
    fn a_video_cannot_be_the_only_support() {
        // En una emergencia la red de datos es lo primero que se degrada, así que
        // un video sin pasos en texto no se publica.
        let mut l = complete();
        l.steps.clear();
        l.video = Some(Video {
            url: "https://example.test/sismo.mp4".into(),
            duration_seconds: Some(90),
            transcript: None,
        });
        assert!(blocking_reason(&l).is_some());
    }

    #[test]
    fn a_video_over_plain_http_is_refused() {
        // Un video en claro expone qué consulta la persona y se puede alterar
        // por el camino.
        let mut l = complete();
        l.video = Some(Video {
            url: "http://ejemplo.test/sismo.mp4".into(),
            duration_seconds: Some(90),
            transcript: None,
        });
        assert_eq!(
            blocking_reason(&l),
            Some("La dirección del video debe ser https")
        );
        l.video = Some(Video {
            url: "https://ejemplo.test/sismo.mp4".into(),
            duration_seconds: Some(90),
            transcript: None,
        });
        assert_eq!(blocking_reason(&l), None);
    }

    #[test]
    fn empty_steps_and_titles_are_caught() {
        let mut l = complete();
        l.steps.push(Step {
            phase: Phase::After,
            text: "   ".into(),
        });
        assert_eq!(blocking_reason(&l), Some("Hay pasos vacíos"));
        let mut l = complete();
        l.title = "  ".into();
        assert_eq!(blocking_reason(&l), Some("La lección necesita un título"));
    }

    #[test]
    fn hazard_types_match_the_alert_engine_plus_general() {
        // Si los nombres se separaran, una lección quedaría asociada a una
        // amenaza que el motor nunca emite.
        assert!(valid_hazard("earthquake"));
        assert!(valid_hazard("flood"));
        assert!(valid_hazard("landslide"));
        assert!(valid_hazard("general"));
        assert!(!valid_hazard("terremoto"));
        assert!(!valid_hazard(""));
    }

    #[test]
    fn identifiers_are_usable_as_cache_keys() {
        assert!(valid_id("sismo-que-hacer"));
        assert!(valid_id("inundacion_antes"));
        assert!(!valid_id("Sismo"), "las mayúsculas romperían la clave");
        assert!(!valid_id("sismo que hacer"));
        assert!(!valid_id(""));
        assert!(!valid_id(&"a".repeat(81)));
    }
}
