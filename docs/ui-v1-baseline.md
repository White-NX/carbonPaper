# UI v1.0 baseline

This baseline records the five surfaces supplied for the v1.0 UI review and the
interaction behavior at commit `8c11b04`. The foundation lives on branch
`feat/ui-v1-foundation`. The original screenshots contain personal history and
remain outside Git; review fixtures use invented content.

## Baseline surfaces

| Surface | Strength to preserve | Problem observed | Foundation response |
|---|---|---|---|
| Windows Hello lock | One clear unlock action | Repeated encryption text and a busy blurred background | Keep the existing gate and coordinator. Visual simplification is phase 4 work. |
| Records and timeline | Horizontal time navigation and a large original | Details cover the screenshot; actions are split between corners and a hover-only bottom bar | Shared viewer with a stable action row and collapsible information pane. |
| Advanced search | Useful query, mode, filter and result hierarchy | Two query owners; a result click replaces the workspace | One title-bar entry focuses the page's field. Results open a contextual preview. |
| Daily recap | Time periods and activities support recall | Long lines, repeated overview/detail text and a different source destination | Sources use the shared viewer. Reading layout remains phase 3 work. |
| Smart clusters | Collection list, document and evidence | Technical status competes with the topic; cards and citations differ | Navigation calls this “Topics”; cards and citations share one default. Reading layout remains phase 3 work. |

Search cards formerly defaulted to the main preview, recap and cluster cards to
a native window, and topic citations always preferred that window. Saved
per-source preferences remain compatible; missing preferences now resolve to
the same contextual-preview default.

## Repeatable review setup

Use Node.js 22.18.0 or newer. From the UI worktree:

```powershell
npm ci
node scripts/ui-review.mjs
```

Open `http://127.0.0.1:1426/tools/ui-review/?page=search`. The server binds only to
loopback. Its Tauri imports resolve to fixture adapters; it does not access the
app database, registry, capture service or credentials. The production entry
does not include those adapters. Stop the server with Ctrl+C.

| Parameter | Values |
|---|---|
| `page` | `records`, `search`, `recap`, `topics`, `preview`, `standalone`, `lock` |
| `lang` | `zh-CN` (default), `en` |
| `theme` | `dark` (default), `light` |

The fixture clock stays on 2026-10-28, with 24 invented screenshots, two recap
activities and one topic. The original is a local SVG of invented design notes.
Pick a timeline thumbnail for the records viewer, a search thumbnail for the
dialog, and the topic for citations. `standalone` exercises native-window
content using a browser adapter; it does not establish real window creation.

The mock source opener logs its URL; native creation is a stub. The fixture does
not validate real source launching, deletion, generation, Hello or capture.

## Review matrix

Compare surfaces at 1300×750 and 1000×700 CSS pixels in both themes and languages.
Review the viewer at 600×460, where its information pane stacks below the image.
Native acceptance additionally covers Windows 125% and 150% display scaling.
Record viewport, display scale and theme with each image; browser zoom is not a
substitute for the native display-scale check.

Save synthetic comparison images under `docs/images/ui-v1/` when browser capture
is available. Keep images of real local history outside the repository.

## Task-based acceptance

1. Search → screenshot → copy → close: preserve query, filters, results, scroll
   position and source focus.
2. Search screenshot → timeline → back: return to the original workspace. A
   records-change notification invalidates retained search results.
3. Recap activity → source → close: preserve date, activity and reading position.
   Nested source dialogs recover their previous focus.
4. Topic citation → source → timeline → back: keep the collection selected.
5. Native preview → switch tabs → timeline: use stored UTC capture time, not OCR
   completion time.
6. Lock during loading: hide main and native content and discard image/detail
   responses. Unlock reloads the same selected screenshot.
7. Cancel deletion: no delete command runs. Failed deletion returns to the viewer
   with an error rather than pretending the record disappeared.

See [design conventions](ui-workspace-conventions.md),
[roadmap](ui-v1-roadmap.md) and [validation](ui-v1-validation.md).
