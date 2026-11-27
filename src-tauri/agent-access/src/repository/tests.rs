use super::*;
fn binding(path: &Path) -> AgentViewBinding {
    AgentViewBinding {
        dataset_id: "a".repeat(64),
        generation: "b".repeat(64),
        view_path: path.to_string_lossy().into_owned(),
    }
}
#[test]
fn shared_links_and_retired_snapshot_ranges_follow_revision_fences() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-memory.db");
    let binding = binding(&path);
    let mut repo = AgentRepository::open(&path, &[7; 32], &binding, false).unwrap();
    let event = |revision, change| ExportEvent {
        dataset_id: binding.dataset_id.clone(),
        generation: binding.generation.clone(),
        revision,
        change,
    };
    let put = |id| TextChange::Put {
        record: TextRecord::Snapshot(SnapshotText {
            screenshot_id: id,
            process_name: Some("fixture.exe".into()),
            window_title: Some("shared fixture".into()),
            visible_links: vec![TextLink {
                text: "synthetic label".repeat(150),
                url: "https://example.test/".into(),
            }],
            ..Default::default()
        }),
    };
    for id in [40, 42, 41] {
        repo.apply(&event(1, put(id))).unwrap();
    }
    let (n, kind): (i64, String) = repo
        .conn
        .query_row(
            "SELECT count(*),typeof(data) FROM snapshot_links",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((n, kind), (1, "blob".into()));
    assert_eq!(
        repo.conn
            .query_row("SELECT count(*) FROM snapshot_contexts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        repo.snapshot(41).unwrap().unwrap().process_name.as_deref(),
        Some("fixture.exe")
    );
    assert_eq!(
        repo.snapshot(41).unwrap().unwrap().visible_links[0]
            .text
            .len(),
        15 * 150
    );
    for id in [40, 42, 41] {
        repo.apply(&event(
            3,
            TextChange::Delete {
                kind: "snapshot".into(),
                key: id.to_string(),
            },
        ))
        .unwrap();
    }
    repo.seal_revision(3).unwrap();
    repo.reclaim_batch().unwrap();
    assert_eq!(
        repo.conn
            .query_row("SELECT count(*) FROM snapshot_links", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        repo.conn
            .query_row(
                "SELECT first,last FROM retired_snapshot_ranges",
                [],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            )
            .unwrap(),
        (40, 42)
    );
    repo.apply(&event(2, put(41))).unwrap();
    assert!(repo.snapshot_retired(41).unwrap());
    assert!(repo.snapshot(41).unwrap().is_none());
    repo.apply(&event(4, put(41))).unwrap();
    assert!(!repo.snapshot_retired(41).unwrap());
    assert!(repo.snapshot_retired(42).unwrap());
    assert_eq!(
        repo.conn
            .query_row("SELECT count(*) FROM snapshot_contexts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn idle_cleanup_steps_a_full_bounded_vacuum_batch() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("agent-memory.db");
    let mut repo = AgentRepository::open(&path, &[7; 32], &binding(&path), false).unwrap();
    repo.conn
        .execute_batch(
            "CREATE TABLE scratch(data BLOB);
        WITH RECURSIVE rows(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM rows WHERE i<512)
        INSERT INTO scratch SELECT zeroblob(3000) FROM rows; DELETE FROM scratch;",
        )
        .unwrap();
    let pages = |conn: &Connection| {
        conn.query_row("PRAGMA freelist_count", [], |r| r.get::<_, i64>(0))
            .unwrap()
    };
    let before = pages(&repo.conn);
    assert!(before > 256);
    assert!(repo.reclaim_batch().unwrap());
    assert_eq!(before - pages(&repo.conn), 64);
}

#[test]
fn earlier_view_formats_are_rejected_without_rewriting_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent-memory.db");
    let binding = binding(&path);
    let key = [7u8; 32];
    for format in [None, Some("2")] {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA key=\"x'{}'\";
             CREATE TABLE IF NOT EXISTS agent_metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);",
            hex::encode(key)
        ))
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO agent_metadata VALUES('binding',?1)",
            [serde_json::to_string(&binding).unwrap()],
        )
        .unwrap();
        if let Some(format) = format {
            conn.execute("INSERT INTO agent_metadata VALUES('format',?1)", [format])
                .unwrap();
        }
        drop(conn);

        let before = std::fs::read(&path).unwrap();
        for read_only in [true, false] {
            assert!(matches!(
                AgentRepository::open(&path, &key, &binding, read_only),
                Err(error) if error.starts_with("AGENT_STORAGE_FORMAT_UNSUPPORTED")
            ));
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn projection_is_independently_encrypted_and_tombstones_fence_late_callbacks() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("agent-memory.db");
    let binding = binding(&path);
    let mut repo = AgentRepository::open(&path, &[7; 32], &binding, false).unwrap();
    let put = |revision| ExportEvent {
        dataset_id: binding.dataset_id.clone(),
        generation: binding.generation.clone(),
        revision,
        change: TextChange::Put {
            record: TextRecord::Snapshot(SnapshotText {
                screenshot_id: 42,
                window_title: Some("private history fixture".into()),
                ..Default::default()
            }),
        },
    };
    repo.apply(&put(1)).unwrap();
    repo.apply(&ExportEvent {
        change: TextChange::Delete {
            kind: "snapshot".into(),
            key: "42".into(),
        },
        ..put(3)
    })
    .unwrap();
    repo.apply(&put(2)).unwrap();
    assert!(repo.snapshot(42).unwrap().is_none());
    drop(repo);
    assert!(AgentRepository::open(&path, &[8; 32], &binding, true).is_err());
    let image = aes_gcm::Aes256Gcm::new_from_slice(&[9; 32])
        .unwrap()
        .encrypt(Nonce::from_slice(&[0; 12]), b"image fixture".as_ref())
        .unwrap();
    assert!(aes_gcm::Aes256Gcm::new_from_slice(&[7; 32])
        .unwrap()
        .decrypt(Nonce::from_slice(&[0; 12]), image.as_ref())
        .is_err());
    assert!(!std::fs::read(&path)
        .unwrap()
        .windows(23)
        .any(|v| v == b"private history fixture"));
}
#[test]
fn outbox_batches_roll_back_together_and_preserve_tombstones_and_checkpoints() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("agent-memory.db");
    let binding = binding(&path);
    let mut repo = AgentRepository::open(&path, &[7; 32], &binding, false).unwrap();
    let put = |revision| ExportEvent {
        dataset_id: binding.dataset_id.clone(),
        generation: binding.generation.clone(),
        revision,
        change: TextChange::Put {
            record: TextRecord::Snapshot(SnapshotText {
                screenshot_id: 42,
                ..Default::default()
            }),
        },
    };
    let mut invalid = put(2);
    invalid.generation = "c".repeat(64);
    assert!(repo.apply_outbox_batch(&[put(1), invalid], 17).is_err());
    assert!(repo.snapshot(42).unwrap().is_none());
    assert_eq!(repo.checkpoint().unwrap(), 0);

    let delete = ExportEvent {
        change: TextChange::Delete {
            kind: "snapshot".into(),
            key: "42".into(),
        },
        ..put(3)
    };
    repo.apply_outbox_batch(&[put(1), delete, put(2)], 17)
        .unwrap();
    assert!(repo.snapshot(42).unwrap().is_none());
    assert_eq!(repo.checkpoint().unwrap(), 17);
    repo.apply_outbox_batch(&[put(2)], 16).unwrap();
    assert!(repo.snapshot(42).unwrap().is_none());
    assert_eq!(repo.checkpoint().unwrap(), 17);
}

