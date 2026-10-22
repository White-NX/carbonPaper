# Searchable recap

This branch combines cross-date activity retrieval with selective, cited key
details. Existing recaps are searchable from Advanced Search alongside OCR
snapshot matches. Keywords reuse the snapshot HMAC blind index and plaintext
scorer. Local MiniLM embeddings add meaning-based retrieval when the user opts
in; the Advanced Search vector toggle starts on.
Newly generated recaps preserve a few concrete details for important activities.

## Try it

1. Open **Advanced Search**, choose **Text**, and select **Snapshots only**,
   **Snapshots and recaps** (default), or **Recaps only**. Date and process
   filters apply to both result types; category filters apply to snapshots
   and are hidden for recap-only searches.
2. Keyword results share one ranking based on the snapshot text scorer. In
   mixed or recap-only mode, turn on **Recap semantic search** to add vector
   matches. The toggle is on by default. With it on, keyword matches appear
   first, followed by a hybrid ranking using reciprocal-rank fusion (constant
   60). Only vector matches with a cosine score of at least 0.5 enter the semantic
   ranking. Verified keyword matches remain available at any vector score,
   including when vectors are missing. Recap cards label keyword and approximate
   keyword matches; vector matches show their semantic relevance to two decimals.
3. In **Settings → Organize → Daily recap**, use **Enable recap semantic
   search** to pause or resume vector indexing and semantic queries. Turning it
   off keeps recap keyword matches available.
4. Open a result to read its key details and screenshots. Each detail has its
   own source action. Closing a source dialog returns to the activity; closing
   the activity retains the search. **Open daily recap** returns to that date.
5. The **Daily recap** page remains the place to read, correct, inspect sources,
   and delete activities. To enrich an existing day, use that day's
   **Regenerate** action. This uses the existing provider, consent and
   generation budgets; older recaps are not automatically regenerated.

The existing `npm run debug` workflow runs the experiment from its worktree.
It uses the normal local database, registry settings and Windows CNG key, as
documented in `AGENTS.md`. UI preview screenshots below use synthetic records.

## Interface

![Search targets and opt-in recap vector relevance](images/recap-search-options.png)

![Recap activity details and source references](images/recap-search-details.png)

![Search filters in a narrow window](images/recap-search-options-narrow.png)

Advanced Search keeps snapshot rows and recap rows in one relevance stream while
retaining their distinct cards and opening behavior. Recap rows show the date,
activity interval, title, summary, key-detail excerpt, source count, primary
process, source-screenshot action, and a link back to that day's recap. The
statistics bar reports total, snapshot, and recap counts; category filtering is
called out as snapshot-scoped. Empty history, no matches, partial coverage and
unavailable inference have distinct messages. Bounded retrieval shows a prompt
to narrow the scope. Vector fallback messages appear when the search toggle
is enabled; vector preparation still uses the shared background scheduler.

## Generation and evidence

- The extraction request selects up to three important activities per period,
  with up to three details each, at most 300 characters per detail. The local
  postprocessor enforces the selected-activity bound. Legacy detail opt-outs are
  normalized on read and save; `include_details` remains an accepted search
  field and is always treated as true for existing clients.
- Prompts prioritize useful discoveries, comparisons, decisions, explicit
  outcomes and unresolved questions. Brief events can qualify. Existing targeted
  gap requests can obtain additional evidence within the same request budgets.
- Detail citations must be nonempty subsets of the activity's received evidence.
  Existing segment and source validation still applies. Reading, proposals,
  attributed reports and completed work remain distinct in the prompt.
- Correcting an activity's task retains a detail only when all its cited sources
  belong to that corrected activity. Summaries receive the supported details.
- Old payloads deserialize with empty details. The addition does not force a
  rewrite of saved activity text or a paid regeneration.

Citation validation establishes source identity, not factual entailment. Inspect
the cited material when judging generation quality.

## Retrieval and storage

The searchable unit is a corrected activity. Its title, description and key
details form one keyword document. `storage/search_plan.rs`, `search_rank.rs`
and the extracted `blind_search.rs` share query planning, case variants,
rarest-first posting intersections, bounded fuzzy recall and plaintext scoring
with snapshots. Both indexes use `StorageState::bigram_tokenize()` and the same
HMAC key. `BlindIndex::recaps()` selects separate recap posting tables while
reusing the complete inline/chunked posting format and mutation logic; document
IDs cannot collide with snapshot IDs. Ranking favors phrases and word boundaries,
then keyword coverage and density, and tolerates small typing/OCR errors.

`recap_search_documents` stores encrypted per-activity JSON and CNG-wrapped row
keys. `recap_search_tokens` stores reverse HMAC postings for deletion, and
`recap_search_processes` stores HMAC process filters. Date, revision and
observation-time metadata support filtering before candidate caps. Dates are
prepared one at a time after source or correction changes; ordinary indexed
queries decrypt only the recalled candidates, up to 800 activities, without
loading the full recap corpus. Blank and single-character queries inspect a
bounded recent observation window. Shared text verification removes blind-index
false positives before returning hits. HMAC postings do not store plaintext
keywords; text and vectors retain the existing SQLCipher and row encryption.

Mixed keyword search compares the same text scores across snapshot and recap
hits, then observation time and stable ID. Explicit vector retrieval adds a
second rank channel against this combined lexical list using reciprocal-rank
fusion with a constant of 60. Unified search retains at most 200 candidates per
route; recap semantic retrieval also caps its candidates at 200. `complete: false`
reports bounded retrieval, and `total` counts retained candidates rather
than every possible match in history.

