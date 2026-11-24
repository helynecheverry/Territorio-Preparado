//! Cobro del plan de pago.
//!
//! # La referencia es nuestra
//!
//! El diseño descansa en una decisión: **nunca se confía en que el proveedor diga
//! de quién es el pago.** Al iniciar el cobro se crea aquí una referencia propia,
//! con la cuenta y el plan ya atados a ella, y el proveedor solo tiene que
//! informar de que *esa referencia* se pagó. Si la cuenta viniera en el cuerpo del
//! webhook, falsificar un evento otorgaría el plan a cualquiera.
//!
//! Por el mismo motivo se compara el **importe**. Sin esa comprobación, pagar mil
//! pesos de un plan de cincuenta mil daría el plan igual.
//!
//! # Verificación del webhook
//!
//! El webhook no se autentica por cookie —lo llama un servidor, no un navegador—
//! sino con HMAC-SHA256 sobre el cuerpo **crudo**, en comparación de tiempo
//! constante. Y falla cerrado: sin `TP_BILLING_WEBHOOK_SECRET` la ruta responde
//! 503 en lugar de aceptar lo que llegue.
//!
//! Hay tres defensas más, y cada una cubre un ataque distinto:
//!
//! - **Ventana temporal**: una firma válida capturada ayer no sirve hoy, así que
//!   reenviar un evento antiguo no vuelve a otorgar el plan.
//! - **Idempotencia** por identificador de evento: los proveedores reintentan, y
//!   un reintento no puede sumar otro mes de plan.
//! - **Libro de eventos**: todo lo recibido queda registrado, incluido lo que no
//!   se pudo aplicar, porque conciliar un cobro disputado requiere el rastro.
//!
//! # Por qué es configurable y no específico de un proveedor
//!
//! Cada pasarela firma distinto y nombra sus campos distinto, y la elección
//! —Mercado Pago, dLocal, Wompi, ePayco— depende de los países que se cubran.
//! Atar el código a una obligaría a tocar Rust para cambiar de pasarela. Lo que
//! **no** es configurable es ninguna de las defensas de arriba: eso no se negocia
//! por entorno.
//!
//! No hay plantillas de proveedores concretos aquí. No están verificadas contra
//! sus servicios, y en una firma de webhook una plantilla equivocada significa o
//! rechazar cobros reales o aceptar eventos falsificados.

use crate::{
    AppState,
    alerts::entitlements::{self, Plan},
    auth, db,
    error::{self, Result},
    legal, security,
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use mongodb::bson::{DateTime, Document, doc};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;

/// Documento legal cuya aceptación se exige antes de cobrar.
const TERMS: &str = "terms";

/// Ruta del webhook, exenta de la comprobación de `Origin` en `security::guard`.
///
/// La exención es segura y necesaria: la comprobación de `Origin` protege contra
/// CSRF, que consiste en que un sitio ajeno haga que **el navegador** emita una
/// escritura autenticada por cookie. Esta ruta no acepta autenticación por cookie
/// en absoluto —se valida con firma HMAC— así que no hay sesión que un tercero
/// pueda aprovechar, y un servidor de pagos no envía nuestro `Origin`.
pub const WEBHOOK_PATH: &str = "/api/v1/billing/webhook";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/billing/plans", get(plans))
        .route("/billing/me", get(me))
        .route("/billing/checkout", post(checkout))
        .route("/billing/webhook", post(webhook))
}

/// Valor en un documento JSON por ruta con puntos, por ejemplo `data.status`.
fn at<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |acc, key| acc.get(key))
}

/// Primer token hexadecimal de 64 caracteres del valor del encabezado.
///
/// Admite tanto un encabezado que sea solo el resumen como uno compuesto del
/// estilo `ts=1699999999,v1=abc…`, sin necesidad de otra variable de entorno
/// para describir el formato.
fn digest_in(header: &str) -> Option<&str> {
    header
        .split(|c: char| !c.is_ascii_hexdigit())
        .find(|token| token.len() == 64)
}

/// Comparación en tiempo constante, para no filtrar el secreto por el tiempo de
/// respuesta.
fn same_digest(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.as_bytes().ct_eq(b.as_bytes()).into()
}

/// Precio del plan de pago, tal como se cobra y se comprueba.
fn price(s: &AppState) -> Value {
    json!({
        "plan": Plan::Paid.as_str(),
        "amount_cents": s.config.paid_plan_amount_cents,
        "currency": s.config.paid_plan_currency,
        "days": s.config.paid_plan_days,
    })
}

