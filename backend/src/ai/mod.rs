pub mod bert;
pub mod translation;
use crate::{
    AppState, auth, db, domain,
    error::{self, Result},
    security,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
};
use mongodb::{
    bson::{Document, doc},
    options::ReturnDocument,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub struct Engine {
    pub bert: Option<bert::Bert>,
    pub translator: Option<translation::Translator>,
}
impl Engine {
    pub fn load(model_dir: &str) -> Self {
        let bert = match bert::Bert::load(std::path::Path::new(model_dir)) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!(error=%e,"BERT unavailable");
                None
            }
        };
        let path = std::env::var("TP_TRANSLATION_DIR")
            .unwrap_or_else(|_| "../models/translation-es-en".into());
        let translator = match translation::Translator::load(std::path::Path::new(&path)) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!(error=%e,"Spanish translation unavailable");
                None
            }
        };
        Self { bert, translator }
    }
    pub fn predict(&mut self, text: &str, language: &str) -> anyhow::Result<Value> {
        let translated = match language {
            "en" => text.to_owned(),
            "es" => self
                .translator
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("Spanish translator unavailable"))?
                .translate(text)?,
            _ => anyhow::bail!("Unsupported language"),
        };
        let mut result = self
            .bert
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("BERT unavailable"))?
            .predict(&translated)?;
        result["language"] = json!(language);
        result["english_text"] = json!(translated);
        result["translation_model"] = if language == "es" {
            json!("Helsinki-NLP/opus-mt-es-en; greedy decoding; review required")
        } else {
            Value::Null
        };
        Ok(result)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Enqueue {
    pub report_id: String,
    pub language: String,
    pub request_key: String,
}
pub async fn enqueue(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<Enqueue>,
) -> Result<(StatusCode, Json<Value>)> {
    let a = auth::actor(&state, &headers).await?;
    a.operator()?;
    if !["en", "es"].contains(&input.language.as_str())
        || input.request_key.is_empty()
        || input.request_key.len() > 128
    {
        return Err(error::bad("Idioma o clave de idempotencia inválidos"));
    }
    let col = state.db.collection::<Document>("jobs");
    if let Some(d) = col
        .find_one(doc! {"owner":&a.id,"request_key":&input.request_key})
        .await?
    {
        if d.get_str("report_id").ok() != Some(&input.report_id)
            || d.get_str("language").ok() != Some(&input.language)
        {
            return Err(error::Error(
                StatusCode::CONFLICT,
                "La clave ya corresponde a otro trabajo",
            ));
        }
        return Ok((
            StatusCode::OK,
            Json(json!({"id":d.get_str("_id").unwrap_or_default()})),
        ));
    }
    security::limit(&state, "ai", &a.id, 30, 3600).await?;
    let report = state
        .db
        .collection::<Document>("reports")
        .find_one(doc! {"_id":&input.report_id})
        .await?
        .ok_or(error::Error(StatusCode::NOT_FOUND, "Reporte no encontrado"))?;
    let id = uuid::Uuid::new_v4().to_string();
    db::audit(&state.db, &a.id, "analysis_enqueue", &id).await?;
    let d = doc! {"_id":&id,"owner":&a.id,"request_key":&input.request_key,"report_id":&input.report_id,"text":report.get_str("text").unwrap_or_default(),"language":&input.language,"status":"queued","attempts":0,"created":db::now(),"lease":db::now()};
    match col.insert_one(d).await {
        Ok(_) => {}
        Err(e) if e.to_string().contains("E11000") => {
            let existing = col
                .find_one(doc! {"owner":&a.id,"request_key":&input.request_key})
                .await?
                .ok_or_else(|| error::internal("missing idempotent job"))?;
            if existing.get_str("report_id").ok() != Some(&input.report_id)
                || existing.get_str("language").ok() != Some(&input.language)
            {
                return Err(error::Error(
                    StatusCode::CONFLICT,
                    "La clave ya corresponde a otro trabajo",
                ));
            }
            return Ok((
                StatusCode::OK,
                Json(json!({"id":existing.get_str("_id").unwrap_or_default()})),
            ));
        }
        Err(e) => return Err(e.into()),
    }
    Ok((StatusCode::ACCEPTED, Json(json!({"id":id}))))
}
pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(p): Query<domain::Page>,
) -> Result<Json<Value>> {
    let a = auth::actor(&state, &headers).await?;
    a.operator()?;
    Ok(Json(
        json!({"items":domain::documents(&state,"jobs",doc!{},p.page).await?}),
    ))
}
pub async fn retry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>> {
    let a = auth::actor(&state, &headers).await?;
    a.operator()?;
    security::limit(&state, "ai", &a.id, 30, 3600).await?;
    db::audit(&state.db, &a.id, "analysis_retry", &id).await?;
    let r = state
        .db
        .collection::<Document>("jobs")
        .update_one(
            doc! {"_id":id,"status":"failed"},
            doc! {"$set":{"status":"queued","attempts":0,"lease":db::now()},"$unset":{"error":""}},
        )
        .await?;
    if r.matched_count != 1 {
        return Err(error::bad("Solo se pueden reintentar trabajos fallidos"));
    }
    Ok(Json(json!({"ok":true})))
}
pub async fn worker(state: AppState, engine: Arc<Mutex<Engine>>) {
    let col = state.db.collection::<Document>("jobs");
    loop {
        let lease_token = security::token();
        let claimed=col.find_one_and_update(doc!{"status":{"$in":["queued","running"]},"lease":{"$lte":db::now()},"attempts":{"$lt":3}},doc!{"$set":{"status":"running","lease":db::after(180),"lease_token":&lease_token},"$inc":{"attempts":1}}).sort(doc!{"created":1}).return_document(ReturnDocument::After).await;
        match claimed {
            Ok(Some(job)) => {
                let id = job.get_str("_id").unwrap_or_default().to_owned();
                let text = job.get_str("text").unwrap_or_default().to_owned();
                let language = job.get_str("language").unwrap_or("en").to_owned();
                let e = engine.clone();
                let task = tokio::task::spawn_blocking(move || {
                    e.lock()
                        .map_err(|_| anyhow::anyhow!("poisoned engine"))?
                        .predict(&text, &language)
                });
                tokio::pin!(task);
                // Heartbeat prevents another worker reclaiming a live long-running translation.
                let result = loop {
                    tokio::select! {
                        result=&mut task=>break result,
                        _=tokio::time::sleep(std::time::Duration::from_secs(30))=>{
                            if let Err(e)=col.update_one(doc!{"_id":&id,"lease_token":&lease_token,"status":"running"},doc!{"$set":{"lease":db::after(180)}}).await {tracing::error!(error=%e,"lease heartbeat failed");}
                        }
                    }
                };
                let update = match result {
                    Ok(Ok(v)) => match mongodb::bson::to_bson(&v) {
                        Ok(v) => {
                            doc! {"$set":{"status":"completed","result":v,"finished":db::now()},"$unset":{"text":"","lease_token":""}}
                        }
                        Err(e) => {
                            tracing::error!(error=%e,"result serialization failed");
                            doc! {"$set":{"status":"failed","error":"Error al guardar resultado"}}
                        }
                    },
                    other => {
                        tracing::warn!(error=?other.as_ref().err(),"inference failed");
                        doc! {"$set":{"status":"failed","error":"Motor no disponible o inferencia fallida; revisa configuración y pesos","finished":db::now()},"$unset":{"lease_token":""}}
                    }
                };
                if let Err(e) = col
                    .update_one(
                        doc! {"_id":&id,"lease_token":&lease_token,"status":"running"},
                        update,
                    )
                    .await
                {
                    tracing::error!(error=%e,"job completion failed");
                }
            }
            Ok(None) => {
                let _=col.update_many(doc!{"status":"running","attempts":{"$gte":3},"lease":{"$lte":db::now()}},doc!{"$set":{"status":"failed","error":"Se agotaron los intentos tras interrupciones del worker","finished":db::now()}}).await;
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            Err(e) => {
                tracing::error!(error=%e,"claim failed");
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
    }
}
