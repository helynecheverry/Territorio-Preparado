//! Documentos legales versionados y aceptación registrada.
//!
//! # Por qué esto existe antes de la pasarela de pago
//!
//! El proyecto opera en modo informar y recomendar, sin convenio con la autoridad
//! de gestión del riesgo. En cuanto se cobra una cuota, esa postura deja de ser
//! una decisión interna y pasa a ser una afirmación comercial: hay que poder
//! demostrar **qué** se vendió y **que la persona lo aceptó**.
//!
//! Lo que se cobra es el servicio de monitoreo y el contenido, no una garantía de
//! entrega. Ningún canal garantiza que un aviso llegue: Web Push depende del
//! servicio del navegador y el SMS de una red que en una emergencia se degrada.
//! Si eso no está escrito y aceptado, una cuota mensual puede leerse como la
//! promesa de que el aviso llegará.
//!
//! # Cómo
//!
//! Cada documento tiene una **versión**, y la aceptación se registra contra esa
//! versión concreta. Cambiar el texto obliga a volver a aceptarlo, lo que permite
//! responder con precisión a «qué aceptó esta persona y cuándo». Igual que con el
//! contenido educativo, publicar exige un revisor identificado: unos términos que
//! nadie revisó no pueden ser la base de una relación comercial.
//!
//! El texto que viene en `legal/` es un **borrador** y necesita revisión de un
//! abogado antes de publicarse. Este módulo construye el mecanismo, no presta
//! asesoría legal.

use crate::{
    AppState, auth, db,
    error::{self, Result},
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use mongodb::bson::{Document, doc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LegalDocument {
    /// Identificador estable: `terms`, `privacy`.
    #[serde(rename = "_id", alias = "id")]
    pub id: String,
    /// Versión del texto. La aceptación se registra contra este valor, así que
    /// cambiarla obliga a volver a aceptar.
    pub version: String,
    pub locale: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub reviewed_by: Option<String>,
    #[serde(default)]
    pub published: bool,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 40
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
}

/// Por qué un documento **no** se puede publicar, o `None` si sí.
pub fn blocking_reason(d: &LegalDocument) -> Option<&'static str> {
    if d.title.trim().is_empty() {
        return Some("El documento necesita un título");
    }
    if d.body.trim().chars().count() < 200 {
        // Unos términos de dos frases no describen un servicio de avisos de
        // emergencia, y publicarlos daría una falsa sensación de cobertura legal.
        return Some("El texto es demasiado corto para describir el servicio");
    }
    if !valid_version(&d.version) {
        return Some("La versión está vacía o tiene caracteres no admitidos");
    }
    if d.reviewed_by
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        return Some("Falta la persona que revisó el documento");
    }
    None
}

/// Documento publicado vigente.
pub async fn current(db: &mongodb::Database, id: &str) -> Result<LegalDocument> {
    db.collection::<LegalDocument>("legal_documents")
        .find_one(doc! {"_id": id, "published": true})
        .await?
        .ok_or(error::Error(
            StatusCode::NOT_FOUND,
            "Documento no publicado",
        ))
}

/// Si una cuenta aceptó una versión concreta.
///
/// La clave del registro incluye la versión, así que aceptar la versión anterior
/// no cuenta como aceptar la vigente.
pub async fn accepted(
    db: &mongodb::Database,
    owner: &str,
    document: &str,
    version: &str,
) -> Result<bool> {
    Ok(db
        .collection::<Document>("legal_acceptances")
        .find_one(doc! {"_id": format!("{owner}:{document}:{version}")})
        .await?
        .is_some())
}

// --- API -------------------------------------------------------------------

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/legal/{id}", get(read).patch(update))
        .route("/legal/{id}/accept", post(accept))
}

fn view(d: &LegalDocument) -> Value {
    json!({
        "id": d.id,
        "version": d.version,
        "locale": d.locale,
        "title": d.title,
        "body": d.body,
        "reviewed_by": d.reviewed_by,
        "published": d.published,
    })
}

