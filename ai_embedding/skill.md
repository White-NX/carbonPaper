# CarbonPaper Memory Retrieval Skill

You have access to the user's **CarbonPaper** screenshot history — a continuous, text-searchable archive of everything they've seen on screen. Use the MCP tools below to help them recall information, find past activities, and answer questions about their computer usage history.

## Core Concepts

- **Snapshot**: A screenshot captured at a specific moment, with metadata (process name, window title, timestamp, category) and OCR-extracted text.
- **OCR results**: Text blocks recognized from screenshots, each with bounding-box coordinates and confidence scores.
- **Task cluster**: A group of related snapshots automatically clustered by activity patterns (e.g. "Writing report in Word", "Browsing GitHub issues"). Tasks have labels (auto-generated or user-renamed), a dominant process/category, and a time span.
- **Timestamps**: All timestamps are **milliseconds since Unix epoch**. To convert a human date: `new Date("2025-06-15T10:00:00").getTime()` → `1750006800000`.

## Available Tools

| Tool | Purpose | Key params |
|------|---------|------------|
| `search_ocr_text` | Full-text search on OCR text (exact/fuzzy keyword match) | `query`, `limit`, `offset`, `fuzzy`, `process_names`, `start_time`, `end_time`, `categories` |
| `search_nl` | Natural language semantic search on screenshot images (requires Python service running) | `query`, `limit`, `offset`, `process_names`, `start_time`, `end_time` |
| `get_snapshots_by_time_range` | List snapshots within a time window (metadata only, no OCR) | `start_time`, `end_time`, `max_records` |
| `get_snapshot_details` | Full details of one snapshot: metadata + OCR text + task cluster membership | `id`, `include_coords` (default false) |
| `get_task_clusters` | List task clusters with metadata and relevance scores | `layer`, `start_time`, `end_time`, `hide_inactive` |
| `get_task_screenshots` | List snapshots in a task cluster (paginated) | `task_id`, `page`, `page_size` |
| `rename_task` | Rename a task cluster | `task_id`, `label` |

## Search Strategy Guide

### When to use which search tool

- **`search_ocr_text`** — Best for: specific text the user remembers seeing (error messages, code snippets, names, URLs, numbers). Supports CJK and English. Use `fuzzy: true` (default) for lenient matching, `fuzzy: false` for exact phrases.
- **`search_nl`** — Best for: describing what was on screen visually ("a chart showing sales data", "a video call with 4 people", "a dark-themed code editor"). This searches by image embedding similarity, not text.
- **`get_snapshots_by_time_range`** — Best for: browsing what happened during a known time period ("what was I doing yesterday afternoon").
- **`get_task_clusters`** — Best for: high-level overview of activities over a time span ("what projects was I working on last week").

### Retrieval patterns

**1. Keyword recall** — User remembers specific text they saw:
```
search_ocr_text(query="connection refused", limit=10)
→ find matching snapshots
→ get_snapshot_details(id=...) for the most relevant one
```

**2. Time-based recall** — User knows approximately when:
```
get_snapshots_by_time_range(start_time=..., end_time=..., max_records=50)
→ scan metadata to find relevant entries
→ get_snapshot_details(id=...) for details
```

**3. Activity recall** — User wants to know what they were working on:
```
get_task_clusters(start_time=..., end_time=..., hide_inactive=true)
→ present task list with labels, dominant processes, and time spans
→ get_task_screenshots(task_id=...) to drill into a specific task
```

**4. Combined search** — Start broad, then narrow:
```
search_ocr_text(query="API key", process_names=["chrome.exe"], start_time=..., end_time=...)
→ found in a browser window
→ get_snapshot_details(id=...) to see full OCR text + task context
→ get_task_screenshots(task_id=...) to see related activities
```

**5. Visual recall** — User describes what it looked like:
```
search_nl(query="presentation slides with a blue bar chart")
→ find visually matching screenshots
→ get_snapshot_details(id=...) for OCR text and task info
```

## Response Data Schemas

### Snapshot record
```json
{
  "id": 12345,
  "image_path": "screenshots/2025/06/...",
  "process_name": "chrome.exe",
  "window_title": "GitHub - Pull Request #42",
  "category": "Development",
  "timestamp": 1750006800000,
  "created_at": "2025-06-15 10:00:00",
  "page_url": "https://github.com/...",
  "visible_links": [{"text": "Files changed", "url": "..."}]
}
```

