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
Already running SQLite statements and native CNG calls finish before cancellation
can take effect.

SQL still runs under the primary connection mutex, which is released before
decryption. Sampling preserves the existing standardized time buckets and minimum
ID selection. The preliminary count stops at `limit + 1` matching rows; sampled
selection still visits the requested time range. Each response is capped at 500
records. Timestamp fields retain Unix seconds and RFC 3339 UTC semantics.

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
| `db_wait`, `count`, `sql` | Connection mutex wait, bounded count, row selection |
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

To measure end-to-end improvement, repeat the same drag/zoom sequence with the
same dataset and background jobs. Compare the latest viewport's completion time
and the new CNG wait/execution fields. No production speedup is inferred from
the synthetic fixture.
