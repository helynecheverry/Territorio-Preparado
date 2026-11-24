//! Transporte Web Push (RFC 8030, 8291 y 8292).
//!
//! Es el canal sin coste por mensaje: el servidor firma y cifra el aviso y lo
//! entrega al servicio de push del propio navegador —Mozilla, Google, Apple—, sin
//! intermediario que cobre ni cuota que negociar. Por eso el plan gratuito lo
//! tiene en todos los niveles.
//!
//! El cifrado y la firma los construye `web-push-native`, en Rust puro. Se eligió
//! frente al crate `web-push`, más conocido, porque aquel arrastra `ece` con
//! OpenSSL y este proyecto es deliberadamente libre de OpenSSL. La petición
//! firmada se envía con `reqwest` y rustls, igual que el resto del proyecto.
//!
//! # Lo que el navegador tiene que haber hecho antes
//!
//! Una suscripción de push la crea el navegador llamando a `pushManager.subscribe`
//! con la clave pública VAPID del servidor, que se sirve en
//! `GET /api/v1/push-public-key`. El resultado —`endpoint`, `p256dh` y `auth`— se
//! registra con `POST /api/v1/push-devices`. Sin esos tres valores no hay a quién
//! cifrarle nada.

use super::Outcome;
use crate::config::Config;
use base64::Engine;
use web_push_native::{
    Auth,
    WebPushBuilder,
    // El acceso a la clave pública P-256 vive en un trait, no en el tipo.
    jwt_simple::algorithms::{ECDSAP256PublicKeyLike, ES256KeyPair},
    p256::PublicKey,
};

/// Suscripción de push de un navegador concreto.
pub struct Device {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

pub struct WebPush {
    key: ES256KeyPair,
    contact: String,
    client: reqwest::Client,
}

/// Base64 sin relleno y con alfabeto seguro para URL, que es como el navegador
/// entrega las claves de la suscripción.
fn decode(value: &str) -> anyhow::Result<Vec<u8>> {
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(value.trim())?)
}

impl WebPush {
    /// Construye el transporte si hay clave VAPID configurada.
    ///
    /// Devolver `None` desactiva el canal sin necesidad de una bandera aparte que
    /// pudiera quedar descoordinada con la clave.
    pub fn from_config(config: &Config) -> Option<Self> {
        let raw = config.vapid_private_key.trim();
        if raw.is_empty() {
            return None;
        }
        let bytes = match decode(raw) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(error = %e, "TP_VAPID_PRIVATE_KEY no es base64url válido");
                return None;
            }
        };
        let key = match ES256KeyPair::from_bytes(&bytes) {
            Ok(k) => k,
            Err(e) => {
                tracing::error!(error = %e, "TP_VAPID_PRIVATE_KEY no es una clave P-256 válida");
                return None;
            }
        };
        if config.vapid_contact.trim().is_empty() {
            // RFC 8292 pide un contacto en la reclamación `sub`: es cómo el
            // servicio de push avisa al operador si algo va mal con sus envíos.
            tracing::error!("TP_VAPID_CONTACT es obligatorio junto a la clave VAPID");
            return None;
        }
        Some(Self {
            key,
            contact: config.vapid_contact.trim().to_owned(),
            client: super::client(),
        })
    }

    /// Clave pública que el navegador necesita para suscribirse, en base64url.
    pub fn public_key(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(self.key.public_key().public_key().to_bytes_uncompressed())
    }

    pub async fn send(&self, device: &Device, payload: Vec<u8>) -> Outcome {
        let endpoint = match device.endpoint.parse() {
            Ok(u) => u,
            Err(e) => return Outcome::Permanent(format!("endpoint inválido: {e}")),
        };
        let public = match decode(&device.p256dh).and_then(|b| {
            PublicKey::from_sec1_bytes(&b).map_err(|e| anyhow::anyhow!("p256dh inválido: {e}"))
        }) {
            Ok(k) => k,
            Err(e) => return Outcome::Permanent(e.to_string()),
        };
        let auth = match decode(&device.auth) {
            // `Auth` son exactamente 16 bytes y construirlo con otra longitud
            // entra en pánico, así que se comprueba antes en lugar de confiar.
            Ok(b) if b.len() == 16 => Auth::clone_from_slice(&b),
            Ok(b) => {
                return Outcome::Permanent(format!("auth debe tener 16 bytes, tiene {}", b.len()));
            }
            Err(e) => return Outcome::Permanent(e.to_string()),
        };

        let built = WebPushBuilder::new(endpoint, public, auth)
            .with_vapid(&self.key, &self.contact)
            .build(payload);
        let request = match built {
            Ok(r) => r,
            Err(e) => return Outcome::Permanent(format!("no se pudo cifrar el aviso: {e}")),
        };

        let (parts, body) = request.into_parts();
        let mut send = self.client.post(parts.uri.to_string()).body(body);
        for (name, value) in parts.headers.iter() {
            send = send.header(name, value);
        }
        match send.send().await {
            Ok(response) => classify(response.status().as_u16()),
            // Un fallo de red es temporal por definición: se reintenta.
            Err(e) => Outcome::Retry(format!("red: {e}")),
        }
    }
}

