//! Transporte SMS contra el proveedor del propio operador.
//!
//! # Por qué es genérico y no de un proveedor concreto
//!
//! El requisito fue poder usar las API propias para no elevar el coste de la
//! aplicación, y el coste por SMS varía mucho entre proveedores: en Colombia los
//! nacionales suelen cobrar bastante menos que Twilio por un mensaje local. Atar
//! el código a un proveedor obligaría a tocar Rust para cambiar de tarifa.
//!
//! Así que el transporte se configura por entorno: URL, método, encabezado de
//! autorización, tipo de contenido y una plantilla de cuerpo con `{{to}}` y
//! `{{text}}`. Cambiar de proveedor es cambiar variables de entorno y reiniciar.
//!
//! No se incluyen plantillas de proveedores concretos: no están verificadas
//! contra sus servicios y una plantilla equivocada que parezca oficial es peor
//! que no tener ninguna. La plantilla se escribe leyendo la documentación del
//! proveedor que se contrate.
//!
//! # Sustitución segura
//!
//! El texto del aviso contiene comillas, acentos y signos de puntuación, y el
//! número viene de datos de usuario. Sustituir en crudo rompería el JSON o
//! permitiría inyectar parámetros en un cuerpo de formulario, así que cada
//! marcador se escapa según el tipo de contenido declarado.

use super::Outcome;
use crate::config::Config;

/// Cómo se codifica el cuerpo, que determina cómo se escapan los marcadores.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encoding {
    Json,
    Form,
}
impl Encoding {
    fn content_type(self) -> &'static str {
        match self {
            Self::Json => "application/json",
            Self::Form => "application/x-www-form-urlencoded",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "json" | "application/json" => Some(Self::Json),
            "form" | "application/x-www-form-urlencoded" => Some(Self::Form),
            _ => None,
        }
    }
}

pub struct Sms {
    url: String,
    encoding: Encoding,
    template: String,
    /// Nombre y valor del encabezado de autorización. El valor es un secreto y
    /// nunca se registra.
    auth: Option<(String, String)>,
    client: reqwest::Client,
}

/// Escapa un valor para insertarlo en el cuerpo sin romperlo.
fn escape(value: &str, encoding: Encoding) -> String {
    match encoding {
        // `to_string` de una cadena JSON produce el literal entre comillas con
        // todo escapado; se quitan las comillas porque la plantilla ya las pone.
        Encoding::Json => {
            let quoted = serde_json::Value::String(value.to_owned()).to_string();
            quoted[1..quoted.len() - 1].to_owned()
        }
        Encoding::Form => value
            .bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    (b as char).to_string()
                }
                b' ' => "+".to_owned(),
                other => format!("%{other:02X}"),
            })
            .collect(),
    }
}

/// Rellena la plantilla.
fn render(template: &str, to: &str, text: &str, encoding: Encoding) -> String {
    template
        .replace("{{to}}", &escape(to, encoding))
        .replace("{{text}}", &escape(text, encoding))
}

impl Sms {
    /// Construye el transporte si hay un proveedor configurado.
    pub fn from_config(config: &Config) -> Option<Self> {
        let url = config.sms_url.trim();
        if url.is_empty() {
            return None;
        }
        if !super::acceptable_endpoint(url, config.production) {
            // Un SMS lleva el número de teléfono y el texto del aviso. En
            // producción eso no puede ir en claro; en desarrollo se admite
            // loopback para poder probar el canal.
            tracing::error!("TP_SMS_URL debe ser https (o loopback en desarrollo)");
            return None;
        }
        let encoding = match Encoding::parse(&config.sms_content_type) {
            Some(e) => e,
            None => {
                tracing::error!(
                    "TP_SMS_CONTENT_TYPE debe ser json o form, no {:?}",
                    config.sms_content_type
                );
                return None;
            }
        };
        let template = config.sms_body.trim();
        if !template.contains("{{to}}") || !template.contains("{{text}}") {
            tracing::error!("TP_SMS_BODY debe contener los marcadores {{to}} y {{text}}");
            return None;
        }
        let auth = config
            .sms_auth_header
            .split_once(':')
            .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
            .filter(|(name, value)| !name.is_empty() && !value.is_empty());
        if auth.is_none() && !config.sms_auth_header.trim().is_empty() {
            tracing::error!("TP_SMS_AUTH_HEADER debe tener la forma Nombre: valor");
            return None;
        }
        Some(Self {
            url: url.to_owned(),
            encoding,
            template: template.to_owned(),
            auth,
            client: super::client(),
        })
    }

