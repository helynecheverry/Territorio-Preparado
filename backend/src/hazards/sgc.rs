//! Fuente SGC: el Servicio Geológico Colombiano, autoridad sísmica nacional.
//!
//! # Por qué está desactivada por defecto
//!
//! El SGC es la autoridad para Colombia y conoce su territorio mejor que
//! cualquier catálogo global, así que tiene precedencia sobre USGS. Pero **no
//! publica una API**. Se comprobó, al escribir este módulo, que:
//!
//! - No responde en `/fdsnws/event/1/...`, el estándar FDSN que usa USGS.
//! - `sismo.sgc.gov.co` es una aplicación Django que carga los sismos por un
//!   `POST ajax/query` protegido con `csrfmiddlewaretoken`, cuyo contrato de
//!   entrada y salida no está documentado en ninguna parte pública.
//! - `www.sgc.gov.co` está detrás de un CDN que responde 403 a clientes que no
//!   se presentan como navegador.
//!
//! Raspar ese endpoint pondría una alerta ciudadana a depender de un formato
//! HTML que puede cambiar sin aviso, y lo haría fallar precisamente cuando el
//! portal esté bajo carga, que es durante un sismo fuerte. Así que la fuente
//! queda implementada y conectada al motor, pero solo se activa si `TP_SGC_URL`
//! apunta a un servicio **compatible con FDSN**.
//!
//! El camino correcto es solicitar al SGC acceso a su catálogo —la Red Sismológica
//! Nacional de Colombia ya comparte datos con la comunidad FDSN— y poner esa URL
//! en la configuración. Mientras eso no exista, USGS cubre el territorio: detecta
//! con fiabilidad los sismos que importan para avisar, y los locales pequeños que
//! solo ve el SGC no alcanzan los umbrales de aviso de todas formas.

use super::{EventSource, Observation, usgs};
use crate::config::Config;

pub struct Sgc {
    url: String,
    bbox: [f64; 4],
    min_magnitude: f64,
    lookback_minutes: i64,
    client: reqwest::Client,
}

impl Sgc {
    /// Construye la fuente solo si hay una URL configurada. Devolver `None` es lo
    /// que mantiene la fuente fuera de la lista activa sin necesidad de una
    /// bandera aparte que pudiera quedar descoordinada.
    pub fn from_config(config: &Config) -> Option<Self> {
        let url = config.sgc_url.trim();
        if url.is_empty() {
            return None;
        }
        Some(Self {
            url: url.to_owned(),
            bbox: config.hazard_bbox,
            min_magnitude: config.hazard_min_magnitude,
            lookback_minutes: config.hazard_lookback_minutes,
            client: usgs::client(),
        })
    }
}

impl EventSource for Sgc {
    fn name(&self) -> &'static str {
        "SGC"
    }
    /// Por encima de USGS: cuando ambos publican el mismo sismo, manda la
    /// autoridad nacional.
    fn authority_rank(&self) -> i32 {
        100
    }
    fn fetch(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<Vec<Observation>>> + Send + '_>,
    > {
        Box::pin(usgs::fetch_fdsn(
            &self.client,
            &self.url,
            "SGC",
            self.bbox,
            self.min_magnitude,
            self.lookback_minutes,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(sgc_url: &str) -> Config {
        let mut c = Config::for_tests();
        c.sgc_url = sgc_url.into();
        c
    }

    #[test]
    fn without_a_url_the_source_stays_off() {
        assert!(Sgc::from_config(&config("")).is_none());
        assert!(Sgc::from_config(&config("   ")).is_none());
    }

    #[test]
    fn with_a_url_the_source_outranks_usgs() {
        let sgc = Sgc::from_config(&config("https://example.test/fdsnws/event/1/query")).unwrap();
        let usgs = usgs::Usgs::from_config(&Config::for_tests());
        assert_eq!(sgc.name(), "SGC");
        assert!(
            sgc.authority_rank() > usgs.authority_rank(),
            "la autoridad nacional debe ganar al catálogo global"
        );
    }
}