/// Catálogo de planes, para que la interfaz tenga qué mostrar.
async fn plans(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    auth::actor(&s, &h).await?;
    Ok(Json(json!({
        "items": [
            {
                "plan": Plan::Free.as_str(),
                "amount_cents": 0,
                "currency": s.config.paid_plan_currency,
                "includes": ["Avisos por Web Push en todos los niveles", "Contenido educativo"],
            },
            {
                "plan": Plan::Paid.as_str(),
                "amount_cents": s.config.paid_plan_amount_cents,
                "currency": s.config.paid_plan_currency,
                "days": s.config.paid_plan_days,
                "includes": [
                    "Todo lo del plan gratuito",
                    "SMS en avisos de evacuación, que llegan sin datos móviles",
                ],
            }
        ],
        "provider": s.config.billing_provider,
        // Lo que se cobra y lo que no, en la respuesta y no solo en los términos.
        "meaning": "Se cobra el servicio de monitoreo y el contenido educativo. \
                    Ningún canal garantiza la entrega de un aviso: Web Push depende del \
                    servicio del navegador y el SMS de una red que en una emergencia se \
                    degrada. El plan gratuito recibe avisos en todos los niveles.",
    })))
}

/// Plan vigente de la propia cuenta.
async fn me(State(s): State<AppState>, h: HeaderMap) -> Result<Json<Value>> {
    let a = auth::actor(&s, &h).await?;
    let plan = entitlements::plan_for(&s.db, &a.id).await?;
    let record =
        s.db.collection::<Document>("entitlements")
            .find_one(doc! {"_id": &a.id})
            .await?;
    let terms = legal::current(&s.db, TERMS).await.ok();
    let accepted = match &terms {
        Some(t) => legal::accepted(&s.db, &a.id, TERMS, &t.version).await?,
        None => false,
    };
    Ok(Json(json!({
        "plan": plan.as_str(),
        "valid_until": record
            .as_ref()
            .and_then(|d| d.get_datetime("valid_until").ok())
            .map(ToString::to_string),
        "terms_version": terms.as_ref().map(|t| t.version.clone()),
        "terms_accepted": accepted,
    })))
}

/// Inicia un cobro y devuelve la referencia que el proveedor debe informar.
///
/// Exige que la versión vigente de los términos esté aceptada. No se cobra a
/// alguien que no ha aceptado el texto que dice qué se le está vendiendo.
async fn checkout(State(s): State<AppState>, h: HeaderMap) -> Result<(StatusCode, Json<Value>)> {
    let a = auth::actor(&s, &h).await?;
    security::limit(&s, "billing-checkout", &a.id, 10, 3600).await?;
    if s.config.billing_provider.trim().is_empty() {
        return Err(error::Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "El cobro no está configurado en este despliegue",
        ));
    }
    let terms = legal::current(&s.db, TERMS).await?;
    if !legal::accepted(&s.db, &a.id, TERMS, &terms.version).await? {
        return Err(error::Error(
            StatusCode::CONFLICT,
            "Acepta los términos vigentes antes de contratar",
        ));
    }

    // La referencia la generamos aquí, con la cuenta y el plan ya atados.
    let reference = format!("tp-{}", uuid::Uuid::new_v4().simple());
    db::audit(&s.db, &a.id, "billing_checkout", &reference).await?;
    s.db.collection::<Document>("payments")
        .insert_one(doc! {
            "_id": &reference,
            "owner": &a.id,
            "plan": Plan::Paid.as_str(),
            "amount_cents": s.config.paid_plan_amount_cents,
            "currency": &s.config.paid_plan_currency,
            "days": s.config.paid_plan_days,
            "status": "pending",
            "terms_version": &terms.version,
            "created": db::now(),
            "updated": db::now(),
        })
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "reference": reference,
            "provider": s.config.billing_provider,
            "price": price(&s),
            "terms_version": terms.version,
        })),
    ))
}

