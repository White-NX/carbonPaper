//! Independently encrypted Office document references associated with screenshots.

use crate::credential_manager::CngKeySession;
use crate::office_protocol::OfficeDocumentRef;
use rusqlite::{params, OptionalExtension};

use super::StorageState;

/// Returned when the database was swapped between the moment a document
/// reference was collected and the moment it would have been written. Callers
/// treat this as a deliberate discard rather than a storage failure.
pub const STALE_DOCUMENT_REF_GENERATION: &str = "storage_changed_before_document_ref_write";

impl StorageState {
    /// Upsert a document reference without reading or decrypting the screenshot row.
    ///
    /// `expected_generation` is the [`StorageState::db_generation`] observed
    /// when `screenshot_id` was collected. Office resolution runs on its own
    /// worker and can finish after a backup restore or a data-directory switch
    /// has replaced the database; ids are only unique within one generation, so
    /// without this check a late write could attach the reference to a
    /// completely unrelated screenshot that reuses the id — and the upsert
    /// would overwrite whatever correct reference that row already had.
    pub fn save_screenshot_document_ref_for_generation(
        &self,
        screenshot_id: i64,
        reference: &OfficeDocumentRef,
        expected_generation: u64,
    ) -> Result<(), String> {
        if screenshot_id <= 0 {
            return Err("Invalid screenshot id for Office document reference".to_string());
        }
        // Cheap pre-check: skip key wrapping and encryption for a write that is
        // already known to be stale. The authoritative check happens below,
        // under the connection lock.
        if self.db_generation() != expected_generation {
            return Err(STALE_DOCUMENT_REF_GENERATION.to_string());
        }
        reference.validate()?;
        let plaintext = serde_json::to_vec(reference)
            .map_err(|error| format!("Failed to serialize Office document reference: {error}"))?;
        let (ref_enc, content_key_encrypted) = self.encrypt_payload_with_row_key(&plaintext)?;

        let guard = self.get_connection_named("save_screenshot_document_ref")?;
        // Holding the connection lock, a concurrent `shutdown`/`initialize`
        // cannot bump the generation between this check and the statement.
        if self.db_generation() != expected_generation {
            return Err(STALE_DOCUMENT_REF_GENERATION.to_string());
        }
        let conn = guard.as_ref().unwrap();
        conn.execute(
            "INSERT INTO screenshot_document_refs (
                screenshot_id, ref_enc, content_key_encrypted, updated_at
             ) VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP)
             ON CONFLICT(screenshot_id) DO UPDATE SET
                ref_enc = excluded.ref_enc,
                content_key_encrypted = excluded.content_key_encrypted,
                updated_at = CURRENT_TIMESTAMP",
            params![screenshot_id, ref_enc, content_key_encrypted],
        )
        .map_err(|error| format!("Failed to save Office document reference: {error}"))?;
        Ok(())
    }

    /// Read and validate the Office reference after the caller has authenticated.
    pub fn get_screenshot_document_ref(
        &self,
        screenshot_id: i64,
    ) -> Result<Option<OfficeDocumentRef>, String> {
        if screenshot_id <= 0 {
            return Ok(None);
        }
        let encrypted = {
            let guard = self.get_connection_named("get_screenshot_document_ref")?;
            let conn = guard.as_ref().unwrap();
            conn.query_row(
                "SELECT ref_enc, content_key_encrypted
                   FROM screenshot_document_refs
                  WHERE screenshot_id = ?1",
                params![screenshot_id],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()
            .map_err(|error| format!("Failed to read Office document reference: {error}"))?
        };
        let Some((ref_enc, content_key_encrypted)) = encrypted else {
            return Ok(None);
        };

        let session = CngKeySession::open_silent()
            .map_err(|error| format!("Failed to open Office document key session: {error}"))?;
        let plaintext =
            Self::decrypt_payload_with_unwrap(&ref_enc, &content_key_encrypted, &|ciphertext| {
                session.unwrap_row_key(ciphertext)
            })
            .map_err(|error| format!("Failed to decrypt Office document reference: {error}"))?;
        let reference: OfficeDocumentRef = serde_json::from_slice(&plaintext)
            .map_err(|error| format!("Failed to decode Office document reference: {error}"))?;
        reference.validate()?;
        Ok(Some(reference))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_manager::CredentialManagerState;
    use crate::office_protocol::{OfficeApplication, OfficeDocumentKind};
    use rusqlite::Connection;
    use std::sync::Arc;

    fn test_storage() -> (tempfile::TempDir, StorageState) {
        let temp = tempfile::tempdir().expect("temp storage directory");
        let credential = Arc::new(CredentialManagerState::new(temp.path().to_path_buf()));
        let storage = StorageState::new(temp.path().to_path_buf(), credential);
        let connection = Connection::open_in_memory().expect("in-memory database");
        storage.init_tables(&connection).expect("initialize schema");
        *storage.db.lock().unwrap_or_else(|error| error.into_inner()) = Some(connection);
        (temp, storage)
    }

    fn document() -> OfficeDocumentRef {
        OfficeDocumentRef {
            provider: "office_nativeom".to_string(),
            application: OfficeApplication::Word,
            kind: OfficeDocumentKind::LocalFile,
            display_name: "Report.docx".to_string(),
            locator: Some("D:\\docs\\Report.docx".to_string()),
            observed_at_ms: 1,
            confidence: "exact".to_string(),
        }
    }

    #[test]
    fn closing_the_database_starts_a_new_generation() {
        let (_temp, storage) = test_storage();
        let generation = storage.db_generation();
        storage.shutdown().expect("close storage");
        assert_ne!(
            storage.db_generation(),
            generation,
            "ids collected before the close must not look current afterwards"
        );
    }

    #[test]
    fn a_reference_collected_against_a_previous_database_is_refused() {
        let (_temp, storage) = test_storage();
        let generation = storage.db_generation();
        storage.shutdown().expect("close storage");

        let error = storage
            .save_screenshot_document_ref_for_generation(7, &document(), generation)
            .expect_err("a write from the previous database must not be attempted");
        assert_eq!(error, STALE_DOCUMENT_REF_GENERATION);
    }
}
