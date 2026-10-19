# MCP and Agent tool contract v3

MCP clients and Advanced Search AI use the same v3 catalog and result projections.
This version separates capture time from OCR processing time, names screenshot
and OCR IDs explicitly, and returns recognizable errors for failed reads.
The [generated contract](mcp-tool-contract-v3.json) is the catalog source for clients.
The MCP transport protocol remains `2025-03-26`.

## Client migration

| v2 field or behavior | v3 contract |
| --- | --- |
| `start_time` / `end_time`, with inferred seconds or milliseconds | Integer `start_ms` / `end_ms`, always Unix milliseconds |
| Inclusive screenshot end bound | Half-open capture interval: `start_ms <= captured_at_ms < end_ms` |
| Snapshot `id`, lookup argument `id` | `screenshot_id` |
| OCR search `id` | `ocr_id`; use its accompanying `screenshot_id` for details and citations |
| Ambiguous `timestamp`, `created_at`, `screenshot_created_at` in screenshot tools | `captured_at_ms` and, on OCR rows, `ocr_processed_at_ms` |
| Archive metadata date strings | Explicit `created_at_ms`, `updated_at_ms`, `last_assigned_at_ms`, `assigned_at_ms` |
| Missing snapshot returned as an empty OCR result | Tool failure with `error.code = SNAPSHOT_NOT_FOUND` |
| Recap dates interpreted in the current system time zone | Saved calendar returned as `calendar` or `calendars[date]` |

Update argument names and result readers together. The old `id`, `start_time`, and
`end_time` arguments are rejected with `INVALID_ARGUMENT`; units are never inferred
from magnitude. Unknown filters and invalid optional values also fail explicitly.
Omit optional fields instead of sending `null` unless their schema allows it.
Update external skill references from the v3 generated catalog before using them
with this server; `npm run test:mcp-skill-contract` checks a sibling skill checkout.

Screenshot list, detail, OCR, visual search, archive corpus, and recap evidence
all use the same screenshot reference. For example, a screenshot captured at
00:30 on October 1 in Shanghai, then recognized the following day, has:

```json
{
  "screenshot_id": 83558,
  "ocr_id": 3689731,
  "captured_at_ms": 1790785800000,
  "ocr_processed_at_ms": 1790905929000
}
```

Only OCR rows carry `ocr_id` and `ocr_processed_at_ms`. A missing instant is
`null`; zero is an actual Unix epoch instant. To search five minutes before and
after a capture, subtract and add `300000` to `captured_at_ms`. The storage layer
retains second precision, so adapters include exactly the stored captures inside
the requested interval, including intervals with fractional-second boundaries.
An interval with no representable capture instants returns an empty result.

`get_snapshot_details` accepts `{ "screenshot_id": 83558 }`. An existing screenshot
returns `status: "found"`, its `record`, and `ocr_results`. An empty OCR list means
that no text is currently available for export. It does not imply that the
screenshot is missing or that a privacy-rejected read succeeded. The Agent's
compaction preserves explicit null times and pagination end markers.

OCR search is lexical: extract words likely visible on screen, such as `保存路径`
or `save location`. Whitespace separates terms; fuzzy matching tolerates OCR and
spelling errors and does not turn an arbitrary question into semantic retrieval.

## Errors

The Agent receives this payload for a failed tool call:

```json
{"error":{"code":"SNAPSHOT_NOT_FOUND","message":"SNAPSHOT_NOT_FOUND: Screenshot 83558 was not found"}}
```

MCP returns the same JSON in `result.content[0].text` with `result.isError: true`.
Successful tool results set `isError: false`. Malformed JSON-RPC requests remain
protocol errors. Codes include `INVALID_ARGUMENT`, `TOOL_NOT_FOUND`,
`SNAPSHOT_NOT_FOUND`, `PRIVACY_REJECTED`, `AUTH_REQUIRED`,
`MAINTENANCE_IN_PROGRESS`, and `TOOL_FAILED`; existing domain codes such as
`RECAP_SOURCE_CHANGED` remain available. Callers should inspect the code rather
than treating any empty result as a successful read.

## Saved calendars and local months

New recap days persist an IANA `time_zone`, `rule: "system_local_at_creation"`,
absolute `start_ms` / `end_ms`, endpoint UTC offsets in seconds, and
`period_boundaries_ms`. Capture collection, revision coverage, regeneration and
record reads reuse those saved boundaries. Named daylight-saving rules can
produce 23- or 25-hour days; the six periods need not each last four elapsed hours.
Changing the system time zone does not reassign an existing recap day or its cache.

Existing recaps retain their stored UTC bounds and available period keys.
They receive `rule: "recorded_bounds"` and `time_zone: null`, because the original
zone was not saved. Unfinished periods after the last saved key are reconstructed
from the closing offset. No current zone is attributed to old data. The UI uses
the recorded offset when both endpoint offsets agree; if the transition rule is
unknown, it displays explicitly labelled UTC times. Recap deletion retains the
day's calendar alongside its UTC bounds and monotonic revision, so regeneration
uses the same day. Removing the day row removes its calendar. Existing source
and usage protections are preserved.

`get_recap_day` includes `calendar`. Date discovery and search include a
`calendars` object keyed by returned date. Recap date filters are inclusive saved
date labels, so Agent history preserves those labels instead of inventing an
absolute range from today's time zone. Other successful searches record their
explicit millisecond bounds and exclusive end.

Storage management groups thumbnails in the named zone returned by the page.
Pagination and month deletion pass that zone back to the backend; deletion
requires it and uses an indexed half-open UTC range for the local month.
The confirmation retains the displayed page's zone even if the OS setting changes.
Selected-screenshot deletion still addresses the supplied screenshot IDs.

These changes do not rewrite screenshot timestamps or change the existing Tauri
storage APIs that intentionally use seconds. Archive strings returned to the UI
are normalized to RFC 3339 UTC through `storage/wire_time.rs`.

## Validation

`npm run generate:mcp-contract` updates the versioned JSON; `npm run test:mcp-contract`
checks it against the running Rust catalog. Rust and frontend tests share the
synthetic fixture in `src/tests/fixtures/screenshot_contract_v3.json`. Regression
coverage includes cross-tool capture identity, delayed OCR, unknown times, error
transport, midnight and month boundaries, DST, pinned calendars, and deletion
using the displayed page's zone.
