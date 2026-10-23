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
