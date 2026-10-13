# UI Overlay Conventions

This page defines the shared rules for dialogs, startup prompts, lock screens,
maintenance progress, onboarding, and native prompts in the main and settings
windows. New surfaces use the existing primitives, layer tokens, and interaction
policy. Existing surfaces follow these rules when they are changed.

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

## Current surface inventory

| Surface | Owner and primitive | Dismissal or completion |
|---|---|---|
| Recap corrections, sources and diagnostics; OCR repair | Main window, `Dialog` | Close/Escape/backdrop, with close disabled during an active operation. |
| Smart Cluster and recap deletion; settings deletion and token decisions | Calling window, `ConfirmDialog` | Explicit confirm/cancel; pending work disables cancellation. |
| Backup import/export, storage-location choice, Agent Access privacy | Settings window, `Dialog` | Close/Escape/backdrop while idle; backup processing disables close. Typed confirmations stay within the existing form. |
| Storage-directory migration progress | Settings window, `Dialog` and `useStorageMigration` | Explicit cancellation waits for the operation to settle; no generic close. |
| Unsaved settings discard decision | Settings window, `ConfirmDialog` with `allowWhenLocked` | Explicit discard-and-close or keep editing; remains available over the lock. |
| Windows Hello lock (`auth`) | Each window, `AuthMask` / `OverlayShell`, `gate` | User unlocks explicitly; no dismissal. |
| Fatal error (`fatal`) | Main window, `ErrorWindow`, `fatal` | Restart/exit and presentation switch only; covers the title bar too. |
| Rust index maintenance (`maintenance`) and startup database cleanup (`vacuum`) | Main window, `OverlayShell`, `maintenance` | Complete automatically; maintenance offers its own unlock action when required. |
| First-run onboarding and What's New (`onboarding`) | Main window, `OnboardingOverlay` / `OverlayShell`, `prompt` | Wizard navigation follows its step state; What's New can defer while idle. Applying choices blocks dismissal. |
| Update offer/progress (`update`) | Main window, `UpdateModal` / `OverlayShell`, `prompt` | Explicit later/hide/retry controls; hiding progress leaves the operation running. |
| Required-model download (`modelDownload`) | Main window, `Mask` / `OverlayShell`, `prompt` | Explicit background/retry actions; shares the download controller with onboarding. |
| Search-index security upgrade (`hmac`) | Main window, `HmacMigrationDialog` / `OverlayShell`, `prompt` | Close/Escape hides progress while the upgrade continues; admission waits for authentication. |
| Optional history indexing (`clipBackfill`) and protected processing offer (`appBound`) | Main window, `OverlayShell`, `prompt` | The former accepts a decision or dismissal; the latter offers explicit enable/repair/later actions. |

`SettingsWindow` owns a separate WebView rather than an outer settings dialog.
Python executable/setup/install prompts, `SecurityAlertMask`,
`ClusteringSetupWizard`, `ActivityContextDrawer`, and the old extension and
Smart Cluster setup wizards are retired. Their inventory entries are not new
migration work. No new `Modal`, `WizardDialog`, or `Drawer` abstraction is required
to implement the current workflows.

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

Startup surfaces declare one of four layers, from lowest to highest:

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

An `OverlayShell` that passes `onDismiss` handles Escape through that action.
Its backdrop does not dismiss it. Without `onDismiss`, Escape is blocked;
footer actions follow the view's state. An action may mean going back a wizard
step or hiding ongoing progress, so it must not imply cancellation unless
cancellation is actually supported. `Dialog.disableClose` and
`ConfirmDialog.loading` similarly block generic dismissal. Focus stays inside
the active card (`useDialogFocus`).

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

The current grant order is `fatal`, `maintenance`, `vacuum`, `auth`,
`onboarding`, `update`, `modelDownload`, `hmac`, `clipBackfill`, then `appBound`.
This controls which startup view renders; CSS layer bands control stacking
against ordinary dialogs and the explicit settings discard decision.

