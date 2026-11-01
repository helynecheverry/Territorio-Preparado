pub mod google;
pub mod mail;
use crate::{
    AppState, db,
    error::{self, Result},
    security,
};
use argon2::{
    Argon2, PasswordHasher, PasswordVerifier,
    password_hash::{PasswordHash, SaltString, rand_core::OsRng},
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use mongodb::bson::{Document, doc};
use rand::Rng;
use serde::Deserialize;
use serde_json::{Value, json};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/register", post(register))
        .route("/login", post(login))
        .route("/verify", post(verify))
        .route("/logout", post(logout))
        .route("/logout-all", post(logout_all))
        .route("/me", get(me))
        .route("/forgot", post(forgot))
        .route("/reset", post(reset))
        .route("/google", get(google::start))
        .route("/google/callback", get(google::callback))
}
#[derive(Clone)]
pub struct Actor {
    pub id: String,
    pub role: String,
    pub email: String,
}
impl Actor {
    pub fn operator(&self) -> Result<()> {
        if matches!(self.role.as_str(), "operator" | "admin") {
            Ok(())
        } else {
            Err(error::Error(
                StatusCode::FORBIDDEN,
                "Permiso de operador requerido",
            ))
        }
    }
    pub fn admin(&self) -> Result<()> {
        if self.role == "admin" {
            Ok(())
        } else {
            Err(error::Error(
                StatusCode::FORBIDDEN,
                "Permiso de administrador requerido",
            ))
        }
    }
}
pub async fn actor(state: &AppState, headers: &HeaderMap) -> Result<Actor> {
    let token = security::cookie(headers, "tp_session").ok_or_else(error::unauthorized)?;
    let session = state
        .db
        .collection::<Document>("sessions")
        .find_one(doc! {"token_hash":security::hash(&token),"expires":{"$gt":db::now()}})
        .await?
        .ok_or_else(error::unauthorized)?;
    let id = session.get_str("user").map_err(|_| error::unauthorized())?;
    let user = state.db.collection::<Document>("users").find_one(doc!{"_id":id,"verified":true,"disabled":false,"auth_epoch":session.get_i64("auth_epoch").unwrap_or(-1)}).await?.ok_or_else(error::unauthorized)?;
    Ok(Actor {
        id: id.into(),
        role: user.get_str("role").unwrap_or("citizen").into(),
        email: user.get_str("email").unwrap_or_default().into(),
    })
}
pub fn email(value: &str) -> Result<String> {
    let value = value.trim().to_lowercase();
    if value.len() > 254
        || value.contains(['\r', '\n', ' '])
        || value.parse::<lettre::Address>().is_err()
    {
        return Err(error::bad("Correo inválido"));
    }
    Ok(value)
}
fn password(value: &str) -> Result<()> {
    if !(12..=128).contains(&value.len()) {
        return Err(error::bad("La contraseña debe tener entre 12 y 128 bytes"));
    }
    Ok(())
}
pub async fn hash_password(value: String) -> Result<String> {
    tokio::task::spawn_blocking(move || {
        Argon2::default()
            .hash_password(value.as_bytes(), &SaltString::generate(&mut OsRng))
            .map(|v| v.to_string())
            .map_err(error::internal)
    })
    .await
    .map_err(error::internal)?
}
async fn check_password(value: String, hash: String) -> Result<bool> {
    tokio::task::spawn_blocking(move || {
        PasswordHash::new(&hash).ok().is_some_and(|h| {
            Argon2::default()
                .verify_password(value.as_bytes(), &h)
                .is_ok()
        })
    })
    .await
    .map_err(error::internal)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Credentials {
    email: String,
    password: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmailInput {
    email: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    challenge: String,
    code: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reset {
    challenge: String,
    code: String,
    password: String,
}

pub async fn challenge(state: &AppState, user: &Document, purpose: &str) -> Result<String> {
    let id = security::token();
    let code = format!("{:06}", rand::thread_rng().gen_range(0..1_000_000));
    let user_id = user.get_str("_id").map_err(error::internal)?;
    let email = user.get_str("email").map_err(error::internal)?;
    state.db.collection::<Document>("challenges").insert_one(doc!{"_id":&id,"user":user_id,"purpose":purpose,"digest":security::keyed(&state.config.secret,&format!("{id}:{code}")),"attempts":0,"auth_epoch":user.get_i64("auth_epoch").unwrap_or(0),"expires":db::after(600)}).await?;
    if let Err(e) = mail::send_code(state, email, &code, &id).await {
        state
            .db
            .collection::<Document>("challenges")
            .delete_one(doc! {"_id":&id})
            .await?;
        return Err(e);
    }
    Ok(id)
}
async fn consume(state: &AppState, id: &str, code: &str, purpose: &str) -> Result<(String, i64)> {
    use subtle::ConstantTimeEq;
    if id.len() != 64 || code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(error::unauthorized());
    }
    let col = state.db.collection::<Document>("challenges");
    let item = col
        .find_one_and_update(
            doc! {"_id":id,"purpose":purpose,"attempts":{"$lt":5},"expires":{"$gt":db::now()}},
            doc! {"$inc":{"attempts":1}},
        )
        .await?
        .ok_or_else(error::unauthorized)?;
    let expected = item.get_str("digest").map_err(error::internal)?;
    let actual = security::keyed(&state.config.secret, &format!("{id}:{code}"));
    if !bool::from(expected.as_bytes().ct_eq(actual.as_bytes())) {
        return Err(error::unauthorized());
    }
    let consumed = col.delete_one(doc! {"_id":id}).await?;
    if consumed.deleted_count != 1 {
        return Err(error::unauthorized());
    }
    Ok((
        item.get_str("user").map_err(error::internal)?.to_owned(),
        item.get_i64("auth_epoch").unwrap_or(0),
    ))
}
async fn register(
    State(state): State<AppState>,
    Json(input): Json<Credentials>,
) -> Result<Json<Value>> {
    let email = email(&input.email)?;
    password(&input.password)?;
    security::limit(&state, "register", &email, 3, 3600).await?;
    let col = state.db.collection::<Document>("users");
    // An unverified duplicate never overwrites credentials chosen during registration.
    let mut id = security::token();
    if col.find_one(doc! {"email":&email}).await?.is_none() {
        let user = doc! {"_id":uuid::Uuid::new_v4().to_string(),"email":&email,"password":hash_password(input.password).await?,"auth_epoch":0_i64,"role":"citizen","verified":false,"disabled":false,"created":db::now()};
        match col.insert_one(user.clone()).await {
            Ok(_) => {
                id = challenge(&state, &user, "login").await?;
            }
            Err(e) if e.to_string().contains("E11000") => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(Json(
        json!({"challenge":id,"message":"Si el registro es válido, recibirás un código por correo"}),
    ))
}
async fn login(
    State(state): State<AppState>,
    Json(input): Json<Credentials>,
) -> Result<Json<Value>> {
    let email = email(&input.email)?;
    security::limit(&state, "login", &email, 10, 900).await?;
    if input.password.len() > 128 {
        return Err(error::unauthorized());
    }
    let user = state
        .db
        .collection::<Document>("users")
        .find_one(doc! {"email":email,"disabled":false})
        .await?;
    let hash = user
        .as_ref()
        .and_then(|u| u.get_str("password").ok())
        .map(str::to_owned)
        .unwrap_or_else(|| state.dummy_password.clone());
    let valid = check_password(input.password, hash).await?;
    let user = user.filter(|_| valid).ok_or_else(error::unauthorized)?;
    let id = challenge(&state, &user, "login").await?;
    Ok(Json(
        json!({"challenge":id,"message":"Introduce el código enviado a tu correo"}),
    ))
}
pub async fn session(state: &AppState, id: &str, epoch: i64) -> Result<Response> {
    let token = security::token();
    state.db.collection::<Document>("sessions").insert_one(doc!{"token_hash":security::hash(&token),"user":id,"auth_epoch":epoch,"created":db::now(),"expires":db::after(8*3600)}).await?;
    db::audit(&state.db, id, "login", id).await?;
    let mut res = Json(json!({"authenticated":true})).into_response();
    res.headers_mut().insert(
        "set-cookie",
        security::set_cookie(state, "tp_session", &token, 8 * 3600)
            .parse()
            .map_err(error::internal)?,
    );
    Ok(res)
}
async fn verify(State(state): State<AppState>, Json(input): Json<Proof>) -> Result<Response> {
    let (id, epoch) = consume(&state, &input.challenge, &input.code, "login").await?;
    let updated = state
        .db
        .collection::<Document>("users")
        .update_one(
            doc! {"_id":&id,"disabled":false,"auth_epoch":epoch},
            doc! {"$set":{"verified":true}},
        )
        .await?;
    if updated.matched_count != 1 {
        return Err(error::unauthorized());
    }
    session(&state, &id, epoch).await
}
async fn me(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<Value>> {
    let a = actor(&state, &headers).await?;
    Ok(Json(json!({"id":a.id,"email":a.email,"role":a.role})))
}
async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response> {
    if let Some(token) = security::cookie(&headers, "tp_session") {
        state
            .db
            .collection::<Document>("sessions")
            .delete_one(doc! {"token_hash":security::hash(&token)})
            .await?;
    }
    let mut res = Json(json!({"ok":true})).into_response();
    res.headers_mut().insert(
        "set-cookie",
        security::set_cookie(&state, "tp_session", "", 0)
            .parse()
            .map_err(error::internal)?,
    );
    Ok(res)
}
async fn logout_all(State(state): State<AppState>, headers: HeaderMap) -> Result<Response> {
    let a = actor(&state, &headers).await?;
    state
        .db
        .collection::<Document>("users")
        .update_one(doc! {"_id":&a.id}, doc! {"$inc":{"auth_epoch":1_i64}})
        .await?;
    state
        .db
        .collection::<Document>("sessions")
        .delete_many(doc! {"user":&a.id})
        .await?;
    logout(State(state), headers).await
}
async fn forgot(
    State(state): State<AppState>,
    Json(input): Json<EmailInput>,
) -> Result<Json<Value>> {
    let email = email(&input.email)?;
    security::limit(&state, "reset", &email, 3, 3600).await?;
    let user = state
        .db
        .collection::<Document>("users")
        .find_one(doc! {"email":email,"disabled":false,"verified":true,"password":{"$exists":true}})
        .await?;
    let id = if let Some(user) = user {
        challenge(&state, &user, "reset").await?
    } else {
        security::token()
    };
    Ok(Json(
        json!({"challenge":id,"message":"Si la cuenta existe, recibirás un código"}),
    ))
}
async fn reset(State(state): State<AppState>, Json(input): Json<Reset>) -> Result<Json<Value>> {
    password(&input.password)?;
    let (id, epoch) = consume(&state, &input.challenge, &input.code, "reset").await?;
    let password = hash_password(input.password).await?;
    let changed = state
        .db
        .collection::<Document>("users")
        .update_one(
            doc! {"_id":&id,"auth_epoch":epoch,"disabled":false},
            doc! {"$set":{"password":password},"$inc":{"auth_epoch":1_i64}},
        )
        .await?;
    if changed.matched_count != 1 {
        return Err(error::unauthorized());
    }
    state
        .db
        .collection::<Document>("sessions")
        .delete_many(doc! {"user":&id})
        .await?;
    // Revoke outstanding login challenges when the password changes.
    state
        .db
        .collection::<Document>("challenges")
        .delete_many(doc! {"user":&id})
        .await?;
    db::audit(&state.db, &id, "password_reset", &id).await?;
    Ok(Json(json!({"ok":true})))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validation() {
        assert!(email("a\r\n@b.com").is_err());
        assert!(password("short").is_err());
        assert_eq!(email(" A@Example.com ").unwrap(), "a@example.com");
    }
    #[tokio::test]
    async fn passwords() {
        let h = hash_password("a strong password".into()).await.unwrap();
        assert!(
            check_password("a strong password".into(), h.clone())
                .await
                .unwrap()
        );
        assert!(!check_password("incorrect".into(), h).await.unwrap());
    }
}
