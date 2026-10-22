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

Ranked OCR rows include `match_evidence`. `relevance_score` is the lexical score
used for ordering, with higher values first; it is not a probability or a score
to compare across different queries. `matched_terms` lists normalized query terms
matched literally or approximately; `literal_terms` is the literal subset.
`match_type` is `literal` when any term matched literally, `approximate` when only
approximate terms matched, and `bigram_only` for a weak recall candidate without
verified text matches. `phrase_matched` includes approximate phrase matches.
`matched_bigram_groups` / `total_bigram_groups` are populated only for the fuzzy
recall tier; other tiers and scan fallback leave them null.

These fields explain the representative OCR row before privacy filtering;
`match_evidence.scope` is `representative_ocr_row_before_privacy_filter`.
A multi-word query may recall a screenshot using several rows; it returns the
best ranked row as its representative. Its terms do not describe the whole
screenshot. Inspect `get_snapshot_details` to check other rows. Scores and match
evidence survive cached pagination. Recent-list fallback has no match evidence.
`confidence` is the recognizer's confidence, unrelated to search relevance.

## Pagination and Agent context

All list tools accept `offset` / `limit` and return an object containing `items`,
`offset`, `limit`, `returned_count`, and `next_offset`. Screenshot details use
`ocr_results` as their paginated list. `total` is included only when known.
Screenshot listing keeps `max_records` as a legacy alias for `limit`; archive
corpus keeps `page` / `page_size` aliases. Do not combine `limit` with a legacy
size argument, or `offset` with `page`. To continue a legacy request, use the
returned `next_offset` as `offset` and omit `page`.

Always follow `next_offset` with the same filters. Export filtering can withhold
rows, so a short or empty page may still have a continuation. `result_offset`
identifies a retained row's position before that filtering. A full final storage
page may require an empty probe to establish the end. A null continuation means
the current result set ended, except when `end_reason: "offset_limit"` explicitly
reports the 100,000-offset ceiling; narrow the query in that case. Visual search
accepts at most 200 records per page and the same 100,000-offset ceiling.

The Agent targets 12,000 characters per result by deferring whole trailing items
and setting `truncated: true`. It updates `next_offset` to the first omitted row,
prunes calendars belonging to omitted dates, and counts only visible evidence.
It never cuts strings, nested evidence lists, or JSON fragments. A single record
larger than the target remains whole. External MCP clients receive the requested
page without this additional Agent budget.

If the complete model request needs further compression, it applies the same
whole-item paging to tool results and prefetched result blocks. Continuations,
fact citations and member lists remain intact. A request that cannot fit even
one complete item fails with `AI_CONTEXT_LIMIT`; a larger context budget is
required to read that item. Legacy plain-text transcripts retain labelled excerpts.

Offset pagination assumes unchanged source data, index and filters. Restart after
these change. Recap `ranking_revision` rejects a changed ranking explicitly;
it must be carried to subsequent pages. No other list promises a frozen snapshot.

## Recap retrieval coverage and execution

`search_recaps.process_names` matches any cited source process name without case
distinctions. It filters activities before ranking, coverage counting and paging;
an empty list includes all processes. A match retains the activity's full evidence.
Date bounds remain inclusive saved date labels.

When vector retrieval is requested, `coverage.global` counts the privacy-filtered
searchable corpus. `query_scope`
and `dates` apply the current date/process filters before text matching and paging.
Chunk counts refer to unique content-addressed text windows, never durations,
dates or recall percentages. Shared chunks count once in each scope; per-date
counts therefore need not sum to the global count. Activity counts distinguish
fully, partially and unindexed activities. `status` is `empty`, `not_prepared`,
`partial` or `ready`. `missing_dates` lists scoped dates with missing chunks.
Dates without searchable saved activities are absent; coverage does not describe
whether recap generation has finished. Top-level chunk counts are global.
Keyword-only execution leaves `coverage`, `indexed_chunks` and `total_chunks`
null so ordinary keyword queries do not materialize the vector corpus.

`execution` reports the requested mode, `keyword_executed`, `keyword_matches`,
`keyword_complete`,
`semantic_executed`, `semantic_matches`, `semantic_candidates`,
`semantic_candidate_limit` (200), `semantic_candidates_truncated`, and
`fallback_reason` (`disabled`, `empty`, `not_prepared`, `unavailable`, or null).
Counts precede pagination. Keyword retrieval uses the shared HMAC blind index
and verifies at most 800 candidate activities with the snapshot plaintext scorer.
Hybrid adds semantic retrieval over available vectors, including partial coverage.
`keyword_rank: null` means that activity did not match the keyword branch.
`total` counts the retained fused candidates. `complete: false` means the keyword
verification budget or semantic candidate cap bounded retrieval; narrow the scope
instead of treating this count as exhaustive. `keyword_score` and `match_evidence`
describe lexical matches. `semantic_score` is the activity's best positive cosine
similarity, or null without a vector match. Coverage remains available after failed
hybrid inference. Pure semantic searches still fail if semantics cannot run.
`mode` retains the effective configured mode; `execution` states what ran.
Keyword-only `semantic_status` is `not_requested` when semantic search is enabled,
otherwise `disabled`. Use non-null `coverage` for index coverage and `execution`
for this search. `index_progress` is the runtime construction
snapshot (running/phase/wait/retry), distinct from either search branch.

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

## Activity observations and membership

Both `get_recap_day` and `search_recaps` expose
`time_semantics: "evidence_observation_span"`. `start_ms` and `end_ms` are the
earliest and latest direct citation capture instants (both observed endpoints).
`span_ms` is their nonnegative difference. `observation_kind` is `instant` when
the endpoints coincide and `span` otherwise. A zero span is a single observed
instant; neither kind estimates continuous work duration. Membership can cover
captures beyond the directly cited span. Period boundaries remain half-open.

`sources` is the complete set of direct citations for the activity;
`details[].evidence_ids` cites the subset supporting that fact. `member_ids` is
the distinct, ascending screenshot ID list from the saved assignment of records
to the nearest activity in their segment, after user corrections. Membership is
an assignment, not factual support for every sentence. Use screenshot details to
verify individual members. Process filters match cited sources and keep the full
activity membership; they do not trim it to one process.

`membership_status` is `complete` when all stored members can be exported,
`filtered` when known members were withheld by privacy filtering, `incomplete`
when stored IDs cannot be resolved in the period directory or direct citations,
and `unavailable` when old data has no stored member list. An empty list with
`unavailable` does not
establish that the activity has no members. No missing membership is reconstructed
from citations. Lists are kept whole during Agent compaction. Members alone do not
become factual screenshot citations.

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
