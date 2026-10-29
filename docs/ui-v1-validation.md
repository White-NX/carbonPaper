# UI foundation validation

Before implementation, baseline `8c11b04` passed 100 frontend files / 682 tests.
The foundation runs on Node.js 24.16.0. `npm ci` succeeded; the older
`i18next-parser` reports an engine warning for Node 24, while the project requires
Node 22.18.0 or newer.

## Automated checks

| Check | Result |
|---|---|
| `npm run build` | Passed: TypeScript and Vite production build |
| `npm run lint:ci` | Passed: frontend and browser-extension error gate |
| `npm run i18n:check` | Passed: translation keys and locale parity |
| `npm run test:security` | Passed: security guards and all 4 overlay-boundary tests |
| `npm run test:frontend` | Passed: 106 files / 705 tests |
| `git diff --check` | Passed |
| Synthetic review entry production transform | Passed: fixture modules transform and the review entry builds |
| Synthetic timeline time contract | Passed: both second and millisecond ranges return all 24 sample screenshots |
| Documentation links | Passed: all four UI documents resolve their relative links |

Final checks ran on 2026-10-29. The application entry is approximately 623 kB
after minification. The synthetic review build emits Vite's chunk-size warning
above 500 kB; both builds succeed. Git reports ordinary LF/CRLF conversion
notices for the rewritten components.

Coverage includes contextual Escape/focus restoration, explicit timeline actions,
search focus and result retention, records invalidation, saved window choices,
independent image/details loading, late-response suppression, lock clearing,
UTC navigation, copy/delete failures, native creation deduplication, bounded tabs
and standalone title/content hiding. An application integration test exercises
search → preview → copy → timeline → return, preserving the query and result
without another search request and restoring focus to the source result. A
second test checks that the global search shortcut is disabled while locked.

## Visual and native limits

The local synthetic server starts on port 1426. Browser Use reported “The browser
access host is unavailable”. Computer Use then stopped because it could not
determine the current Windows browser URL with enough confidence to enforce its
policy. No visual screenshots or native interactions were verified through
those tools.

Run `node scripts/ui-review.mjs` for the sample pages. See [baseline](ui-v1-baseline.md)
for URLs and review matrix. Screenshots and 125%/150% display-scale acceptance
remain roadmap tasks. Fixture adapters cannot validate Hello, real database
reads, source launching, capture or native window creation.

No Rust files changed, so this phase does not require Cargo checks. The real Tauri
app and app-bound service were not launched. The original worktree's uncommitted
migration work was not included or modified.
