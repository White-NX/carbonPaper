# UI Overlay Conventions

This page is the rulebook for everything that covers the main window: startup
prompts, the lock screen, maintenance progress, migration dialogs and the
first-run wizard. It exists because these grew one at a time and ended up with
three visual styles, four stacking values and a dozen copies of the same
button class string. New overlays follow these rules; old ones are brought in
line when they are touched.

On-demand dialogs opened from a button (confirmations, the OCR repair card,
dialogs inside settings) keep using `components/Dialog.jsx`. This page is about
overlays the app raises by itself.

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

Never write a `z-*` value on an overlay directly. If a new kind of overlay does
not fit one of these bands, add a band to `layers.js` and explain it there.

`OverlayShell` covers its positioned parent, not the window. Startup overlays
are rendered inside the main content area, so the title bar and its window
controls stay usable.

## Dismissal

An overlay that passes `onDismiss` can be closed with Escape. One that does not
pass it cannot be closed at all; that is how a blocking overlay is expressed.
Focus is trapped inside the card while it is open (`useDialogFocus`).

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

User-facing text follows the UI Text Guidelines in `CLAUDE.md`: say what
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
