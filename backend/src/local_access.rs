//! Local OS helper, deliberately absent from HTTP routes.
use crate::{config::Config, db, security};
use anyhow::{Context, Result, ensure};
use mongodb::{
    Database,
    bson::{Document, doc},
};
use serde_json::json;

pub async fn show_code(
    config: &Config,
    database: &Database,
    email: Option<&str>,
    as_json: bool,
) -> Result<()> {
    ensure!(
        !config.production && config.smtp_host.is_none(),
        "Este ayudante solo está disponible en desarrollo local sin SMTP"
    );
    let email = if let Some(email) = email.filter(|v| *v != "--json") {
        crate::auth::email(email).map_err(|_| anyhow::anyhow!("Correo inválido"))?
    } else {
        let account: serde_json::Value = serde_json::from_slice(
            &std::fs::read("var/local-account.json")
                .context("No se encontró la cuenta local configurada")?,
        )?;
        account["email"]
            .as_str()
            .context("La cuenta local no tiene correo")?
            .to_string()
    };
    let user = database
        .collection::<Document>("users")
        .find_one(doc! {"email":&email,"disabled":false})
        .await?
        .context("La cuenta local no está habilitada")?;
    let user_id = user.get_str("_id")?;
    let challenge=database.collection::<Document>("challenges").find_one(doc!{"user":user_id,"expires":{"$gt":db::now()},"attempts":{"$lt":5},"purpose":{"$in":["login","reset"]}}).sort(doc!{"expires":-1}).await?.context("No hay un código vigente. Primero introduce tu correo y contraseña en la aplicación y pulsa Continuar")?;
    let id = challenge.get_str("_id")?;
    ensure!(
        id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()),
        "Identificador de desafío inválido"
    );
    let path = std::path::Path::new(&config.mail_dir).join(format!("{id}.eml"));
    let mail =
        std::fs::read_to_string(path).context("No se encontró el correo local de este acceso")?;
    let code = mail
        .lines()
        .find_map(|line| line.strip_prefix("Tu código de Territorio Preparado es: "))
        .context("No se pudo leer el código local")?;
    ensure!(
        code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit()),
        "Código local inválido"
    );
    ensure!(
        security::keyed(&config.secret, &format!("{id}:{code}")) == challenge.get_str("digest")?,
        "El código no coincide con el desafío vigente"
    );
    let expires = challenge.get_datetime("expires")?.timestamp_millis();
    let seconds = ((expires - db::now().timestamp_millis()) / 1000).max(0);
    if as_json {
        println!(
            "{}",
            json!({"email":email,"code":code,"challenge":id,"expires_ms":expires,"seconds_remaining":seconds,"purpose":challenge.get_str("purpose")?})
        );
    } else {
        println!(
            "Cuenta: {email}\n\nCódigo de acceso: {code}\n\nTiempo restante: {} min {} s\n\nEscribe este código en la pantalla de verificación.\nEl código cambia en cada acceso y deja de servir al utilizarlo.",
            seconds / 60,
            seconds % 60
        );
    }
    Ok(())
}