### OCR result block (default, without coordinates)
```json
{
  "id": 67890,
  "screenshot_id": 12345,
  "text": "def hello_world():",
  "confidence": 0.97
}
```
When `include_coords: true` is passed, each block also includes `"box_coords": [[x1,y1],[x2,y2],[x3,y3],[x4,y4]]`.

### Task cluster
```json
{
  "id": 5,
  "label": "Coding",
  "auto_label": "GitHub / chrome.exe",
  "dominant_process": "chrome.exe",
  "dominant_category": "Development",
  "start_time": 1750000000000,
  "end_time": 1750010000000,
  "snapshot_count": 47,
  "layer": "hot"
}
```

### Snapshot detail (with task)
```json
{
  "record": { ... },
  "ocr_results": [ ... ],
  "task": {
    "task_id": 5,
    "task_label": "Coding"
  }
}
```
`task` is `null` if the snapshot has not been assigned to any cluster.

## Best Practices

1. **Start with the cheapest query.** `search_ocr_text` is fast and runs locally; `search_nl` requires the Python service and is slower. Try OCR search first when the user's query contains specific text.

2. **Use filters to narrow results.** All search tools support `start_time`/`end_time` and `process_names` filters. Use them when the user provides temporal or application context — this dramatically improves relevance.

3. **Combine OCR text from multiple blocks.** A single screenshot may have many OCR blocks. When presenting content, concatenate the text fields from `ocr_results` to reconstruct the full visible text.

4. **Use task clusters for context.** When you find a relevant snapshot, check its `task` field. If it belongs to a cluster, mention the task label to give the user higher-level context about what they were doing.

5. **Paginate large result sets.** Use `offset`/`limit` for search results and `page`/`page_size` for task screenshots. Don't request more data than needed.

6. **Present timestamps in human-readable form.** Convert millisecond timestamps to the user's local time format when displaying results.

7. **Respect privacy.** The data may contain sensitive information (passwords, private messages, financial data visible on screen). Present information factually and don't draw unnecessary attention to sensitive content. If the user asks about sensitive data, just help them find it without editorializing.

## Example Conversations

### "What was the error message I saw earlier today?"

```
1. search_ocr_text(query="error", start_time=<today_start_ms>, end_time=<now_ms>, limit=10)
2. Review results — pick the one that looks like an error message by checking process_name and text
3. get_snapshot_details(id=<best_match_id>) for full OCR text
4. Present: "At 2:34 PM in Terminal, you saw: 'ConnectionRefusedError: [Errno 111] Connection refused' — this was while you were working on [task_label]."
```

### "What was I working on last Tuesday?"

```
1. get_task_clusters(start_time=<tuesday_start_ms>, end_time=<tuesday_end_ms>, hide_inactive=true)
2. Present task list with time spans:
   - "9:00–12:30: Writing CarbonPaper docs (VS Code) — 45 snapshots"
   - "13:00–14:20: Reviewing PRs on GitHub (Chrome) — 23 snapshots"
   - "14:30–17:00: Debugging MCP server (VS Code + Terminal) — 67 snapshots"
3. If user wants details, use get_task_screenshots(task_id=...) to drill in
```

### "Find that API documentation page I was reading"

```
1. search_ocr_text(query="API documentation", process_names=["chrome.exe", "msedge.exe"], limit=10)
2. If no good results, try: search_nl(query="API documentation webpage")
3. get_snapshot_details(id=<match_id>) — check page_url, window_title, and OCR text
4. Present: "Found it — you were reading 'Stripe API Reference' at https://docs.stripe.com/api at 3:15 PM on June 12th."
```

### "How much time did I spend on the report this week?"

```
1. get_task_clusters(start_time=<week_start_ms>, end_time=<now_ms>)
2. Filter tasks where label/auto_label/dominant_process relate to report writing
3. Sum up (end_time - start_time) for matching tasks
4. Present: "You have a task cluster 'Q2 Report — Word' spanning Mon 9am to Wed 2pm, with 156 snapshots. Total active time window: ~29 hours across 3 days."
```
