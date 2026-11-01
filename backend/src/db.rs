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
