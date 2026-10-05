# Timeline query performance

`storage_get_timeline` returns `TimelineRecord`: screenshot identity, UTC time,
window/process labels, the selected process icon/path and category. Full screenshot
details and MCP time-range reads retain their existing `ScreenshotRecord` APIs.
The timeline no longer joins link sets or decrypts page URLs and visible links.

The timeline request queue allows one active read and one latest pending viewport.
Gestures cancel the active read; the next read starts only after the backend read
settles. Identical requests coalesce. Periodic follow refreshes allow a useful
active read to finish, so slow reads are not repeatedly cancelled by the timer.
Pause, reset and component cleanup cancel outstanding work.

Each read has a random request ID. `storage_cancel_timeline` is authenticated and
scoped to the calling Tauri window. A bounded set of cancellation tombstones
handles cancellation arriving before read registration. The backend checks
cancellation and UI authorization before reading, between rows and before returning;
the CNG helper also checks cancellation after waiting for its private-key mutex.
A SQLite progress handler checks cancellation and authorization every 1,000 VM
instructions, including inside grouping/sorting before the first result. The
handler is removed on exit. Connection setup and an already running native CNG
call still finish before their next cancellation check.

SQL uses an independent read-only SQLCipher connection and the existing maintenance
activity gate, leaving the primary connection mutex available to capture and
background jobs. The read connection closes before decryption. In DELETE journal
mode, normal SQLite reader/writer locking still applies while a statement runs.

Sampling preserves the existing standardized time buckets and minimum ID selection.
The preliminary count stops at `limit + 1` matching rows. A `MATERIALIZED` CTE
computes the selected IDs once; `CROSS JOIN` makes those IDs drive primary-key
lookups for screenshot payloads. Range predicates stay inside the ID selection.
Sampled selection visits the covering time index once and groups its entries;
it no longer repeats the grouping for each matching screenshot. Each response
is capped at 500 records. Timestamp fields retain Unix seconds and RFC 3339 UTC
semantics.

Within one response, repeated references to a page icon reuse its decrypted text.
A metadata process icon takes precedence and avoids decrypting the page icon.
This cache ends with the request, rechecks authorization before use, and contains
no row keys. Temporary keys and cached icon strings use `Zeroizing`. Unreadable
row keys preserve legacy plaintext fallback; authorization failure aborts the read.

## Diagnostics

`[DIAG:DB] get_timeline_records timing` logs aggregate fields only:

| Field | Meaning |
| --- | --- |
| `status`, `rows`, `processed` | Completion/cancellation/error and row counts |
| `read_open`, `count`, `sql` | Read connection setup, bounded count, row selection |
| `sql_vm_steps` | SQLite VM instructions for row selection, including interrupted work |
| `cng_calls`, `cng_wait`, `cng_decrypt` | Key unwrap attempts, private-key mutex wait, native execution |
| `icon_cache_hits` | Reused shared icons |
| `assembly`, `total` | Other hydration work and total storage-call duration |

Reads taking at least one second log at WARN; faster reads log at DEBUG. Native
execution includes the size query and decrypt pair. `total` excludes Tauri task
queueing, serialization, IPC and rendering. Timings are wall time, including
scheduling delays, and do not establish CPU time.

## Regression coverage and measurement limits

The fixture with 338 encrypted rows sharing one icon requires 339 key unwraps
(338 row keys and one icon key). The former full-record path would unwrap that
icon 338 times, in addition to any link-set keys. This is a deterministic operation
count, not a measurement of production latency.

Tests cover request coalescing, backend cancellation acknowledgement, refresh
starvation, disposal/resumption, cancellation registration races, native CNG
cancellation after lock wait, shared-icon reuse, legacy fallback, authorization
revocation, UTC serialization, sampling, limits and deleted records. Native tests
use an unnamed temporary CNG key; they do not open the user's persisted key.

Large-range query regressions cover 1,000, 4,000 and 16,000 rows, with and without
`ANALYZE`. They assert materialized ID selection, primary-key hydration, linear VM
work bounds, and exact results even when IDs and timestamps have different orders.
Additional tests interrupt SQL before its first result and run the timeline while
an uncommitted primary writer holds its mutex in both WAL and DELETE modes.

## October 5 SQL regression

Commit `31b13e1` repeated the time/deletion predicate outside the sampled-ID
subquery. On an unanalyzed database SQLite reversed the join to preserve the
outer time ordering: `SEARCH s USING INDEX ...` preceded `SCAN picks`. The picks
subquery was a coroutine, so its full grouping restarted for each matching outer
row. This was quadratic work, not a missing time index or expensive decryption.
The primary mutex and the lack of cancellation inside `sqlite3_step` amplified
the latency into background-job backlog.

The following opt-in tests use the bundled SQLite/SQLCipher engine and print only
plans, counts and timings. The synthetic comparison retains the regressed query
as a reference. The snapshot comparison opens a supplied plaintext snapshot
read-only, imposes a two-second deadline per timed query and verifies fixed query
IDs against the original sampling semantics. It does not decrypt field payloads
or use the application's private key.

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --lib storage::timeline::query_regression::compare_regressed_query_plan -- --ignored --nocapture
$env:CARBONPAPER_TIMELINE_BENCH_DB = 'D:\path\to\screenshots_decrypted.db'
cargo test --manifest-path src-tauri/Cargo.toml --lib storage::timeline::query_regression::compare_snapshot_queries -- --ignored --nocapture
```

On October 5, the full-projection synthetic comparison on SQLite 3.51.3 /
SQLCipher 4.14.0 in a debug build produced:

| Synthetic rows | Regressed query | Fixed query | Regressed VM steps | Fixed VM steps |
| --- | ---: | ---: | ---: | ---: |
| 1,000 | 679 ms | 0.73 ms | 18,079,106 | 18,237 |
| 4,000 | 12,692 ms | 3.49 ms | 288,928,358 | 72,822 |

The local plaintext snapshot contained 67,747 live screenshots and no
`sqlite_stat1` table. Read-only comparisons ending at its latest screenshot gave:

| Range | Returned rows | Regressed query | Fixed query | Fixed VM steps |
| --- | ---: | ---: | ---: | ---: |
| 7 days | 23 | 739 ms | 2.02 ms | 20,098 |
| 30 days | 78 | Interrupted at 2 s | 8.34 ms | 134,538 |
| 90 days | 165 | Interrupted at 2 s | 40.91 ms | 641,610 |

All fixed ID sequences matched the reference. These are single-run query
measurements using the bundled engine, with the reference query warming caches
before timing. The snapshot lacks SQLCipher page encryption; connection setup,
field decryption, concurrent app work, IPC and rendering are excluded. These
numbers establish the query-plan correction, not end-to-end application latency.

To measure end-to-end improvement, repeat the same drag/zoom sequence with the
same dataset and background jobs. Compare the latest viewport's completion time
and the new CNG wait/execution fields. No production speedup is inferred from
the synthetic fixture.
