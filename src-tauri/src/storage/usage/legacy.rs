//! Preserve the row-key usage format without decrypting it during startup.
//! Authenticated day reads hydrate only the selected day's application rows.
use super::{StorageState, UsageAppIdentity, UsageIdentity, UsageSpan, UsageState};
use crate::credential_manager::{decrypt_with_master_key, unwrap_storage_domain_key};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use zeroize::Zeroizing;

const TABLE: &str = "usage_intervals_legacy";

pub(super) fn preserve_schema(conn: &Connection) -> Result<(), String> {
    let mut query = conn
        .prepare("SELECT name FROM pragma_table_info('usage_intervals')")
        .map_err(|e| e.to_string())?;
    let columns = query
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .collect::<Result<HashSet<_>, _>>()
        .map_err(|e| e.to_string())?;
    drop(query);
    if columns.is_empty()
        || [
            "day_id",
            "run_id",
            "sequence",
            "start_ms",
            "end_ms",
            "state",
            "context_id",
        ]
        .iter()
        .all(|column| columns.contains(*column))
    {
        return Ok(());
    }
    if ![
        "id",
        "run_id",
        "sequence",
        "start_ms",
        "end_ms",
        "process_name",
        "identity_enc",
        "identity_key_enc",
        "input_active",
        "media_active",
        "end_reason",
        "is_deleted",
    ]
    .iter()
    .all(|column| columns.contains(*column))
        || columns.contains("day_id")
    {
        return Err("Unsupported usage interval schema; existing records were preserved".into());
    }
    if StorageState::table_exists(conn, TABLE)? {
        return Err(
            "Legacy usage interval table already exists; existing records were preserved".into(),
        );
    }
    conn.execute_batch("ALTER TABLE usage_intervals RENAME TO usage_intervals_legacy;")
        .map_err(|e| format!("Failed to preserve legacy usage intervals: {e}"))
}

pub(super) struct Row {
    pub id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub state: UsageState,
    pub process_name: String,
}

impl Row {
    pub(super) fn span(&self, identity: Option<Arc<UsageIdentity>>) -> UsageSpan {
        UsageSpan {
            // Native recorder run IDs are nonnegative. A legacy row's primary
            // key identifies the same span on either side of midnight.
            run_id: i64::MIN,
            sequence: self.id,
            start_ms: self.start_ms,
            end_ms: self.end_ms,
            state: self.state,
            identity,
        }
    }
}

