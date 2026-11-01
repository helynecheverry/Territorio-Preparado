//! Real HTTP + MongoDB integration. Isolated database; opt in with --ignored.
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
        let child = Command::new(env!("CARGO_BIN_EXE_territorio-api"))
            .arg("api")
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
#[tokio::test]
#[ignore = "requires local MongoDB; creates and drops an isolated tp_test_* database"]
async fn real_workflow_security_and_persistence() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let db = format!("tp_test_{}", uuid::Uuid::new_v4().simple());
    let mail = std::env::temp_dir().join(&db);
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
    let planning_request = json!({"space_id":"epou-8413","profile":"camp","days":3,"target_people":120,"service_reserve_percent":30,"water_liters_per_day":1500,"toilets_available":5,"kits_available":10,"evacuation_capacity":90,"covered_footprint_confirmed":false,"exclusions":[],"request_key":"integration-plan"});
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
    point_request["space_id"] = json!("deporte-984");
    assert_eq!(
        server
            .post(&alice, "v1/planning/preview", point_request)
            .await
            .status(),
        StatusCode::BAD_REQUEST
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
