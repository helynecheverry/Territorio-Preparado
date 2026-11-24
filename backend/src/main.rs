mod ai;
mod alerts;
mod auth;
mod billing;
mod config;
mod db;
mod delivery;
mod destinations;
mod domain;
mod error;
mod hazards;
mod legal;
mod lessons;
mod local_access;
mod planning;
mod sectors;
mod security;
mod subscriptions;
mod territory;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    routing::get,
};
use mongodb::{
    Database,
    bson::{Document, doc},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower_http::{services::ServeDir, trace::TraceLayer};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<config::Config>,
    pub db: Database,
    pub dummy_password: String,
    pub planning: Arc<planning::Planning>,
}
async fn health(State(s): State<AppState>) -> error::Result<Json<Value>> {
    s.db.run_command(doc! {"ping":1}).await?;
    Ok(Json(
        json!({"status":"ok","backend":"rust","database":"mongodb"}),
    ))
}
/// Qué puede hacer de verdad este despliegue.
///
/// `channels` y `billing` salen de la configuración real, no de una lista
/// fija: la interfaz no debe ofrecer un canal que nadie va a entregar ni un
/// plan que no se puede cobrar.
async fn capabilities(State(s): State<AppState>) -> Json<Value> {
    Json(
        json!({"google":!s.config.google_id.is_empty()&&!s.config.google_secret.is_empty(),"email_delivery":if s.config.smtp_host.is_some(){"smtp"}else{"local_files"},"languages":["es","en"],"review_required":true,"hazard_sources":hazards::sources(&s.config).iter().map(|v|v.name()).collect::<Vec<_>>(),"early_warning":false,"channels":delivery::Transports::from_config(&s.config).names(),"billing":!s.config.billing_provider.trim().is_empty()}),
    )
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "territorio_api=info,tower_http=info".into()),
        )
        .init();
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("infer") {
        let language = args
            .get(2)
            .ok_or_else(|| anyhow::anyhow!("Uso: infer es|en 'mensaje'"))?;
        let text = args
            .get(3)
            .ok_or_else(|| anyhow::anyhow!("Falta mensaje"))?;
        let dir = std::env::var("TP_MODEL_DIR").unwrap_or_else(|_| "../models/sos".into());
        let mut engine = ai::Engine::load(&dir);
        println!(
            "{}",
            serde_json::to_string_pretty(&engine.predict(text, language)?)?
        );
        return Ok(());
    }
    let config = Arc::new(config::Config::load()?);
    let db = db::connect(&config).await?;
    if args.get(1).map(String::as_str) == Some("local-code") {
        local_access::show_code(
            &config,
            &db,
            args.get(2).map(String::as_str),
            args.iter().any(|a| a == "--json"),
        )
        .await?;
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("import-territory") {
        let spec_path = args.get(2).ok_or_else(|| {
            anyhow::anyhow!("Uso: territorio-api import-territory territorio.json")
        })?;
        let spec: territory::Territory = serde_json::from_slice(&std::fs::read(spec_path)?)?;
        anyhow::ensure!(
            territory::Territory::valid_id(&spec.id),
            "El identificador del territorio solo admite minúsculas, dígitos y guiones"
        );
        territory::upsert(&db, &spec).await?;

        // Las capas administrativas van primero: una afiliación sin sectores
        // cargados no se puede resolver, y el inventario de espacios no depende
        // de ellas.
        for (kind, source) in [
            (sectors::Kind::Commune, &spec.sources.communes),
            (sectors::Kind::Neighborhood, &spec.sources.neighborhoods),
        ] {
            let Some(path) = source else {
                tracing::warn!(
                    territory = %spec.id,
                    kind = kind.as_str(),
                    "Capa de sectores no declarada: no se podrán resolver afiliaciones a ese nivel"
                );
                continue;
            };
            let report = sectors::import(&db, &spec, kind, std::path::Path::new(path)).await?;
            tracing::info!(
                territory = %spec.id,
                kind = kind.as_str(),
                imported = report.imported,
                rejected = report.rejected.len(),
                "Sectores importados"
            );
            for (sector, reason) in &report.rejected {
                tracing::warn!(
                    %sector,
                    reason = %reason,
                    "Sector rechazado: ese trozo de ciudad queda sin cobertura"
                );
            }
        }

        if let Some(path) = &spec.sources.spaces {
            let report = planning::catalog::import(&db, &spec, std::path::Path::new(path)).await?;
            tracing::info!(
                territory = %spec.id,
                total = report.total,
                measurable = report.measurable,
                points = report.points,
                invalid = report.rejected.len(),
                sha256 = %report.sha256,
                "Inventario importado"
            );
            // Los polígonos rechazados se nombran uno por uno: son datos que
            // alguien debe corregir en la fuente, no un detalle que convenga
            // resumir.
            for (id, reason) in &report.rejected {
                tracing::warn!(
                    space = %id,
                    reason = %reason,
                    "Huella poligonal inválida: corregir en la fuente"
                );
            }
        } else {
            tracing::warn!(
                territory = %spec.id,
                "Inventario de espacios no declarado: el planificador no tendrá espacios"
            );
        }

        db::audit(&db, "local-cli", "territory_import", &spec.id)
            .await
            .map_err(|_| anyhow::anyhow!("No se pudo auditar"))?;
        let missing = spec.layers.missing();
        tracing::info!(
            territory = %spec.id,
            coverage = spec.layers.coverage().as_str(),
            "Territorio listo"
        );
        if !missing.is_empty() {
            tracing::warn!(
                territory = %spec.id,
                missing = ?missing,
                "Capas de amenaza ausentes: las recomendaciones deben declararlo"
            );
        }
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("alerts-pass") {
        // Fuerza una pasada del motor y sale. Sirve para operar —repartir ya un
        // evento sin esperar al temporizador— y para que las pruebas sean
        // deterministas en lugar de depender de un reloj.
        let state = AppState {
            config: config.clone(),
            db: db.clone(),
            dummy_password: String::new(),
            planning: Arc::new(planning::Planning::new()),
        };
        let pass = alerts::run_once(&state)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e.1))?;
        tracing::info!(
            events = pass.events,
            enqueued = pass.enqueued,
            raised = pass.raised,
            duplicates = pass.duplicates,
            over_budget = pass.over_budget,
            superseded = pass.superseded,
            "Pasada de alertas completada"
        );
        println!(
            "{}",
            serde_json::to_string(&json!({
                "events": pass.events,
                "enqueued": pass.enqueued,
                "raised": pass.raised,
                "duplicates": pass.duplicates,
                "over_budget": pass.over_budget,
                "superseded": pass.superseded,
            }))?
        );
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("deliver-pass") {
        // Fuerza un lote de reparto y sale. Operativo —vaciar la cola ya— y lo
        // que hace deterministas las pruebas del canal.
        let state = AppState {
            config: config.clone(),
            db: db.clone(),
            dummy_password: String::new(),
            planning: Arc::new(planning::Planning::new()),
        };
        let transports = delivery::Transports::from_config(&config);
        let pass = delivery::run_once(&state, &transports)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e.1))?;
        println!(
            "{}",
            serde_json::to_string(&json!({
                "claimed": pass.claimed,
                "sent": pass.sent,
                "retried": pass.retried,
                "failed": pass.failed,
                "devices_removed": pass.devices_removed,
            }))?
        );
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("vapid-keys") {
        // Genera un par de claves VAPID sin depender de la CLI de openssl, para
        // que arrancar el canal de push no requiera otra herramienta.
        use web_push_native::jwt_simple::algorithms::{ECDSAP256PublicKeyLike, ES256KeyPair};
        let pair = ES256KeyPair::generate();
        let encode = |b: &[u8]| {
            use base64::Engine;
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "TP_VAPID_PRIVATE_KEY": encode(&pair.to_bytes()),
                "public_key_for_browser": encode(
                    &pair.public_key().public_key().to_bytes_uncompressed()
                ),
            }))?
        );
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("import-lessons") {
        let path = args
            .get(2)
            .ok_or_else(|| anyhow::anyhow!("Uso: territorio-api import-lessons lecciones.json"))?;
        let report = lessons::import(&db, std::path::Path::new(path)).await?;
        db::audit(&db, "local-cli", "lessons_import", path)
            .await
            .map_err(|_| anyhow::anyhow!("No se pudo auditar"))?;
        tracing::info!(
            imported = report.imported,
            blocked = report.blocked.len(),
            "Lecciones importadas como borrador"
        );
        // Lo que le falta a cada lección para poder publicarse se nombra una por
        // una: es la lista de tareas de quien tenga que revisarlas.
        for (id, reason) in &report.blocked {
            tracing::warn!(lesson = %id, reason = %reason, "No publicable todavía");
        }
        // Un enlace de video roto se descubre aquí y no durante un sismo.
        for (id, reason) in lessons::check_videos(&report.lessons).await {
            tracing::warn!(lesson = %id, reason = %reason, "El video declarado no responde");
        }
        tracing::info!(
            "Nada queda publicado. Publicar es: PATCH /api/v1/lessons/{{id}} \
             con reviewed_by y published, desde una cuenta de operador"
        );
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("import-legal") {
        let path = args
            .get(2)
            .ok_or_else(|| anyhow::anyhow!("Uso: territorio-api import-legal documentos.json"))?;
        let report = legal::import(&db, std::path::Path::new(path)).await?;
        db::audit(&db, "local-cli", "legal_import", path)
            .await
            .map_err(|_| anyhow::anyhow!("No se pudo auditar"))?;
        tracing::info!(
            imported = report.imported,
            blocked = report.blocked.len(),
            "Documentos legales importados como borrador"
        );
        for (id, reason) in &report.blocked {
            tracing::warn!(document = %id, reason = %reason, "No publicable todavía");
        }
        tracing::info!(
            "Nada queda publicado. Publicar exige un revisor identificado, que puede \
             ser del equipo: PATCH /api/v1/legal/{{id}} desde una cuenta de \
             administrador. Para unos términos que respaldan un cobro, conviene \
             que ese revisor sea asesoría jurídica"
        );
        return Ok(());
    }
    if args.get(1).map(String::as_str) == Some("grant-admin") {
        let email = auth::email(
            args.get(2)
                .ok_or_else(|| anyhow::anyhow!("Uso: territorio-api grant-admin correo"))?,
        )
        .map_err(|_| anyhow::anyhow!("Correo inválido"))?;
        let user = db
            .collection::<Document>("users")
            .find_one(doc! {"email":&email,"verified":true,"disabled":false})
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("La cuenta debe registrarse y verificar su correo antes")
            })?;
        let id = user.get_str("_id")?;
        db::audit(&db, "local-cli", "grant-admin", id)
            .await
            .map_err(|_| anyhow::anyhow!("No se pudo auditar"))?;
        db.collection::<Document>("users")
            .update_one(doc! {"_id":id}, doc! {"$set":{"role":"admin"}})
            .await?;
        tracing::info!("Administrador habilitado");
        return Ok(());
    }
    let dummy_password = auth::hash_password(security::token())
        .await
        .map_err(|_| anyhow::anyhow!("Argon2 initialization failed"))?;
    let state = AppState {
        config: config.clone(),
        db,
        dummy_password,
        planning: Arc::new(planning::Planning::new()),
    };
    let mode = args.get(1).map(String::as_str).unwrap_or("serve");
    anyhow::ensure!(
        ["serve", "api", "worker"].contains(&mode),
        "Comando esperado: serve, api, worker, grant-admin, import-territory, import-lessons, import-legal, alerts-pass, deliver-pass o vapid-keys"
    );
    if mode != "api" {
        let model_dir = config.model_dir.clone();
        let worker_state = state.clone();
        let worker = tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || {
                Arc::new(Mutex::new(ai::Engine::load(&model_dir)))
            })
            .await
            {
                Ok(engine) => ai::worker(worker_state, engine).await,
                Err(e) => tracing::error!(error=%e,"Model loading task failed"),
            }
        });
        // La ingesta de amenazas vive con el worker: consulta catálogos externos
        // y no debe competir por el executor que atiende HTTP.
        let hazards_state = state.clone();
        let hazards = tokio::spawn(async move { hazards::worker(hazards_state).await });
        // El motor de alertas va aparte de la ingesta: si un catálogo tarda en
        // responder, el reparto de lo ya ingerido no debe quedarse esperando.
        let alerts_state = state.clone();
        let alerts = tokio::spawn(async move { alerts::worker(alerts_state).await });
        // El reparto va aparte del motor: un proveedor de SMS lento no debe
        // retrasar la decisión sobre el siguiente sismo.
        let delivery_state = state.clone();
        let delivery = tokio::spawn(async move { delivery::worker(delivery_state).await });
        if mode == "worker" {
            tokio::select! {_=worker=>{},_=hazards=>{},_=alerts=>{},_=delivery=>{},_ = shutdown()=>{}};
            return Ok(());
        }
    }
    let api = Router::new()
        .route("/health", get(health))
        .route("/capabilities", get(capabilities))
        .nest("/auth", auth::routes())
        .nest(
            "/v1",
            domain::routes()
                .merge(subscriptions::routes())
                .merge(hazards::routes())
                .merge(alerts::routes())
                .merge(destinations::routes())
                .merge(delivery::routes())
                .merge(lessons::routes())
                .merge(legal::routes())
                .merge(billing::routes())
                .nest("/planning", planning::routes()),
        );
    let app = Router::new()
        .nest("/api", api)
        .nest_service(
            "/app",
            ServeDir::new("web").append_index_html_on_directories(true),
        )
        .fallback_service(ServeDir::new(&config.frontend_dir))
        .layer(DefaultBodyLimit::max(32 * 1024))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            security::guard,
        ))
        .layer(TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<axum::body::Body>| {
            tracing::info_span!("http", method=%request.method(), path=request.uri().path())
        }))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!(address=%config.bind,"Territorio Preparado listo; aplicación en /app/");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    Ok(())
}
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("signal handler");
        tokio::select! {_ = tokio::signal::ctrl_c()=>{},_ = terminate.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
