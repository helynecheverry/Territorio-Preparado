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
    /// Cada cuántos segundos se consultan los catálogos de amenazas.
    pub hazard_poll_seconds: u64,
    /// Recuadro de consulta `[lon_min, lat_min, lon_max, lat_max]`. Es generoso a
    /// propósito: un sismo en Panamá o Ecuador sacude Colombia, y quién recibe
    /// aviso lo decide luego la distancia de cada persona, no este recuadro.
    pub hazard_bbox: [f64; 4],
    /// Magnitud por debajo de la cual no se ingiere nada. Los umbrales de aviso
    /// más bajos empiezan en 3.5, así que pedir menos solo añade ruido.
    pub hazard_min_magnitude: f64,
    /// Ventana hacia atrás de cada consulta. Cubre varios ciclos de sondeo para
    /// que una caída momentánea de red no deje un hueco en el catálogo.
    pub hazard_lookback_minutes: i64,
    /// Servicio FDSN de USGS, el piso universal.
    pub usgs_url: String,
    /// Servicio compatible con FDSN del SGC. Vacío desactiva la fuente: el SGC no
    /// publica una API y no se raspa su portal. Ver `hazards::sgc`.
    pub sgc_url: String,
    /// Cada cuántos segundos busca el motor eventos pendientes de reparto.
    pub alert_poll_seconds: u64,
    /// Eventos que se reparten en una misma pasada.
    pub alert_events_per_pass: u64,
    /// Tope de avisos por evento. Es una red de seguridad contra un reparto
    /// desbocado, no un límite de diseño.
    pub alert_max_per_event: u64,
    /// SMS que un solo evento puede llegar a costar. Al agotarse, el resto se
    /// marca como omitido por presupuesto en lugar de gastarse en silencio.
    pub alert_sms_budget_per_event: u64,
    /// Avisos que se escriben por tramo. Acota la memoria del reparto.
    pub alert_batch: usize,
    /// Escrituras simultáneas por tramo. Secuencialmente, un reparto de cientos
    /// de miles de avisos tardaría minutos.
    pub alert_concurrency: usize,
    /// Radio de búsqueda de destinos, en kilómetros.
    pub destination_radius_km: f64,
    /// Área mínima para que un espacio sirva como destino. Un separador vial no
    /// es un albergue.
    pub destination_min_area_m2: f64,
    /// Espacios que se examinan como máximo por consulta de destinos.
    pub destination_candidates: usize,
    /// Clave privada VAPID en base64url (los 32 bytes del escalar). Vacía
    /// desactiva Web Push, y sin Web Push el plan gratuito no recibe nada.
    pub vapid_private_key: String,
    /// Contacto del operador para la reclamación `sub` de RFC 8292, por ejemplo
    /// `mailto:ops@example.com`. Es cómo el servicio de push avisa si algo va mal.
    pub vapid_contact: String,
    /// Extremo del proveedor de SMS. Vacío desactiva el canal.
    pub sms_url: String,
    /// Encabezado de autorización con la forma `Nombre: valor`. Es un secreto.
    pub sms_auth_header: String,
    /// `json` o `form`, según lo que espere el proveedor.
    pub sms_content_type: String,
    /// Plantilla del cuerpo con los marcadores `{{to}}` y `{{text}}`.
    pub sms_body: String,
    pub delivery_poll_seconds: u64,
    pub delivery_batch: usize,
    pub delivery_max_attempts: i32,
    /// Espera base entre reintentos; se duplica con cada intento.
    pub delivery_backoff_seconds: u64,
    /// Nombre de la pasarela, para el libro de eventos. Vacío desactiva el cobro.
    pub billing_provider: String,
    /// Secreto compartido con la pasarela. Vacío hace que el webhook responda
    /// 503: sin secreto no se puede distinguir un evento real de uno falsificado.
    pub billing_webhook_secret: String,
    pub billing_signature_header: String,
    /// De dónde sale la marca de tiempo: `header:nombre` o `body:ruta.con.puntos`.
    pub billing_timestamp: String,
    /// Qué se firma, con los marcadores `{{ts}}` y `{{body}}`.
    pub billing_signed_template: String,
    /// Antigüedad máxima admitida de un webhook, contra reenvíos.
    pub billing_tolerance_seconds: u64,
    pub billing_path_event_id: String,
    pub billing_path_reference: String,
    pub billing_path_status: String,
    pub billing_path_amount_cents: String,
    pub billing_path_currency: String,
    /// Valor del estado que significa pagado.
    pub billing_success_status: String,
    /// Precio del plan de pago en la unidad mínima de la moneda.
    pub paid_plan_amount_cents: i64,
    pub paid_plan_currency: String,
    pub paid_plan_days: i64,
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
            hazard_poll_seconds: get("TP_HAZARD_POLL_SECONDS", "60")
                .parse::<u64>()?
                .clamp(15, 3600),
            hazard_bbox: bbox(&get("TP_HAZARD_BBOX", "-85,-8,-64,16"))?,
            hazard_min_magnitude: get("TP_HAZARD_MIN_MAGNITUDE", "3.0").parse()?,
            hazard_lookback_minutes: get("TP_HAZARD_LOOKBACK_MINUTES", "120")
                .parse::<i64>()?
                .clamp(5, 10080),
            usgs_url: get(
                "TP_USGS_URL",
                "https://earthquake.usgs.gov/fdsnws/event/1/query",
            ),
            sgc_url: get("TP_SGC_URL", ""),
            alert_poll_seconds: get("TP_ALERT_POLL_SECONDS", "30")
                .parse::<u64>()?
                .clamp(5, 3600),
            alert_events_per_pass: get("TP_ALERT_EVENTS_PER_PASS", "20")
                .parse::<u64>()?
                .clamp(1, 500),
            alert_max_per_event: get("TP_ALERT_MAX_PER_EVENT", "2000000")
                .parse::<u64>()?
                .clamp(1, 50_000_000),
            alert_sms_budget_per_event: get("TP_ALERT_SMS_BUDGET_PER_EVENT", "5000")
                .parse::<u64>()?
                .clamp(0, 10_000_000),
            alert_batch: get("TP_ALERT_BATCH", "2000")
                .parse::<usize>()?
                .clamp(1, 50_000),
            alert_concurrency: get("TP_ALERT_CONCURRENCY", "64")
                .parse::<usize>()?
                .clamp(1, 512),
            destination_radius_km: get("TP_DESTINATION_RADIUS_KM", "5")
                .parse::<f64>()?
                .clamp(0.1, 50.0),
            destination_min_area_m2: get("TP_DESTINATION_MIN_AREA_M2", "1000")
                .parse::<f64>()?
                .clamp(0.0, 1_000_000.0),
            destination_candidates: get("TP_DESTINATION_CANDIDATES", "200")
                .parse::<usize>()?
                .clamp(1, 5000),
            vapid_private_key: get("TP_VAPID_PRIVATE_KEY", ""),
            vapid_contact: get("TP_VAPID_CONTACT", ""),
            sms_url: get("TP_SMS_URL", ""),
            sms_auth_header: get("TP_SMS_AUTH_HEADER", ""),
            sms_content_type: get("TP_SMS_CONTENT_TYPE", "json"),
            sms_body: get("TP_SMS_BODY", r#"{"to":"{{to}}","message":"{{text}}"}"#),
            delivery_poll_seconds: get("TP_DELIVERY_POLL_SECONDS", "10")
                .parse::<u64>()?
                .clamp(1, 600),
            delivery_batch: get("TP_DELIVERY_BATCH", "500")
                .parse::<usize>()?
                .clamp(1, 20_000),
            delivery_max_attempts: get("TP_DELIVERY_MAX_ATTEMPTS", "5")
                .parse::<i32>()?
                .clamp(1, 20),
            delivery_backoff_seconds: get("TP_DELIVERY_BACKOFF_SECONDS", "30")
                .parse::<u64>()?
                .clamp(1, 3600),
            billing_provider: get("TP_BILLING_PROVIDER", ""),
            billing_webhook_secret: get("TP_BILLING_WEBHOOK_SECRET", ""),
            billing_signature_header: get("TP_BILLING_SIGNATURE_HEADER", "x-signature"),
            billing_timestamp: get("TP_BILLING_TIMESTAMP", "header:x-timestamp"),
            billing_signed_template: get("TP_BILLING_SIGNED_TEMPLATE", "{{ts}}.{{body}}"),
            billing_tolerance_seconds: get("TP_BILLING_TOLERANCE_SECONDS", "300")
                .parse::<u64>()?
                .clamp(30, 3600),
            billing_path_event_id: get("TP_BILLING_PATH_EVENT_ID", "data.id"),
            billing_path_reference: get("TP_BILLING_PATH_REFERENCE", "data.reference"),
            billing_path_status: get("TP_BILLING_PATH_STATUS", "data.status"),
            billing_path_amount_cents: get("TP_BILLING_PATH_AMOUNT_CENTS", "data.amount_in_cents"),
            billing_path_currency: get("TP_BILLING_PATH_CURRENCY", "data.currency"),
            billing_success_status: get("TP_BILLING_SUCCESS_STATUS", "APPROVED"),
            paid_plan_amount_cents: get("TP_PAID_PLAN_AMOUNT_CENTS", "2000000")
                .parse::<i64>()?
                .clamp(1, 1_000_000_000),
            paid_plan_currency: get("TP_PAID_PLAN_CURRENCY", "COP"),
            paid_plan_days: get("TP_PAID_PLAN_DAYS", "30")
                .parse::<i64>()?
                .clamp(1, 3650),
        })
    }
}

