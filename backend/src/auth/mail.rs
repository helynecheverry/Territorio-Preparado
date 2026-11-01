use crate::{
    AppState,
    error::{Result, internal},
};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    transport::smtp::authentication::Credentials,
};
use tokio::io::AsyncWriteExt;

pub async fn send_code(state: &AppState, email: &str, code: &str, challenge: &str) -> Result<()> {
    let body = format!(
        "Tu código de Territorio Preparado es: {code}\nCaduca en 10 minutos y admite cinco intentos.\nSi no solicitaste este acceso, ignora el mensaje.\n"
    );
    if let Some(host) = &state.config.smtp_host {
        let message = Message::builder()
            .from(state.config.mail_from.parse().map_err(internal)?)
            .to(email.parse().map_err(internal)?)
            .subject("Código de seguridad · Territorio Preparado")
            .body(body)
            .map_err(internal)?;
        let transport = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
            .map_err(internal)?
            .port(state.config.smtp_port)
            .credentials(Credentials::new(
                state.config.smtp_user.clone(),
                state.config.smtp_password.clone(),
            ))
            .build();
        transport.send(message).await.map_err(internal)?;
    } else {
        // Local development only. Files are never served by the HTTP application.
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::create_dir_all(&state.config.mail_dir)
            .await
            .map_err(internal)?;
        tokio::fs::set_permissions(
            &state.config.mail_dir,
            std::fs::Permissions::from_mode(0o700),
        )
        .await
        .map_err(internal)?;
        let path = std::path::Path::new(&state.config.mail_dir).join(format!("{challenge}.eml"));
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .await
            .map_err(internal)?;
        file.write_all(format!("To: {email}\nSubject: Código local\n\n{body}").as_bytes())
            .await
            .map_err(internal)?;
    }
    Ok(())
}
