# Searchable recap

This branch combines cross-date activity retrieval with selective, cited key
details. Existing recaps are searchable from Advanced Search alongside OCR
snapshot matches. Local MiniLM embeddings add meaning-based retrieval when the
recap semantic-search setting is enabled; keyword retrieval remains immediate.
Newly generated recaps preserve a few concrete details for important activities.

## Try it

1. Open **Advanced Search**, choose **Text**, and enter a description or
   keywords. The field is labelled **Search snapshots and recaps**. Date and
   process filters apply to both result types; category filters apply to
   snapshots.
2. Keyword matches appear first. When semantic search is enabled, the local
   index is queried in the background and the same list is replaced by a hybrid
   ranking using reciprocal-rank fusion (constant 60). Indexing is automatic,
   resumable, and runs through the shared background scheduler.
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

![Unified snapshot and recap results](images/unified-search-results.jpg)

![Recap activity details and source references](images/unified-search-recap-details.jpg)

![English results in a narrow window](images/unified-search-narrow.jpg)

Advanced Search keeps snapshot rows and recap rows in one relevance stream while
retaining their distinct cards and opening behavior. Recap rows show the date,
activity interval, title, summary, key-detail excerpt, source count, primary
process, source-screenshot action, and a link back to that day's recap. The
statistics bar reports total, snapshot, and recap counts; category filtering is
called out as snapshot-scoped. Empty history, no matches, partial coverage and
unavailable inference have distinct messages. The top search bar reports a
short **Recap vectors** progress message while automatic indexing is active.

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

The searchable unit is a corrected activity, not a whole day. Titles,
descriptions and details are encoded separately in overlapping windows. A
72-byte title prefix and 160-byte content window leave conservative space under
the current encoder's 256-token limit. Full titles also have their own windows.
Content fingerprints include the search contract and MiniLM vector-space
revision; the screenshot index and its 30-day expiry are not reused.

Keyword retrieval normalizes case and punctuation, requires all whitespace-
separated query terms, and boosts title and phrase matches. Meaning retrieval
uses the best positive cosine score among an activity's included windows and
retains up to 200 semantic candidates. Combined retrieval uses reciprocal rank
fusion with a constant of 60. Only the requested page's source metadata is
cloned into results. Similarity is a retrieval signal, not proof of an event.

Vectors are stored as encrypted `recap_payloads` of kind `search`, grouped by
date in deterministic shards of up to 64 vectors. Preparation encodes and saves
four vectors at a time within each shard; interrupted runs resume from the last
encrypted batch commit. Shard writes leave the source/correction epoch intact;
database generation, day revision, privacy and session remain fenced. Search
itself never starts preparation. The shared scheduler creates a `recap_index`
task after recap generation, correction, deletion, or source/privacy changes.
The task uses encrypted shards, checkpointed batches, retries, foreground
request yielding, and the same progress and failure policy as other background
indexers. The index covers all saved dates, independently of the calendar
discovery API's 730-date limit.

A resident activity projection avoids decrypting every recap for every query.
Its identity includes database generation, a source/correction epoch, privacy
policy and authenticated session. Lock notifications clear it and cancel
preparation. Every composed read checks its identity before returning; a later
unlock cannot authorize a request from the preceding session. Normalized
keyword text and vectors are retained alongside activities and zeroized when
the projection's last reference drops. Concurrent first reads share one corpus
materialization.

Source edits, recap regeneration, corrections and deletions invalidate the
projection. Screenshot deletion also purges persisted search payloads. Search
refilters generated text, corrected names, details and source identities before
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
questions keep the originally searched dates.

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
The UI's query time includes corpus loading and query encoding, but excludes
past preparation and recap generation. Do not infer end-to-end improvements
from the ranking unit tests or synthetic UI screenshots.

See [the timeout investigation and scaling measurements](recap-search-performance.md)
for cold-start budgets, reproducible synthetic benchmarks and remaining costs
when searching several years of activities.

Relevant checks are `npm run test:frontend`, `npm run build`, `npm run lint:ci`,
`npm run i18n:check`, `npm run test:security`, `npm run test:mcp-contract`,
`cargo check --manifest-path src-tauri/Cargo.toml`, and
`cargo test --manifest-path src-tauri/Cargo.toml --lib`.

Validation on Windows: 559 frontend tests passed across 84 files; 867 Rust
library tests passed and nine opt-in tests were ignored. Build, lint, locale
parity, security guards, Cargo check, Rust formatting, and MCP contract checks
passed. Browser checks exercised Chinese/dark and English/light views and a
560-pixel window using synthetic records. Real-provider generation quality and
end-to-end retrieval performance on personal history have not been benchmarked.