/// Lee `lon_min,lat_min,lon_max,lat_max` y comprueba que describe un recuadro
/// real. Un recuadro invertido o fuera del mundo dejaría la ingesta sin eventos
/// en silencio, así que falla al arrancar en lugar de al primer sismo.
fn bbox(value: &str) -> Result<[f64; 4]> {
    let parts: Vec<f64> = value
        .split(',')
        .map(|v| v.trim().parse::<f64>())
        .collect::<std::result::Result<_, _>>()?;
    let [lon_min, lat_min, lon_max, lat_max] = <[f64; 4]>::try_from(parts.as_slice())
        .map_err(|_| anyhow::anyhow!("TP_HAZARD_BBOX requiere cuatro números"))?;
    ensure!(
        (-180.0..=180.0).contains(&lon_min)
            && (-180.0..=180.0).contains(&lon_max)
            && (-90.0..=90.0).contains(&lat_min)
            && (-90.0..=90.0).contains(&lat_max),
        "TP_HAZARD_BBOX fuera del rango de coordenadas"
    );
    ensure!(
        lon_min < lon_max && lat_min < lat_max,
        "TP_HAZARD_BBOX debe ir de la esquina suroeste a la noreste"
    );
    Ok([lon_min, lat_min, lon_max, lat_max])
}