The semantic score threshold of 0.5 is applied before ranking, candidate caps and
pagination in both UI and MCP recap search. Weaker vectors contribute no semantic
rank or fusion boost. Keyword eligibility uses the verified plaintext scorer
independently, preserving literal, approximate and key-detail matches. Semantic
execution counts include only activities that meet the threshold.

For vectors, titles, descriptions and details are encoded in separate overlapping
windows. A 72-byte title prefix and 160-byte content window leave conservative
space under the encoder's 256-token limit. Full titles also have their own
windows. Content fingerprints include the search contract and MiniLM vector-space
revision. The best cosine score of at least 0.5 among an activity's windows is
exposed as `semantic_score`, or null without a qualifying vector match. The
snapshot vector index's 30-day expiry does not apply to recap vectors. Similarity
is a retrieval signal, not proof of an event.

Vectors are stored as encrypted `recap_payloads` of kind `search`, grouped by
date in deterministic shards of up to 64 vectors. Preparation encodes and saves
four vectors at a time within each shard; interrupted runs resume from the last
encrypted batch commit. Shard writes leave the source/correction epoch intact;
database generation, day revision, privacy and session remain fenced. Search
never starts embedding preparation. The shared scheduler creates a `recap_index`
task after recap generation, correction, deletion, or source/privacy changes.
The task uses encrypted shards, checkpointed batches, retries, foreground
request yielding, and the same progress and failure policy as other background
indexers. The index covers all saved dates, independently of the calendar
discovery API's 730-date limit.

A keyword preparation marker caches only database generation, a source/correction
epoch, privacy policy and authenticated session. Triggers remove stale encrypted
documents and queue posting removals on payload edits, day revision changes and
deletions. Keyword preparation prunes these postings and rebuilds changed dates.
The optional vector path still uses a resident activity/window projection, shared
by concurrent first reads. Lock notifications clear both markers and the vector
corpus and cancel preparation; the projection zeroizes text and vectors when its
last reference drops. Every composed read checks its identity before returning;
a later unlock cannot authorize a request from the preceding session.

Source edits, recap regeneration, corrections and deletions invalidate search
documents and the vector projection. Screenshot deletion also purges persisted
search payloads. Dates saved under a different privacy policy are excluded through
metadata before reading their content. Search refilters generated text,
corrected names, details and source identities before
returning data. Privacy and source changes during indexing or inference reject
the operation rather than publish results from the old identity.

Pagination includes a `ranking_revision`: source changes, changed search options
or newly prepared semantic vectors cannot silently mix two rankings. Restart
the query when `RECAP_SEARCH_RESULTS_CHANGED` is returned.

## AI and MCP

`search_recaps` is in the shared read-only catalog. It uses the same implementation
as the UI and returns dates, corrected activities, key details, screenshot
references, coverage and pagination. Screenshot timestamps remain Unix
milliseconds in the MCP contract. Pass the response's `ranking_revision` with
later pages; restart without it when the ranking changes.
Executed local date bounds are retained in AI turn context so later follow-up
questions keep the originally searched dates. The MCP `mode` default remains
`hybrid`; the Advanced Search toggle controls only UI requests. Keyword-only
execution leaves `coverage`, `indexed_chunks` and `total_chunks` null so vector
coverage does not require a full corpus read. It reports
`semantic_status: "not_requested"` when semantics are enabled, `"disabled"` otherwise.

AI can inspect a relevant activity and then verify its sources with
`get_snapshot_details`. Missing or filtered recap content does not prove that an
activity never happened. OCR and image search remain available when details
were omitted from the recap.

## Evaluate the combined experiment

Use the same locally selected questions and saved corpus for each comparison:

| Comparison | What it isolates |
|---|---|
| Keyword first page / completed hybrid ranking | Latency and additional semantic recall |
| Snapshot matches / recap activities in the shared stream | Relevance and useful source navigation |
| Current AI tools / AI with `search_recaps`, same model and call budget | End-to-end time, calls, context usage and citation accuracy |

Include exact names, paraphrases, activities repeated across dates, facts omitted
from summaries, and questions with no recorded answer. Record relevant activity
and screenshot IDs, top-ten retrieval, final answer correctness, citations, cold
and warm query time, indexing time and memory, and generation cost separately.
The UI's query time includes changed-date keyword preparation and, when selected,
vector corpus loading and query encoding. It excludes past preparation and recap
generation. Do not infer end-to-end improvements from the ranking unit tests
or synthetic UI screenshots.

The opt-in synthetic benchmark in `src-tauri/src/ai/recap/search_benchmark.rs`
measures in-memory scoring and vector-window ranking. It does not measure the
new blind-index lookup, SQLCipher reads, CNG decryption or end-to-end latency.

Relevant checks are `npm run test:frontend`, `npm run build`, `npm run lint:ci`,
`npm run i18n:check`, `npm run test:security`, `npm run test:mcp-contract`,
`cargo check --manifest-path src-tauri/Cargo.toml`, and
`cargo test --manifest-path src-tauri/Cargo.toml --lib`.

Validation on Windows: 587 frontend tests passed across 88 files; pagination
and API checks passed all 25 targeted tests. 905 Rust
library tests passed and nine opt-in tests were ignored. Build, lint, locale
parity, security guards, Cargo check, Rust formatting, and MCP contract checks
passed. Browser checks exercised both locales at 1280 and 560 pixels using
synthetic records, including target switching and opt-in vector relevance.
Real-provider generation quality and
end-to-end retrieval performance on personal history have not been benchmarked.
