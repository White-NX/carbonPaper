//! One compact rule/cache row per application, independent of interval tokens.
//! Lookup keys contain no executable names or paths; SQLCipher protects the
//! category columns. Reads never hydrate screenshot or window-title payloads.
use super::{StorageState, UsageAppIdentity, UsageSpan};
use crate::usage::{
    aggregate,
    categories::{self, AppKey},
};
use rusqlite::{params, Connection, OptionalExtension};
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Debug)]
pub(crate) struct StoredCategory {
    pub category: Option<String>,
    pub fingerprint: Vec<u8>,
    pub checked_ms: i64,
}

pub(super) fn initialize(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS usage_category_overrides (
            app_key BLOB PRIMARY KEY CHECK(length(app_key)=32), category TEXT NOT NULL
        ) WITHOUT ROWID;
        CREATE TABLE IF NOT EXISTS usage_category_cache (
            app_key BLOB PRIMARY KEY CHECK(length(app_key)=32), category TEXT,
            fingerprint BLOB NOT NULL, checked_ms INTEGER NOT NULL
        ) WITHOUT ROWID;",
    )
    .map_err(|e| e.to_string())
}

impl StorageState {
    pub(crate) fn read_usage_category(
        &self,
        generation: u64,
        app: &UsageAppIdentity,
    ) -> Result<(Option<String>, Option<StoredCategory>), String> {
        let manual_key = categories::override_key(&app.process_name);
        let app_key = categories::app_key(app);
        self.with_read_connection_named("usage.category", |conn| {
            if generation != self.db_generation() {
                return Err("USAGE_SOURCE_CHANGED".into());
            }
            let manual = conn
                .query_row(
                    "SELECT category FROM usage_category_overrides WHERE app_key=?1",
                    [manual_key.as_slice()],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| e.to_string())?;
            let cached = read_cached(conn, &app_key)?;
            Ok((manual, cached))
        })
    }

