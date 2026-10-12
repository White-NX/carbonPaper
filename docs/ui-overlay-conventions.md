# UI Overlay Conventions

This page is the rulebook for everything that covers the main window: startup
prompts, the lock screen, maintenance progress, migration dialogs and the
first-run wizard. It exists because these grew one at a time and ended up with
three visual styles, four stacking values and a dozen copies of the same
button class string. New overlays follow these rules; old ones are brought in
line when they are touched.

On-demand dialogs opened from a button (including recap corrections and
sources, the OCR repair card, and dialogs inside settings) use
`components/Dialog.jsx`; short decisions use `ConfirmDialog`.

Main and settings are separate WebViews. Each owns its DOM and focus stack.
Both provide `DialogVisibilityContext` from their authenticated session, so
protected dialogs disappear on lock without discarding their owner's state.
In the main window, ordinary dialogs also yield while any startup overlay
holds the coordinator. The settings discard-and-close confirmation explicitly
uses `allowWhenLocked`; it may appear above the lock but never above maintenance
or a fatal surface.

## Building blocks

| Need | Use | Location |
|---|---|---|
| Backdrop, card, header, footer | `OverlayShell` | `src/components/overlay/OverlayShell.jsx` |
| A progress bar with label and counts | `ProgressBlock` | `src/components/overlay/ProgressBlock.jsx` |
| Raw log lines behind a toggle | `LogDisclosure` | `src/components/overlay/LogDisclosure.jsx` |
| Any button | `Button` | `src/components/ui/Button.jsx` |
| Info, success, warning, error messages | `Banner` | `src/components/ui/Banner.jsx` |
| Stacking order | `OVERLAY_LAYERS` | `src/components/overlay/layers.js` |
| Polling a backend status | `usePolling` | `src/hooks/usePolling.js` |
| Turning a rejection into text | `formatError` | `src/lib/errors.js` |

`SettingsButton` is the same component as `Button`, re-exported for the
settings code.

Do not hand-write button, banner or progress-bar class strings. Do not use raw
palette colours such as `red-400`, `rose-400` or `blue-600` for states; use the
`ide-*` tokens (`ide-accent`, `ide-error`, `ide-warning`, `ide-info-success`)
through the components above.

## Layers

Every overlay declares one of four layers, from lowest to highest:

1. `prompt` — questions and offers the user can answer or put off.
2. `gate` — the Windows Hello lock.
3. `maintenance` — blocking index or database work. It sits above the gate
   because a run that waits for authentication offers its own unlock button.
4. `fatal` — the critical error window. Nothing may cover it.

`ErrorWindow` keeps its normal and illustrated modes. Both expose the same
labelled `alertdialog`, block Escape, and isolate the entire window, including
the title bar. Restart, exit, and presentation switching remain explicit actions.

Ordinary dialogs use the `dialog` and `confirmation` bands. The one approved
discard-and-close decision uses `lockedAction` above the gate. These bands also
live in `layers.js`; visibility and keyboard ownership follow the same rules.

Never write a `z-*` value on an overlay directly. If a new kind of overlay does
not fit one of these bands, add a band to `layers.js` and explain it there.

`OverlayShell` covers its positioned parent, not the window. Startup overlays
are rendered inside the main content area, so the title bar and its window
controls stay usable.

## Dismissal

An overlay that passes `onDismiss` can be closed with Escape. One that does not
pass it cannot be closed at all; that is how a blocking overlay is expressed.
Focus is trapped inside the card while it is open (`useDialogFocus`).

The focus stack uses the same priority as the visual layers, including when a
lower surface mounts later. Only the top surface handles Escape and Tab.
Background branches inside its covered area are `inert` and hidden from
assistive technology; previous attributes are restored when it leaves.
Autofocused form controls retain focus. Closing a child restores its trigger
inside the remaining dialog, and removing a covered parent never moves focus
away from the active child. Body scrolling stays locked until the last
dialog releases it.

Storage-directory migration stays open until its operation settles. Its view
declares `disableClose` and offers an explicit cancel action owned by
`useStorageMigration`. Cancellation is deduplicated, an accepted request waits
for the worker to settle, and a failed request leaves progress visible with a
retryable error. Cancelling before admission prevents the migration from
starting, including after authentication retries.

## One overlay at a time

