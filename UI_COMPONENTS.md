# UI Components

## Small Reading Map

Read this file first, then only the component and CSS section being changed.
Do not load provider logic or the entire server module for a styling task.

| File | Responsibility |
| --- | --- |
| `src/ui.rs` | Shared icon, navigation, library summary, job status and scroll region primitives |
| `src/web/views.rs` | Composer, preview actions, metadata details, history and page composition |
| `assets/style.css` | Shared tokens, control styles, layouts and responsive rules |
| `assets/app.js` | Same-origin form transport, status polling, result patching and local UI preservation |
| `assets/player.js` | SDK lifecycle, in-memory playback queue, state-driven radio controls |
| `src/web/playback.rs` | Account/CSRF-bound SDK token and current-preview playback endpoints |
| `src/web.rs` | Routes, authorization and job state; not a styling entry point |
| `src/refinement.rs` | Prompt-only conversational state, bounded context and local refinement fallback |
| `tests/browser-acceptance.cjs` | Synthetic workflow and responsive regression checks |
| `tests/flow-acceptance.cjs` | Connection gate, merged Library, local/web mode and auto-sync transport checks |

## Design Contract

Island Stone theme: supplied painting fills the viewport, unchanged. A graphite
scrim and mild desaturation start below the clear 104px header (166px on mobile).
Satoshi Black 900 wordmark, regular/medium/bold body type. Exact sampled bone
`#ebe5d9`, graphite `#181a19`, moss `#444d48`, water `#2a2c2b`, lichen `#aaa970`,
limestone `#bfaf8d` and sand `#c9b495`; no cobalt or neon accents.
Large prompt, track titles, playlist name and refinement text. All text inputs
are transparent with minimal rules. Sections remain unframed; only real menus
have opaque surfaces for legibility. The radio sits directly in the header.
No slogans or promotional copy.
The build view has only compact branding/navigation before the composer/list;
library totals belong on Library. Completed-success progress is hidden.

Disconnected users see only the wordmark, necessary status and a centered Connect
Spotify action over the painting. Connected views share one stable tab row with
no duplicate page heading. Library owns Tracks/Sources/Excluded subviews and fills
the remaining desktop height. Textareas cannot be manually resized.

Only local runtime exposes the bottom-corner Web/Local switch. Its cookie affects
presentation, never authentication. Web mode hides evidence, diagnostics and
Coverage; hosted configuration enforces this regardless of query/cookie values
and rejects the Coverage route. Local counts distinguish current enrichment from
historical numeric matches; source counts show actual unique snapshot memberships.

Reference: the Maria Norris Esparza clear-strip/filtered-photo treatment and
Kraf Engineering's Satoshi Black typography, adapted to a compact working app.
The reusable ten-color palette lives in the parent workspace's
`design/palettes/island-stone/` (JSON, CSS, swatches and exact pixel provenance).
See `assets/THEME_ASSETS.md` for local asset setup and redistribution boundaries.

Keep CSS colors, typography and corner radius centralized in `:root`. Never
scale type with viewport width or use negative letter spacing. Fixed controls
and grid tracks must not jump when content changes. Desktop/mobile breakpoints
use explicit type sizes. Respect reduced motion.

Use existing local Lucide icons for tool actions with accessible names and
tooltips. Use native details, popovers, inputs, progress and keyboard-focusable
scroll regions. Long queues and tables scroll internally. Avoid decorating
ordinary sections as cards or adding explanatory developer copy to the UI.

## Change Workflow

1. Update an existing token or shared primitive when its semantics are the same.
   This updates every caller and avoids repeated styling instructions.
2. Add a component only for a genuinely new reusable pattern or substantial
   self-contained interaction. Keep page-specific composition in views.rs.
3. Keep presentation in CSS and Rust markup. Browser JavaScript transports
   forms/status and integrates the Spotify SDK; never duplicate ranking or
   server authorization decisions there.
4. Update this map only when ownership or contracts change. Keep historical
   implementation narratives out of this short reference.
5. Run Rust tests and the synthetic browser acceptance check. Inspect screenshots
   at desktop, 390px and 320px; verify controls, scrolls, keyboard focus and text.

## Live Update Contract

`GET /api/job` requires the current connected session and returns only its job
identifier, running state and Rust-rendered status markup. Responses are no-store.
Polling does not call providers or record page views. Determinate progress is
per-stage measured work, never a fabricated overall percentage.

The browser intercepts an explicit allowlist of same-origin forms. It submits
each action once, patches status while running and patches the result after
completion. Composer values, focus/caret and scrolling are preserved. OAuth,
sign-out and deletion retain native form navigation. With JavaScript disabled,
forms still work and Check progress manually retrieves the current state.

A successful OAuth connection persists a one-shot auto-sync marker. The rendered
auto-sync form is submitted once using existing CSRF/job transport. The server
consumes the marker; page refreshes, polling and duplicate requests do not launch
another completed sync. Jobs retain account locks, durable checkpoints and atomic
snapshot promotion. Header progress shows measured per-stage work, or indeterminate
progress when the provider total is unknown. Without JavaScript, use Sync library.

Never add automatic POST retries, remove CSRF/preview protections, or collect
keystrokes as part of UI updates. New actions need explicit review before they
join the transport allowlist.

## Refinement And Player

The second prompt submits `/refine` with CSRF and the current preview nonce.
The server resolves the parent run within the connected account; it never trusts
browser-supplied conversation state. Each refinement is a new linked history
run. Prompt-only original request, previous structured preferences and latest
message form the bounded AI context. Six recent messages are shown locally;
the full chain remains in account-scoped history. Simple count reductions keep
the current order without model/provider calls. General changes rerank the
eligible library with the prior preferences. Explicit advanced filters remain.

Player buttons use `preview_button`; `ui::player` owns one stable header radio.
`app.js` retains its DOM node and playback status across result and view patches.
`player.js` preconnects the SDK when authenticated, but never autoplays on login.
Track clicks activate audio synchronously before Rust authorizes the preview queue.
SDK state drives title, artwork, seek and play/pause; queue navigation remains
available on other app views. The official SDK owns its hidden audio iframe.
CSP permits SDK scripts/frames at sdk.scdn.co, not arbitrary embeds. Never scrape
preview URLs or proxy/download audio. Keep track links to Spotify. Browser/account
restrictions remain possible; error text is small and unboxed beside the wordmark.

At 1024px+ the body is a viewport-height grid: header, minmax(0,1fr) workspace,
30px footer. Composer, results and non-build views scroll internally. Never hide
content without a scrollable ancestor. Below 1024px the radio gets its own header
row and normal page scrolling returns. Check 1440/1280/1024 and 390/320 widths.
