use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
#[derive(Debug)]
pub struct Error(pub StatusCode, pub &'static str);
pub type Result<T> = std::result::Result<T, Error>;
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}
impl From<mongodb::error::Error> for Error {
    fn from(e: mongodb::error::Error) -> Self {
        tracing::error!(error = %e, "database operation failed");
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            "No se pudo completar la operación",
        )
    }
}
pub fn bad(msg: &'static str) -> Error {
    Error(StatusCode::BAD_REQUEST, msg)
}
pub fn unauthorized() -> Error {
    Error(
        StatusCode::UNAUTHORIZED,
        "Autenticación requerida o credenciales inválidas",
    )
}
pub fn internal(e: impl std::fmt::Display) -> Error {
    tracing::error!(error = %e, "operation failed");
    Error(
        StatusCode::SERVICE_UNAVAILABLE,
        "Servicio temporalmente no disponible",
    )
}
