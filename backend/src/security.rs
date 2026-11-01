use crate::{
    AppState, db,
    error::{self, Result},
};
use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::Response,
};
use hmac::{Hmac, Mac};
use mongodb::{
    bson::{Document, doc},
    options::ReturnDocument,
};
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};

pub fn token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}
pub fn hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
pub fn keyed(secret: &str, value: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(value.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}
pub fn cookie(headers: &HeaderMap, key: &str) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|v| {
            let (k, v) = v.trim().split_once('=')?;
            (k == key).then(|| v.to_string())
        })
}
pub fn set_cookie(state: &AppState, name: &str, value: &str, seconds: u64) -> String {
    format!(
        "{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={seconds}{}",
        if state.config.production {
            "; Secure"
        } else {
            ""
        }
    )
}
pub async fn limit(state: &AppState, scope: &str, key: &str, max: i32, seconds: i64) -> Result<()> {
    // A fixed window is shared across API replicas; upsert increments are atomic.
    let window = db::now().timestamp_millis() / (seconds * 1000);
    let id = keyed(&state.config.secret, &format!("{scope}:{key}:{window}"));
    let col = state.db.collection::<Document>("limits");
    let result = col
        .find_one_and_update(
            doc! {"_id":&id},
            doc! {"$inc":{"count":1},"$setOnInsert":{"expires":db::after(seconds*2)}},
        )
        .upsert(true)
        .return_document(ReturnDocument::After)
        .await;
    let d = match result {
        Ok(Some(d)) => d,
        Ok(None) => return Err(error::internal("missing limit")),
        Err(e) if e.to_string().contains("E11000") => {
            return Err(error::Error(
                StatusCode::TOO_MANY_REQUESTS,
                "Inténtalo más tarde",
            ));
        }
        Err(e) => return Err(e.into()),
    };
    if d.get_i32("count").unwrap_or(max + 1) > max {
        return Err(error::Error(
            StatusCode::TOO_MANY_REQUESTS,
            "Demasiados intentos; inténtalo más tarde",
        ));
    }
    Ok(())
}
pub async fn guard(State(state): State<AppState>, req: Request, next: Next) -> Result<Response> {
    let strict = req.uri().path().starts_with("/app") || req.uri().path().starts_with("/api");
    if req.uri().path().starts_with("/api/")
        && let Some(peer) = req
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
    {
        // Never trust an arbitrary X-Forwarded-For header. Proxies need their own edge limits.
        limit(&state, "http", &peer.0.ip().to_string(), 240, 60).await?;
        if req.uri().path().starts_with("/api/auth/") && req.method() == "POST" {
            limit(&state, "auth-http", &peer.0.ip().to_string(), 40, 60).await?;
        }
    }
    if req.method() != "GET" && req.method() != "HEAD" && req.method() != "OPTIONS" {
        let origin = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok());
        if origin != Some(state.config.origin.as_str()) {
            return Err(error::Error(
                StatusCode::FORBIDDEN,
                "Origen de solicitud inválido",
            ));
        }
    }
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert("x-content-type-options", "nosniff".parse().unwrap());
    h.insert(
        "x-frame-options",
        if strict { "DENY" } else { "SAMEORIGIN" }.parse().unwrap(),
    );
    // Public map tiles require a browser Referer; send only the origin cross-site.
    // Authenticated app/API pages retain the stricter policy.
    h.insert(
        "referrer-policy",
        if strict {
            "no-referrer"
        } else {
            "strict-origin-when-cross-origin"
        }
        .parse()
        .unwrap(),
    );
    h.insert(
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(self)"
            .parse()
            .unwrap(),
    );
    if strict {
        h.insert("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; connect-src 'self'; object-src 'none'; base-uri 'self'; frame-ancestors 'none'; form-action 'self'".parse().unwrap());
    }
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    if state.config.production {
        h.insert(
            "strict-transport-security",
            "max-age=31536000; includeSubDomains".parse().unwrap(),
        );
    }
    Ok(res)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn random_tokens_and_hmac() {
        assert_ne!(token(), token());
        assert_ne!(keyed("a", "b"), keyed("b", "b"));
        assert_eq!(hash("hello").len(), 64);
    }
}
