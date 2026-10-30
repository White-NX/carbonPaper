# Workspace and screenshot conventions

Use these rules for the main window and screenshot previews alongside
[UI overlay conventions](ui-overlay-conventions.md).

## Application frame

The title bar owns brand, quick search, recording status, settings, notifications,
theme and native window controls. Text and picture queries show inline results;
Enter or the view-more action sends the query and mode to the full-search page.
Ctrl+K focuses the title-bar input without leaving the current workspace or
clearing its query. Startup gates and modal dialogs retain keyboard ownership.

Navigation names user tasks: Records, Search, Recap and Topics. Page IDs remain
`preview`, `advanced-search`, `recap` and `smart-cluster` for compatibility.
`WORKSPACE_PAGES` owns labels and order. Labels expand by default at desktop
widths with a saved collapse preference and an icon rail at narrow widths. Both
presentations expose labels and `aria-current`.

`WorkspaceHeader` identifies the current workspace. `PageHeader` holds local
actions, modes and filters. Topic selection and recap tasks provide local context.
Avoid stacking equivalent page titles. Recording status has visible text or an
accessible label at narrow widths; use semantic `ide-*` tokens for states.

## Opening and returning

| Operation | Destination |
|---|---|
| Click a title-bar quick-search result | Records viewer and timeline |
| Click a full-search, recap or topic screenshot/citation | Contextual dialog in the same workspace |
| Click a records timeline thumbnail | Records viewer below the timeline |
| “View in timeline” | Records with an explicit return action |
| “Separate window” | Native window with the same viewer and bounded tabs |
| Close / Escape | Close the top dialog and restore source focus |
| Back / Alt+Left | Source workspace with its query, filters and reading selection |

`useSnapshotOpenPreference` resolves all missing per-source preferences to
`preview`. Existing `cardClickBehavior_search`, `cardClickBehavior_recap` and
`cardClickBehavior_clusters` values keep working. A saved `standalone` choice
selects the native window; the alternate action remains available. Topic cards
and citations obey the same preference.

Keep workspaces mounted during preview and navigation. Completed results survive
a visit to Records; pending search responses are ignored after leaving. A records
revision or explicit refresh invalidates retained results. Large originals in
hidden picture-result cards are released and reloaded on visibility.

## Screenshot viewer

`SnapshotViewer` owns the action row, original image and information pane.
Records supply selected screenshot data; dialogs and native tabs use
`useSnapshotPreviewData`. Image and metadata complete independently.

Actions provide copy text, related content, timeline navigation, separate window
and information-pane controls as applicable. Deletion lives in the records menu
and uses `ConfirmDialog`. Source choosers open downward from top action rows. Use
`Button`, `Banner`, `MenuPanel` and `MenuItem` for new actions and feedback.

Details and recognized text use labelled keyboard tabs. The pane is manually
collapsible with saved visibility, never timer-dismissed or covering the image.
At 760 CSS pixels or less it stacks below the image. Content layouts may differ;
shared actions retain their meaning and placement.

## Time, data and authentication

Prefer screenshot `record.created_at` for navigation; otherwise use the normalized
capture instant. Keep `captured_at_ms` literal, including zero and unknown/null,
and preserve UTC parsing. Never substitute OCR completion for capture time.

Reads and writes stay behind existing API wrappers and authentication patterns.
Contextual previews use `Dialog` for priority, focus isolation, Escape and lock
visibility. Native previews own a session and shared Hello gate; viewer content
and native titles disappear on lock. Selected IDs can survive, but images and
details are discarded and reloaded after unlock.

Persist sanitized descriptors, never base64 images, extracted text or complete
details. Native tabs remain capped at eight. Window events synchronize descriptors
and do not bypass backend authentication.

## Visual language

Retain light/dark theme tokens and the restrained accent. Use that accent for
navigation selection, focus and actions. Screenshots occupy the canvas; metadata
sits on the panel. New user-action labels belong in both locale files and use
`t()`. Recap/topic reading widths and technical-status disclosure remain phase 3
decisions in [the roadmap](ui-v1-roadmap.md).
