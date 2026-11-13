use super::{tests::*, *};

fn object(
    ledger: &mut Ledger,
    who: &Principal,
    ds: &str,
    slot: &str,
    sources: &[i64],
    time: i64,
) -> RecapObjectKey {
    let Response::RecapKey(key) = ledger
        .handle(
            who,
            Request::PrepareRecapObject {
                dataset_id: ds.into(),
                slot: slot.into(),
                payload_bytes: 34,
            },
            time,
            &TestProtection,
        )
        .unwrap()
    else {
        panic!()
    };
    ledger
        .handle(
            who,
            Request::AddRecapSources {
                object_id: key.binding.object_id.clone(),
                screenshot_ids: sources.to_vec(),
            },
            time,
            &TestProtection,
        )
        .unwrap();
    ledger
        .handle(
            who,
            Request::ActivateRecapObject {
                object_id: key.binding.object_id.clone(),
                ciphertext_digest: "b".repeat(64),
            },
            time,
            &TestProtection,
        )
        .unwrap();
    key
}

fn acquire_object(
    ledger: &mut Ledger,
    who: &Principal,
    key: &RecapObjectKey,
    time: i64,
) -> Result<Response> {
    ledger.handle(
        who,
        Request::AcquireRecapObject {
            object_id: key.binding.object_id.clone(),
            ciphertext_digest: "b".repeat(64),
        },
        time,
        &TestProtection,
    )
}