/// Documento vigente y si la propia cuenta ya lo aceptó.
async fn read(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    if !valid_id(&id) {
        return Err(error::bad("Identificador de documento inválido"));
    }
    let document = current(&s.db, &id).await?;
    let mine = accepted(&s.db, &a.id, &id, &document.version).await?;
    Ok(Json(json!({
        "document": view(&document),
        "accepted": mine,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Accept {
    /// Versión que se está aceptando. Se exige explícita para que no se pueda
    /// aceptar «lo que haya» y luego discutir qué era.
    version: String,
}

async fn accept(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(i): Json<Accept>,
) -> Result<(StatusCode, Json<Value>)> {
    let a = auth::actor(&s, &h).await?;
    if !valid_id(&id) || !valid_version(&i.version) {
        return Err(error::bad("Documento o versión inválidos"));
    }
    let document = current(&s.db, &id).await?;
    if document.version != i.version {
        // Aceptar una versión que ya no es la vigente no sirve: el texto cambió.
        return Err(error::Error(
            StatusCode::CONFLICT,
            "La versión indicada no es la vigente; vuelve a leer el documento",
        ));
    }
    let key = format!("{}:{id}:{}", a.id, document.version);
    db::audit(&s.db, &a.id, "legal_accept", &key).await?;
    s.db.collection::<Document>("legal_acceptances")
        .update_one(
            doc! {"_id": &key},
            doc! {
                "$set": {"owner": &a.id, "document": &id, "version": &document.version},
                "$setOnInsert": {"accepted_at": db::now()},
            },
        )
        .upsert(true)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"document": id, "version": document.version, "accepted": true})),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    reviewed_by: Option<String>,
    published: Option<bool>,
}

/// Revisa y publica, o retira, un documento legal.
async fn update(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(i): Json<Review>,
) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    a.admin()?;
    if !valid_id(&id) {
        return Err(error::bad("Identificador de documento inválido"));
    }
    let collection = s.db.collection::<LegalDocument>("legal_documents");
    let mut document = collection
        .find_one(doc! {"_id": &id})
        .await?
        .ok_or(error::Error(
            StatusCode::NOT_FOUND,
            "Documento no encontrado",
        ))?;
    if let Some(reviewer) = &i.reviewed_by {
        let trimmed = reviewer.trim();
        if trimmed.is_empty() || trimmed.chars().count() > 120 {
            return Err(error::bad("El revisor está vacío o es demasiado largo"));
        }
        document.reviewed_by = Some(trimmed.to_owned());
    }
    if let Some(published) = i.published {
        if published && let Some(reason) = blocking_reason(&document) {
            return Err(error::Error(StatusCode::CONFLICT, reason));
        }
        document.published = published;
    }
    db::audit(
        &s.db,
        &a.id,
        if document.published {
            "legal_publish"
        } else {
            "legal_unpublish"
        },
        &id,
    )
    .await?;
    collection
        .update_one(
            doc! {"_id": &id},
            doc! {"$set": {
                "reviewed_by": document.reviewed_by.as_deref(),
                "published": document.published,
                "updated": db::now(),
            }},
        )
        .await?;
    Ok(Json(json!({"document": view(&document)})))
}

// --- Importación -----------------------------------------------------------

#[derive(Deserialize)]
struct Collection {
    documents: Vec<LegalDocument>,
}

pub struct Report {
    pub imported: u64,
    pub blocked: Vec<(String, &'static str)>,
}

/// Importa documentos legales como borrador.
///
/// Igual que el contenido educativo, nada queda publicado al importar y
/// reimportar despublica. Un cambio en los términos de un servicio de pago no
/// puede entrar en vigor sin que alguien lo revise, y las aceptaciones de la
/// versión anterior siguen guardadas: son la prueba de qué aceptó cada persona.
pub async fn import(db: &mongodb::Database, path: &std::path::Path) -> anyhow::Result<Report> {
    use anyhow::Context;
    let data = std::fs::read(path)
        .with_context(|| format!("No se pudo leer los documentos en {}", path.display()))?;
    let parsed: Collection =
        serde_json::from_slice(&data).context("El archivo legal no es un JSON válido")?;
    let collection = db.collection::<Document>("legal_documents");
    let mut imported = 0;
    let mut blocked = Vec::new();
    for document in parsed.documents {
        anyhow::ensure!(
            valid_id(&document.id),
            "Identificador de documento inválido: {}",
            document.id
        );
        anyhow::ensure!(
            valid_version(&document.version),
            "Versión inválida en {}: {}",
            document.id,
            document.version
        );
        if let Some(reason) = blocking_reason(&document) {
            blocked.push((document.id.clone(), reason));
        }
        collection
            .update_one(
                doc! {"_id": &document.id},
                doc! {"$set": {
                    "version": &document.version,
                    "locale": &document.locale,
                    "title": &document.title,
                    "body": &document.body,
                    "reviewed_by": document.reviewed_by.as_deref(),
                    "published": false,
                    "updated": db::now(),
                }, "$setOnInsert": {"created": db::now()}},
            )
            .upsert(true)
            .await?;
        imported += 1;
    }
    Ok(Report { imported, blocked })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> LegalDocument {
        LegalDocument {
            id: "terms".into(),
            version: "2026-11-v1".into(),
            locale: "es-CO".into(),
            title: "Términos del servicio".into(),
            body: "x".repeat(400),
            reviewed_by: None,
            published: false,
        }
    }

    #[test]
    fn terms_cannot_be_published_without_a_reviewer() {
        // Unos términos que nadie revisó no pueden ser la base de una relación
        // comercial.
        assert_eq!(
            blocking_reason(&draft()),
            Some("Falta la persona que revisó el documento")
        );
        let mut d = draft();
        d.reviewed_by = Some("Asesoría jurídica".into());
        assert_eq!(blocking_reason(&d), None);
    }

    #[test]
    fn a_two_sentence_text_is_refused() {
        let mut d = draft();
        d.reviewed_by = Some("Asesoría jurídica".into());
        d.body = "Usa la app a tu riesgo.".into();
        assert_eq!(
            blocking_reason(&d),
            Some("El texto es demasiado corto para describir el servicio")
        );
    }

    #[test]
    fn versions_must_be_usable_as_acceptance_keys() {
        assert!(valid_version("2026-11-v1"));
        assert!(valid_version("1.0.2"));
        // Un espacio o dos puntos romperían la clave `owner:document:version`.
        assert!(!valid_version("v1 final"));
        assert!(!valid_version("v1:final"));
        assert!(!valid_version(""));
    }

    #[test]
    fn document_identifiers_are_bounded() {
        assert!(valid_id("terms"));
        assert!(valid_id("privacy-policy"));
        assert!(!valid_id("Terms"));
        assert!(!valid_id("terms:v1"));
        assert!(!valid_id(""));
    }

    #[test]
    fn the_title_is_required() {
        let mut d = draft();
        d.reviewed_by = Some("Asesoría jurídica".into());
        d.title = "   ".into();
        assert_eq!(blocking_reason(&d), Some("El documento necesita un título"));
    }
}