    pub async fn send(&self, phone: &str, text: &str) -> Outcome {
        let body = render(&self.template, phone, text, self.encoding);
        let mut request = self
            .client
            .post(&self.url)
            .header("Content-Type", self.encoding.content_type())
            .body(body);
        if let Some((name, value)) = &self.auth {
            request = request.header(name, value);
        }
        match request.send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                match status {
                    200..=299 => Outcome::Sent,
                    // 429 y 5xx son del proveedor: se reintenta.
                    429 | 500..=599 => Outcome::Retry(format!("el proveedor respondió {status}")),
                    // 401 y 403 son configuración: reintentar no la arregla, y
                    // conviene que se note en el registro en lugar de agotar
                    // intentos en silencio.
                    401 | 403 => {
                        tracing::error!(
                            status,
                            "el proveedor de SMS rechazó las credenciales; revisa TP_SMS_AUTH_HEADER"
                        );
                        Outcome::Permanent(format!("credenciales rechazadas ({status})"))
                    }
                    _ => Outcome::Permanent(format!("el proveedor respondió {status}")),
                }
            }
            Err(e) => Outcome::Retry(format!("red: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_substitution_cannot_break_the_body() {
        // Un aviso real lleva comillas y acentos; sin escapar, el JSON quedaría
        // malformado y el proveedor devolvería 400 en cada envío.
        let body = render(
            r#"{"to":"{{to}}","message":"{{text}}"}"#,
            "+573001234567",
            r#"EVACÚA. Ve al "Parque Vipasa" (930 m)."#,
            Encoding::Json,
        );
        let parsed: serde_json::Value =
            serde_json::from_str(&body).expect("debe seguir siendo JSON");
        assert_eq!(parsed["to"], "+573001234567");
        assert_eq!(
            parsed["message"],
            r#"EVACÚA. Ve al "Parque Vipasa" (930 m)."#
        );
    }

    #[test]
    fn json_substitution_cannot_inject_fields() {
        // Sin escapar, un texto con comillas podría añadir parámetros al cuerpo.
        let body = render(
            r#"{"to":"{{to}}","message":"{{text}}"}"#,
            "+57300",
            r#"x","from":"suplantado"#,
            Encoding::Json,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(parsed.get("from").is_none(), "se inyectó un campo: {body}");
    }

    #[test]
    fn form_substitution_percent_encodes() {
        let body = render(
            "To={{to}}&Body={{text}}",
            "+573001234567",
            "Hola & adiós",
            Encoding::Form,
        );
        // El `+` del número tiene que ir codificado o el proveedor lo leería
        // como un espacio y el número quedaría sin prefijo internacional.
        assert!(body.starts_with("To=%2B573001234567"), "{body}");
        // Y el `&` del texto no puede terminar un parámetro.
        assert!(!body["To=%2B573001234567".len()..].contains("Hola & adiós"));
        assert!(body.contains("Hola+%26+adi%C3%B3s"), "{body}");
    }

    #[test]
    fn without_a_url_the_channel_stays_off() {
        let mut config = Config::for_tests();
        assert!(Sms::from_config(&config).is_none());
        config.sms_url = "   ".into();
        assert!(Sms::from_config(&config).is_none());
    }

    #[test]
    fn plain_http_to_the_internet_is_refused() {
        let mut config = Config::for_tests();
        config.sms_body = r#"{"to":"{{to}}","message":"{{text}}"}"#.into();
        config.sms_url = "http://proveedor.example/send".into();
        assert!(
            Sms::from_config(&config).is_none(),
            "un SMS lleva el teléfono y el texto: no puede ir en claro"
        );
        // Loopback sí, para poder probar el canal en desarrollo.
        config.sms_url = "http://127.0.0.1:9999/send".into();
        assert!(Sms::from_config(&config).is_some());
        // Pero no en producción.
        config.production = true;
        assert!(Sms::from_config(&config).is_none());
    }

    #[test]
    fn a_template_without_placeholders_is_refused() {
        let mut config = Config::for_tests();
        config.sms_url = "https://proveedor.example/send".into();
        // Sin marcadores, el proveedor recibiría el mismo cuerpo fijo para todos.
        config.sms_body = r#"{"to":"fijo","message":"fijo"}"#.into();
        assert!(Sms::from_config(&config).is_none());
        config.sms_body = r#"{"to":"{{to}}","message":"fijo"}"#.into();
        assert!(Sms::from_config(&config).is_none(), "falta {{text}}");
        config.sms_body = r#"{"to":"{{to}}","message":"{{text}}"}"#.into();
        assert!(Sms::from_config(&config).is_some());
    }

    #[test]
    fn a_malformed_auth_header_is_refused() {
        let mut config = Config::for_tests();
        config.sms_url = "https://proveedor.example/send".into();
        config.sms_body = r#"{"to":"{{to}}","message":"{{text}}"}"#.into();
        // Sin los dos puntos no se puede separar nombre y valor; arrancar sin
        // autorización mandaría los avisos a un 401 en cada intento.
        config.sms_auth_header = "Bearer algo".into();
        assert!(Sms::from_config(&config).is_none());
        config.sms_auth_header = "Authorization: Bearer algo".into();
        assert!(Sms::from_config(&config).is_some());
        // Vacío es válido: hay proveedores que autentican en la propia URL.
        config.sms_auth_header = String::new();
        assert!(Sms::from_config(&config).is_some());
    }

    #[test]
    fn content_type_names_are_accepted_in_both_forms() {
        assert_eq!(Encoding::parse("json"), Some(Encoding::Json));
        assert_eq!(Encoding::parse("application/json"), Some(Encoding::Json));
        assert_eq!(Encoding::parse("form"), Some(Encoding::Form));
        assert_eq!(
            Encoding::parse("application/x-www-form-urlencoded"),
            Some(Encoding::Form)
        );
        assert_eq!(Encoding::parse("xml"), None);
    }
}