    pub(crate) fn save_usage_category_override(
        &self,
        generation: u64,
        app_id: &str,
        category: Option<&str>,
        authorize: impl Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        categories::validate_app_id(app_id)?;
        if let Some(category) = category {
            categories::validate_category(category)?;
        }
        crate::maintenance::guard()?;
        let key = categories::override_key(app_id);
        let guard = self.get_connection_named("usage.category.override")?;
        if generation != self.db_generation() {
            return Err("USAGE_SOURCE_CHANGED".into());
        }
        if self.is_database_maintenance_pending() {
            return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.into());
        }
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        authorize()?;
        match category {
            Some(category) => conn.execute(
                "INSERT INTO usage_category_overrides(app_key,category) VALUES(?1,?2)
                 ON CONFLICT(app_key) DO UPDATE SET category=excluded.category",
                params![key.as_slice(), category],
            ),
            None => conn.execute(
                "DELETE FROM usage_category_overrides WHERE app_key=?1",
                [key.as_slice()],
            ),
        }
        .map_err(|e| e.to_string())?;
        // The cache lock never participates in SQLite/decryption lock ordering.
        drop(guard);
        self.usage_categories
            .manual_changed(generation, app_id, category);
        Ok(())
    }

    pub(crate) fn save_usage_inferred_category(
        &self,
        generation: u64,
        app: &UsageAppIdentity,
        record: &StoredCategory,
    ) -> Result<(), String> {
        if let Some(category) = record.category.as_deref() {
            categories::validate_category(category)?;
        }
        let key = categories::app_key(app);
        // Automatic work must not sit behind an archive write or maintenance.
        let guard = self
            .try_get_connection_named("usage.category.cache")?
            .ok_or("USAGE_WRITE_BUSY")?;
        if generation != self.db_generation() {
            return Err("USAGE_SOURCE_CHANGED".into());
        }
        if crate::maintenance::is_active() || self.is_database_maintenance_pending() {
            return Err(crate::maintenance::MAINTENANCE_IN_PROGRESS.into());
        }
        let conn = guard.as_ref().ok_or("Database not initialized")?;
        conn.execute(
            "INSERT INTO usage_category_cache(app_key,category,fingerprint,checked_ms)
             VALUES(?1,?2,?3,?4) ON CONFLICT(app_key) DO UPDATE SET
             category=excluded.category,fingerprint=excluded.fingerprint,checked_ms=excluded.checked_ms",
            params![key.as_slice(), record.category, record.fingerprint, record.checked_ms],
        ).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub(super) fn apply_usage_categories(
        &self,
        generation: u64,
        spans: &mut [UsageSpan],
    ) -> Result<HashMap<String, String>, String> {
        let mut apps: HashMap<AppKey, Arc<UsageAppIdentity>> = HashMap::new();
        for identity in spans.iter().filter_map(|span| span.identity.as_ref()) {
            apps.entry(categories::app_key(&identity.app))
                .or_insert_with(|| identity.app.clone());
        }
        let manual_revision = self.usage_categories.manual_revision(generation);
        let (manual, cached) = self.with_read_connection_named("usage.category.rules", |conn| {
            let mut overrides = conn
                .prepare("SELECT category FROM usage_category_overrides WHERE app_key=?1")
                .map_err(|e| e.to_string())?;
            let mut cache = conn
                .prepare("SELECT category,checked_ms FROM usage_category_cache WHERE app_key=?1")
                .map_err(|e| e.to_string())?;
            let mut manual = HashMap::new();
            let mut cached = HashMap::new();
            for (key, app) in &apps {
                let id = aggregate::app_id(app);
                let manual_key = categories::override_key(&id);
                if let Some(category) = overrides
                    .query_row([manual_key.as_slice()], |r| r.get::<_, String>(0))
                    .optional()
                    .map_err(|e| e.to_string())?
                {
                    manual.insert(id, category);
                }
                if let Some((category, checked_ms)) = cache
                    .query_row([key.as_slice()], |r| {
                        Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
                    })
                    .optional()
                    .map_err(|e| e.to_string())?
                {
                    cached.insert(*key, (category, checked_ms));
                }
            }
            Ok((manual, cached))
        })?;
        if generation != self.db_generation() {
            return Err("USAGE_SOURCE_CHANGED".into());
        }
        let mut resolved = HashMap::new();
        let now_ms = chrono::Utc::now().timestamp_millis();
        for (key, app) in apps {
            let id = aggregate::app_id(&app);
            let automatic = self.usage_categories.history_category(
                generation,
                app,
                manual.get(&id).map(String::as_str),
                cached.get(&key),
                manual_revision,
                now_ms,
            );
            resolved.insert(key, automatic);
        }
        for span in spans.iter_mut() {
            let Some(identity) = span.identity.as_mut() else {
                continue;
            };
            let mut automatic = resolved
                .get(&categories::app_key(&identity.app))
                .map(String::as_str);
            // Legacy rows predate the per-app cache. Keep their original fallback
            // when automatic classification has no result. New contexts may
            // contain a manual snapshot, which must follow an override reset.
            if span.run_id == i64::MIN {
                automatic =
                    automatic.filter(|category| !category.is_empty() && *category != "未分类");
            }
            let category = categories::effective_category(
                &identity.category,
                manual
                    .get(&aggregate::app_id(&identity.app))
                    .map(String::as_str),
                automatic,
            );
            if category != identity.category {
                Arc::make_mut(identity).category = category;
            }
        }
        Ok(manual)
    }
}