`App` owns an overlay coordinator (`src/components/overlay/coordinator.js`).
Each startup overlay calls `useOverlaySlot(id, wantsToShow)` and renders only
when the coordinator grants it the screen. The grant goes to the highest entry
in `OVERLAY_PRIORITY` that is asking; the others wait their turn and appear
when it leaves. A component never shows itself on its own authority, and
`App.jsx` holds no "show A unless B is showing" conditions.

A new startup overlay needs an id in `OVERLAY_PRIORITY`, placed with a comment
that says why it ranks where it does.

While any overlay holds the screen, the top bar's search, capture control and
settings button are disabled (`TopBar`'s `interfaceLocked`). Window controls,
theme and notifications stay usable.

Outside a coordinator, as in component tests, every request is granted.

Debug previews go through one event: `showDebugOverlay(id, variant)` sends it
and `useDebugOverlay(id, handler)` receives it. The developer tools in the
settings window map their preview names to overlay ids in `useSettingsHost`.

## Structure: a hook and a view

Each overlay is split into:

- a hook that owns the backend traffic (polling, Tauri events, commands) and
  returns plain state plus action functions; and
- a view that receives that state as props and renders only shared components.

`useRequiredModelDownload` with `Mask` is the reference example. The split lets
a view be tested with plain props, lets debug previews render a view without
faking a backend, and keeps the question of *whether* to show (the hook's
answer, passed to `useOverlaySlot`) apart from *how* to show.

Polling uses `usePolling` (a `setTimeout` chain, so a slow call is never
overlapped). Tauri events use `useTauriEventListener`. Error text goes through
`formatError`.

## Text

User-facing text follows the [UI Text Guidelines in AGENTS.md](../AGENTS.md#ui-text-guidelines): say what
something does for the user, not how it is implemented. Model names, library
names, quantisation formats and SQL keywords (VACUUM, HMAC and similar) do not
appear in overlay text. Raw logs may contain them, which is why logs live
behind `LogDisclosure`.

One deliberate exception: a download the user is asked to start states its
approximate size, so they know what they are agreeing to. Sizes come from
`src/lib/modelSizes.js`, never from literals in a component or a locale string.

Translation keys are defined only in `src/i18n/locales/*.json`. New code does
not pass inline default strings to `t()`; the locale files are the single
source of truth, and `npm run i18n:check` verifies that both languages have the
same keys and that every literal key used in the source exists.

## Native file selection

`src/lib/native_dialogs.js` owns the three current file-picker workflows:
backup export destination, backup import archive, and storage directory.
Production code imports its named helpers instead of the Tauri dialog plugin.
They preserve archive filters, request a single selection, and normalize an
empty result to `null`. Invalid results and native failures reject for the
caller to handle. The plugin owns the OS dialog through the calling WebView.

Selecting a path does not authenticate or start work. A cancelled selection
returns to the existing form; the caller uses `withAuth` only after a path has
been selected. Python executable selection has been removed.

## Windows Hello entry points

Frontend verification goes through `src/lib/auth_api.js`:

| Trigger | Policy |
|---|---|
| Lock-screen unlock button | `requestAuth({ mode: 'unlock' })` initializes credentials, then verifies; missing-key initialization gets one retry. |
| Unlock button within blocking maintenance | `requestAuth({ mode: 'gate' })` verifies the existing setup and leaves the gate visible on failure. |
| Explicit protected action | `withAuth(operation, { promptMode: 'action' })` requests verification only after `AUTH_REQUIRED`, then retries within `maxRetries` (default one). Direct verification uses `requestAuth({ mode: 'action' })`. |
| Mount effects, polling, automatic restore, background work | `withAuth(operation, { promptMode: 'defer' })` reports `AUTH_REQUIRED` to the session UI and waits for the user to unlock. It never opens Hello. This is the default. |

`autoPrompt: true` remains a compatibility spelling for an explicit action;
`promptMode` takes precedence. Do not use it for automatic effects. Index-health
reads, automatic capture-filter application, and resumed smart-cluster downloads
(including their settings write) defer authentication. Resumed downloads keep
their queued intent until the authenticated readiness state returns.

Each WebView shares a pending frontend request. Cancellation becomes
`AUTH_CANCELLED`, provider unavailability becomes `AUTH_NOT_AVAILABLE`, and
unrelated errors propagate. A failed or cancelled attempt releases the pending
request so a later explicit click can retry; it does not schedule another prompt.