/// Traduce la respuesta del servicio de push a una decisión.
///
/// La distinción que importa es `Gone`: un 404 o 410 significa que el navegador
/// canceló la suscripción —se desinstaló la aplicación, se limpiaron los datos del
/// sitio— y seguir intentándolo no la va a resucitar. Ese dispositivo se borra,
/// porque un registro muerto consume un envío en cada emergencia.
fn classify(status: u16) -> Outcome {
    match status {
        200..=299 => Outcome::Sent,
        404 | 410 => Outcome::Gone,
        // 429 pide esperar; 5xx es el servicio, no el mensaje.
        429 | 500..=599 => Outcome::Retry(format!("el servicio de push respondió {status}")),
        // 400 mal formado, 403 firma VAPID rechazada, 413 demasiado grande.
        _ => Outcome::Permanent(format!("el servicio de push respondió {status}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_subscription_is_deleted_not_retried() {
        assert!(matches!(classify(404), Outcome::Gone));
        assert!(matches!(classify(410), Outcome::Gone));
    }

    #[test]
    fn throttling_and_outages_are_retried() {
        assert!(matches!(classify(429), Outcome::Retry(_)));
        assert!(matches!(classify(500), Outcome::Retry(_)));
        assert!(matches!(classify(503), Outcome::Retry(_)));
    }

    #[test]
    fn a_rejected_message_is_not_retried_forever() {
        // Reintentar una firma rechazada o un cuerpo demasiado grande solo gasta
        // intentos: el resultado va a ser el mismo.
        assert!(matches!(classify(400), Outcome::Permanent(_)));
        assert!(matches!(classify(403), Outcome::Permanent(_)));
        assert!(matches!(classify(413), Outcome::Permanent(_)));
    }

    #[test]
    fn success_covers_the_whole_2xx_range() {
        // Los servicios de push responden 201, y algunos 202.
        for status in [200, 201, 202] {
            assert!(matches!(classify(status), Outcome::Sent), "{status}");
        }
    }

    #[test]
    fn without_a_key_the_channel_stays_off() {
        let mut config = Config::for_tests();
        assert!(WebPush::from_config(&config).is_none());
        config.vapid_private_key = "   ".into();
        assert!(WebPush::from_config(&config).is_none());
        // Una clave que no es base64url tampoco activa el canal: es mejor que no
        // envíe nada que que falle en cada aviso.
        config.vapid_private_key = "no-es-base64-!!".into();
        assert!(WebPush::from_config(&config).is_none());
    }

    #[test]
    fn a_key_without_a_contact_is_refused() {
        let mut config = Config::for_tests();
        // Clave válida de 32 bytes, generada para esta prueba.
        config.vapid_private_key =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7u8; 32]);
        config.vapid_contact = String::new();
        assert!(
            WebPush::from_config(&config).is_none(),
            "RFC 8292 exige un contacto en la reclamación sub"
        );
        config.vapid_contact = "mailto:ops@example.com".into();
        let transport = WebPush::from_config(&config).expect("con clave y contacto debe activarse");
        // La clave pública se sirve al navegador en formato sin comprimir, 65
        // bytes, que es lo que espera `pushManager.subscribe`.
        let public = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(transport.public_key())
            .unwrap();
        assert_eq!(public.len(), 65);
        assert_eq!(public[0], 0x04, "debe ser un punto sin comprimir");
    }
}