#[test]
fn outbox_cannot_cross_dataset_or_generation_and_never_contains_plaintext() {
    let binding = binding(Path::new("fixture"));
    let event = ExportEvent {
        dataset_id: binding.dataset_id.clone(),
        generation: binding.generation.clone(),
        revision: 1,
        change: TextChange::Put {
            record: TextRecord::Snapshot(SnapshotText {
                window_title: Some("private OCR fixture".into()),
                ..Default::default()
            }),
        },
    };
    let bytes = seal_event(&[7; 32], &event).unwrap();
    assert!(!bytes.windows(19).any(|v| v == b"private OCR fixture"));
    assert!(matches!(
        open_event(&[7; 32], &binding, &bytes).unwrap().change,
        TextChange::Put { .. }
    ));
    let mut other = binding;
    other.generation = "c".repeat(64);
    assert!(open_event(&[7; 32], &other, &bytes).is_err());
}

#[test]
fn preparation_pages_and_resume_cursors_commit_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("agent-memory.db");
    let binding = binding(&path);
    let mut repo = AgentRepository::open(&path, &[7; 32], &binding, false).unwrap();
    repo.apply_preparation_batch(&[], "initial cursor").unwrap();
    let event = ExportEvent {
        dataset_id: binding.dataset_id.clone(),
        generation: binding.generation.clone(),
        revision: 1,
        change: TextChange::Put {
            record: TextRecord::Snapshot(SnapshotText {
                screenshot_id: 42,
                ..Default::default()
            }),
        },
    };
    let invalid = ExportEvent {
        generation: "c".repeat(64),
        ..event.clone()
    };
    assert!(repo
        .apply_preparation_batch(&[event.clone(), invalid], "failed page")
        .is_err());
    assert!(repo.snapshot(42).unwrap().is_none());
    assert_eq!(
        repo.preparation_checkpoint().unwrap().as_deref(),
        Some("initial cursor")
    );
    repo.apply_preparation_batch(&[event], "committed page")
        .unwrap();
    drop(repo);
    let repo = AgentRepository::open(&path, &[7; 32], &binding, true).unwrap();
    assert!(repo.snapshot(42).unwrap().is_some());
    assert_eq!(
        repo.preparation_checkpoint().unwrap().as_deref(),
        Some("committed page")
    );
    assert_eq!(repo.checkpoint().unwrap(), 0);
}
#[test]
fn stale_deletes_and_gaps_cannot_damage_newer_rows_or_revive_deleted_children() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("agent-memory.db");
    let binding = binding(&path);
    let mut repo = AgentRepository::open(&path, &[7; 32], &binding, false).unwrap();
    crate::test_source::configure(&mut repo, &binding);
    let event = |revision, change| ExportEvent {
        dataset_id: binding.dataset_id.clone(),
        generation: binding.generation.clone(),
        revision,
        change,
    };
    let snapshot = TextRecord::Snapshot(SnapshotText {
        screenshot_id: 42,
        ..Default::default()
    });
    let ocr = crate::test_source::ocr(
        &binding,
        OcrText {
            ocr_id: 43,
            screenshot_id: 42,
            text: "current searchable text".into(),
            ..Default::default()
        },
    );
    repo.apply(&event(10, TextChange::Put { record: snapshot }))
        .unwrap();
    repo.apply(&event(
        11,
        TextChange::Put {
            record: ocr.clone(),
        },
    ))
    .unwrap();
    repo.apply(&event(
        5,
        TextChange::Delete {
            kind: "snapshot".into(),
            key: "42".into(),
        },
    ))
    .unwrap();
    repo.apply(&event(
        6,
        TextChange::Gap {
            kind: "ocr".into(),
            key: "43".into(),
        },
    ))
    .unwrap();
    assert!(repo.snapshot(42).unwrap().is_some());
    assert_eq!(repo.ocr(42).unwrap().len(), 1);
    assert!(!repo.has_gaps().unwrap());
    repo.apply(&event(
        12,
        TextChange::Delete {
            kind: "snapshot".into(),
            key: "42".into(),
        },
    ))
    .unwrap();
    repo.apply(&event(13, TextChange::Put { record: ocr }))
        .unwrap();
    repo.apply(&event(
        14,
        TextChange::Put {
            record: crate::test_source::ocr(
                &binding,
                OcrText {
                    ocr_id: 44,
                    screenshot_id: 42,
                    text: "late new OCR row".into(),
                    ..Default::default()
                },
            ),
        },
    ))
    .unwrap();
    repo.apply(&event(
        15,
        TextChange::Put {
            record: crate::test_source::vector(
                &binding,
                VectorText {
                    kind: "clip_image".into(),
                    screenshot_id: 42,
                    model_id: "fixture".into(),
                    model_revision: "fixture".into(),
                    vector: vec![1.0],
                },
            ),
        },
    ))
    .unwrap();
    assert!(repo.ocr(42).unwrap().is_empty());
    assert!(repo.vector_page("clip_image", 0, 256).unwrap().is_empty());
    assert_eq!(
        repo.conn
            .query_row(
                "SELECT COUNT(*) FROM ocr_access WHERE key_envelope IS NOT NULL",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(
        repo.conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('text_terms','text_fts')",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}