/// Resultado de verificar la autenticidad de un webhook.
enum Verdict {
    Ok(Value),
    /// Rechazado: firma inválida, fuera de ventana o cuerpo ilegible.
    Rejected(&'static str),
}

/// Comprueba firma y ventana temporal sobre el cuerpo crudo.
///
/// El HMAC se calcula sobre los bytes recibidos y no sobre un JSON reserializado:
/// reserializar cambia el orden y el espaciado, y la firma no coincidiría.
fn verify(c: &crate::config::Config, headers: &HeaderMap, body: &[u8]) -> Verdict {
    let Some(signature) = headers
        .get(c.billing_signature_header.as_str())
        .and_then(|v| v.to_str().ok())
        .and_then(digest_in)
    else {
        return Verdict::Rejected("falta la firma del webhook");
    };

    // Marca de tiempo: del encabezado o del cuerpo, según se configure.
    let parsed: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return Verdict::Rejected("el cuerpo no es JSON"),
    };
    let (source, path) = c
        .billing_timestamp
        .split_once(':')
        .unwrap_or(("header", "x-timestamp"));
    let timestamp = match source {
        "body" => at(&parsed, path).and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|t| t.parse().ok()))
        }),
        _ => headers
            .get(path)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok()),
    };
    let Some(timestamp) = timestamp else {
        return Verdict::Rejected("falta la marca de tiempo del webhook");
    };
    let age = (DateTime::now().timestamp_millis() / 1000 - timestamp).abs();
    if age > c.billing_tolerance_seconds as i64 {
        // Una firma válida capturada ayer no puede volver a otorgar el plan.
        return Verdict::Rejected("el webhook está fuera de la ventana temporal");
    }

    let signed = c
        .billing_signed_template
        .replace("{{ts}}", &timestamp.to_string())
        .replace("{{body}}", &String::from_utf8_lossy(body));
    let expected = security::keyed(&c.billing_webhook_secret, &signed);
    if !same_digest(&expected, signature) {
        return Verdict::Rejected("la firma del webhook no coincide");
    }
    Verdict::Ok(parsed)
}

