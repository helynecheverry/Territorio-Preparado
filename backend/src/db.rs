use crate::{config::Config, error::Result};
use mongodb::{
    Database, IndexModel,
    bson::{DateTime, Document, doc},
    options::IndexOptions,
};
use std::time::Duration;
pub async fn connect(config: &Config) -> anyhow::Result<Database> {
    let mut opts = mongodb::options::ClientOptions::parse(&config.mongo_uri).await?;
    opts.server_selection_timeout = Some(Duration::from_secs(5));
    opts.app_name = Some("territorio-rust".into());
    let db = mongodb::Client::with_options(opts)?.database(&config.mongo_db);
    db.run_command(doc! {"ping": 1}).await?;
    for (collection, keys, unique, ttl) in [
        ("users", doc! {"email":1}, true, false),
        ("sessions", doc! {"token_hash":1}, true, false),
        ("sessions", doc! {"expires":1}, false, true),
        ("challenges", doc! {"expires":1}, false, true),
        ("oauth", doc! {"expires":1}, false, true),
        ("limits", doc! {"expires":1}, false, true),
        ("reports", doc! {"owner":1,"created":-1}, false, false),
        (
            "jobs",
            doc! {"status":1,"lease":1,"created":1},
            false,
            false,
        ),
        ("jobs", doc! {"owner":1,"request_key":1}, true, false),
        ("plans", doc! {"owner":1,"request_key":1}, true, false),
        ("plans", doc! {"created":-1,"_id":-1}, false, false),
        (
            "plan_reviews",
            doc! {"actor":1,"request_key":1},
            true,
            false,
        ),
        (
            "plan_reviews",
            doc! {"plan_id":1,"created":-1},
            false,
            false,
        ),
        ("audit", doc! {"created":1}, false, false),
        (
            "territories",
            doc! {"country_code":1,"name":1},
            false,
            false,
        ),
        // El inventario se consulta siempre dentro de un territorio, así que ese
        // campo encabeza cada índice compuesto y acota el recorrido.
        ("spaces", doc! {"territory_id":1,"search":1}, false, false),
        (
            "spaces",
            doc! {"territory_id":1,"properties.commune":1},
            false,
            false,
        ),
        (
            "spaces",
            doc! {"territory_id":1,"measurable":1},
            false,
            false,
        ),
        // Para borrar los restos de una importación anterior del territorio.
        (
            "spaces",
            doc! {"territory_id":1,"source_sha256":1},
            false,
            false,
        ),
        // Cercanía para recomendar un destino. Se indexa el punto representativo
        // y no la huella: MongoDB rechaza los polígonos que considera inválidos y
        // el inventario contiene algunos, mientras que un punto siempre es válido.
        ("spaces", doc! {"point":"2dsphere"}, false, false),
        // Sectores administrativos. Aquí la geometría sí se indexa, porque
        // resolver un punto a su barrio es una consulta de contención.
        ("sectors", doc! {"geometry":"2dsphere"}, false, false),
        (
            "sectors",
            doc! {"territory_id":1,"kind":1,"code":1},
            false,
            false,
        ),
        // Afiliaciones. El índice geoespacial es el que permitirá al motor de
        // alertas recuperar a los afectados por un sismo con una sola consulta
        // acotada al alcance del evento, en lugar de recorrer a todos los
        // suscriptores comparando magnitudes.
        ("subscriptions", doc! {"point":"2dsphere"}, false, false),
        ("subscriptions", doc! {"owner":1,"created":1}, false, false),
        (
            "subscriptions",
            doc! {"territory_id":1,"commune_code":1},
            false,
            false,
        ),
        // Eventos de amenaza. `canonical_id` agrupa las publicaciones del mismo
        // sismo por distintos catálogos.
        ("events", doc! {"origin_utc":-1}, false, false),
        ("events", doc! {"canonical_id":1}, false, false),
        (
            "events",
            doc! {"hazard_type":1,"origin_utc":-1},
            false,
            false,
        ),
        ("events", doc! {"point":"2dsphere"}, false, false),
        // Lo que el motor de alertas busca en cada pasada: eventos pendientes de
        // reparto, del más reciente al más antiguo.
        (
            "events",
            doc! {"alert_pending":1,"origin_utc":-1},
            false,
            false,
        ),
        // Para elegir la fuente con más autoridad de un mismo sismo.
        (
            "events",
            doc! {"canonical_id":1,"authority_rank":-1,"updated":-1},
            false,
            false,
        ),
        // Cola de avisos. El orden de reparto es lo más grave primero y, dentro
        // de eso, quien está más cerca del epicentro.
        (
            "notifications",
            doc! {"status":1,"level_rank":-1,"distance_km":1},
            false,
            false,
        ),
        (
            "notifications",
            doc! {"owner":1,"level_rank":-1,"created":-1},
            false,
            false,
        ),
        (
            "notifications",
            doc! {"canonical_id":1,"status":1},
            false,
            false,
        ),
        // Planes vigentes, para precargar quién tiene derecho a SMS.
        (
            "entitlements",
            doc! {"plan":1,"valid_until":-1},
            false,
            false,
        ),
        // Lo que el reparto reclama en cada lote: pendientes listos, por
        // gravedad y después por cercanía al epicentro.
        (
            "notifications",
            doc! {"status":1,"next_attempt":1,"lease":1},
            false,
            false,
        ),
        // Un navegador es un endpoint: volver a suscribirse actualiza el
        // registro en lugar de añadir otro envío por emergencia.
        ("push_devices", doc! {"endpoint":1}, true, false),
        ("push_devices", doc! {"owner":1,"created":1}, false, false),
        // El paquete sin red se sirve filtrando por publicadas e idioma.
        (
            "lessons",
            doc! {"published":1,"hazard_type":1},
            false,
            false,
        ),
        ("lessons", doc! {"locale":1,"published":1}, false, false),
        // Cobros: la referencia es el `_id`, así que basta indexar por titular
        // y por estado para soporte y conciliación.
        ("payments", doc! {"owner":1,"created":-1}, false, false),
        ("payments", doc! {"status":1,"created":-1}, false, false),
        ("payment_events", doc! {"received":-1}, false, false),
        // Aceptaciones de documentos legales: la clave incluye la versión, así
        // que este índice sirve para responder qué aceptó una cuenta.
        (
            "legal_acceptances",
            doc! {"owner":1,"document":1},
            false,
            false,
        ),
    ] {
        let options = IndexOptions::builder()
            .unique(unique)
            .expire_after(if ttl { Some(Duration::ZERO) } else { None })
            .build();
        db.collection::<Document>(collection)
            .create_index(IndexModel::builder().keys(keys).options(options).build())
            .await?;
    }
    db.collection::<Document>("users")
        .create_index(
            IndexModel::builder()
                .keys(doc! {"google_sub":1})
                .options(
                    IndexOptions::builder()
                        .unique(true)
                        .partial_filter_expression(doc! {"google_sub":{"$type":"string"}})
                        .build(),
                )
                .build(),
        )
        .await?;
    Ok(db)
}
pub fn now() -> DateTime {
    DateTime::now()
}
pub fn after(seconds: i64) -> DateTime {
    DateTime::from_millis(now().timestamp_millis() + seconds * 1000)
}
pub async fn audit(db: &Database, actor: &str, action: &str, target: &str) -> Result<()> {
    db.collection::<Document>("audit")
        .insert_one(doc! {"actor":actor,"action":action,"target":target,"created":now()})
        .await?;
    Ok(())
}
