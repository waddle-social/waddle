use std::path::Path;

use sqlx::Row;
use uuid::Uuid;
use waddle_broker_core::admission::AttemptId;

pub async fn load_attempt_ids(data_dir: &Path) -> Vec<AttemptId> {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect(&format!(
            "sqlite://{}",
            data_dir.join("admission.db").display()
        ))
        .await
        .expect("open admission sqlite");
    let rows = sqlx::query("SELECT attempt_id FROM admission_attempts")
        .fetch_all(&pool)
        .await
        .expect("query admission_attempts");
    rows.into_iter()
        .map(|row| {
            let id: String = row.get(0);
            AttemptId::from_uuid(Uuid::parse_str(&id).expect("attempt uuid"))
        })
        .collect()
}
