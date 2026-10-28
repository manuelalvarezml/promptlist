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
| `src/web.rs` | Routes, authorization and job state; not a styling entry point |
| `tests/browser-acceptance.cjs` | Synthetic workflow and responsive regression checks |

## Design Contract

Graphite background, off-white type, electric lime for primary actions, ice and
coral accents. Sans-serif body and large prompt; monospace only for small
technical accents. Prompt is unframed. Sections are unframed or separated by
rules, not nested cards. Real library artwork supplies the imagery.

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
3. Keep presentation in CSS and Rust markup. Browser JavaScript only transports
   forms/status; never duplicate ranking, validation or provider decisions there.
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

Never add automatic POST retries, remove CSRF/preview protections, or collect
keystrokes as part of UI updates. New actions need explicit review before they
join the transport allowlist.