pub(super) fn rows(conn: &Connection, start_ms: i64, end_ms: i64) -> Result<Vec<Row>, String> {
    if !StorageState::table_exists(conn, TABLE)? {
        return Ok(vec![]);
    }
    // The old time/end indexes survive the rename. This projection contains
    // no private payloads or row keys, including for the seven-day totals.
    let mut query = conn
        .prepare(
            "SELECT id,start_ms,end_ms,input_active,media_active,process_name
             FROM usage_intervals_legacy
             WHERE is_deleted=0 AND end_ms>?1 AND start_ms<?2 AND end_ms>start_ms
             ORDER BY start_ms,id",
        )
        .map_err(|e| e.to_string())?;
    let result = query
        .query_map(params![start_ms, end_ms], |row| {
            let active = row.get::<_, bool>(3)? || row.get::<_, bool>(4)?;
            Ok(Row {
                id: row.get(0)?,
                start_ms: row.get(1)?,
                end_ms: row.get(2)?,
                state: if active {
                    UsageState::Active
                } else {
                    UsageState::Idle
                },
                process_name: row.get(5)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string());
    result
}

pub(super) fn recorded_since(conn: &Connection) -> Result<Option<i64>, String> {
    if !StorageState::table_exists(conn, "usage_tracking")? {
        return Ok(None);
    }
    conn.query_row(
        "SELECT started_ms FROM usage_tracking WHERE singleton=1",
        [],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

#[derive(Deserialize)]
struct Identity {
    #[serde(default)]
    app: Option<UsageAppIdentity>,
    #[serde(default)]
    process_name: Option<String>,
    #[serde(default, alias = "app_name", alias = "display_name")]
    name: Option<String>,
    #[serde(default)]
    process_path: Option<String>,
    #[serde(default, alias = "app_icon")]
    icon: Option<String>,
    #[serde(default, alias = "window_title")]
    title: Option<String>,
    #[serde(default, alias = "page_url")]
    url: Option<String>,
    #[serde(default)]
    category: Option<String>,
}

pub(super) fn identities(
    storage: &StorageState,
    rows: &[&Row],
    generation: u64,
) -> Result<HashMap<i64, Arc<UsageIdentity>>, String> {
    if storage.db_generation() != generation {
        return Err("USAGE_SOURCE_CHANGED".into());
    }
    if rows.is_empty() {
        return Ok(HashMap::new());
    }
    let payloads = storage.with_read_connection_named("usage.legacy.identities", |conn| {
        let mut query = conn
            .prepare("SELECT identity_enc,identity_key_enc FROM usage_intervals_legacy WHERE id=?1 AND is_deleted=0")
            .map_err(|e| e.to_string())?;
        rows.iter()
            .map(|row| {
                let payload = query
                    .query_row([row.id], |record| {
                        Ok((record.get::<_, Vec<u8>>(0)?, record.get::<_, Vec<u8>>(1)?))
                    })
                    .optional()
                    .map_err(|e| e.to_string())?;
                Ok((row.id, row.process_name.clone(), payload))
            })
            .collect::<Result<Vec<_>, String>>()
    })?;
    if storage.db_generation() != generation {
        return Err("USAGE_SOURCE_CHANGED".into());
    }
    // CNG checks the current authentication proof for every unwrap. Both the
    // private-key operations and JSON decoding happen after releasing SQLite.
    let mut identities = HashMap::new();
    for (id, process_name, payload) in payloads {
        let Some((encrypted, wrapped)) = payload else {
            continue;
        };
        let key = Zeroizing::new(
            unwrap_storage_domain_key(&storage.credential_state, &wrapped)
                .map_err(|e| e.to_string())?,
        );
        let bytes =
            Zeroizing::new(decrypt_with_master_key(&key, &encrypted).map_err(|e| e.to_string())?);
        let identity: Identity = serde_json::from_slice(&bytes)
            .map_err(|_| "Invalid legacy usage identity".to_owned())?;
        let app = identity.app.unwrap_or_else(|| UsageAppIdentity {
            process_name: identity
                .process_name
                .unwrap_or_else(|| process_name.clone()),
            name: identity
                .name
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| process_name.clone()),
            process_path: identity.process_path.unwrap_or_default(),
            icon: identity.icon,
        });
        identities.insert(
            id,
            Arc::new(UsageIdentity {
                app: Arc::new(app),
                title: identity.title.unwrap_or_default(),
                url: identity.url,
                category: identity.category.unwrap_or_else(|| "未分类".into()),
            }),
        );
    }
    Ok(identities)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        calendar::CalendarDay,
        credential_manager::{
            encrypt_with_master_key, wrap_storage_domain_key, CredentialManagerState,
        },
        storage::UsageWriter,
        usage::types::LiveUsage,
    };
    use std::path::PathBuf;

    const SCHEMA: &str = "
        CREATE TABLE usage_intervals (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL,
            sequence INTEGER NOT NULL,
            start_ms INTEGER NOT NULL,
            end_ms INTEGER NOT NULL CHECK(end_ms >= start_ms),
            process_name TEXT NOT NULL,
            identity_enc BLOB NOT NULL,
            identity_key_enc BLOB NOT NULL,
            input_active INTEGER NOT NULL,
            media_active INTEGER NOT NULL,
            end_reason TEXT NOT NULL,
            is_deleted INTEGER NOT NULL DEFAULT 0,
            UNIQUE(run_id,sequence)
        );
        CREATE INDEX idx_usage_intervals_time ON usage_intervals(is_deleted,start_ms,id);
        CREATE INDEX idx_usage_intervals_end ON usage_intervals(is_deleted,end_ms,start_ms,id);
        CREATE INDEX idx_usage_intervals_process ON usage_intervals(process_name,is_deleted,start_ms);
        CREATE TABLE usage_tracking (
            singleton INTEGER PRIMARY KEY CHECK(singleton=1),
            started_ms INTEGER NOT NULL,
            policy_version INTEGER NOT NULL DEFAULT 1
        );";

    fn storage() -> StorageState {
        let directory = PathBuf::from("usage-legacy-fixture");
        let credentials = Arc::new(CredentialManagerState::new(directory.clone()));
        StorageState::new(directory, credentials)
    }

    #[test]
    fn startup_preserves_legacy_ciphertext_and_tracking_and_is_idempotent() {
        let storage = storage();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        // These deliberately cannot be decrypted: startup must only move the
        // schema, even when no legacy key has been authorized for reading.
        conn.execute_batch(
            "INSERT INTO usage_intervals VALUES(1,'old-run',0,10,20,'old.exe',X'010203',X'040506',1,0,'stop',0);
             INSERT INTO usage_tracking VALUES(1,10,1);",
        )
        .unwrap();
        storage.init_tables(&conn).unwrap();
        let payload: (Vec<u8>, Vec<u8>, String) = conn
            .query_row(
                "SELECT identity_enc,identity_key_enc,run_id FROM usage_intervals_legacy WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(payload, (vec![1, 2, 3], vec![4, 5, 6], "old-run".into()));
        assert_eq!(recorded_since(&conn).unwrap(), Some(10));
        conn.execute_batch(
            "INSERT INTO usage_days VALUES(1,'2026-11-24','{}');
             INSERT INTO usage_intervals VALUES(1,1,0,20,30,1,NULL);",
        )
        .unwrap();
        storage.init_tables(&conn).unwrap();
        for table in ["usage_intervals", "usage_intervals_legacy"] {
            assert_eq!(
                conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
        assert_eq!(
            conn.query_row(
                "SELECT tbl_name FROM sqlite_master WHERE name='idx_usage_day_time'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "usage_intervals"
        );
        assert_eq!(
            conn.query_row(
                "SELECT tbl_name FROM sqlite_master WHERE name='idx_usage_intervals_end'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            TABLE
        );
    }

    #[test]
    fn replacement_failure_rolls_back_the_legacy_table_rename() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        conn.execute_batch(
            "INSERT INTO usage_intervals VALUES(1,'old-run',0,10,20,'old.exe',X'01',X'02',1,0,'stop',0);
             CREATE TABLE idx_usage_day_time(id INTEGER);",
        )
        .unwrap();
        assert!(super::super::initialize(&conn).is_err());
        assert!(!StorageState::table_exists(&conn, TABLE).unwrap());
        assert!(!StorageState::table_exists(&conn, "usage_apps").unwrap());
        assert_eq!(
            conn.query_row(
                "SELECT identity_enc FROM usage_intervals WHERE id=1",
                [],
                |row| row.get::<_, Vec<u8>>(0)
            )
            .unwrap(),
            vec![1]
        );
    }

    fn insert(conn: &Connection, row: &Row, encrypted: &[u8], wrapped: &[u8], deleted: bool) {
        conn.execute(
            "INSERT INTO usage_intervals_legacy
             (id,run_id,sequence,start_ms,end_ms,process_name,identity_enc,identity_key_enc,input_active,media_active,end_reason,is_deleted)
             VALUES(?1,'old-run',?1,?2,?3,?4,?5,?6,?7,0,'boundary',?8)",
            params![row.id,row.start_ms,row.end_ms,row.process_name,encrypted,wrapped,row.state==UsageState::Active,deleted],
        ).unwrap();
    }

    #[test]
    fn mixed_history_keeps_midnight_totals_and_only_decrypts_requested_details() {
        let storage = storage();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        storage.init_tables(&conn).unwrap();
        let day = CalendarDay::in_zone("2026-11-24", chrono_tz::UTC).unwrap();
        let key = [7; 32];
        let encrypted = encrypt_with_master_key(
            &key,
            br#"{"process_name":"endfield.exe","display_name":"Endfield","process_path":"D:/Games/endfield.exe","window_title":"Legacy title","page_url":"https://private.example/old","category":"gaming"}"#,
        ).unwrap();
        let wrapped = wrap_storage_domain_key(&storage.credential_state, &key).unwrap();
        let crossing = Row {
            id: 1,
            start_ms: day.end_ms - 10_000,
            end_ms: day.end_ms + 10_000,
            state: UsageState::Active,
            process_name: "endfield.exe".into(),
        };
        insert(&conn, &crossing, &encrypted, &wrapped, false);
        let earlier = Row {
            id: 2,
            start_ms: day.start_ms - 60_000,
            end_ms: day.start_ms - 55_000,
            state: UsageState::Active,
            process_name: "earlier.exe".into(),
        };
        insert(&conn, &earlier, b"unreadable", b"unreadable", false);
        conn.execute(
            "UPDATE usage_intervals_legacy SET input_active=0,media_active=1 WHERE id=2",
            [],
        )
        .unwrap();
        let deleted = Row { id: 3, ..crossing };
        insert(&conn, &deleted, b"unreadable", b"unreadable", true);
        conn.execute("INSERT INTO usage_tracking VALUES(1,?1,1)", [day.start_ms])
            .unwrap();
        *storage.db.lock().unwrap() = Some(conn);

        let generation = storage.db_generation();
        let mut writer = UsageWriter::new(generation, chrono_tz::UTC);
        let observed = UsageSpan {
            run_id: 1,
            sequence: 1,
            start_ms: day.end_ms + 10_000,
            end_ms: day.end_ms + 30_000,
            state: UsageState::Active,
            identity: Some(Arc::new(UsageIdentity {
                app: Arc::new(UsageAppIdentity {
                    process_name: "endfield.exe".into(),
                    name: "Endfield".into(),
                    process_path: "D:/Games/endfield.exe".into(),
                    icon: None,
                }),
                title: "New title".into(),
                url: Some("https://private.example/new".into()),
                category: "gaming".into(),
            })),
        };
        writer.write(&storage, &[observed.clone()]).unwrap();
        let live = LiveUsage {
            generation,
            time_zone: Some("UTC".into()),
            spans: vec![observed.clone()],
        };
        let first = storage
            .usage_day("2026-11-24", live.clone(), observed.end_ms)
            .unwrap();
        let second = storage
            .usage_day("2026-11-25", live.clone(), observed.end_ms)
            .unwrap();
        assert_eq!(first.total_ms, 10_000);
        assert_eq!(second.total_ms, 30_000);
        assert_eq!(second.recorded_since_ms, Some(day.start_ms));
        assert_eq!(second.apps[0].name, "Endfield");
        assert_eq!(second.apps[0].visits, 1);
        assert_eq!(
            second
                .week
                .iter()
                .find(|day| day.date == second.date)
                .unwrap()
                .total_ms,
            second.total_ms
        );
        assert_eq!(
            second
                .week
                .iter()
                .find(|day| day.date == "2026-11-23")
                .unwrap()
                .total_ms,
            5_000
        );

        // A different app's unreadable identity must not be decrypted while
        // requesting endfield.exe, but its span still participates in overlap.
        let other = Row {
            id: 4,
            start_ms: day.end_ms + 30_000,
            end_ms: day.end_ms + 40_000,
            state: UsageState::Active,
            process_name: "other.exe".into(),
        };
        {
            let guard = storage.get_connection_named("usage.legacy.test").unwrap();
            insert(
                guard.as_ref().unwrap(),
                &other,
                b"unreadable",
                b"unreadable",
                false,
            );
        }
        let details = storage
            .usage_app_details("2026-11-25", "endfield.exe", live, other.end_ms)
            .unwrap();
        assert_eq!(details.titles[0].label, "New title");
        assert_eq!(details.titles[0].total_ms, 20_000);
        assert_eq!(details.titles[1].label, "Legacy title");
        assert_eq!(details.titles[1].total_ms, 10_000);
        assert_eq!(details.websites[0].total_ms, 30_000);
    }
}