/// Recibe la confirmación de pago del proveedor.
///
/// Responde 200 a todo lo que se haya podido registrar, incluido un evento
/// repetido o uno cuya referencia no reconocemos: así el proveedor deja de
/// reintentar y el caso queda en el libro de eventos y en el registro para
/// conciliarlo. Solo devuelve error cuando la petición no es auténtica.
async fn webhook(
    State(s): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>)> {
    if s.config.billing_webhook_secret.trim().is_empty() {
        // Falla cerrado: sin secreto no se puede distinguir un evento real de uno
        // falsificado, y aceptar sería peor que no aceptar.
        return Err(error::Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "El webhook de cobro no está configurado",
        ));
    }
    if body.len() > 256 * 1024 {
        return Err(error::bad("Cuerpo de webhook demasiado grande"));
    }
    let parsed = match verify(&s.config, &headers, &body) {
        Verdict::Ok(v) => v,
        Verdict::Rejected(reason) => {
            tracing::warn!(reason, "Webhook de cobro rechazado");
            return Err(error::Error(StatusCode::UNAUTHORIZED, reason));
        }
    };

    let event_id = at(&parsed, &s.config.billing_path_event_id)
        .and_then(|v| {
            v.as_str()
                .map(ToOwned::to_owned)
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        })
        .filter(|v| !v.is_empty() && v.len() <= 120)
        .ok_or(error::bad("El evento no trae identificador"))?;
    let key = format!("{}:{event_id}", s.config.billing_provider);

    // Idempotencia: los proveedores reintentan, y un reintento no puede sumar
    // otro mes de plan. El choque de clave única es la señal de que ya se aplicó.
    let events = s.db.collection::<Document>("payment_events");
    let first_time = events
        .insert_one(doc! {
            "_id": &key,
            "provider": &s.config.billing_provider,
            "event_id": &event_id,
            "received": db::now(),
            // Se guarda el evento tal cual para poder conciliar un cobro
            // disputado sin depender del panel del proveedor.
            "payload": mongodb::bson::to_bson(&parsed).unwrap_or(mongodb::bson::Bson::Null),
        })
        .await;
    if let Err(e) = first_time {
        if e.to_string().contains("E11000") {
            tracing::info!(event = %key, "Webhook repetido; ya estaba aplicado");
            return Ok((StatusCode::OK, Json(json!({"status": "already_applied"}))));
        }
        return Err(error::Error::from(e));
    }

    let status = at(&parsed, &s.config.billing_path_status)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let reference = at(&parsed, &s.config.billing_path_reference)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let amount = at(&parsed, &s.config.billing_path_amount_cents).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|t| t.parse().ok()))
    });

    if !status.eq_ignore_ascii_case(&s.config.billing_success_status) {
        // Un rechazo también se registra: es información para soporte.
        events
            .update_one(
                doc! {"_id": &key},
                doc! {"$set": {"outcome": "not_approved", "status": &status}},
            )
            .await?;
        if !reference.is_empty() {
            s.db.collection::<Document>("payments")
                .update_one(
                    doc! {"_id": &reference, "status": "pending"},
                    doc! {"$set": {"status": "rejected", "provider_status": &status, "updated": db::now()}},
                )
                .await?;
        }
        return Ok((StatusCode::OK, Json(json!({"status": "not_approved"}))));
    }

    let payment =
        s.db.collection::<Document>("payments")
            .find_one(doc! {"_id": &reference})
            .await?;
    let Some(payment) = payment else {
        // Referencia desconocida: un pago que no originamos. Se registra con
        // ruido en el log y se responde 200 para que el proveedor no reintente
        // indefinidamente; el libro de eventos queda para conciliar.
        tracing::error!(
            event = %key,
            %reference,
            "Pago aprobado con referencia desconocida; requiere conciliación manual"
        );
        events
            .update_one(
                doc! {"_id": &key},
                doc! {"$set": {"outcome": "unknown_reference", "reference": &reference}},
            )
            .await?;
        return Ok((StatusCode::OK, Json(json!({"status": "unknown_reference"}))));
    };

    let expected_amount = payment.get_i64("amount_cents").unwrap_or(i64::MAX);
    let expected_currency = payment.get_str("currency").unwrap_or_default();
    let currency = at(&parsed, &s.config.billing_path_currency)
        .and_then(Value::as_str)
        .unwrap_or(expected_currency);
    if amount != Some(expected_amount) || !currency.eq_ignore_ascii_case(expected_currency) {
        // Sin esta comprobación, pagar mil pesos de un plan de cincuenta mil
        // daría el plan igual.
        tracing::error!(
            event = %key,
            %reference,
            expected = expected_amount,
            received = ?amount,
            "Importe o moneda no coinciden; no se otorga el plan"
        );
        events
            .update_one(
                doc! {"_id": &key},
                doc! {"$set": {"outcome": "amount_mismatch", "reference": &reference}},
            )
            .await?;
        return Ok((StatusCode::OK, Json(json!({"status": "amount_mismatch"}))));
    }

    let owner = payment.get_str("owner").unwrap_or_default().to_owned();
    let days = payment.get_i64("days").unwrap_or(0);
    entitlements::grant(
        &s.db,
        &format!("billing:{}", s.config.billing_provider),
        &owner,
        Plan::Paid,
        days,
        Some(&key),
    )
    .await?;
    s.db.collection::<Document>("payments")
        .update_one(
            doc! {"_id": &reference},
            doc! {"$set": {"status": "paid", "provider_event": &key, "updated": db::now()}},
        )
        .await?;
    events
        .update_one(
            doc! {"_id": &key},
            doc! {"$set": {"outcome": "applied", "reference": &reference, "owner": &owner}},
        )
        .await?;
    tracing::info!(event = %key, %reference, days, "Plan otorgado por pago confirmado");
    Ok((StatusCode::OK, Json(json!({"status": "applied"}))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_paths_read_nested_values() {
        let v = json!({"data": {"id": "evt-1", "transaction": {"status": "APPROVED"}}});
        assert_eq!(at(&v, "data.id").unwrap(), "evt-1");
        assert_eq!(at(&v, "data.transaction.status").unwrap(), "APPROVED");
        assert!(at(&v, "data.missing").is_none());
        assert!(at(&v, "nope.nope").is_none());
    }

    #[test]
    fn the_digest_is_found_in_bare_and_composite_headers() {
        let digest = "a".repeat(64);
        assert_eq!(digest_in(&digest), Some(digest.as_str()));
        // Formato compuesto, como el de varias pasarelas.
        let composite = format!("ts=1699999999,v1={digest}");
        assert_eq!(digest_in(&composite), Some(digest.as_str()));
        // Un resumen de longitud equivocada no se acepta como firma.
        assert!(digest_in(&"b".repeat(63)).is_none());
        assert!(digest_in("sin-firma").is_none());
        assert!(digest_in("").is_none());
    }

    #[test]
    fn digests_of_different_length_never_match() {
        let a = "a".repeat(64);
        assert!(same_digest(&a, &a));
        assert!(!same_digest(&a, &"a".repeat(63)));
        assert!(!same_digest(&a, &"b".repeat(64)));
        assert!(!same_digest("", "a"));
    }

    fn signed_config() -> crate::config::Config {
        let mut c = crate::config::Config::for_tests();
        c.billing_provider = "pruebas".into();
        c.billing_webhook_secret = "secreto-de-pruebas".into();
        c
    }

    /// Firma un cuerpo como lo haría el proveedor.
    fn sign(c: &crate::config::Config, timestamp: i64, body: &str) -> String {
        let signed = c
            .billing_signed_template
            .replace("{{ts}}", &timestamp.to_string())
            .replace("{{body}}", body);
        security::keyed(&c.billing_webhook_secret, &signed)
    }

    fn now() -> i64 {
        DateTime::now().timestamp_millis() / 1000
    }

    fn signed_headers(c: &crate::config::Config, timestamp: i64, body: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("x-signature", sign(c, timestamp, body).parse().unwrap());
        h.insert("x-timestamp", timestamp.to_string().parse().unwrap());
        h
    }

    #[test]
    fn a_correctly_signed_webhook_is_accepted() {
        let c = signed_config();
        let body = r#"{"data":{"id":"evt-1","status":"APPROVED"}}"#;
        let headers = signed_headers(&c, now(), body);
        match verify(&c, &headers, body.as_bytes()) {
            Verdict::Ok(parsed) => assert_eq!(at(&parsed, "data.id").unwrap(), "evt-1"),
            Verdict::Rejected(r) => panic!("debía aceptarse: {r}"),
        }
    }

    #[test]
    fn a_tampered_body_is_rejected() {
        // Es el ataque que la firma existe para impedir: cambiar el importe o la
        // referencia de un evento auténtico.
        let c = signed_config();
        let original = r#"{"data":{"id":"evt-1","amount_in_cents":2000000}}"#;
        let headers = signed_headers(&c, now(), original);
        let tampered = r#"{"data":{"id":"evt-1","amount_in_cents":100}}"#;
        assert!(matches!(
            verify(&c, &headers, tampered.as_bytes()),
            Verdict::Rejected(_)
        ));
    }

    #[test]
    fn a_replayed_webhook_is_rejected() {
        // Una firma válida capturada ayer no puede volver a otorgar el plan.
        let c = signed_config();
        let body = r#"{"data":{"id":"evt-1"}}"#;
        let old = now() - c.billing_tolerance_seconds as i64 - 60;
        let headers = signed_headers(&c, old, body);
        assert!(matches!(
            verify(&c, &headers, body.as_bytes()),
            Verdict::Rejected("el webhook está fuera de la ventana temporal")
        ));
    }

    #[test]
    fn a_missing_signature_or_timestamp_is_rejected() {
        let c = signed_config();
        let body = r#"{"data":{"id":"evt-1"}}"#;
        assert!(matches!(
            verify(&c, &HeaderMap::new(), body.as_bytes()),
            Verdict::Rejected("falta la firma del webhook")
        ));
        let mut only_signature = HeaderMap::new();
        only_signature.insert("x-signature", sign(&c, now(), body).parse().unwrap());
        assert!(matches!(
            verify(&c, &only_signature, body.as_bytes()),
            Verdict::Rejected("falta la marca de tiempo del webhook")
        ));
    }

    #[test]
    fn a_body_that_is_not_json_is_rejected() {
        let c = signed_config();
        let body = "no soy json";
        let headers = signed_headers(&c, now(), body);
        assert!(matches!(
            verify(&c, &headers, body.as_bytes()),
            Verdict::Rejected("el cuerpo no es JSON")
        ));
    }

    #[test]
    fn the_timestamp_can_come_from_the_body() {
        // Hay pasarelas que la ponen en el cuerpo en lugar del encabezado.
        let mut c = signed_config();
        c.billing_timestamp = "body:data.timestamp".into();
        let timestamp = now();
        let body = format!(r#"{{"data":{{"id":"evt-1","timestamp":{timestamp}}}}}"#);
        let mut headers = HeaderMap::new();
        headers.insert("x-signature", sign(&c, timestamp, &body).parse().unwrap());
        assert!(matches!(
            verify(&c, &headers, body.as_bytes()),
            Verdict::Ok(_)
        ));
    }

    #[test]
    fn the_signed_template_covers_timestamp_and_raw_body() {
        // La firma depende de los dos: cambiar cualquiera la invalida, que es lo
        // que impide reutilizar una firma con otro cuerpo o con otra hora.
        let c = signed_config();
        let body = r#"{"data":{"id":"evt-1"}}"#;
        let a = sign(&c, 1_700_000_000, body);
        assert_ne!(a, sign(&c, 1_700_000_001, body));
        assert_ne!(a, sign(&c, 1_700_000_000, r#"{"data":{"id":"evt-2"}}"#));
        // Y del secreto.
        let mut other = signed_config();
        other.billing_webhook_secret = "otro-secreto".into();
        assert_ne!(a, sign(&other, 1_700_000_000, body));
    }
}
