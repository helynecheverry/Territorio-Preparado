use crate::{
    AppState, db,
    error::{self, Result},
    security,
};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
};
use mongodb::bson::{Document, doc};
use openidconnect::{
    AuthenticationFlow, AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
    core::{CoreClient, CoreProviderMetadata, CoreResponseType},
};
use serde::Deserialize;

async fn metadata() -> Result<CoreProviderMetadata> {
    CoreProviderMetadata::discover_async(
        IssuerUrl::new("https://accounts.google.com".into()).map_err(error::internal)?,
        &http()?,
    )
    .await
    .map_err(error::internal)
}
fn http() -> Result<openidconnect::reqwest::Client> {
    openidconnect::reqwest::Client::builder()
        .redirect(openidconnect::reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(error::internal)
}
pub async fn start(State(state): State<AppState>) -> Result<Response> {
    if state.config.google_id.is_empty() || state.config.google_secret.is_empty() {
        return Err(error::Error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Google no configurado",
        ));
    }
    let client = CoreClient::from_provider_metadata(
        metadata().await?,
        ClientId::new(state.config.google_id.clone()),
        Some(ClientSecret::new(state.config.google_secret.clone())),
    )
    .set_redirect_uri(
        RedirectUrl::new(format!("{}/api/auth/google/callback", state.config.origin))
            .map_err(error::internal)?,
    );
    let (pkce, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, csrf, nonce) = client
        .authorize_url(
            AuthenticationFlow::<CoreResponseType>::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        .add_scope(Scope::new("email".into()))
        .set_pkce_challenge(pkce)
        .url();
    let binding = security::token();
    state.db.collection::<Document>("oauth").insert_one(doc!{"_id":security::hash(csrf.secret()),"binding":security::hash(&binding),"nonce":nonce.secret(),"pkce":verifier.secret(),"expires":db::after(600)}).await?;
    let mut res = Redirect::to(url.as_str()).into_response();
    res.headers_mut().insert(
        "set-cookie",
        security::set_cookie(&state, "tp_oauth", &binding, 600)
            .parse()
            .map_err(error::internal)?,
    );
    Ok(res)
}
#[derive(Deserialize)]
pub struct Callback {
    code: String,
    state: String,
}
pub async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(input): Query<Callback>,
) -> Result<Response> {
    let binding = security::cookie(&headers, "tp_oauth").ok_or_else(error::unauthorized)?;
    let flow=state.db.collection::<Document>("oauth").find_one_and_delete(doc!{"_id":security::hash(&input.state),"binding":security::hash(&binding),"expires":{"$gt":db::now()}}).await?.ok_or_else(error::unauthorized)?;
    let client = CoreClient::from_provider_metadata(
        metadata().await?,
        ClientId::new(state.config.google_id.clone()),
        Some(ClientSecret::new(state.config.google_secret.clone())),
    )
    .set_redirect_uri(
        RedirectUrl::new(format!("{}/api/auth/google/callback", state.config.origin))
            .map_err(error::internal)?,
    );
    let tokens = client
        .exchange_code(AuthorizationCode::new(input.code))
        .map_err(error::internal)?
        .set_pkce_verifier(PkceCodeVerifier::new(
            flow.get_str("pkce").map_err(error::internal)?.into(),
        ))
        .request_async(&http()?)
        .await
        .map_err(|_| error::unauthorized())?;
    let nonce = Nonce::new(flow.get_str("nonce").map_err(error::internal)?.into());
    let claims = tokens
        .id_token()
        .ok_or_else(error::unauthorized)?
        .claims(&client.id_token_verifier(), &nonce)
        .map_err(|_| error::unauthorized())?;
    if claims.email_verified() != Some(true) {
        return Err(error::unauthorized());
    }
    let email = super::email(claims.email().ok_or_else(error::unauthorized)?.as_str())?;
    let sub = claims.subject().as_str();
    let col = state.db.collection::<Document>("users");
    let user = if let Some(user) = col
        .find_one(doc! {"google_sub":sub,"disabled":false})
        .await?
    {
        user
    } else {
        // Do not silently merge an existing password account by email.
        if col.find_one(doc! {"email":&email}).await?.is_some() {
            return Err(error::Error(
                StatusCode::CONFLICT,
                "Este correo ya tiene una cuenta. Inicia sesión con contraseña",
            ));
        }
        let user = doc! {"_id":uuid::Uuid::new_v4().to_string(),"google_sub":sub,"email":&email,"auth_epoch":0_i64,"role":"citizen","verified":true,"disabled":false,"created":db::now()};
        col.insert_one(user.clone()).await?;
        user
    };
    // Google authentication is followed by our email second factor as well.
    security::limit(&state, "google-code", sub, 5, 900).await?;
    let challenge = super::challenge(&state, &user, "login").await?;
    let mut res = Redirect::to(&format!("/app/#challenge={challenge}")).into_response();
    res.headers_mut().insert(
        "set-cookie",
        security::set_cookie(&state, "tp_oauth", "", 0)
            .parse()
            .map_err(error::internal)?,
    );
    Ok(res)
}
