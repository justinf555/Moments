use async_trait::async_trait;
use tracing::{debug, instrument};

use crate::library::error::LibraryError;

use super::{CounterKind, HandlerResult, SyncContext, SyncEntityHandler};
use crate::sync::providers::immich::types::*;

pub struct PersonHandler;

#[async_trait]
impl SyncEntityHandler for PersonHandler {
    fn entity_type(&self) -> &'static str {
        "PersonV1"
    }

    #[instrument(skip(self, data, ctx), fields(entity = "PersonV1"))]
    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let person: SyncPersonV1 = deserialize_entity(data, "PersonV1", line_number)?;
        let id = person.id.clone();

        ctx.library
            .faces()
            .upsert_person(
                &person.id,
                &person.name,
                person.birth_date.as_deref(),
                person.is_hidden,
                person.is_favorite,
                person.color.as_deref(),
                person.face_asset_id.as_deref(),
                Some(&person.id),
            )
            .await?;

        // Download person face thumbnail.
        let person_thumb_dir = ctx.thumbnails_dir.join("people");
        let thumb_path = person_thumb_dir.join(format!("{}.jpg", person.id));
        if !thumb_path.exists() {
            let api_path = format!("/people/{}/thumbnail", person.id);
            match ctx.client.get_bytes(&api_path).await {
                Ok(bytes) => {
                    if let Some(parent) = thumb_path.parent() {
                        let _ = tokio::fs::create_dir_all(parent).await;
                    }
                    let _ = tokio::fs::write(&thumb_path, &bytes).await;
                    debug!(person_id = %person.id, "person thumbnail downloaded");
                }
                Err(e) => {
                    debug!(person_id = %person.id, "person thumbnail download failed: {e}");
                }
            }
        }

        Ok(HandlerResult {
            entity_id: id,
            audit_action: "upsert",
            counter: CounterKind::People,
        })
    }
}

pub struct PersonDeleteHandler;

#[async_trait]
impl SyncEntityHandler for PersonDeleteHandler {
    fn entity_type(&self) -> &'static str {
        "PersonDeleteV1"
    }

    async fn handle(
        &self,
        data: &serde_json::Value,
        line_number: usize,
        ctx: &SyncContext,
    ) -> Result<HandlerResult, LibraryError> {
        let delete: SyncPersonDeleteV1 = deserialize_entity(data, "PersonDeleteV1", line_number)?;
        let id = delete.person_id.clone();
        ctx.library.faces().delete_person_by_id(&id).await?;
        Ok(HandlerResult {
            entity_id: id,
            audit_action: "delete",
            counter: CounterKind::Deletes,
        })
    }
}
