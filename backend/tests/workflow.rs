//! Real HTTP + MongoDB integration. Isolated database; opt in with --ignored.
use futures_util::TryStreamExt;
use mongodb::{Client, bson::doc};
use reqwest::{Client as Http, StatusCode};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};
struct Server {
    child: Child,
    port: u16,
    db: String,
    mail: PathBuf,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Server {
    fn start(db: String, mail: PathBuf, port: u16) -> Self {
        Self::start_with(db, mail, port, Some(BILLING_SECRET))
    }
    /// `billing_secret` en `None` deja el webhook de cobro sin configurar, que es
    /// como se comprueba que falla cerrado en lugar de aceptar lo que llegue.
    fn start_with(db: String, mail: PathBuf, port: u16, billing_secret: Option<&str>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_territorio-api"));
        command
            .arg("api")
            .env("TP_BILLING_PROVIDER", "pruebas")
            .env("TP_VAPID_PRIVATE_KEY", VAPID)
            .env("TP_VAPID_CONTACT", "mailto:ops@ejemplo.test")
            .env("TP_SMS_URL", SMS_URL_PLACEHOLDER)
            .env("TP_PAID_PLAN_AMOUNT_CENTS", "2000000")
            .env("TP_PAID_PLAN_CURRENCY", "COP")
            .env("TP_PAID_PLAN_DAYS", "30");
        if let Some(secret) = billing_secret {
            command.env("TP_BILLING_WEBHOOK_SECRET", secret);
        }
        let child = command
            .env(
                "TP_SECRET",
                "test-secret-longer-than-thirty-two-characters-only-tests",
            )
            .env("TP_MONGO_DB", &db)
            .env("TP_BIND", format!("127.0.0.1:{port}"))
            .env("TP_ORIGIN", format!("http://127.0.0.1:{port}"))
            .env("TP_ENV", "development")
            .env("TP_SMTP_HOST", "")
            .env("TP_MAIL_DIR", &mail)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Self {
            child,
            port,
            db,
            mail,
        }
    }
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/api/{path}", self.port)
    }
    async fn ready(&self) {
        for _ in 0..100 {
            if Http::new()
                .get(self.url("health"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("server failed to start")
    }
    async fn post(&self, c: &Http, path: &str, body: Value) -> reqwest::Response {
        c.post(self.url(path))
            .header("Origin", format!("http://127.0.0.1:{}", self.port))
            .json(&body)
            .send()
            .await
            .unwrap()
    }
    async fn code(&self, id: &str) -> String {
        let mail = tokio::fs::read_to_string(self.mail.join(format!("{id}.eml")))
            .await
            .unwrap();
        mail.split("es: ").nth(1).unwrap()[..6].to_owned()
    }
    async fn register(&self, c: &Http, email: &str) -> String {
        let r = self
            .post(
                c,
                "auth/register",
                json!({"email":email,"password":"A long test password 2026"}),
            )
            .await;
        assert_eq!(r.status(), StatusCode::OK);
        let d: Value = r.json().await.unwrap();
        let id = d["challenge"].as_str().unwrap();
        let code = self.code(id).await;
        let r = self
            .post(c, "auth/verify", json!({"challenge":id,"code":code}))
            .await;
        assert_eq!(r.status(), StatusCode::OK);
        let d: Value = c
            .get(self.url("auth/me"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        d["id"].as_str().unwrap().to_owned()
    }
}
fn http() -> Http {
    Http::builder()
        .cookie_store(true)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap()
}
/// Secreto compartido con la pasarela simulada. Solo para pruebas.
const BILLING_SECRET: &str = "secreto-de-webhook-solo-para-pruebas";

/// Clave VAPID del ejemplo público de `web-push-native`, y una suscripción de
/// navegador de ese mismo ejemplo: valores válidos, nunca usados en producción.
const VAPID: &str = "RS0WdYWWo1HajXg3NZR1olzCf31i-ZBGDkFyCs7j1jw";
const P256DH: &str =
    "BLn9b-VR0ca83knDNZ32dCHGyjJp-1riX9ZTN40MqV8K_LpQmLqxC_DoHvqvFXO_nGdAB4W9dogZb_sM-uV4JbY";
const AUTH: &str = "_ordMnz7uTCmrpBTeUV4Bw";

/// La API rechaza afiliarse a un canal sin proveedor configurado, así que el
/// servidor de pruebas declara ambos. El envío real usa el extremo del servidor
/// de pruebas, que se pasa aparte a `deliver-pass`.
const SMS_URL_PLACEHOLDER: &str = "https://sms.ejemplo.test/send";

/// Servidor mínimo que hace de servicio de push y de proveedor de SMS.
///
/// Permite probar el canal de entrega completo —cifrado RFC 8291, firma VAPID,
/// plantilla de SMS, reintentos y borrado de suscripciones canceladas— sin
/// depender de un servicio externo ni mandar nada a nadie.
struct Stub {
    port: u16,
    received: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    bodies: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
}
impl Stub {
    async fn start(status: u16) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let received = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let bodies = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let counter = received.clone();
        let recorded = bodies.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let counter = counter.clone();
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    // Se lee la petición completa antes de responder. Cerrar con
                    // el cliente a medio enviar el cuerpo le da un error de red y
                    // el aviso se contaría como reintento en lugar de entregado.
                    let mut buffer = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let mut expected: Option<usize> = None;
                    loop {
                        let read = match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        buffer.extend_from_slice(&chunk[..read]);
                        let text = String::from_utf8_lossy(&buffer).to_lowercase();
                        if let Some(headers_end) = text.find("\r\n\r\n") {
                            if expected.is_none() {
                                expected = Some(
                                    text[..headers_end]
                                        .split("content-length:")
                                        .nth(1)
                                        .and_then(|v| {
                                            v.split("\r\n").next()?.trim().parse::<usize>().ok()
                                        })
                                        .unwrap_or(0),
                                );
                            }
                            if buffer.len() - (headers_end + 4) >= expected.unwrap_or(0) {
                                break;
                            }
                        }
                    }
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if let Ok(mut b) = recorded.lock() {
                        b.push(buffer);
                    }
                    let response = format!(
                        "HTTP/1.1 {status} Stub\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self {
            port,
            received,
            bodies,
        }
    }
    fn count(&self) -> usize {
        self.received.load(std::sync::atomic::Ordering::SeqCst)
    }
    fn last_body(&self) -> Vec<u8> {
        self.bodies
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap_or_default()
    }
}

// Multihilo a propósito: los servidores de prueba del canal de entrega viven
// en este proceso, y los subcomandos se invocan de forma bloqueante. Con un
// solo hilo, el bucle que acepta conexiones no correría mientras el proceso
// hijo intenta conectarse, y el envío caducaría sin que nadie respondiera.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires local MongoDB; creates and drops an isolated tp_test_* database"]
async fn real_workflow_security_and_persistence() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let db = format!("tp_test_{}", uuid::Uuid::new_v4().simple());
    let mail = std::env::temp_dir().join(&db);
    // El inventario ya no se carga desde un archivo al arrancar: vive en MongoDB
    // y se importa por territorio. La prueba importa Cali en su base aislada.
    let import = Command::new(env!("CARGO_BIN_EXE_territorio-api"))
        .arg("import-territory")
        .arg("territories/co-cali.json")
        .env(
            "TP_SECRET",
            "test-secret-longer-than-thirty-two-characters-only-tests",
        )
        .env("TP_MONGO_DB", &db)
        .env("TP_ORIGIN", format!("http://127.0.0.1:{port}"))
        .env("TP_BIND", format!("127.0.0.1:{port}"))
        .env("TP_ENV", "development")
        .env("TP_SMTP_HOST", "")
        .env("TP_MAIL_DIR", &mail)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(import.success(), "territory import failed");
    let mut server = Server::start(db.clone(), mail.clone(), port);
    server.ready().await;
    let mongo = Client::with_uri_str(
        std::env::var("TP_MONGO_URI").unwrap_or_else(|_| "mongodb://127.0.0.1:27017".into()),
    )
    .await
    .unwrap();
    let database = mongo.database(&db);
    let public = Http::new()
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        public.headers()["referrer-policy"],
        "strict-origin-when-cross-origin"
    );
    assert_eq!(public.headers()["x-frame-options"], "SAMEORIGIN");
    let app = Http::new()
        .get(format!("http://127.0.0.1:{port}/app/"))
        .send()
        .await
        .unwrap();
    assert_eq!(app.headers()["referrer-policy"], "no-referrer");
    assert_eq!(app.headers()["x-frame-options"], "DENY");
    let alice = http();
    let bob = http();
    let anonymous = http();
    assert_eq!(
        anonymous
            .get(server.url("v1/reports"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        anonymous
            .post(server.url("auth/login"))
            .json(&json!({"email":"a@example.com","password":"anything"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let aid = server.register(&alice, "alice@example.com").await;
    let bid = server.register(&bob, "bob@example.com").await;
    assert_eq!(
        server
            .post(
                &alice,
                "v1/jobs",
                json!({"report_id":"none","language":"en","request_key":"a"})
            )
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        server
            .post(
                &alice,
                "v1/reports",
                json!({"text":"Help","zone":"Cali","latitude":99,"longitude":0})
            )
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(server.post(&alice,"v1/reports",json!({"text":"We need drinking water","zone":"Cali","latitude":3.4,"longitude":-76.5})).await.status(),StatusCode::CREATED);
    let own: Value = alice
        .get(server.url("v1/reports"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let report_id = own["items"][0]["_id"].as_str().unwrap();
    let other: Value = bob
        .get(server.url("v1/reports"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        other["items"].as_array().unwrap().len(),
        0,
        "citizens must not access other reports"
    );
    let planning_request = json!({"space_id":"co-cali:epou-8413","profile":"camp","days":3,"target_people":120,"service_reserve_percent":30,"water_liters_per_day":1500,"toilets_available":5,"kits_available":10,"evacuation_capacity":90,"covered_footprint_confirmed":false,"exclusions":[],"request_key":"integration-plan"});
    assert_eq!(
        anonymous
            .get(server.url("v1/planning/spaces"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let spaces: Value = alice
        .get(server.url("v1/planning/spaces?q=Vipasa"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(spaces["total"].as_u64().unwrap() > 0);
    assert_eq!(spaces["catalogue_total"], 2991);
    assert_eq!(spaces["territory"]["id"], "co-cali");
    assert_eq!(spaces["territory"]["coverage"], "complete");
    let preview = server
        .post(&alice, "v1/planning/preview", planning_request.clone())
        .await;
    assert_eq!(preview.status(), StatusCode::OK);
    let preview: Value = preview.json().await.unwrap();
    assert!(preview["capacity"]["proposed_people"].as_u64().unwrap() <= 90);
    assert!(preview["operational_capacity"].is_null());
    assert_eq!(preview["visual_analysis"]["performed"], false);
    assert_eq!(
        server
            .post(&alice, "v1/planning/plans", planning_request.clone())
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    let mut point_request = planning_request.clone();
    point_request["space_id"] = json!("co-cali:deporte-984");
    assert_eq!(
        server
            .post(&alice, "v1/planning/preview", point_request)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    // --- Sectores administrativos (fase 1) ---------------------------------
    // La importación carga las dos capas declaradas por el territorio. Sin ellas
    // no se puede resolver ninguna afiliación.
    let communes: Value = alice
        .get(server.url("v1/sectors?territory=co-cali&kind=commune"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(communes["items"].as_array().unwrap().len(), 22);
    let neighborhoods: Value = alice
        .get(server.url("v1/sectors?territory=co-cali&kind=neighborhood"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(neighborhoods["items"].as_array().unwrap().len(), 342);
    assert_eq!(
        alice
            .get(server.url("v1/sectors?territory=co-cali&kind=galaxia"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );

    // --- Afiliación (fase 1) -----------------------------------------------
    // El punto se toma del propio inventario, así que está garantizado dentro de
    // Cali sin fijar coordenadas en la prueba.
    let space: Value = alice
        .get(server.url("v1/planning/spaces/co-cali:epou-8413"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // La huella puede ser Polygon o MultiPolygon, así que se baja por los
    // anidamientos hasta el primer par de números en lugar de fijar la
    // profundidad, que cambiaría con el tipo de geometría.
    fn first_vertex(v: &Value) -> [f64; 2] {
        match (v[0].as_f64(), v[1].as_f64()) {
            (Some(lon), Some(lat)) => [lon, lat],
            _ => first_vertex(&v[0]),
        }
    }
    let [home_lon, home_lat] = first_vertex(&space["geometry"]["coordinates"]);
    let affiliation = json!({
        "label": "Casa",
        "latitude": home_lat,
        "longitude": home_lon,
        "channels": {"push": true, "sms": false},
        "min_level": "info",
        "consent": true
    });

    // Sin consentimiento explícito no se guarda la ubicación de nadie.
    let mut without_consent = affiliation.clone();
    without_consent["consent"] = json!(false);
    assert_eq!(
        server
            .post(&alice, "v1/subscriptions", without_consent)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    // Un punto fuera del territorio prometería una cobertura que no existe.
    let mut elsewhere = affiliation.clone();
    elsewhere["latitude"] = json!(40.4168);
    elsewhere["longitude"] = json!(-3.7038);
    assert_eq!(
        server
            .post(&alice, "v1/subscriptions", elsewhere)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    // Un canal cuyo proveedor no está configurado no se acepta al afiliarse: la
    // afiliación prometería un aviso que nadie va a entregar.
    {
        let bare_port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let bare_db = format!("tp_test_{}", uuid::Uuid::new_v4().simple());
        let mut bare = Command::new(env!("CARGO_BIN_EXE_territorio-api"));
        bare.arg("api")
            .env(
                "TP_SECRET",
                "test-secret-longer-than-thirty-two-characters-only-tests",
            )
            .env("TP_MONGO_DB", &bare_db)
            .env("TP_BIND", format!("127.0.0.1:{bare_port}"))
            .env("TP_ORIGIN", format!("http://127.0.0.1:{bare_port}"))
            .env("TP_ENV", "development")
            .env("TP_SMTP_HOST", "")
            .env("TP_MAIL_DIR", &mail)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = bare.spawn().unwrap();
        for _ in 0..100 {
            if Http::new()
                .get(format!("http://127.0.0.1:{bare_port}/api/health"))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let caps: Value = Http::new()
            .get(format!("http://127.0.0.1:{bare_port}/api/capabilities"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            caps["channels"].as_array().unwrap().len(),
            0,
            "sin claves no debe anunciarse ningún canal: {caps}"
        );
        assert_eq!(caps["billing"], false);
        let _ = child.kill();
        let _ = child.wait();
    }

    // El canal SMS sin teléfono no puede avisar a nadie.
    let mut sms_without_phone = affiliation.clone();
    sms_without_phone["channels"] = json!({"push": false, "sms": true});
    assert_eq!(
        server
            .post(&alice, "v1/subscriptions", sms_without_phone)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let mut bad_phone = affiliation.clone();
    bad_phone["channels"] = json!({"push": true, "sms": true});
    bad_phone["phone"] = json!("3001234567");
    assert_eq!(
        server
            .post(&alice, "v1/subscriptions", bad_phone)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let mut no_channel = affiliation.clone();
    no_channel["channels"] = json!({"push": false, "sms": false});
    assert_eq!(
        server
            .post(&alice, "v1/subscriptions", no_channel)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );

    let created = server
        .post(&alice, "v1/subscriptions", affiliation.clone())
        .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await.unwrap();
    let subscription_id = created["subscription"]["id"].as_str().unwrap().to_owned();
    // El punto se resolvió a su sector: es lo que permite dirigir un aviso a una
    // zona y cruzarlo con las capas de amenaza.
    let commune_code = created["subscription"]["commune_code"].as_str().unwrap();
    assert_eq!(commune_code.len(), 2, "comuna {commune_code}");
    assert!(created["subscription"]["neighborhood"].is_string());
    assert_eq!(created["territory"]["coverage"], "complete");
    assert_eq!(
        created["missing_layers"].as_array().unwrap().len(),
        0,
        "Cali tiene las tres capas"
    );

    // El listado es propio: otra cuenta no ve la afiliación de nadie.
    let mine: Value = alice
        .get(server.url("v1/subscriptions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(mine["items"].as_array().unwrap().len(), 1);
    let others: Value = bob
        .get(server.url("v1/subscriptions"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(others["items"].as_array().unwrap().len(), 0);
    assert_eq!(
        bob.get(server.url(&format!("v1/subscriptions/{subscription_id}/events")))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );

    // Activar SMS sin número sigue rechazándose al modificar, no solo al crear.
    let patch = |c: &Http, path: String, body: Value| {
        c.patch(server.url(&path))
            .header("Origin", format!("http://127.0.0.1:{port}"))
            .json(&body)
            .send()
    };
    assert_eq!(
        patch(
            &alice,
            format!("v1/subscriptions/{subscription_id}"),
            json!({"channels": {"push": true, "sms": true}})
        )
        .await
        .unwrap()
        .status(),
        StatusCode::BAD_REQUEST
    );
    let updated: Value = patch(
        &alice,
        format!("v1/subscriptions/{subscription_id}"),
        json!({"channels": {"push": true, "sms": true}, "phone": "+573001234567", "min_level": "prepare"}),
    )
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(updated["subscription"]["min_level"], "prepare");
    // El teléfono completo no vuelve nunca en una respuesta.
    assert_eq!(updated["subscription"]["phone_last4"], "4567");
    assert!(!updated.to_string().contains("+573001234567"));

    // --- Eventos que alcanzan la afiliación (fases 1 + 2) ------------------
    // Se insertan tres eventos con la forma que produce la ingesta y se
    // comprueba que el filtro geográfico y el de nivel hacen cada uno su parte.
    let events = database.collection::<mongodb::bson::Document>("events");
    let near = doc! {
        "_id": "usgs:prueba-cerca", "canonical_id": "c1", "source": "USGS",
        "authority_rank": 10i32, "external_id": "prueba-cerca",
        "hazard_type": "earthquake", "magnitude": 7.0f64, "depth_km": 12.0f64,
        "point": {"type": "Point", "coordinates": [home_lon, home_lat]},
        "reach_km": 1200.0f64, "origin_utc": mongodb::bson::DateTime::now(),
        "status": "reviewed", "place": "Prueba junto a la afiliación",
        "validation_level": "automatic", "notified_level": "none",
        "created": mongodb::bson::DateTime::now(), "updated": mongodb::bson::DateTime::now(),
    };
    // Dentro del alcance máximo, pero demasiado pequeño para avisar a esa distancia.
    let weak = doc! {
        "_id": "usgs:prueba-debil", "canonical_id": "c2", "source": "USGS",
        "authority_rank": 10i32, "external_id": "prueba-debil",
        "hazard_type": "earthquake", "magnitude": 3.6f64,
        "point": {"type": "Point", "coordinates": [-74.0721f64, 4.7110f64]},
        "reach_km": 80.0f64, "origin_utc": mongodb::bson::DateTime::now(),
        "validation_level": "automatic", "notified_level": "none",
        "created": mongodb::bson::DateTime::now(), "updated": mongodb::bson::DateTime::now(),
    };
    // Fuera del alcance máximo: lo descarta el índice geoespacial, no el nivel.
    let far = doc! {
        "_id": "usgs:prueba-lejos", "canonical_id": "c3", "source": "USGS",
        "authority_rank": 10i32, "external_id": "prueba-lejos",
        "hazard_type": "earthquake", "magnitude": 8.5f64,
        "point": {"type": "Point", "coordinates": [139.6917f64, 35.6895f64]},
        "reach_km": 1200.0f64, "origin_utc": mongodb::bson::DateTime::now(),
        "validation_level": "automatic", "notified_level": "none",
        "created": mongodb::bson::DateTime::now(), "updated": mongodb::bson::DateTime::now(),
    };
    events.insert_many(vec![near, weak, far]).await.unwrap();

    let reaching: Value = alice
        .get(server.url(&format!("v1/subscriptions/{subscription_id}/events")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let items = reaching["items"].as_array().unwrap();
    assert_eq!(
        items.len(),
        1,
        "solo el sismo cercano y fuerte avisa: {items:?}"
    );
    assert_eq!(items[0]["event_id"], "usgs:prueba-cerca");
    assert_eq!(items[0]["level"], "evacuate");
    assert_eq!(items[0]["validation_level"], "automatic");
    assert!(items[0]["distance_km"].as_f64().unwrap() < 1.0);
    // El catálogo general sí los tiene todos: lo que filtra es la afiliación.
    let catalogue: Value = alice
        .get(server.url("v1/events"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(catalogue["items"].as_array().unwrap().len(), 3);
    let strong: Value = alice
        .get(server.url("v1/events?magnitude=8"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(strong["items"].as_array().unwrap().len(), 1);

    // Retirar la afiliación borra el dato, no lo marca como inactivo.
    let removed = alice
        .delete(server.url(&format!("v1/subscriptions/{subscription_id}")))
        .header("Origin", format!("http://127.0.0.1:{port}"))
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("subscriptions")
            .count_documents(doc! {"_id": &subscription_id})
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        alice
            .delete(server.url(&format!("v1/subscriptions/{subscription_id}")))
            .header("Origin", format!("http://127.0.0.1:{port}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );

    // Privileged role provisioning is out of band; new accounts cannot choose it.
    database
        .collection::<mongodb::bson::Document>("users")
        .update_one(doc! {"_id":&aid}, doc! {"$set":{"role":"admin"}})
        .await
        .unwrap();
    let saved = server
        .post(&alice, "v1/planning/plans", planning_request.clone())
        .await;
    assert_eq!(saved.status(), StatusCode::CREATED);
    let saved: Value = saved.json().await.unwrap();
    let plan_id = saved["id"].as_str().unwrap().to_owned();
    let saved_plan = saved["plan"].clone();
    let again = server
        .post(&alice, "v1/planning/plans", planning_request.clone())
        .await;
    assert_eq!(again.status(), StatusCode::OK);
    let again: Value = again.json().await.unwrap();
    assert_eq!(saved["id"], again["id"]);
    let mut changed = planning_request.clone();
    changed["target_people"] = json!(200);
    assert_eq!(
        server
            .post(&alice, "v1/planning/plans", changed)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    // --- Motor de alertas (fase 3) -----------------------------------------
    // Alice ya es administradora, así que puede conceder planes.
    let alerts_pass = |sms_budget: Option<&str>| -> Value {
        let mut command = Command::new(env!("CARGO_BIN_EXE_territorio-api"));
        command
            .arg("alerts-pass")
            .env(
                "TP_SECRET",
                "test-secret-longer-than-thirty-two-characters-only-tests",
            )
            .env("TP_MONGO_DB", &db)
            .env("TP_ORIGIN", format!("http://127.0.0.1:{port}"))
            .env("TP_BIND", format!("127.0.0.1:{port}"))
            .env("TP_ENV", "development")
            .env("TP_SMTP_HOST", "")
            .env("TP_MAIL_DIR", &mail);
        if let Some(budget) = sms_budget {
            command.env("TP_ALERT_SMS_BUDGET_PER_EVENT", budget);
        }
        let out = command.output().unwrap();
        assert!(
            out.status.success(),
            "alerts-pass falló: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let events = database.collection::<mongodb::bson::Document>("events");
    let notifications = database.collection::<mongodb::bson::Document>("notifications");
    // Un evento con la forma que produce la ingesta, situado sobre la afiliación.
    let quake = |id: &str, canonical: &str, source: &str, rank: i32, magnitude: f64| {
        doc! {
            "_id": id, "canonical_id": canonical, "source": source,
            "authority_rank": rank, "external_id": id,
            "hazard_type": "earthquake", "magnitude": magnitude,
            "point": {"type": "Point", "coordinates": [home_lon, home_lat]},
            "reach_km": 1200.0f64, "origin_utc": mongodb::bson::DateTime::now(),
            "status": "reviewed", "validation_level": "automatic",
            "alert_pending": true,
            "created": mongodb::bson::DateTime::now(),
            "updated": mongodb::bson::DateTime::now(),
        }
    };

    let alert_sub = server
        .post(
            &alice,
            "v1/subscriptions",
            json!({
                "label": "Casa con alertas",
                "latitude": home_lat,
                "longitude": home_lon,
                "channels": {"push": true, "sms": true},
                "phone": "+573001234567",
                "min_level": "info",
                "consent": true
            }),
        )
        .await;
    assert_eq!(alert_sub.status(), StatusCode::CREATED);
    let alert_sub: Value = alert_sub.json().await.unwrap();
    let alert_sub_id = alert_sub["subscription"]["id"].as_str().unwrap().to_owned();

    // 1. Un M4.5 sobre la afiliación corresponde al nivel `prepare`. El plan
    //    gratuito recibe Web Push y ningún SMS, que es la regla del módulo.
    events
        .insert_one(quake("usgs:a1", "cA", "USGS", 10, 4.5))
        .await
        .unwrap();
    let pass = alerts_pass(None);
    assert_eq!(pass["events"], 1);
    assert_eq!(
        pass["enqueued"], 1,
        "solo el push del plan gratuito: {pass}"
    );
    assert_eq!(pass["raised"], 0);
    let mine: Value = alice
        .get(server.url("v1/alerts/mine"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(mine["plan"], "free");
    assert_eq!(mine["items"].as_array().unwrap().len(), 1);
    assert_eq!(mine["items"][0]["channel"], "push");
    assert_eq!(mine["items"][0]["level"], "prepare");
    assert_eq!(mine["items"][0]["status"], "pending");

    // 2. La bandera queda limpia: repetir la pasada no reparte nada otra vez.
    let repeat = alerts_pass(None);
    assert_eq!(repeat["events"], 0, "el evento seguía pendiente: {repeat}");

    // 3. Con plan de pago, el SMS entra solo cuando el nivel es `evacuate`.
    let granted = alice
        .patch(server.url(&format!("v1/alerts/entitlements/{aid}")))
        .header("Origin", format!("http://127.0.0.1:{port}"))
        .json(&json!({"plan": "paid", "days": 30}))
        .send()
        .await
        .unwrap();
    assert_eq!(granted.status(), StatusCode::OK);

    // 4. La revisión sube la magnitud: el push existente se eleva a `evacuate` y
    //    el SMS se encola por primera vez.
    events
        .update_one(
            doc! {"_id": "usgs:a1"},
            doc! {"$set": {"magnitude": 6.0f64, "alert_pending": true,
            "updated": mongodb::bson::DateTime::now()}},
        )
        .await
        .unwrap();
    let raised = alerts_pass(None);
    assert_eq!(raised["raised"], 1, "el push debía elevarse: {raised}");
    assert_eq!(raised["enqueued"], 1, "el SMS debía encolarse: {raised}");
    let mine: Value = alice
        .get(server.url("v1/alerts/mine"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(mine["plan"], "paid");
    let items = mine["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "un push y un SMS: {items:?}");
    assert!(items.iter().all(|i| i["level"] == "evacuate"));
    let channels: Vec<&str> = items.iter().filter_map(|i| i["channel"].as_str()).collect();
    assert!(channels.contains(&"push") && channels.contains(&"sms"));

    // 5. La revisión a la baja **no** vuelve a avisar. Es la regla que evita
    //    gastar la confianza de la gente —y dinero en SMS— para decir que el
    //    sismo fue algo menos fuerte de lo publicado al principio.
    events
        .update_one(
            doc! {"_id": "usgs:a1"},
            doc! {"$set": {"magnitude": 4.5f64, "alert_pending": true,
            "updated": mongodb::bson::DateTime::now()}},
        )
        .await
        .unwrap();
    let lowered = alerts_pass(None);
    assert_eq!(
        lowered["enqueued"], 0,
        "no se encola nada a la baja: {lowered}"
    );
    assert_eq!(lowered["raised"], 0, "nada se eleva a la baja: {lowered}");
    assert_eq!(
        lowered["duplicates"], 1,
        "el push debía contarse omitido: {lowered}"
    );
    assert_eq!(
        notifications
            .count_documents(doc! {"canonical_id": "cA"})
            .await
            .unwrap(),
        2,
        "la revisión a la baja no debe crear avisos nuevos"
    );
    // Y el nivel entregado sigue siendo el más grave alcanzado.
    assert_eq!(
        notifications
            .count_documents(doc! {"canonical_id": "cA", "level": "evacuate"})
            .await
            .unwrap(),
        2
    );

    // 6. Dos catálogos publican el mismo sismo: solo avisa el de más autoridad,
    //    así que nadie recibe el aviso dos veces.
    events
        .insert_many(vec![
            quake("usgs:b1", "cB", "USGS", 10, 5.0),
            quake("sgc:b1", "cB", "SGC", 100, 7.0),
        ])
        .await
        .unwrap();
    let both = alerts_pass(None);
    assert_eq!(both["events"], 2, "ambas observaciones se procesan: {both}");
    assert_eq!(
        both["superseded"], 1,
        "USGS debía quedar descartado: {both}"
    );
    let from_sgc = notifications
        .find_one(doc! {"canonical_id": "cB", "channel": "push"})
        .await
        .unwrap()
        .expect("el aviso del grupo cB");
    assert_eq!(
        from_sgc.get_str("event_id").unwrap(),
        "sgc:b1",
        "el aviso debía venir de la autoridad nacional, no del catálogo global"
    );
    assert_eq!(
        notifications
            .count_documents(doc! {"canonical_id": "cB"})
            .await
            .unwrap(),
        2,
        "un push y un SMS, no cuatro: los dos catálogos no duplican el aviso"
    );

    // 7. El presupuesto de SMS acota el coste de un evento: al agotarse, el
    //    aviso se cuenta como omitido en lugar de gastarse en silencio.
    events
        .insert_one(quake("usgs:c1", "cC", "USGS", 10, 6.0))
        .await
        .unwrap();
    let broke = alerts_pass(Some("0"));
    assert_eq!(
        broke["over_budget"], 1,
        "el SMS debía quedar fuera: {broke}"
    );
    assert_eq!(
        broke["enqueued"], 1,
        "el push no depende del presupuesto: {broke}"
    );
    assert_eq!(
        notifications
            .count_documents(doc! {"canonical_id": "cC", "channel": "sms"})
            .await
            .unwrap(),
        0
    );

    // 8. La cola es de operación y lleva el orden de reparto; un ciudadano no la
    //    ve, y cada persona solo ve lo suyo.
    assert_eq!(
        bob.get(server.url("v1/alerts/queue"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let queue: Value = alice
        .get(server.url("v1/alerts/queue?status=pending"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(queue["pending"].as_u64().unwrap() >= 5);
    let levels: Vec<&str> = queue["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["level"].as_str())
        .collect();
    assert_eq!(levels[0], "evacuate", "lo más grave se reparte primero");
    let bobs: Value = bob
        .get(server.url("v1/alerts/mine"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(bobs["items"].as_array().unwrap().len(), 0);
    assert_eq!(
        alice
            .patch(server.url(&format!("v1/alerts/entitlements/{aid}")))
            .header("Origin", format!("http://127.0.0.1:{port}"))
            .json(&json!({"plan": "oro", "days": 30}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    // Retirar la afiliación no borra el historial de avisos ya decididos.
    alice
        .delete(server.url(&format!("v1/subscriptions/{alert_sub_id}")))
        .header("Origin", format!("http://127.0.0.1:{port}"))
        .send()
        .await
        .unwrap();

    // --- A dónde ir (fase 4) ------------------------------------------------
    // El caso documentado en docs/DATASETS.md: el parque de Chiminangos Primera
    // Etapa donde se montaron albergues autogestionados tras el sismo. Tiene
    // amenaza alta de inundación y suelo licuable, y el inventario lo demuestra.
    let chiminangos = (-76.490_251_85_f64, 3.477_776_33_f64);
    let destinations: Value = alice
        .get(server.url(&format!(
            "v1/destinations?latitude={}&longitude={}&hazard=earthquake",
            chiminangos.1, chiminangos.0
        )))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // Cali tiene las tres capas, así que las dos comprobaciones se hacen de verdad.
    let checked: Vec<&str> = destinations["checked"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        checked.contains(&"liquefaction") && checked.contains(&"flood"),
        "{checked:?}"
    );
    assert_eq!(destinations["unverified"].as_array().unwrap().len(), 0);

    // Lo que da sentido al proyecto: hay espacios abiertos más cerca, y se
    // descartan nombrando el motivo. Sin esto, la app mandaría a la gente al
    // mismo sitio al que fue sola.
    let unsuitable = destinations["closer_but_unsuitable"].as_array().unwrap();
    assert!(
        !unsuitable.is_empty(),
        "en Chiminangos tiene que haber espacios cercanos descartados: {destinations}"
    );
    let reasons: Vec<&str> = unsuitable
        .iter()
        .flat_map(|u| u["reasons"].as_array().unwrap())
        .filter_map(|r| r["requirement"].as_str())
        .collect();
    assert!(
        reasons.contains(&"liquefaction"),
        "el suelo licuable debía ser un motivo: {reasons:?}"
    );

    // Y lo que se recomienda no arrastra ninguna de las dos amenazas.
    for item in destinations["items"].as_array().unwrap() {
        assert_eq!(
            item["declared_seismic"].as_array().unwrap().len(),
            0,
            "se recomendó un espacio licuable: {item}"
        );
        for declared in item["declared_flood"].as_array().unwrap() {
            assert!(
                !declared.as_str().unwrap().contains("Amenaza Alta"),
                "se recomendó un espacio con amenaza alta: {item}"
            );
        }
        assert_eq!(item["suitability"], "sin_amenaza_declarada");
        assert!(item["area_m2"].as_f64().unwrap() >= 1000.0);
        assert!(item["area_upper_bound_people"].as_f64().unwrap() > 0.0);
    }

    // Una inundación es más estricta que un sismo con el mismo inventario: la
    // amenaza media también descarta, así que no puede recomendar más espacios.
    let for_flood: Value = alice
        .get(server.url(&format!(
            "v1/destinations?latitude={}&longitude={}&hazard=flood&radius_km=8",
            chiminangos.1, chiminangos.0
        )))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for item in for_flood["items"].as_array().unwrap() {
        for declared in item["declared_flood"].as_array().unwrap() {
            let text = declared.as_str().unwrap();
            assert!(
                !text.contains("Amenaza Alta") && !text.contains("Amenaza Media"),
                "una inundación no puede recomendar amenaza media: {item}"
            );
        }
    }

    // Sin capa de deslizamientos, no se finge comprobar nada: `checked` queda
    // vacío y es la señal de que la recomendación no filtró por amenaza.
    let for_landslide: Value = alice
        .get(server.url(&format!(
            "v1/destinations?latitude={}&longitude={}&hazard=landslide",
            chiminangos.1, chiminangos.0
        )))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(for_landslide["checked"].as_array().unwrap().len(), 0);

    assert_eq!(
        alice
            .get(server.url(&format!(
                "v1/destinations?latitude={}&longitude={}&hazard=terremoto",
                chiminangos.1, chiminangos.0
            )))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );

    // Si al territorio le falta la capa, la respuesta lo declara en lugar de
    // dejar pasar los espacios como si se hubieran comprobado. Es la diferencia
    // entre «no intersecta» y «no se miró».
    database
        .collection::<mongodb::bson::Document>("territories")
        .update_one(
            doc! {"_id": "co-cali"},
            doc! {"$set": {"layers.flood": false}},
        )
        .await
        .unwrap();
    let partial: Value = alice
        .get(server.url(&format!(
            "v1/destinations?latitude={}&longitude={}&hazard=earthquake",
            chiminangos.1, chiminangos.0
        )))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(partial["territory"]["coverage"], "partial");
    let unverified: Vec<&str> = partial["unverified"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        unverified,
        vec!["flood"],
        "la capa ausente debía declararse"
    );
    database
        .collection::<mongodb::bson::Document>("territories")
        .update_one(
            doc! {"_id": "co-cali"},
            doc! {"$set": {"layers.flood": true}},
        )
        .await
        .unwrap();

    // --- Entrega (fase 5) ---------------------------------------------------
    let push_stub = Stub::start(201).await;
    let sms_stub = Stub::start(200).await;
    let deliver_pass = |sms_url: Option<String>, vapid: bool| -> Value {
        let mut command = Command::new(env!("CARGO_BIN_EXE_territorio-api"));
        command
            .arg("deliver-pass")
            .env(
                "TP_SECRET",
                "test-secret-longer-than-thirty-two-characters-only-tests",
            )
            .env("TP_MONGO_DB", &db)
            .env("TP_ORIGIN", format!("http://127.0.0.1:{port}"))
            .env("TP_BIND", format!("127.0.0.1:{port}"))
            .env("TP_ENV", "development")
            .env("TP_SMTP_HOST", "")
            .env("TP_MAIL_DIR", &mail)
            .env("TP_VAPID_CONTACT", "mailto:ops@example.com");
        if vapid {
            command.env("TP_VAPID_PRIVATE_KEY", VAPID);
        }
        if let Some(url) = sms_url {
            command.env("TP_SMS_URL", url);
        }
        let out = command.output().unwrap();
        assert!(
            out.status.success(),
            "deliver-pass falló: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    };

    // El navegador registra su suscripción. Un endpoint en claro solo se admite
    // en loopback y en desarrollo.
    let device = server
        .post(
            &alice,
            "v1/push-devices",
            json!({
                "endpoint": format!("http://127.0.0.1:{}/push", push_stub.port),
                "p256dh": P256DH,
                "auth": AUTH,
                "label": "Portátil"
            }),
        )
        .await;
    assert_eq!(device.status(), StatusCode::CREATED);
    assert_eq!(
        server
            .post(
                &alice,
                "v1/push-devices",
                json!({
                    "endpoint": "http://proveedor.example/push",
                    "p256dh": P256DH,
                    "auth": AUTH
                }),
            )
            .await
            .status(),
        StatusCode::BAD_REQUEST,
        "un endpoint externo en claro no se admite"
    );
    // Volver a suscribirse desde el mismo navegador actualiza, no duplica.
    server
        .post(
            &alice,
            "v1/push-devices",
            json!({
                "endpoint": format!("http://127.0.0.1:{}/push", push_stub.port),
                "p256dh": P256DH,
                "auth": AUTH,
                "label": "Portátil otra vez"
            }),
        )
        .await;
    let devices: Value = alice
        .get(server.url("v1/push-devices"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(devices["items"].as_array().unwrap().len(), 1);
    // El endpoint identifica al navegador y no se devuelve: es dato de seguimiento.
    assert!(!devices.to_string().contains("/push"));

    // Afiliación con los dos canales y plan de pago, para que el reparto tenga
    // un push y un SMS que entregar.
    let delivery_sub = server
        .post(
            &alice,
            "v1/subscriptions",
            json!({
                "label": "Casa entrega",
                "latitude": home_lat,
                "longitude": home_lon,
                "channels": {"push": true, "sms": true},
                "phone": "+573001234567",
                "min_level": "info",
                "consent": true
            }),
        )
        .await;
    assert_eq!(delivery_sub.status(), StatusCode::CREATED);
    events
        .insert_one(quake("usgs:d1", "cD", "USGS", 10, 6.5))
        .await
        .unwrap();
    let decided = alerts_pass(None);
    assert!(decided["enqueued"].as_u64().unwrap() >= 2, "{decided}");

    // Sin clave VAPID no se entrega ningún push: se reintenta, no se pierde.
    let without_key = deliver_pass(None, false);
    assert_eq!(
        without_key["sent"], 0,
        "sin clave no se puede cifrar: {without_key}"
    );
    assert_eq!(push_stub.count(), 0);

    // Ese intento fallido dejó los avisos con la espera del reintento aplicada,
    // que es lo correcto. Se vence a mano en lugar de dormir un minuto en una
    // prueba; que la espera se aplica de verdad se comprueba más abajo.
    let expire = |_: ()| async {
        notifications
            .update_many(
                doc! {"status": "pending"},
                doc! {"$set": {"next_attempt": mongodb::bson::DateTime::now()}},
            )
            .await
            .unwrap();
    };
    expire(()).await;

    // Con clave y con proveedor, el reparto entrega por los dos canales.
    let delivered = deliver_pass(
        Some(format!("http://127.0.0.1:{}/send", sms_stub.port)),
        true,
    );
    if delivered["sent"].as_u64().unwrap() < 2 {
        // Si no se entrega, decir por qué: el recuento solo no sirve para
        // diagnosticar un transporte.
        let detail: Vec<String> = notifications
            .find(doc! {"canonical_id": "cD"})
            .await
            .unwrap()
            .try_collect::<Vec<mongodb::bson::Document>>()
            .await
            .unwrap()
            .iter()
            .map(|d| {
                format!(
                    "{}={} ({})",
                    d.get_str("channel").unwrap_or("?"),
                    d.get_str("status").unwrap_or("?"),
                    d.get_str("last_error").unwrap_or("sin error")
                )
            })
            .collect();
        panic!("no se entregó: {delivered} · {detail:?}");
    }
    assert!(
        push_stub.count() >= 1,
        "el servicio de push no recibió nada"
    );
    assert!(sms_stub.count() >= 1, "el proveedor de SMS no recibió nada");
    // El cuerpo del push va cifrado: el texto del aviso no puede verse en claro.
    let pushed = push_stub.last_body();
    let as_text = String::from_utf8_lossy(&pushed);
    assert!(
        !as_text.contains("Evacúa") && !as_text.contains("Sismo"),
        "el aviso viajó sin cifrar"
    );
    // Los nombres de encabezado se envían en minúsculas, así que se compara
    // sobre el texto normalizado.
    let headers = as_text.to_lowercase();
    assert!(
        headers.contains("aes128gcm"),
        "falta la codificación de contenido"
    );
    assert!(
        headers.contains("authorization: vapid"),
        "falta la firma VAPID: {}",
        &headers[..headers.len().min(400)]
    );
    // El SMS va en claro hacia el proveedor, con el destino recomendado dentro.
    let texted = String::from_utf8_lossy(&sms_stub.last_body()).to_string();
    assert!(
        texted.contains("EVAC"),
        "el SMS no lleva el nivel: {texted}"
    );
    assert!(
        texted.contains("Ve a:"),
        "el SMS no lleva el destino: {texted}"
    );

    let sent = notifications
        .count_documents(doc! {"canonical_id": "cD", "status": "sent"})
        .await
        .unwrap();
    assert!(sent >= 2, "los avisos debían quedar como entregados");
    // Repetir el reparto no reenvía nada: ya no están pendientes.
    let again = deliver_pass(
        Some(format!("http://127.0.0.1:{}/send", sms_stub.port)),
        true,
    );
    assert_eq!(again["claimed"], 0, "se reclamó algo ya entregado: {again}");

    // Un servicio de push que responde 410 significa suscripción cancelada: el
    // dispositivo se borra en lugar de consumir un envío en cada emergencia.
    let gone_stub = Stub::start(410).await;
    server
        .post(
            &alice,
            "v1/push-devices",
            json!({
                "endpoint": format!("http://127.0.0.1:{}/push", gone_stub.port),
                "p256dh": P256DH,
                "auth": AUTH,
                "label": "Navegador cancelado"
            }),
        )
        .await;
    database
        .collection::<mongodb::bson::Document>("push_devices")
        .delete_many(doc! {"endpoint": format!("http://127.0.0.1:{}/push", push_stub.port)})
        .await
        .unwrap();
    events
        .insert_one(quake("usgs:e1", "cE", "USGS", 10, 6.5))
        .await
        .unwrap();
    alerts_pass(None);
    expire(()).await;
    let expired = deliver_pass(None, true);
    assert_eq!(
        expired["devices_removed"], 1,
        "el dispositivo debía borrarse: {expired}"
    );
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("push_devices")
            .count_documents(doc! {"owner": &aid})
            .await
            .unwrap(),
        0
    );

    // Un proveedor que responde 503 se reintenta: el aviso vuelve a pendiente
    // con la espera aplicada, no se marca como fallido en el primer tropiezo.
    let flaky = Stub::start(503).await;
    events
        .insert_one(quake("usgs:f1", "cF", "USGS", 10, 6.5))
        .await
        .unwrap();
    alerts_pass(None);
    expire(()).await;
    let retried = deliver_pass(Some(format!("http://127.0.0.1:{}/send", flaky.port)), true);
    assert!(retried["retried"].as_u64().unwrap() >= 1, "{retried}");
    let pending_again = notifications
        .find_one(doc! {"canonical_id": "cF", "channel": "sms", "status": "pending"})
        .await
        .unwrap()
        .expect("el SMS debía volver a pendiente");
    assert!(pending_again.get_i32("attempts").unwrap() >= 1);
    assert!(
        *pending_again.get_datetime("next_attempt").unwrap() > mongodb::bson::DateTime::now(),
        "la espera entre reintentos no se aplicó"
    );
    // Y mientras la espera no venza, no se vuelve a reclamar.
    let too_soon = deliver_pass(Some(format!("http://127.0.0.1:{}/send", flaky.port)), true);
    assert_eq!(
        notifications
            .count_documents(doc! {"canonical_id": "cF", "channel": "sms", "status": "sending"})
            .await
            .unwrap(),
        0,
        "se reclamó antes de que venciera la espera: {too_soon}"
    );

    // --- Contenido educativo (fase 6) --------------------------------------
    let import_lessons = || -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_territorio-api"))
            .arg("import-lessons")
            .arg("lessons/co-es.json")
            .env(
                "TP_SECRET",
                "test-secret-longer-than-thirty-two-characters-only-tests",
            )
            .env("TP_MONGO_DB", &db)
            .env("TP_ORIGIN", format!("http://127.0.0.1:{port}"))
            .env("TP_BIND", format!("127.0.0.1:{port}"))
            .env("TP_ENV", "development")
            .env("TP_SMTP_HOST", "")
            .env("TP_MAIL_DIR", &mail)
            .output()
            .unwrap()
    };
    assert!(import_lessons().status.success());

    // Nada se publica al importar: un ciudadano no ve contenido sin revisar.
    let public: Value = bob
        .get(server.url("v1/lessons"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        public["items"].as_array().unwrap().len(),
        0,
        "se publicó contenido de emergencia sin revisión"
    );
    // Y tampoco puede mirar los borradores.
    assert_eq!(
        bob.get(server.url("v1/lessons?drafts=true"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );

    // El operador ve los borradores y qué le falta a cada uno.
    let drafts: Value = alice
        .get(server.url("v1/lessons?drafts=true"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(drafts["items"].as_array().unwrap().len(), 3);
    let pending = drafts["pending_review"].as_array().unwrap();
    assert_eq!(pending.len(), 3);
    assert!(
        pending
            .iter()
            .all(|p| p["reason"].as_str() == Some("Falta la persona que revisó el contenido")),
        "el motivo debía ser la revisión pendiente: {pending:?}"
    );

    let publish = |c: &Http, id: &str, body: Value| {
        c.patch(server.url(&format!("v1/lessons/{id}")))
            .header("Origin", format!("http://127.0.0.1:{port}"))
            .json(&body)
            .send()
    };
    // Publicar sin revisor se rechaza nombrando el motivo.
    let refused = publish(&alice, "sismo-que-hacer", json!({"published": true}))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let refused: Value = refused.json().await.unwrap();
    assert_eq!(refused["error"], "Falta la persona que revisó el contenido");
    // Un ciudadano tampoco puede publicar.
    assert_eq!(
        publish(&bob, "sismo-que-hacer", json!({"reviewed_by": "yo"}))
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );

    // Con revisor identificado, se publica.
    let published = publish(
        &alice,
        "sismo-que-hacer",
        json!({"reviewed_by": "Especialista en gestión del riesgo", "published": true}),
    )
    .await
    .unwrap();
    assert_eq!(published.status(), StatusCode::OK);
    let published: Value = published.json().await.unwrap();
    assert_eq!(published["published"], true);
    let visible: Value = bob
        .get(server.url("v1/lessons?hazard=earthquake"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(visible["items"].as_array().unwrap().len(), 1);
    // Los pasos de la fase que decide viajan como texto, no como video.
    let steps = visible["items"][0]["steps"].as_array().unwrap();
    assert!(
        steps
            .iter()
            .any(|s| s["phase"] == "during" && s["text"].as_str().is_some_and(|t| t.len() > 20)),
        "faltan los pasos de la fase durante"
    );
    // El video acompaña a los pasos, nunca los sustituye, y su dirección es https.
    let video = &visible["items"][0]["video"];
    assert!(
        video.is_object(),
        "la lección debía traer su video: {video}"
    );
    assert!(
        video["url"].as_str().unwrap().starts_with("https://"),
        "un video en claro no debería haberse publicado"
    );
    assert!(
        !visible["items"][0]["sources"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // El paquete sin red se sirve con ETag y revalida con un 304, que es lo que
    // permite guardarlo en el dispositivo antes de la emergencia.
    let first = bob
        .get(server.url("v1/lessons/bundle?locale=es-CO"))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first.headers()["etag"].to_str().unwrap().to_owned();
    let payload: Value = first.json().await.unwrap();
    assert_eq!(payload["count"], 1);
    let revalidated = bob
        .get(server.url("v1/lessons/bundle?locale=es-CO"))
        .header("If-None-Match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(
        revalidated.status(),
        StatusCode::NOT_MODIFIED,
        "revalidar debía costar un 304 y no el paquete entero"
    );
    // Un ETag que no corresponde devuelve el paquete.
    assert_eq!(
        bob.get(server.url("v1/lessons/bundle?locale=es-CO"))
            .header("If-None-Match", "\"otro\"")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // Reimportar devuelve a borrador lo ya publicado: un cambio de redacción en
    // una instrucción de emergencia merece la misma revisión que la original.
    assert!(import_lessons().status.success());
    let after_reimport: Value = bob
        .get(server.url("v1/lessons"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        after_reimport["items"].as_array().unwrap().len(),
        0,
        "reimportar no debe dejar publicado contenido sin revisar de nuevo"
    );
    // Y el ETag cambia, así que el cliente se enterará en su próxima revalidación.
    let changed = bob
        .get(server.url("v1/lessons/bundle?locale=es-CO"))
        .header("If-None-Match", &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::OK);

    // --- Cobro y términos (fase 7) ------------------------------------------
    let import_legal = || -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_territorio-api"))
            .arg("import-legal")
            .arg("legal/co-es.json")
            .env(
                "TP_SECRET",
                "test-secret-longer-than-thirty-two-characters-only-tests",
            )
            .env("TP_MONGO_DB", &db)
            .env("TP_ORIGIN", format!("http://127.0.0.1:{port}"))
            .env("TP_BIND", format!("127.0.0.1:{port}"))
            .env("TP_ENV", "development")
            .env("TP_SMTP_HOST", "")
            .env("TP_MAIL_DIR", &mail)
            .output()
            .unwrap()
    };

    // Sin términos publicados no se puede cobrar a nadie.
    assert_eq!(
        server
            .post(&bob, "v1/billing/checkout", json!({}))
            .await
            .status(),
        StatusCode::NOT_FOUND,
        "no se puede cobrar sin términos vigentes"
    );
    assert!(import_legal().status.success());
    // Importados siguen siendo borrador, así que sigue sin poder cobrarse.
    assert_eq!(
        server
            .post(&bob, "v1/billing/checkout", json!({}))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    // Publicar sin revisor jurídico se rechaza nombrando el motivo.
    let refused = alice
        .patch(server.url("v1/legal/terms"))
        .header("Origin", format!("http://127.0.0.1:{port}"))
        .json(&json!({"published": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
    let refused: Value = refused.json().await.unwrap();
    assert_eq!(refused["error"], "Falta la persona que revisó el documento");
    // Y un ciudadano no puede publicar términos.
    assert_eq!(
        bob.patch(server.url("v1/legal/terms"))
            .header("Origin", format!("http://127.0.0.1:{port}"))
            .json(&json!({"reviewed_by": "yo mismo"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let live = alice
        .patch(server.url("v1/legal/terms"))
        .header("Origin", format!("http://127.0.0.1:{port}"))
        .json(&json!({"reviewed_by": "Asesoría jurídica externa", "published": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(live.status(), StatusCode::OK);

    let terms: Value = bob
        .get(server.url("v1/legal/terms"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let terms_version = terms["document"]["version"].as_str().unwrap().to_owned();
    assert_eq!(terms["accepted"], false);
    // El texto dice expresamente que no es alerta temprana y qué se cobra.
    let body = terms["document"]["body"].as_str().unwrap();
    assert!(body.contains("NO ES UN SISTEMA DE ALERTA SÍSMICA TEMPRANA"));
    assert!(body.contains("NO se cobra, ni se promete, la entrega"));

    // Cobrar antes de aceptar los términos se rechaza.
    assert_eq!(
        server
            .post(&bob, "v1/billing/checkout", json!({}))
            .await
            .status(),
        StatusCode::CONFLICT,
        "no se cobra a quien no aceptó los términos"
    );
    // Aceptar una versión que no es la vigente no cuenta.
    assert_eq!(
        server
            .post(
                &bob,
                "v1/legal/terms/accept",
                json!({"version": "otra-version"})
            )
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        server
            .post(
                &bob,
                "v1/legal/terms/accept",
                json!({"version": &terms_version})
            )
            .await
            .status(),
        StatusCode::CREATED
    );

    // Ahora sí: se genera una referencia propia, con la cuenta y el plan atados.
    let created = server.post(&bob, "v1/billing/checkout", json!({})).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: Value = created.json().await.unwrap();
    let reference = created["reference"].as_str().unwrap().to_owned();
    assert!(reference.starts_with("tp-"));
    assert_eq!(created["price"]["amount_cents"], 2000000);

    // Firma del webhook, como la haría la pasarela.
    let sign_webhook = |timestamp: i64, body: &str| -> String {
        use hmac::{Hmac, Mac};
        let mut mac = Hmac::<sha2::Sha256>::new_from_slice(BILLING_SECRET.as_bytes()).unwrap();
        mac.update(format!("{timestamp}.{body}").as_bytes());
        hex::encode(mac.finalize().into_bytes())
    };
    let now_seconds = || mongodb::bson::DateTime::now().timestamp_millis() / 1000;
    // El webhook no lleva `Origin`: lo llama un servidor, no un navegador.
    let send_webhook = |body: String, timestamp: i64, signature: String| {
        Http::new()
            .post(server.url("v1/billing/webhook"))
            .header("content-type", "application/json")
            .header("x-timestamp", timestamp.to_string())
            .header("x-signature", signature)
            .body(body)
            .send()
    };
    let event = |id: &str, reference: &str, amount: i64| {
        json!({"data": {
            "id": id,
            "reference": reference,
            "status": "APPROVED",
            "amount_in_cents": amount,
            "currency": "COP"
        }})
        .to_string()
    };

    // Firma inválida: rechazado.
    let payload = event("evt-1", &reference, 2000000);
    let ts = now_seconds();
    assert_eq!(
        send_webhook(payload.clone(), ts, "f".repeat(64))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // Cuerpo alterado con una firma auténtica de otro cuerpo: rechazado. Es el
    // ataque que la firma existe para impedir.
    assert_eq!(
        send_webhook(
            event("evt-1", &reference, 100),
            ts,
            sign_webhook(ts, &payload)
        )
        .await
        .unwrap()
        .status(),
        StatusCode::UNAUTHORIZED
    );
    // Reenvío de un evento antiguo con su firma válida: rechazado.
    let old = now_seconds() - 4000;
    assert_eq!(
        send_webhook(payload.clone(), old, sign_webhook(old, &payload))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    // Pago legítimo: otorga el plan.
    let applied = send_webhook(payload.clone(), ts, sign_webhook(ts, &payload))
        .await
        .unwrap();
    assert_eq!(applied.status(), StatusCode::OK);
    let applied: Value = applied.json().await.unwrap();
    assert_eq!(applied["status"], "applied");
    let mine: Value = bob
        .get(server.url("v1/billing/me"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(mine["plan"], "paid");
    assert_eq!(mine["terms_accepted"], true);
    let granted_until = mine["valid_until"].as_str().unwrap().to_owned();

    // El mismo evento otra vez no suma otro mes: las pasarelas reintentan.
    let repeated = send_webhook(payload.clone(), ts, sign_webhook(ts, &payload))
        .await
        .unwrap();
    assert_eq!(repeated.status(), StatusCode::OK);
    let repeated: Value = repeated.json().await.unwrap();
    assert_eq!(repeated["status"], "already_applied");
    let after: Value = bob
        .get(server.url("v1/billing/me"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        after["valid_until"].as_str().unwrap(),
        granted_until,
        "un reintento de la pasarela no puede sumar otro mes"
    );

    // Importe que no corresponde: no se otorga nada.
    let second = server.post(&bob, "v1/billing/checkout", json!({})).await;
    let second: Value = second.json().await.unwrap();
    let cheap_reference = second["reference"].as_str().unwrap().to_owned();
    let cheap = event("evt-2", &cheap_reference, 100);
    let ts2 = now_seconds();
    let mismatch: Value = send_webhook(cheap.clone(), ts2, sign_webhook(ts2, &cheap))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        mismatch["status"], "amount_mismatch",
        "pagar de menos no puede dar el plan"
    );
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("payments")
            .find_one(doc! {"_id": &cheap_reference})
            .await
            .unwrap()
            .unwrap()
            .get_str("status")
            .unwrap(),
        "pending",
        "un importe que no cuadra no marca el cobro como pagado"
    );

    // Referencia que no originamos: se registra para conciliar, sin otorgar nada.
    let orphan = event("evt-3", "tp-referencia-inventada", 2000000);
    let ts3 = now_seconds();
    let unknown: Value = send_webhook(orphan.clone(), ts3, sign_webhook(ts3, &orphan))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(unknown["status"], "unknown_reference");
    // Todo lo recibido queda en el libro de eventos, aplicado o no.
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("payment_events")
            .count_documents(doc! {})
            .await
            .unwrap(),
        3,
        "el libro de eventos debe conservar también lo no aplicado"
    );

    // Sin secreto configurado, el webhook falla cerrado en lugar de aceptar.
    let bare_port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let bare = Server::start_with(db.clone(), mail.clone(), bare_port, None);
    bare.ready().await;
    let unsigned = Http::new()
        .post(bare.url("v1/billing/webhook"))
        .header("content-type", "application/json")
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        unsigned.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "sin secreto el webhook no puede aceptar nada"
    );
    drop(bare);

    assert_eq!(
        bob.get(server.url(&format!("v1/planning/plans/{plan_id}")))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    let review_route = format!("v1/planning/plans/{plan_id}/reviews");
    let review_body = json!({"status":"needs_changes","notes":"Revisión de prueba: verificar accesos y dotación antes de autorizar.","request_key":"review-integration"});
    assert_eq!(
        server
            .post(&bob, &review_route, review_body.clone())
            .await
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        server
            .post(&alice, &review_route, review_body.clone())
            .await
            .status(),
        StatusCode::CREATED
    );
    assert_eq!(
        server
            .post(&alice, &review_route, review_body.clone())
            .await
            .status(),
        StatusCode::OK
    );
    let mut changed_review = review_body;
    changed_review["notes"] = json!("Esta clave no se debe reutilizar para otra observación.");
    assert_eq!(
        server
            .post(&alice, &review_route, changed_review)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    let reviews: Value = alice
        .get(server.url(&review_route))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reviews["items"].as_array().unwrap().len(), 1);
    let mut invalid = planning_request.clone();
    invalid["exclusions"] =
        json!([{"type":"Polygon","coordinates":[[[-76.519,3.48],[-76.518,3.48],[-76.518,3.481]]]}]);
    assert_eq!(
        server
            .post(&alice, "v1/planning/preview", invalid)
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    let body = json!({"report_id":report_id,"language":"en","request_key":"test-idempotency"});
    assert_eq!(
        server.post(&alice, "v1/jobs", body.clone()).await.status(),
        StatusCode::ACCEPTED
    );
    assert_eq!(
        server.post(&alice, "v1/jobs", body).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("jobs")
            .count_documents(doc! {})
            .await
            .unwrap(),
        1
    );
    let concurrent = json!({"report_id":report_id,"language":"en","request_key":"concurrent-key"});
    let (first, second) = tokio::join!(
        server.post(&alice, "v1/jobs", concurrent.clone()),
        server.post(&alice, "v1/jobs", concurrent)
    );
    assert!(first.status().is_success());
    assert!(second.status().is_success());
    let first: Value = first.json().await.unwrap();
    let second: Value = second.json().await.unwrap();
    assert_eq!(first["id"], second["id"]);
    assert_eq!(
        database
            .collection::<mongodb::bson::Document>("jobs")
            .count_documents(doc! {})
            .await
            .unwrap(),
        2
    );

    assert_eq!(
        server
            .post(
                &alice,
                "v1/shelters",
                json!({"name":"Colegio","zone":"Cali","capacity":30,"occupied":31})
            )
            .await
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        server
            .post(
                &alice,
                "v1/shelters",
                json!({"name":"Colegio","zone":"Cali","capacity":30,"occupied":5})
            )
            .await
            .status(),
        StatusCode::CREATED
    );
    let admin: Value = alice
        .get(server.url("v1/admin/users"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!admin.to_string().contains("argon2"));
    assert!(!admin.to_string().contains("google_sub"));
    // A valid OTP may be consumed once. Six wrong attempts lock a challenge.
    let d: Value = server
        .post(
            &bob,
            "auth/login",
            json!({"email":"bob@example.com","password":"A long test password 2026"}),
        )
        .await
        .json()
        .await
        .unwrap();
    let id = d["challenge"].as_str().unwrap();
    let code = server.code(id).await;
    let helper = Command::new(env!("CARGO_BIN_EXE_territorio-api"))
        .args(["local-code", "bob@example.com", "--json"])
        .env(
            "TP_SECRET",
            "test-secret-longer-than-thirty-two-characters-only-tests",
        )
        .env("TP_MONGO_DB", &db)
        .env("TP_MAIL_DIR", &mail)
        .env("TP_ENV", "development")
        .env("TP_SMTP_HOST", "")
        .output()
        .unwrap();
    assert!(helper.status.success());
    let local_code: Value = serde_json::from_slice(&helper.stdout).unwrap();
    assert_eq!(local_code["code"], code);
    assert_eq!(local_code["challenge"], id);
    let wrong = if code == "000000" { "999999" } else { "000000" };
    for _ in 0..5 {
        assert_eq!(
            server
                .post(&bob, "auth/verify", json!({"challenge":id,"code":wrong}))
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }
    assert_eq!(
        server
            .post(&bob, "auth/verify", json!({"challenge":id,"code":code}))
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let locked_helper = Command::new(env!("CARGO_BIN_EXE_territorio-api"))
        .args(["local-code", "bob@example.com", "--json"])
        .env(
            "TP_SECRET",
            "test-secret-longer-than-thirty-two-characters-only-tests",
        )
        .env("TP_MONGO_DB", &db)
        .env("TP_MAIL_DIR", &mail)
        .env("TP_ENV", "development")
        .env("TP_SMTP_HOST", "")
        .output()
        .unwrap();
    assert!(
        !locked_helper.status.success(),
        "The local helper must not display a locked code"
    );
    // Session and business data survive a process restart.
    server.child.kill().unwrap();
    server.child.wait().unwrap();
    server = Server::start(server.db.clone(), server.mail.clone(), port);
    server.ready().await;
    assert_eq!(
        alice
            .get(server.url("auth/me"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let saved: Value = alice
        .get(server.url("v1/reports"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(saved["items"].as_array().unwrap().len(), 1);
    let restored: Value = alice
        .get(server.url(&format!("v1/planning/plans/{plan_id}")))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        restored["plan"], saved_plan,
        "The saved layout and source must survive restart unchanged"
    );
    let persisted_reviews: Value = alice
        .get(server.url(&review_route))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(persisted_reviews["items"][0]["status"], "needs_changes");
    let reset: Value = server
        .post(&bob, "auth/forgot", json!({"email":"bob@example.com"}))
        .await
        .json()
        .await
        .unwrap();
    let id = reset["challenge"].as_str().unwrap();
    let code = server.code(id).await;
    assert_eq!(
        server
            .post(
                &bob,
                "auth/reset",
                json!({"challenge":id,"code":code,"password":"New long password 2026"})
            )
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        bob.get(server.url("auth/me"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        server
            .post(
                &bob,
                "auth/reset",
                json!({"challenge":id,"code":code,"password":"New long password 2026"})
            )
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // Role changes revoke existing sessions and increment the authentication epoch.
    let response = alice
        .patch(server.url(&format!("v1/admin/users/{bid}")))
        .header("Origin", format!("http://127.0.0.1:{port}"))
        .json(&json!({"role":"operator","disabled":false}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let audit: Value = alice
        .get(server.url("v1/admin/audit"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!audit["items"].as_array().unwrap().is_empty());
    assert_eq!(
        server
            .post(&alice, "auth/logout-all", json!({}))
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        alice
            .get(server.url("auth/me"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    drop(server);
    database.drop().await.unwrap();
    tokio::fs::remove_dir_all(mail).await.unwrap();
}