#[cfg(test)]
impl Config {
    /// Configuración mínima para pruebas unitarias, sin leer el entorno.
    pub fn for_tests() -> Self {
        Self {
            bind: "127.0.0.1:4180".into(),
            mongo_uri: "mongodb://127.0.0.1:27017".into(),
            mongo_db: "territorio_test".into(),
            origin: "http://127.0.0.1:4180".into(),
            production: false,
            secret: "x".repeat(32),
            smtp_host: None,
            smtp_port: 587,
            smtp_user: String::new(),
            smtp_password: String::new(),
            mail_from: String::new(),
            mail_dir: "var/mail".into(),
            google_id: String::new(),
            google_secret: String::new(),
            model_dir: "../models/sos".into(),
            frontend_dir: "../frontend/dist".into(),
            hazard_poll_seconds: 60,
            hazard_bbox: [-85.0, -8.0, -64.0, 16.0],
            hazard_min_magnitude: 3.0,
            hazard_lookback_minutes: 120,
            usgs_url: "https://earthquake.usgs.gov/fdsnws/event/1/query".into(),
            sgc_url: String::new(),
            alert_poll_seconds: 30,
            alert_events_per_pass: 20,
            alert_max_per_event: 2_000_000,
            alert_sms_budget_per_event: 5_000,
            alert_batch: 2_000,
            alert_concurrency: 64,
            destination_radius_km: 5.0,
            destination_min_area_m2: 1000.0,
            destination_candidates: 200,
            vapid_private_key: String::new(),
            vapid_contact: "mailto:ops@example.com".into(),
            sms_url: String::new(),
            sms_auth_header: String::new(),
            sms_content_type: "json".into(),
            sms_body: r#"{"to":"{{to}}","message":"{{text}}"}"#.into(),
            delivery_poll_seconds: 10,
            delivery_batch: 500,
            delivery_max_attempts: 5,
            delivery_backoff_seconds: 30,
            billing_provider: String::new(),
            billing_webhook_secret: String::new(),
            billing_signature_header: "x-signature".into(),
            billing_timestamp: "header:x-timestamp".into(),
            billing_signed_template: "{{ts}}.{{body}}".into(),
            billing_tolerance_seconds: 300,
            billing_path_event_id: "data.id".into(),
            billing_path_reference: "data.reference".into(),
            billing_path_status: "data.status".into(),
            billing_path_amount_cents: "data.amount_in_cents".into(),
            billing_path_currency: "data.currency".into(),
            billing_success_status: "APPROVED".into(),
            paid_plan_amount_cents: 2_000_000,
            paid_plan_currency: "COP".into(),
            paid_plan_days: 30,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::bbox;

    #[test]
    fn reads_a_south_west_to_north_east_box() {
        assert_eq!(bbox("-85,-8,-64,16").unwrap(), [-85.0, -8.0, -64.0, 16.0]);
        assert_eq!(
            bbox(" -85 , -8 , -64 , 16 ").unwrap(),
            [-85.0, -8.0, -64.0, 16.0]
        );
    }

    #[test]
    fn rejects_boxes_that_would_silently_return_nothing() {
        assert!(bbox("-64,16,-85,-8").is_err(), "recuadro invertido");
        assert!(bbox("-85,-8,-64").is_err(), "faltan números");
        assert!(bbox("-85,-8,-64,16,3").is_err(), "sobran números");
        assert!(bbox("-200,-8,-64,16").is_err(), "fuera del mundo");
        assert!(bbox("-85,-8,-64,100").is_err(), "latitud imposible");
        assert!(bbox("a,b,c,d").is_err(), "no numérico");
    }
}