fn read_cached(conn: &Connection, key: &AppKey) -> Result<Option<StoredCategory>, String> {
    conn.query_row(
        "SELECT category,fingerprint,checked_ms FROM usage_category_cache WHERE app_key=?1",
        [key.as_slice()],
        |r| {
            Ok(StoredCategory {
                category: r.get(0)?,
                fingerprint: r.get(1)?,
                checked_ms: r.get(2)?,
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{calendar::CalendarDay, storage::UsageWriter, usage::types::*};
    #[test]
    fn automatic_and_manual_categories_correct_history_and_live_totals_without_rewriting_intervals()
    {
        let storage = crate::storage::test_storage_with_schema("usage-category-fixture".into());
        let generation = storage.db_generation();
        let calendar = CalendarDay::in_zone("2026-11-25", chrono_tz::UTC).unwrap();
        let app = Arc::new(UsageAppIdentity {
            process_name: "example.exe".into(),
            name: "Example".into(),
            process_path: "D:/Apps/example.exe".into(),
            icon: None,
        });
        let mut span = UsageSpan {
            run_id: 1,
            sequence: 0,
            start_ms: calendar.start_ms,
            end_ms: calendar.start_ms + 1_200_000,
            state: UsageState::Active,
            identity: Some(Arc::new(UsageIdentity {
                app: app.clone(),
                title: "Private title".into(),
                url: None,
                category: "未分类".into(),
            })),
        };
        let mut writer = UsageWriter::new(generation, chrono_tz::UTC);
        writer.write(&storage, &[span.clone()]).unwrap();
        span.end_ms += 60_000;
        let live = LiveUsage {
            generation,
            time_zone: Some("UTC".into()),
            spans: vec![span.clone()],
        };
        let read = || {
            storage
                .usage_day("2026-11-25", live.clone(), span.end_ms)
                .unwrap()
        };
        assert_eq!(read().apps[0].category, "未分类");
        storage
            .save_usage_inferred_category(
                generation,
                &app,
                &StoredCategory {
                    category: Some("游戏".into()),
                    fingerprint: vec![1],
                    checked_ms: 1,
                },
            )
            .unwrap();
        let automatic = read();
        assert_eq!(automatic.apps[0].category, "游戏");
        assert!(!automatic.apps[0].category_is_manual);
        storage
            .save_usage_category_override(generation, "example.exe", Some("办公文档"), || Ok(()))
            .unwrap();
        let manual = read();
        assert!(manual.apps[0].category_is_manual);
        assert_eq!(manual.apps[0].category, "办公文档");
        assert_eq!(manual.total_ms, 1_260_000);
        assert_eq!(manual.apps[0].visits, 1);
        assert_eq!(manual.categories[0].total_ms, manual.total_ms);
        assert_eq!(manual.categories[0].category, "办公文档");
        assert_eq!(manual.hours[0].categories[0].category, "办公文档");
        assert_eq!(manual.intervals[0].category.as_deref(), Some("办公文档"));
        storage
            .save_usage_category_override(generation, "example.exe", None, || Ok(()))
            .unwrap();
        assert_eq!(read().apps[0].category, "游戏");
        storage
            .save_usage_category_override(generation, "example.exe", Some("未分类"), || Ok(()))
            .unwrap();
        assert_eq!(read().apps[0].category, "未分类");
        let guard = storage.get_connection_named("usage.category.test").unwrap();
        let conn = guard.as_ref().unwrap();
        let (category, intervals): (String, i64) = conn
            .query_row(
                "SELECT category,(SELECT count(*) FROM usage_intervals) FROM usage_contexts",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(category, "未分类");
        assert_eq!(intervals, 1);
    }
    #[test]
    fn overrides_require_current_generation_and_authorization_and_cache_keys_include_the_path() {
        let storage = crate::storage::test_storage_with_schema("usage-category-boundaries".into());
        let generation = storage.db_generation();
        assert_eq!(
            storage
                .save_usage_category_override(generation, "example.exe", Some("游戏"), || Err(
                    "AUTH_REQUIRED".into()
                ))
                .unwrap_err(),
            "AUTH_REQUIRED"
        );
        assert_eq!(
            storage
                .save_usage_category_override(generation, "../example.exe", Some("游戏"), || Ok(
                    ()
                ))
                .unwrap_err(),
            "USAGE_INVALID_APP"
        );
        assert_eq!(
            storage
                .save_usage_category_override(generation, "example.exe", Some("invalid"), || Ok(()))
                .unwrap_err(),
            "USAGE_INVALID_CATEGORY"
        );
        let app = UsageAppIdentity {
            process_name: "example.exe".into(),
            name: "Example".into(),
            process_path: "D:/First/example.exe".into(),
            icon: None,
        };
        storage
            .save_usage_inferred_category(
                generation,
                &app,
                &StoredCategory {
                    category: Some("游戏".into()),
                    fingerprint: vec![1],
                    checked_ms: 1,
                },
            )
            .unwrap();
        let other = UsageAppIdentity {
            process_path: "D:/Second/example.exe".into(),
            ..app.clone()
        };
        assert!(storage
            .read_usage_category(generation, &other)
            .unwrap()
            .1
            .is_none());
        let normalized = UsageAppIdentity {
            process_path: "d:\\FIRST\\example.exe".into(),
            ..app.clone()
        };
        assert_eq!(
            storage
                .read_usage_category(generation, &normalized)
                .unwrap()
                .1
                .unwrap()
                .category
                .as_deref(),
            Some("游戏")
        );
        storage.bump_db_generation();
        assert_eq!(
            storage
                .save_usage_category_override(generation, "example.exe", Some("游戏"), || Ok(()))
                .unwrap_err(),
            "USAGE_SOURCE_CHANGED"
        );
    }
    #[test]
    fn fresh_null_cache_rows_do_not_requeue_cold_history_or_evict_the_active_app() {
        let storage = crate::storage::test_storage_with_schema("usage-negative-cache".into());
        let generation = storage.db_generation();
        let now = chrono::Utc::now().timestamp_millis();
        let hot = Arc::new(UsageAppIdentity {
            process_name: "active-unknown.exe".into(),
            name: "Unknown application".into(),
            process_path: "D:/Apps/active-unknown.exe".into(),
            icon: None,
        });
        storage
            .usage_categories
            .category(generation, hot.clone(), Some(7));
        storage
            .usage_categories
            .publish(generation, &hot, Some("游戏".into()), now);
        let mut spans = Vec::new();
        for index in 0..160 {
            let app = Arc::new(UsageAppIdentity {
                process_name: format!("cold-unknown-{index}.exe"),
                process_path: format!("D:/Apps/cold-unknown-{index}.exe"),
                ..hot.as_ref().clone()
            });
            storage
                .save_usage_inferred_category(
                    generation,
                    &app,
                    &StoredCategory {
                        category: None,
                        fingerprint: vec![1],
                        checked_ms: now,
                    },
                )
                .unwrap();
            spans.push(UsageSpan {
                run_id: 1,
                sequence: index,
                start_ms: index,
                end_ms: index + 1,
                state: UsageState::Active,
                identity: Some(Arc::new(UsageIdentity {
                    app,
                    title: String::new(),
                    url: None,
                    category: "未分类".into(),
                })),
            });
        }
        // Hydrate a negative result into an existing query-created memory entry.
        let warm = spans[0].identity.as_ref().unwrap().app.clone();
        storage
            .usage_categories
            .category(generation, warm.clone(), None);
        for _ in 0..2 {
            storage
                .apply_usage_categories(generation, &mut spans)
                .unwrap();
            assert!(spans
                .iter()
                .all(|span| span.identity.as_ref().unwrap().category == "未分类"));
            storage
                .usage_categories
                .category(generation, warm.clone(), None);
            assert_eq!(storage.usage_categories.pending_count(), 1);
            assert_eq!(
                storage
                    .usage_categories
                    .category(generation, hot.clone(), Some(7)),
                "游戏"
            );
        }
        let expired = Arc::new(UsageAppIdentity {
            process_name: "expired-unknown.exe".into(),
            process_path: "D:/Apps/expired-unknown.exe".into(),
            ..warm.as_ref().clone()
        });
        storage
            .save_usage_inferred_category(
                generation,
                &expired,
                &StoredCategory {
                    category: None,
                    fingerprint: vec![1],
                    checked_ms: now - 86_400_000,
                },
            )
            .unwrap();
        Arc::make_mut(spans[0].identity.as_mut().unwrap()).app = expired;
        storage
            .apply_usage_categories(generation, &mut spans)
            .unwrap();
        assert_eq!(
            storage.usage_categories.pending_count(),
            2,
            "expired negatives may retry"
        );
    }
    #[test]
    fn manual_categories_are_written_into_new_contexts_and_reset_to_the_automatic_rule() {
        let storage = crate::storage::test_storage_with_schema("usage-manual-snapshot".into());
        let generation = storage.db_generation();
        let calendar = CalendarDay::in_zone("2026-11-25", chrono_tz::UTC).unwrap();
        let app = Arc::new(UsageAppIdentity {
            process_name: "code.exe".into(),
            name: "Visual Studio Code".into(),
            process_path: "D:/Apps/code.exe".into(),
            icon: None,
        });
        let mut writer = UsageWriter::new(generation, chrono_tz::UTC);
        for (index, manual) in [Some("办公文档"), Some("设计创作"), None]
            .into_iter()
            .enumerate()
        {
            storage
                .save_usage_category_override(generation, "code.exe", manual, || Ok(()))
                .unwrap();
            let category = storage
                .usage_categories
                .category(generation, app.clone(), Some(7));
            assert_eq!(category, manual.unwrap_or("编程开发"));
            let span = UsageSpan {
                run_id: 1,
                sequence: index as i64,
                start_ms: calendar.start_ms + index as i64 * 60_000,
                end_ms: calendar.start_ms + (index as i64 + 1) * 60_000,
                state: UsageState::Active,
                identity: Some(Arc::new(UsageIdentity {
                    app: app.clone(),
                    title: "Example".into(),
                    url: None,
                    category,
                })),
            };
            writer.write(&storage, &[span]).unwrap();
        }
        let categories = storage
            .with_read_connection_named("usage.manual-snapshot-test", |conn| {
                conn.prepare("SELECT category FROM usage_contexts ORDER BY id")
                    .unwrap()
                    .query_map([], |r| r.get::<_, String>(0))
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())
            })
            .unwrap();
        assert_eq!(categories, ["办公文档", "设计创作", "编程开发"]);
        let day = storage
            .usage_day(
                "2026-11-25",
                LiveUsage {
                    generation,
                    time_zone: Some("UTC".into()),
                    ..Default::default()
                },
                calendar.end_ms,
            )
            .unwrap();
        assert_eq!(day.apps[0].category, "编程开发");
        assert_eq!(day.apps[0].total_ms, 180_000);
    }
    #[test]
    fn clearing_a_manual_snapshot_uses_a_negative_result_and_preserves_legacy_fallbacks() {
        let storage =
            crate::storage::test_storage_with_schema("usage-manual-negative-reset".into());
        let generation = storage.db_generation();
        let day = CalendarDay::in_zone("2026-11-25", chrono_tz::UTC).unwrap();
        let app = Arc::new(UsageAppIdentity {
            process_name: "manual-unknown.exe".into(),
            name: "Unknown application".into(),
            process_path: "D:/Apps/manual-unknown.exe".into(),
            icon: None,
        });
        storage
            .save_usage_inferred_category(
                generation,
                &app,
                &StoredCategory {
                    category: None,
                    fingerprint: vec![1],
                    checked_ms: chrono::Utc::now().timestamp_millis(),
                },
            )
            .unwrap();
        storage
            .usage_categories
            .category(generation, app.clone(), Some(7));
        storage
            .save_usage_category_override(
                generation,
                &app.process_name,
                Some("办公文档"),
                || Ok(()),
            )
            .unwrap();
        let recorded = UsageSpan {
            run_id: 1,
            sequence: 0,
            start_ms: day.start_ms,
            end_ms: day.start_ms + 60_000,
            state: UsageState::Active,
            identity: Some(Arc::new(UsageIdentity {
                app: app.clone(),
                title: "Example".into(),
                url: None,
                category: storage
                    .usage_categories
                    .category(generation, app.clone(), Some(7)),
            })),
        };
        assert_eq!(recorded.identity.as_ref().unwrap().category, "办公文档");
        UsageWriter::new(generation, chrono_tz::UTC)
            .write(&storage, &[recorded.clone()])
            .unwrap();
        storage
            .save_usage_category_override(generation, &app.process_name, None, || Ok(()))
            .unwrap();
        let result = storage
            .usage_day(
                "2026-11-25",
                LiveUsage {
                    generation,
                    time_zone: Some("UTC".into()),
                    ..Default::default()
                },
                day.end_ms,
            )
            .unwrap();
        assert_eq!(result.apps[0].category, "未分类");
        assert!(!result.apps[0].category_is_manual);
        assert_eq!(result.total_ms, 60_000);
        let mut legacy = recorded;
        legacy.run_id = i64::MIN;
        Arc::make_mut(legacy.identity.as_mut().unwrap()).category = "游戏".into();
        storage
            .apply_usage_categories(generation, std::slice::from_mut(&mut legacy))
            .unwrap();
        assert_eq!(legacy.identity.as_ref().unwrap().category, "游戏");
    }
}
