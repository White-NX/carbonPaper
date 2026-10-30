# UI v1.0 roadmap

Work proceeds on `feat/ui-v1-foundation`, starting at `8c11b04`. The goal is
predictable actions, consistent screenshot access and reliable returns. The app
version remains 0.9.1 until the release workflow explicitly bumps it.

## Phase 1 — Baseline and conventions

- [x] Record the five supplied surfaces, strengths and interaction gaps.
- [x] Define navigation, preview, return, focus and capture-time contracts.
- [x] Add a local synthetic fixture for reproducible comparisons.
- [x] Record validation results and visual/native limits.

References: [baseline](ui-v1-baseline.md),
[workspace conventions](ui-workspace-conventions.md).

## Phase 2 — Application frame and preview

- [x] Expand labelled task navigation by default and persist collapse choice.
- [x] Preserve title-bar quick search and submit its query/mode to the full-search page.
- [x] Add a workspace header and explicit source return action.
- [x] Share one viewer across records, source dialogs and native windows.
- [x] Align card/citation defaults while honoring saved preferences.
- [x] Keep completed search results across navigation; refresh on records changes.
- [x] Keep details beside the image and copy/source/window controls visible.
- [x] Discard preview image/details on lock and ignore late responses.
- [x] Cover copy, return, preferences, UTC navigation, deletion and window creation.

Native acceptance and screenshots are pending in phase 4. Tests and fixtures do
not establish Windows Hello or end-to-end native behavior.

## Phase 3 — Page layouts (todo)

- [ ] Search: distinguish method, target and filters; preserve its useful hierarchy
      and clarify active date scope.
- [ ] Records: clarify time-range versus image-zoom controls and review density at
      smaller windows, preserving the horizontal timeline.
- [ ] Recap: constrain reading width, separate overviews from activities and remove
      repeated overview/detail text.
- [ ] Topics: present topic, recent additions and sources before the complete
      generated report; move model/queue details into generation status.
- [ ] Align empty/populated local action headers as a set.
- [ ] Capture synthetic before/after images in both languages and themes.

Acceptance: the three source journeys preserve query/date/topic context; content
fits the review matrix; each page's main action is apparent without hover.

## Phase 4 — States and native acceptance (todo)

- [ ] Review empty, loading, failed, paused, locked and partial states with shared
      buttons, banners and progress components.
- [ ] Simplify lock-screen description/backdrop while retaining the shared gate,
      startup coordination and explicit Hello verification.
- [ ] Audit keyboard navigation, nested source-dialog focus, tooltips and labels.
- [ ] Test the real Tauri app at 100%, 125% and 150% Windows scaling, narrow windows,
      long English text and light theme.
- [ ] Validate native creation/closing, Hello cancellation/expiry and reauthentication
      in an authorized local test session.
- [ ] Record search → preview → copy → return, recap → source → return, and topic
      citation → preview → timeline → return.
- [ ] Review captured artifacts for personal data before including them in a PR.

Acceptance: covered states protect content on lock; source journeys retain context;
native recordings match the agreed operations.

## Follow-up ownership

Continue on this branch or a worktree based on its final commit. Update this roadmap
as scope changes and use existing overlay, dialog, locale and authenticated API
primitives. See [validation](ui-v1-validation.md) for completed checks and pending
visual/native acceptance.
