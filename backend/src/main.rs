mod ai;
mod auth;
mod config;
mod db;
mod domain;
mod error;
mod local_access;
mod planning;
mod security;
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
async fn capabilities(State(s): State<AppState>) -> Json<Value> {
    Json(
        json!({"google":!s.config.google_id.is_empty()&&!s.config.google_secret.is_empty(),"email_delivery":if s.config.smtp_host.is_some(){"smtp"}else{"local_files"},"languages":["es","en"],"review_required":true}),
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
        planning: Arc::new(planning::Planning::load(&config.frontend_dir)?),
    };
    let mode = args.get(1).map(String::as_str).unwrap_or("serve");
    anyhow::ensure!(
        ["serve", "api", "worker"].contains(&mode),
        "Comando esperado: serve, api, worker o grant-admin"
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
        if mode == "worker" {
            tokio::select! {_=worker=>{},_ = shutdown()=>{}};
            return Ok(());
        }
    }
    let api = Router::new()
        .route("/health", get(health))
        .route("/capabilities", get(capabilities))
        .nest("/auth", auth::routes())
        .nest(
            "/v1",
            domain::routes().nest("/planning", planning::routes()),
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