While any overlay holds the screen, the top bar's search, capture control and
settings button are disabled (`TopBar`'s `interfaceLocked`). Window controls,
theme and notifications stay usable during startup prompts, authentication,
and maintenance. The fatal surface isolates the whole window.

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

User-facing text explains capabilities and useful choices. Model names, library
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
reads, automatic capture-filter application, automatic search-index security
upgrades, and resumed smart-cluster downloads (including their settings write)
defer authentication. Resumed downloads keep their queued intent until the
authenticated readiness state returns. The security-upgrade component mounts
only while authenticated; a lost session defers admission until its next mount.

Each WebView shares a pending frontend request. Cancellation becomes
`AUTH_CANCELLED`, provider unavailability becomes `AUTH_NOT_AVAILABLE`, and
unrelated errors propagate. A failed or cancelled attempt releases the pending
request so a later explicit click can retry; it does not schedule another prompt.

The backend credential state also coordinates in-flight verification across
windows. The first request supplies the native owner HWND; overlapping requests
share its success, cancellation, or failure and its post-authentication work.
The application owns the job, so closing a waiting window does not admit another
prompt while native verification is running. Native verification runs on a
blocking worker rather than holding the async executor. Once the job settles,
a later explicit request starts fresh verification even with a valid session;
the retained-key and fresh-consent rules remain in `credential_manager.rs`.

## UAC boundaries

UAC is owned by Windows and the elevated installer, outside the DOM focus stack.
It follows an explicit protected action; rendering an offer or reading its status
does not start elevation.

| Entry point | Elevation and cancellation |
|---|---|
| `app_bound_install` (enable or repair) | Verifies the packaged setup executable, then uses `runas` in `app_bound.rs`. Windows cancellation maps to `APP_BOUND_CANCELLED`; the caller remains available for an explicit retry. |
| `app_bound_uninstall` | Verifies the installed setup helper, then uses the same UAC/cancellation contract. |
| `updater_install` for a protected installation | Installs through `app_bound::install_runtime` before stopping capture. Cancelling UAC keeps the current application and monitor running. |
| `updater_install` for an ordinary installation | A detached update script runs after application exit. It requests elevation only if the destination fails a write-access probe. Cancellation/failure is logged to `%LOCALAPPDATA%/CarbonPaper/update_error.log`; there is no in-app cancellation response after exit. |

These service and updater paths remain Rust-owned. Python installation and
executable selection are not part of the current product. Interactive checks
must distinguish protected installation updates from the ordinary update script;
their cancellation lifetimes differ.

## Enforcement

`npm run lint:ci` uses AST rules to reject production dialog-plugin imports,
re-exports, dynamic imports, and `require()` outside `native_dialogs.js`. Tests
may mock the plugin directly. Direct imports of `useDialogFocus` belong only
to `Dialog`, `ConfirmDialog`, `OverlayShell`, and `ErrorWindow`; ordinary views
use those components and may read `useDialogVisibility` or its context.

`npm run test:security` retains the browser-confirmation and backend guards
and exercises these ESLint boundaries. The checks preserve the translation
rules and allow normal popover positioning; they do not ban every `fixed` or
`z-*` class in the frontend.

The frontend regression suite covers lock/startup suspension, fatal and nested
keyboard ownership, focus and scroll restoration, migration cancellation,
picker result handling, and prompt policy. Rust credential tests cover concurrent
request fan-out, cancellation, later retries, dropped callers, worker failure,
and retained-key consent rules. Run `npm run test:frontend`, `npm run build`,
`npm run lint:ci`, `npm run i18n:check`, `npm run test:security`,
`cargo check --manifest-path src-tauri/Cargo.toml`, and
`cargo test --manifest-path src-tauri/Cargo.toml --lib credential_manager::`
for these boundaries.

Automated tests do not establish native window ownership, secure-desktop focus,
or OS cancellation behavior. Report actual two-window Hello, native file-picker,
and UAC interactions separately from mock-based and isolated native unit tests.