#[test]
fn recap_revocation_compaction_stays_bounded_and_never_restores_sources_or_existing_contexts() {
    let (mut ledger, who, dataset) = fixture();
    let context = object(
        &mut ledger,
        &who,
        &dataset,
        "day:2026-01-01:context",
        &[2],
        102,
    );
    let task = prepare(&mut ledger, &who, &dataset, 9, Consumer::MiniLm.bit());
    {
        let tx = ledger.conn.transaction().unwrap();
        for id in (1..=8191).step_by(2) {
            Ledger::revoke_recap_source(&tx, &who.sid, &dataset, id).unwrap();
        }
        tx.commit().unwrap();
    }
    let (exact, ranges, budget): (i64,i64,i64) = ledger.conn.query_row(
        "SELECT (SELECT COUNT(*) FROM recap_revoked_sources),(SELECT COUNT(*) FROM recap_revoked_ranges),
            (SELECT entries FROM recap_revocation_budgets WHERE owner=?1)", [&who.sid], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!((exact, ranges, budget), (0, 1, 1));
    assert!(ledger.requires_current_service().unwrap());
    assert_eq!(
        acquire_object(&mut ledger, &who, &context, 104).unwrap_err(),
        BrokerError::Retired
    );
    let Response::RecapKey(new) = ledger
        .handle(
            &who,
            Request::PrepareRecapObject {
                dataset_id: dataset.clone(),
                slot: "replacement".into(),
                payload_bytes: 34,
            },
            105,
            &TestProtection,
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        ledger
            .handle(
                &who,
                Request::AddRecapSources {
                    object_id: new.binding.object_id.clone(),
                    screenshot_ids: vec![1]
                },
                106,
                &TestProtection
            )
            .unwrap_err(),
        BrokerError::Retired
    );
    // Gap closure also fences a restored context without affecting task peers.
    assert_eq!(
        ledger
            .handle(
                &who,
                Request::AddRecapSources {
                    object_id: new.binding.object_id.clone(),
                    screenshot_ids: vec![2]
                },
                107,
                &TestProtection
            )
            .unwrap_err(),
        BrokerError::Retired
    );
    assert!(acquire(&mut ledger, &who, &task.task.task_id, Consumer::MiniLm, 108).is_ok());
    ledger.maintenance(100 + 100 * DAY_SECS).unwrap();
    assert_eq!(
        ledger
            .handle(
                &who,
                Request::AddRecapSources {
                    object_id: new.binding.object_id,
                    screenshot_ids: vec![1]
                },
                100 + 100 * DAY_SECS,
                &TestProtection
            )
            .unwrap_err(),
        BrokerError::Retired
    );
    let range_count: i64 = ledger
        .conn
        .query_row("SELECT COUNT(*) FROM recap_revoked_ranges", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(range_count, 1);
    // Persist the actual compacted state. Original services reject its schema,
    // while this service reopens it without losing the fence during uninstall.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("keys.db");
    ledger
        .conn
        .execute("VACUUM INTO ?1", [path.to_str().unwrap()])
        .unwrap();
    let mut reopened = Ledger::open_existing(&path).unwrap();
    assert_eq!(
        reopened
            .conn
            .query_row(
                "SELECT value FROM broker_metadata WHERE key='schema'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        2
    );
    assert!(reopened.requires_current_service().unwrap());
    reopened.unregister_owner(&who.sid).unwrap();
    assert!(reopened.requires_current_service().unwrap());
}

#[test]
fn a_saturated_revocation_budget_keeps_deletion_available_and_preserves_its_fail_closed_fence() {
    let (mut ledger, who, dataset) = fixture();
    let key = object(&mut ledger, &who, &dataset, "old-context", &[3], 102);
    ledger.conn.execute("INSERT OR REPLACE INTO recap_revocation_budgets(owner,entries,deny_all) VALUES(?1,100000,0)", [&who.sid]).unwrap();
    ledger
        .handle(
            &who,
            Request::RevokeScreenshots {
                dataset_id: dataset.clone(),
                screenshot_ids: vec![2],
            },
            103,
            &TestProtection,
        )
        .unwrap();
    assert_eq!(
        acquire_object(&mut ledger, &who, &key, 104).unwrap_err(),
        BrokerError::Retired
    );
    ledger
        .handle(
            &who,
            Request::RevokeScreenshots {
                dataset_id: dataset.clone(),
                screenshot_ids: vec![4],
            },
            105,
            &TestProtection,
        )
        .unwrap();
    let (entries, fenced): (i64, bool) = ledger
        .conn
        .query_row(
            "SELECT entries,deny_all FROM recap_revocation_budgets WHERE owner=?1",
            [&who.sid],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((entries, fenced), (0, true));
    assert!(ledger.requires_current_service().unwrap());
    ledger
        .register_owner(&who.sid, "updated-release", false)
        .unwrap();
    let mut updated = who.clone();
    updated.runtime_id = "updated-release".into();
    let Response::RecapKey(new) = ledger
        .handle(
            &updated,
            Request::PrepareRecapObject {
                dataset_id: dataset,
                slot: "new-context".into(),
                payload_bytes: 34,
            },
            106,
            &TestProtection,
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        ledger
            .handle(
                &updated,
                Request::AddRecapSources {
                    object_id: new.binding.object_id,
                    screenshot_ids: vec![3]
                },
                107,
                &TestProtection
            )
            .unwrap_err(),
        BrokerError::Retired
    );
}

#[test]
fn recap_dependencies_are_immutable_and_deleted_sources_cannot_be_regranted() {
    let (mut ledger, who, ds) = fixture();
    let key = object(
        &mut ledger,
        &who,
        &ds,
        "day:2026-01-01:context",
        &[1, 2],
        102,
    );
    assert!(acquire_object(&mut ledger, &who, &key, 103).is_ok());
    assert_eq!(
        ledger
            .handle(
                &who,
                Request::AddRecapSources {
                    object_id: key.binding.object_id.clone(),
                    screenshot_ids: vec![3]
                },
                103,
                &TestProtection
            )
            .unwrap_err(),
        BrokerError::Retired
    );
    ledger
        .handle(
            &who,
            Request::RevokeScreenshots {
                dataset_id: ds.clone(),
                screenshot_ids: vec![2],
            },
            104,
            &TestProtection,
        )
        .unwrap();
    assert_eq!(
        acquire_object(&mut ledger, &who, &key, 105).unwrap_err(),
        BrokerError::Retired
    );
    let Response::RecapKey(new) = ledger
        .handle(
            &who,
            Request::PrepareRecapObject {
                dataset_id: ds,
                slot: "replacement".into(),
                payload_bytes: 34,
            },
            106,
            &TestProtection,
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        ledger
            .handle(
                &who,
                Request::AddRecapSources {
                    object_id: new.binding.object_id,
                    screenshot_ids: vec![2]
                },
                107,
                &TestProtection
            )
            .unwrap_err(),
        BrokerError::Retired
    );
}

#[test]
fn recap_replacement_retention_disable_and_day_deletion_never_restore_old_keys() {
    let (mut ledger, who, ds) = fixture();
    let old = object(&mut ledger, &who, &ds, "day:2026-01-01:context", &[1], 102);
    let new = object(&mut ledger, &who, &ds, "day:2026-01-01:context", &[1], 103);
    assert_eq!(
        acquire_object(&mut ledger, &who, &old, 104).unwrap_err(),
        BrokerError::Retired
    );
    assert!(acquire_object(&mut ledger, &who, &new, 104).is_ok());
    let config = object(&mut ledger, &who, &ds, "config", &[], 105);
    ledger
        .handle(
            &who,
            Request::SetPolicy {
                enabled: true,
                limits: Limits {
                    retention_days: 1,
                    ..Limits::default()
                },
            },
            106,
            &TestProtection,
        )
        .unwrap();
    assert_eq!(
        acquire_object(&mut ledger, &who, &new, 103 + DAY_SECS).unwrap_err(),
        BrokerError::Retired
    );
    assert!(acquire_object(&mut ledger, &who, &config, 103 + DAY_SECS).is_ok());
    let day = object(
        &mut ledger,
        &who,
        &ds,
        "day:2026-01-02:context",
        &[3],
        104 + DAY_SECS,
    );
    ledger
        .handle(
            &who,
            Request::RevokeRecapDay {
                date: "2026-01-02".into(),
            },
            105 + DAY_SECS,
            &TestProtection,
        )
        .unwrap();
    assert_eq!(
        acquire_object(&mut ledger, &who, &day, 106 + DAY_SECS).unwrap_err(),
        BrokerError::Retired
    );
    ledger
        .handle(
            &who,
            Request::SetPolicy {
                enabled: false,
                limits: Limits::default(),
            },
            107 + DAY_SECS,
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
            108 + DAY_SECS,
            &TestProtection,
        )
        .unwrap();
    assert_eq!(
        acquire_object(&mut ledger, &who, &config, 109 + DAY_SECS).unwrap_err(),
        BrokerError::Retired
    );
}

#[test]
fn finishing_recap_does_not_retire_pending_other_consumers_or_derived_objects() {
    let (mut ledger, who, ds) = fixture();
    let task = prepare(&mut ledger, &who, &ds, 1, Consumer::ALL_MASK);
    let context = object(&mut ledger, &who, &ds, "context", &[1], 102);
    let lease = acquire(&mut ledger, &who, &task.task.task_id, Consumer::Recap, 103).unwrap();
    finish(&mut ledger, &who, &lease, 104).unwrap();
    for consumer in [
        Consumer::Classification,
        Consumer::MiniLm,
        Consumer::Clip,
        Consumer::SmartCluster,
    ] {
        let lease = acquire(&mut ledger, &who, &task.task.task_id, consumer, 105).unwrap();
        finish(&mut ledger, &who, &lease, 106).unwrap();
    }
    assert!(acquire_object(&mut ledger, &who, &context, 107).is_ok());
}
