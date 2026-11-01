use anyhow::{Result, ensure};
use std::env;

#[derive(Clone)]
pub struct Config {
    pub bind: String,
    pub mongo_uri: String,
    pub mongo_db: String,
    pub origin: String,
    pub production: bool,
    pub secret: String,
    pub smtp_host: Option<String>,
    pub smtp_port: u16,
    pub smtp_user: String,
    pub smtp_password: String,
    pub mail_from: String,
    pub mail_dir: String,
    pub google_id: String,
    pub google_secret: String,
    pub model_dir: String,
    pub frontend_dir: String,
}
impl Config {
    pub fn load() -> Result<Self> {
        // Optional, locally provisioned JSON configuration. It is data, never shell code.
        let settings: std::collections::HashMap<String, String> = match env::var("TP_CONFIG_FILE") {
            Ok(path) => serde_json::from_slice(&std::fs::read(path)?)?,
            Err(_) => std::collections::HashMap::new(),
        };
        let get = |key: &str, default: &str| {
            env::var(key)
                .unwrap_or_else(|_| settings.get(key).cloned().unwrap_or_else(|| default.into()))
        };
        let production = get("TP_ENV", "development") == "production";
        let secret = get("TP_SECRET", "");
        ensure!(
            secret.len() >= 32,
            "TP_SECRET debe tener al menos 32 caracteres aleatorios"
        );
        let origin = get("TP_ORIGIN", "http://127.0.0.1:4180");
        let parsed = url::Url::parse(&origin)?;
        ensure!(
            parsed.path() == "/"
                && parsed.query().is_none()
                && parsed.fragment().is_none()
                && parsed.username().is_empty(),
            "TP_ORIGIN debe ser un origen sin ruta"
        );
        ensure!(
            origin == parsed.origin().ascii_serialization(),
            "TP_ORIGIN debe escribirse sin barra final ni credenciales"
        );
        let bind = get("TP_BIND", "127.0.0.1:4180");
        let address: std::net::SocketAddr = bind.parse()?;
        if !production {
            ensure!(
                address.ip().is_loopback(),
                "Desarrollo requiere TP_BIND en loopback"
            );
        }
        let smtp_host = Some(get("TP_SMTP_HOST", "")).filter(|v| !v.is_empty());
        if production {
            ensure!(parsed.scheme() == "https", "Producción requiere HTTPS");
            ensure!(smtp_host.is_some(), "Producción requiere SMTP");
            ensure!(
                get("TP_MONGO_URI", "").contains('@'),
                "Producción requiere MongoDB autenticado"
            );
        } else {
            ensure!(
                parsed.scheme() == "http"
                    && matches!(parsed.host_str(), Some("localhost" | "127.0.0.1")),
                "Desarrollo solo admite localhost HTTP"
            );
        }
        Ok(Self {
            bind,
            mongo_uri: get("TP_MONGO_URI", "mongodb://127.0.0.1:27017"),
            mongo_db: get("TP_MONGO_DB", "territorio"),
            origin,
            production,
            secret,
            smtp_host,
            smtp_port: get("TP_SMTP_PORT", "587").parse()?,
            smtp_user: get("TP_SMTP_USER", ""),
            smtp_password: get("TP_SMTP_PASSWORD", ""),
            mail_from: get("TP_MAIL_FROM", "Territorio Preparado <no-reply@localhost>"),
            mail_dir: get("TP_MAIL_DIR", "var/mail"),
            google_id: get("TP_GOOGLE_CLIENT_ID", ""),
            google_secret: get("TP_GOOGLE_CLIENT_SECRET", ""),
            model_dir: get("TP_MODEL_DIR", "../models/sos"),
            frontend_dir: get("TP_FRONTEND_DIR", "../frontend/dist"),
        })
    }
}
