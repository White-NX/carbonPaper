//! Fail closed on writers that cannot understand compact storage references.
//! This function exposes only a format number; it grants no access to content.
use rusqlite::{functions::FunctionFlags, Connection, OptionalExtension};

const VERSION: i64 = 3;
const KEY: &str = "storage_layout_version";

pub(super) fn register_and_check(conn: &Connection) -> rusqlite::Result<()> {
    conn.create_scalar_function(
        "cp_storage_layout_supported",
        1,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        |ctx| Ok((0..=VERSION).contains(&ctx.get::<i64>(0)?)),
    )?;
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='app_metadata')",
        [],
        |r| r.get(0),
    )?;
    if exists {
        let version: Option<String> = conn
            .query_row("SELECT value FROM app_metadata WHERE key=?1", [KEY], |r| {
                r.get(0)
            })
            .optional()?;
        if version.as_ref().is_some_and(|value| {
            value
                .parse::<i64>()
                .map_or(true, |v| !(0..=VERSION).contains(&v))
        }) {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some(
                    "Unsupported storage layout; upgrade CarbonPaper before opening this archive"
                        .into(),
                ),
            ));
        }
    }
    Ok(())
}

pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO app_metadata(key,value) VALUES(?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        (KEY, VERSION.to_string()),
    )
    .map_err(|e| e.to_string())?;
    // Metadata writes stop old startup before it publishes the connection.
    // Source/icon guards also fence an older process that was already open.
    for table in [
        "app_metadata",
        "screenshots",
        "page_icons",
        "derived_embeddings",
        "vector_blocks",
        "ocr_results",
        "ocr_keys",
    ] {
        for event in ["INSERT", "UPDATE", "DELETE"] {
            tx.execute_batch(&format!(
                "DROP TRIGGER IF EXISTS storage_layout_{table}_{event};
                 CREATE TRIGGER storage_layout_{table}_{event} BEFORE {event} ON {table} BEGIN
                     SELECT CASE WHEN cp_storage_layout_supported({VERSION})=0
                         THEN RAISE(ABORT,'Unsupported storage layout; upgrade CarbonPaper') END;
                 END;"
            ))
            .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_writers_are_rejected_even_with_untrusted_schema() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let conn = Connection::open(temp.path()).unwrap();
        conn.execute_batch(
            "PRAGMA trusted_schema=OFF;
            CREATE TABLE app_metadata(key TEXT PRIMARY KEY,value TEXT);
            CREATE TABLE screenshots(id INTEGER PRIMARY KEY);
            CREATE TABLE page_icons(id INTEGER PRIMARY KEY);
            CREATE TABLE derived_embeddings(id INTEGER PRIMARY KEY);
            CREATE TABLE vector_blocks(id INTEGER PRIMARY KEY);
            CREATE TABLE ocr_results(id INTEGER PRIMARY KEY);
            CREATE TABLE ocr_keys(id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        register_and_check(&conn).unwrap();
        initialize(&conn).unwrap();
        conn.execute("INSERT INTO page_icons VALUES(1)", [])
            .unwrap();
        let old = Connection::open(temp.path()).unwrap();
        assert!(old
            .execute(
                "INSERT OR IGNORE INTO app_metadata VALUES('protected_content_dataset_id','test')",
                []
            )
            .is_err());
        assert!(old.execute("DELETE FROM page_icons", []).is_err());
        assert!(old
            .execute("INSERT INTO screenshots VALUES(1)", [])
            .is_err());
        register_and_check(&old).unwrap();
        old.execute("DELETE FROM page_icons", []).unwrap();
        conn.execute("UPDATE app_metadata SET value='999' WHERE key=?1", [KEY])
            .unwrap();
        assert!(register_and_check(&old).is_err());
    }
}
