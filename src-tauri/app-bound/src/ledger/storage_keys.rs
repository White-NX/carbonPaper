//! Durable, explicit derived-data grants. This does not expose a generic unwrap
//! operation: callers must name an immutable key registered by a verified runtime.
use super::*;
use sha2::Digest;

type StoredStorageKeyRow = (String, Option<Vec<u8>>, Option<String>, bool);

impl Ledger {
    pub(super) fn init_storage_keys(conn: &Connection) -> Result<()> {
        db(conn.execute_batch("CREATE TABLE IF NOT EXISTS storage_public_keys (
                owner TEXT NOT NULL REFERENCES owners(sid),dataset TEXT NOT NULL,fingerprint TEXT NOT NULL,content_dataset TEXT NOT NULL,
                PRIMARY KEY(owner,dataset));
            CREATE TABLE IF NOT EXISTS storage_keys (
                owner TEXT NOT NULL REFERENCES owners(sid),id TEXT NOT NULL,dataset TEXT NOT NULL,
                binding TEXT NOT NULL,key_blob BLOB,key_digest TEXT,
                write_origin INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(owner,dataset,id));"))?;
        for (column, definition) in [
            ("key_digest", "TEXT"),
            ("write_origin", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            let exists: bool = db(conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('storage_keys') WHERE name=?1)",
                [column],
                |r| r.get(0),
            ))?;
            if !exists {
                db(conn.execute(
                    &format!("ALTER TABLE storage_keys ADD COLUMN {column} {definition}"),
                    [],
                ))?;
            }
        }
        let has_content: bool = db(conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('storage_public_keys') WHERE name='content_dataset')", [], |r|r.get(0)))?;
        if !has_content {
            db(conn.execute_batch("ALTER TABLE storage_public_keys ADD COLUMN content_dataset TEXT NOT NULL DEFAULT '';
                UPDATE storage_public_keys SET content_dataset=dataset;"))?;
        }
        let scoped: bool = db(conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('storage_keys') WHERE name='dataset' AND pk>0)", [], |r|r.get(0)))?;
        if !scoped {
            db(conn.execute_batch("SAVEPOINT storage_key_scope;
                ALTER TABLE storage_keys RENAME TO storage_keys_previous;
                CREATE TABLE storage_keys(owner TEXT NOT NULL REFERENCES owners(sid),id TEXT NOT NULL,dataset TEXT NOT NULL,
                    binding TEXT NOT NULL,key_blob BLOB,key_digest TEXT,write_origin INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY(owner,dataset,id));
                INSERT INTO storage_keys SELECT owner,id,dataset,binding,key_blob,key_digest,write_origin FROM storage_keys_previous;
                DROP TABLE storage_keys_previous;
                RELEASE storage_key_scope;"))?;
        }
        db(conn.execute("UPDATE storage_keys SET binding=json_set(binding,'$.content_dataset_id',dataset) WHERE json_type(binding,'$.content_dataset_id') IS NULL", []))?;
        Ok(())
    }

    pub(super) fn dispatch_storage_keys(
        conn: &Connection,
        who: &Principal,
        request: &Request,
        enabled: bool,
        dataset: Option<&str>,
        protector: &dyn KeyProtector,
    ) -> Result<Option<Response>> {
        let binding = match request {
            Request::RegisterStorageKey { binding, .. }
            | Request::AcquireStorageKey { binding } => binding,
            _ => return Ok(None),
        };
        if !enabled {
            return Err(BrokerError::Disabled);
        }
        if dataset != Some(&binding.dataset_id) {
            return Err(BrokerError::DatasetMismatch);
        }
        if binding.key_id.len() != 32
            || !binding
                .key_id
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
            || !valid_id(&binding.dataset_id)
            || !valid_id(&binding.content_dataset_id)
            || !valid_id(&binding.public_key_fingerprint)
            || binding.public_key_fingerprint.len() != 64
            || !valid_id(&binding.wrapped_key_digest)
            || binding.wrapped_key_digest.len() != 64
            || !matches!(binding.purpose.as_str(), "metadata" | "vectors" | "ann")
        {
            return Err(BrokerError::InvalidRequest);
        }
        let fingerprint: Option<(String,String)> = db(conn
            .query_row(
                "SELECT fingerprint,content_dataset FROM storage_public_keys WHERE owner=?1 AND dataset=?2",
                params![who.sid, binding.dataset_id],
                |row| Ok((row.get(0)?,row.get(1)?)),
            )
            .optional())?;
        if fingerprint.as_ref().is_some_and(|(fingerprint, content)| {
            fingerprint != &binding.public_key_fingerprint || content != &binding.content_dataset_id
        }) {
            return Err(BrokerError::Integrity);
        }
        let stored: Option<StoredStorageKeyRow> = db(conn
            .query_row(
                "SELECT binding,key_blob,key_digest,write_origin FROM storage_keys WHERE owner=?1 AND id=?2 AND dataset=?3",
                params![who.sid, binding.key_id, binding.dataset_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional())?;
        match request {
            Request::RegisterStorageKey {
                key, write_origin, ..
            } => {
                if key.0.len() != 32 {
                    return Err(BrokerError::InvalidRequest);
                }
                let key_digest = hex::encode(sha2::Sha256::digest(&key.0));
                let mut origin = *write_origin;
                if let Some((json, sealed, digest, original_origin)) = stored {
                    let original: StorageKeyBinding =
                        serde_json::from_str(&json).map_err(|_| BrokerError::Integrity)?;
                    if original != *binding || (*write_origin && !original_origin) {
                        return Err(BrokerError::Integrity);
                    }
                    origin = original_origin;
                    if let Some(sealed) = sealed {
                        let original_key = protector.unprotect(&sealed)?;
                        if original_key.0 != key.0 {
                            return Err(BrokerError::Integrity);
                        }
                        return Ok(Some(Response::Ok));
                    }
                    // Public-only writers cannot revive a revoked grant. An
                    // authenticated read may restore exactly the original key;
                    // the protected digest prevents substitution after erasure.
                    if *write_origin {
                        return Err(BrokerError::Retired);
                    }
                    if digest.as_deref() != Some(&key_digest) {
                        return Err(BrokerError::Integrity);
                    }
                }
                let count: i64 = db(conn.query_row(
                    "SELECT COUNT(*) FROM storage_keys WHERE owner=?1",
                    [&who.sid],
                    |r| r.get(0),
                ))?;
                if count >= 100_000 {
                    return Err(BrokerError::LimitExceeded);
                }
                let sealed = protector.protect(key)?;
                db(conn.execute(
                    "INSERT OR IGNORE INTO storage_public_keys(owner,dataset,fingerprint,content_dataset) VALUES(?1,?2,?3,?4)",
                    params![who.sid, binding.dataset_id, binding.public_key_fingerprint, binding.content_dataset_id],
                ))?;
                let json = serde_json::to_string(binding).map_err(|_| BrokerError::Integrity)?;
                db(conn.execute(
                    "INSERT INTO storage_keys(owner,id,dataset,binding,key_blob,key_digest,write_origin) VALUES(?1,?2,?3,?4,?5,?6,?7)
                    ON CONFLICT(owner,dataset,id) DO UPDATE SET key_blob=excluded.key_blob",
                    params![who.sid, binding.key_id, binding.dataset_id, json, sealed, key_digest, origin],
                ))?;
                Ok(Some(Response::Ok))
            }
            Request::AcquireStorageKey { .. } => {
                let (json, sealed, _, write_origin) = stored.ok_or(BrokerError::Retired)?;
                let original: StorageKeyBinding =
                    serde_json::from_str(&json).map_err(|_| BrokerError::Integrity)?;
                if original != *binding {
                    return Err(BrokerError::Integrity);
                }
                let key = protector.unprotect(sealed.as_deref().ok_or(BrokerError::Retired)?)?;
                Ok(Some(Response::StorageKey(StorageKeyGrant {
                    binding: original,
                    key,
                    write_origin,
                })))
            }
            _ => unreachable!(),
        }
    }

    pub(super) fn retire_storage_keys(
        conn: &Connection,
        sid: &str,
        dataset: Option<&str>,
    ) -> Result<()> {
        db(conn.execute(
            "UPDATE storage_keys SET key_blob=NULL WHERE owner=?1 AND (?2 IS NULL OR dataset=?2)",
            params![sid, dataset],
        ))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{fixture, TestProtection};
    use super::*;

    fn binding(dataset: &str) -> StorageKeyBinding {
        StorageKeyBinding {
            key_id: "01".repeat(16),
            dataset_id: dataset.into(),
            content_dataset_id: dataset.into(),
            public_key_fingerprint: "a".repeat(64),
            purpose: "vectors".into(),
            wrapped_key_digest: "b".repeat(64),
        }
    }
    fn register(
        ledger: &mut Ledger,
        who: &Principal,
        binding: &StorageKeyBinding,
    ) -> Result<Response> {
        ledger.handle(
            who,
            Request::RegisterStorageKey {
                binding: binding.clone(),
                key: TaskKey(vec![7; 32]),
                write_origin: true,
            },
            101,
            &TestProtection,
        )
    }
    fn acquire(
        ledger: &mut Ledger,
        who: &Principal,
        binding: &StorageKeyBinding,
    ) -> Result<Response> {
        ledger.handle(
            who,
            Request::AcquireStorageKey {
                binding: binding.clone(),
            },
            102,
            &TestProtection,
        )
    }

    #[test]
    fn grants_bind_owner_runtime_dataset_fingerprint_domain_and_wrapped_key() {
        let (mut ledger, who, dataset) = fixture();
        let binding = binding(&dataset);
        assert!(matches!(
            register(&mut ledger, &who, &binding).unwrap(),
            Response::Ok
        ));
        assert!(matches!(
            register(&mut ledger, &who, &binding).unwrap(),
            Response::Ok
        ));
        let Response::StorageKey(grant) = acquire(&mut ledger, &who, &binding).unwrap() else {
            panic!()
        };
        assert_eq!(grant.binding, binding);
        assert_eq!(grant.key.0, vec![7; 32]);
        let protected: Vec<u8> = ledger
            .conn
            .query_row("SELECT key_blob FROM storage_keys", [], |r| r.get(0))
            .unwrap();
        assert_ne!(protected, grant.key.0);
        for field in ["fingerprint", "purpose", "digest"] {
            let mut substituted = binding.clone();
            match field {
                "fingerprint" => substituted.public_key_fingerprint = "c".repeat(64),
                "purpose" => substituted.purpose = "metadata".into(),
                _ => substituted.wrapped_key_digest = "c".repeat(64),
            }
            assert_eq!(
                acquire(&mut ledger, &who, &substituted).unwrap_err(),
                BrokerError::Integrity
            );
            assert_eq!(
                register(&mut ledger, &who, &substituted).unwrap_err(),
                BrokerError::Integrity
            );
        }
        assert_eq!(
            ledger
                .handle(
                    &who,
                    Request::RegisterStorageKey {
                        binding: binding.clone(),
                        key: TaskKey(vec![8; 32]),
                        write_origin: true,
                    },
                    103,
                    &TestProtection
                )
                .unwrap_err(),
            BrokerError::Integrity
        );
        let mut other = who.clone();
        other.runtime_id = "substituted-runtime".into();
        assert_eq!(
            acquire(&mut ledger, &other, &binding).unwrap_err(),
            BrokerError::AccessDenied
        );
        other = who.clone();
        other.sid = "other-user".into();
        assert_eq!(
            acquire(&mut ledger, &other, &binding).unwrap_err(),
            BrokerError::AccessDenied
        );
        let mut other_dataset = binding.clone();
        other_dataset.dataset_id = random_id();
        assert_eq!(
            acquire(&mut ledger, &who, &other_dataset).unwrap_err(),
            BrokerError::DatasetMismatch
        );
    }

    #[test]
    fn disabled_and_retired_grants_cannot_resurrect_and_fingerprint_pins_survive() {
        let (mut ledger, who, dataset) = fixture();
        let original = binding(&dataset);
        register(&mut ledger, &who, &original).unwrap();
        ledger
            .handle(
                &who,
                Request::SetPolicy {
                    enabled: false,
                    limits: Limits::default(),
                },
                103,
                &TestProtection,
            )
            .unwrap();
        assert_eq!(
            acquire(&mut ledger, &who, &original).unwrap_err(),
            BrokerError::Disabled
        );
        ledger
            .handle(
                &who,
                Request::SetPolicy {
                    enabled: true,
                    limits: Limits::default(),
                },
                104,
                &TestProtection,
            )
            .unwrap();
        assert_eq!(
            acquire(&mut ledger, &who, &original).unwrap_err(),
            BrokerError::Retired
        );
        assert_eq!(
            register(&mut ledger, &who, &original).unwrap_err(),
            BrokerError::Retired
        );
        let mut replacement = original.clone();
        replacement.key_id = "02".repeat(16);
        replacement.public_key_fingerprint = "c".repeat(64);
        assert_eq!(
            register(&mut ledger, &who, &replacement).unwrap_err(),
            BrokerError::Integrity
        );
        replacement.public_key_fingerprint = original.public_key_fingerprint.clone();
        register(&mut ledger, &who, &replacement).unwrap();
        ledger
            .handle(
                &who,
                Request::RetireDataset {
                    dataset_id: dataset.clone(),
                },
                105,
                &TestProtection,
            )
            .unwrap();
        ledger
            .handle(
                &who,
                Request::AttachDataset {
                    dataset_id: dataset,
                },
                106,
                &TestProtection,
            )
            .unwrap();
        assert_eq!(
            acquire(&mut ledger, &who, &replacement).unwrap_err(),
            BrokerError::Retired
        );
        ledger.unregister_owner(&who.sid).unwrap();
        assert_eq!(
            ledger
                .conn
                .query_row("SELECT COUNT(*) FROM storage_keys", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            ledger
                .conn
                .query_row("SELECT COUNT(*) FROM storage_public_keys", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn immutable_grants_survive_service_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("isolated-key-ledger.db");
        let who = Principal {
            sid: "fixture-user".into(),
            runtime_id: "fixture-runtime".into(),
            process_identity: "1:1".into(),
        };
        let dataset = random_id();
        let binding = binding(&dataset);
        let mut ledger = Ledger::open(&path).unwrap();
        ledger
            .register_owner(&who.sid, &who.runtime_id, true)
            .unwrap();
        ledger
            .handle(
                &who,
                Request::AttachDataset {
                    dataset_id: dataset,
                },
                100,
                &TestProtection,
            )
            .unwrap();
        register(&mut ledger, &who, &binding).unwrap();
        drop(ledger);
        let mut restarted = Ledger::open(&path).unwrap();
        let Response::StorageKey(grant) = acquire(&mut restarted, &who, &binding).unwrap() else {
            panic!()
        };
        assert_eq!(grant.binding, binding);
        assert_eq!(grant.key.0, vec![7; 32]);
    }

    #[test]
    fn authenticated_read_registration_cannot_promote_an_untrusted_envelope_to_a_write_key() {
        let (mut ledger, who, dataset) = fixture();
        let mut binding = binding(&dataset);
        binding.purpose = "ann".into();
        ledger
            .handle(
                &who,
                Request::RegisterStorageKey {
                    binding: binding.clone(),
                    key: TaskKey(vec![7; 32]),
                    write_origin: false,
                },
                101,
                &TestProtection,
            )
            .unwrap();
        let Response::StorageKey(grant) = acquire(&mut ledger, &who, &binding).unwrap() else {
            panic!()
        };
        assert!(!grant.write_origin);
        assert_eq!(
            register(&mut ledger, &who, &binding).unwrap_err(),
            BrokerError::Integrity
        );
        ledger
            .handle(
                &who,
                Request::SetPolicy {
                    enabled: false,
                    limits: Limits::default(),
                },
                103,
                &TestProtection,
            )
            .unwrap();
        ledger
            .handle(
                &who,
                Request::SetPolicy {
                    enabled: true,
                    limits: Limits::default(),
                },
                104,
                &TestProtection,
            )
            .unwrap();
        assert_eq!(
            ledger
                .handle(
                    &who,
                    Request::RegisterStorageKey {
                        binding: binding.clone(),
                        key: TaskKey(vec![8; 32]),
                        write_origin: false
                    },
                    105,
                    &TestProtection
                )
                .unwrap_err(),
            BrokerError::Integrity
        );
        ledger
            .handle(
                &who,
                Request::RegisterStorageKey {
                    binding: binding.clone(),
                    key: TaskKey(vec![7; 32]),
                    write_origin: false,
                },
                105,
                &TestProtection,
            )
            .unwrap();
        let Response::StorageKey(grant) = acquire(&mut ledger, &who, &binding).unwrap() else {
            panic!()
        };
        assert!(!grant.write_origin);
    }

    #[test]
    fn authorized_recovery_restores_only_a_previously_registered_original_write_key() {
        let (mut ledger, who, dataset) = fixture();
        let binding = binding(&dataset);
        register(&mut ledger, &who, &binding).unwrap();
        ledger
            .handle(
                &who,
                Request::SetPolicy {
                    enabled: false,
                    limits: Limits::default(),
                },
                103,
                &TestProtection,
            )
            .unwrap();
        ledger
            .handle(
                &who,
                Request::SetPolicy {
                    enabled: true,
                    limits: Limits::default(),
                },
                104,
                &TestProtection,
            )
            .unwrap();
        assert_eq!(
            register(&mut ledger, &who, &binding).unwrap_err(),
            BrokerError::Retired
        );
        ledger
            .handle(
                &who,
                Request::RegisterStorageKey {
                    binding: binding.clone(),
                    key: TaskKey(vec![7; 32]),
                    write_origin: false,
                },
                105,
                &TestProtection,
            )
            .unwrap();
        let Response::StorageKey(grant) = acquire(&mut ledger, &who, &binding).unwrap() else {
            panic!()
        };
        assert!(grant.write_origin);
    }
}
