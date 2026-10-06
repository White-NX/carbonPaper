# Daily recap query tools

Daily recaps are available through the same authenticated MCP tool catalog as
smart clusters. Advanced Search AI receives both tools in its read-only catalog
and calls the same in-process handlers. External clients use `tools/call` with
the existing MCP bearer token and unlocked application session.

- `get_recap_days`: optional inclusive `start_date` and `end_date` in local
  `YYYY-MM-DD`, `offset` (default 0), and `limit` (default 30, maximum 100).
  Returns dates newest first in `items`, plus `total`, `offset`, `limit` and
  `next_offset`. Discovery uses the existing directory of the most recent 730
  non-deleted recap dates. A listed date can still contain pending periods.
- `get_recap_day`: required local `date`, optional `batch_start_ms`, `offset`
  (default 0), and `limit` (default 10, maximum 50). Returns period status and
  overviews in `periods`, and corrected activities in chronological `items`.
  Each activity includes its task title, text, time bounds and `sources` with
  `screenshot_id`, Unix-millisecond `timestamp`, application and window title.
  Use `get_snapshot_details` to examine the underlying screenshot text.

Example MCP call:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "get_recap_day",
    "arguments": { "date": "2026-10-01", "limit": 10 }
  }
}
```

Follow `next_offset` while it is non-null. A `batch_start_ms` from `periods`
narrows the result to one period. When AI context compaction reduces a page,
its continuation offset is adjusted to the activities actually shown.

These tools read saved content; they do not trigger generation or edit user
corrections. Empty or pending periods do not prove that the user was inactive.
The shared recap read path applies corrections and fences session, privacy,
source revision and database changes. Exported text and source identities pass
through the current sensitive-content and PII filters, including generated text
and user-entered titles. Rejected activities are removed before pagination.
Prompts, reasoning previews, provider errors, usage counters, icons and full
record directories are excluded from tool results.

The additive tool definitions are recorded in
[`mcp-tool-contract-v2.json`](mcp-tool-contract-v2.json). Run
`npm run generate:mcp-contract` after changing the runtime catalog and
`npm run test:mcp-contract` to check that the document matches it.
