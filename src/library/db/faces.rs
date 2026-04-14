//! Thin forwarding layer for face operations on `Database`.
//!
//! All SQL lives in `FacesRepository` (`library/faces/repository.rs`).
//! This module exists so that code holding a `Database` (e.g. media
//! tests that set up face fixtures) can still call face methods.
//! It will be removed when all features are converted to repositories.

use crate::library::error::LibraryError;
use crate::library::faces::repository::FacesRepository;

pub(crate) use crate::library::faces::repository::AssetFaceRow;

use super::Database;

impl Database {
    /// Forwarding shim — delegates to `FacesRepository`.
    pub async fn list_people(
        &self,
        include_hidden: bool,
        include_unnamed: bool,
    ) -> Result<Vec<crate::library::faces::Person>, LibraryError> {
        FacesRepository::new(self.clone())
            .list_people(include_hidden, include_unnamed)
            .await
    }

    /// Forwarding shim — delegates to `FacesRepository`.
    pub async fn list_media_for_person(
        &self,
        person_id: &str,
    ) -> Result<Vec<String>, LibraryError> {
        FacesRepository::new(self.clone())
            .list_media_for_person(person_id)
            .await
    }

    /// Forwarding shim — delegates to `FacesRepository`.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_person(
        &self,
        id: &str,
        name: &str,
        birth_date: Option<&str>,
        is_hidden: bool,
        is_favorite: bool,
        color: Option<&str>,
        face_asset_id: Option<&str>,
    ) -> Result<(), LibraryError> {
        FacesRepository::new(self.clone())
            .upsert_person(
                id,
                name,
                birth_date,
                is_hidden,
                is_favorite,
                color,
                face_asset_id,
            )
            .await
    }

    /// Forwarding shim — delegates to `FacesRepository`.
    pub async fn rename_person(&self, id: &str, name: &str) -> Result<(), LibraryError> {
        FacesRepository::new(self.clone())
            .rename_person(id, name)
            .await
    }

    /// Forwarding shim — delegates to `FacesRepository`.
    pub async fn set_person_hidden(&self, id: &str, hidden: bool) -> Result<(), LibraryError> {
        FacesRepository::new(self.clone())
            .set_person_hidden(id, hidden)
            .await
    }

    /// Forwarding shim — delegates to `FacesRepository`.
    pub async fn delete_person(&self, id: &str) -> Result<(), LibraryError> {
        FacesRepository::new(self.clone()).delete_person(id).await
    }

    /// Forwarding shim — delegates to `FacesRepository`.
    #[allow(dead_code)] // used in db/media.rs tests
    pub(crate) async fn upsert_asset_face(&self, face: &AssetFaceRow) -> Result<(), LibraryError> {
        FacesRepository::new(self.clone())
            .upsert_asset_face(face)
            .await
    }

    /// Forwarding shim — delegates to `FacesRepository`.
    pub async fn delete_asset_face(&self, id: &str) -> Result<(), LibraryError> {
        FacesRepository::new(self.clone())
            .delete_asset_face(id)
            .await
    }

    /// Forwarding shim — delegates to `FacesRepository`.
    pub async fn update_face_count(&self, person_id: &str) -> Result<(), LibraryError> {
        FacesRepository::new(self.clone())
            .update_face_count(person_id)
            .await
    }

    /// Forwarding shim — delegates to `FacesRepository`.
    pub async fn clear_people(&self) -> Result<(), LibraryError> {
        FacesRepository::new(self.clone()).clear_people().await
    }

    /// Forwarding shim — delegates to `FacesRepository`.
    pub async fn clear_asset_faces(&self) -> Result<(), LibraryError> {
        FacesRepository::new(self.clone()).clear_asset_faces().await
    }
}
