# Promptlist

Turn a request into a Spotify playlist made from your own music library.

Local-first Rust application with a floating prompt composer, live progress,
scrollable track queues, editable previews and private Spotify export. Library
snapshots and prompt history persist locally; syncing is always manual.

Refine a selection conversationally with the second prompt field. Each turn
retains previous preferences and creates a linked history entry. Simple count
reductions preserve the current order without another lookup. Play buttons
start the account-linked Spotify player in the page header.

## Run Locally

Requires a current Rust toolchain and a Spotify developer application with Web
API access. Register this exact redirect URI in the Spotify app:

```text
http://127.0.0.1:8787/callback
```

Create `.env.local` from the variable names in `.env.example`:

```dotenv
SPOTIFY_CLIENT_ID=your_client_id
SPOTIFY_REDIRECT_URI=http://127.0.0.1:8787/callback
LASTFM_API_KEY=your_lastfm_key
OPENAI_API_KEY=your_openai_key
OPENAI_MODEL=gpt-5.4-mini
```

Keys may be unquoted. Never commit them. Spotify uses PKCE, so a client secret
is not required. Last.fm supplies community tags; OpenAI translates requests
that the local parser cannot handle. Missing providers reduce matching quality
but do not prevent the local fallback. Provider access and quotas depend on
your accounts and current provider rules.

```sh
cargo run --locked --bin promptlist
```

Open [127.0.0.1:8787](http://127.0.0.1:8787), connect Spotify and sync your library.
Build a preview, adjust the selection and export when ready. Coverage is an
optional diagnostic, not a prerequisite. Restarting requires reconnecting;
the completed library snapshot remains available without another sync.

## Matching

- Candidates come only from Liked Songs and readable saved playlists, including
  collaborative and followed sources when Spotify grants access.
- Genre filtering considers the entire eligible library before numeric filters.
  There is no 100-candidate cutoff; the output playlist limit is 100 tracks.
- Exact metadata/numeric filters coexist with best-effort subjective requests.
  Community tags and artist priors are imperfect evidence, not measured moods.
- ReccoBeats supplies available numeric features. Missing information is not
  fabricated. First uncached requests can be slower than subsequent requests.
- Spotify API restrictions can exclude sources. Their reasons remain visible.

## Architecture And UI

Axum, Tokio, Maud, reqwest and SQLite. All playlist logic, provider adapters,
authorization, persistence and HTML rendering are Rust. A small dependency-free
browser controller updates progress/results without document reloads. A separate
controller integrates Spotify's JavaScript Web Playback SDK; playlist decisions
remain in Rust. No Node build or frontend framework is required to run the app.

See [UI_COMPONENTS.md](UI_COMPONENTS.md) for the component map, design tokens
and the efficient workflow for consistent interface changes.
The painting-led Island Stone theme uses optional locally supplied artwork and
Satoshi fonts. See [theme assets](assets/THEME_ASSETS.md) for setup and licensing.
These binaries are not redistributed in this repository; absent assets fall
back to a solid graphite background and system typography.

## Privacy And Safety

- Binds only to loopback. Hosted mode requires an HTTPS reverse proxy and an
  explicit Spotify account allowlist; it is not an open-signup service.
- `.data/` contains private account-scoped SQLite snapshots, prompt/results
  history and app-action records. Do not commit or publicly upload it.
- History remains until account deletion; action records expire after 90 days.
  Compact daily action/outcome/timing summaries remain for 365 UTC days and are
  deleted with the account. Private audit exports include both forms of data;
  upgrade the application database before using the matching export binary.
  No external analytics or model training pipeline is enabled. The selected
  [serverless hosting plan](deploy/README.md) is not deployed yet.
- OpenAI receives request text and static schemas, not your Spotify library.
  Refinement adds the original request and previous prompt-derived preferences,
  never result tracks, account identity or activity logs.
  Last.fm receives artist/title lookups; ReccoBeats receives candidate track IDs.
- Provider cache directives are honored. No-store numeric features are transient.
- Tokens remain in memory. The playback SDK receives the short-lived access token
  through an authenticated, same-origin, CSRF-protected, no-store endpoint.
  Refresh tokens stay server-only; no token is stored in browser storage.
  Signing out retains saved local state; confirmed
  account-data deletion clears that account's records. Separate backups and
  developer exports must be deleted separately.
- CSRF/origin checks, preview nonces and ambiguous-write protections guard
  Spotify exports. The browser never automatically retries a write.
- The SDK connects when signed in but audio starts only on a playback action.
  Promptlist never proxies, downloads or analyzes Spotify audio.

## Spotify Playback

Enable **Web API** and **Web Playback SDK** in the Spotify app dashboard.
Use a Premium account, then reconnect to approve `streaming`, `user-read-email`,
`user-read-private` and `user-modify-playback-state`, alongside existing library
and private-playlist scopes. No client secret or extra API key is needed.
The email scope is required by the SDK; Promptlist does not persist email.

The header radio shows the SDK's current track/artwork, play/pause, previous/next,
seek and volume. Row Play activates browser audio in the click handler and starts
the selected song plus the remaining preview order on the explicit browser device.
Rust verifies session, CSRF, account, current preview nonce and track membership.
No arbitrary Spotify API proxy, automatic write retries or embed double-click.
Switching app views and refreshing results preserves the player. A full browser
reload reconnects it; playing here can move playback from another Spotify device.

Desktop (1024px+) uses a fixed viewport with internal scrolling. Mobile keeps a
normal scrolling page and a two-row header. iOS volume uses hardware controls.
Browser protected-media/autoplay restrictions, account eligibility and regional
availability still apply; genuine account playback needs live acceptance.
Private development testing does not imply Spotify commercial streaming approval.

Official implementation references (checked 2026-10-28):
[SDK requirements](https://developer.spotify.com/documentation/web-playback-sdk),
[SDK methods/events and activation](https://developer.spotify.com/documentation/web-playback-sdk/reference),
[web player tutorial](https://developer.spotify.com/documentation/web-playback-sdk/howtos/web-app-player),
[start playback API](https://developer.spotify.com/documentation/web-api/reference/start-a-users-playback).

## Verify

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
```

Live-provider tests are ignored by default and require explicit opt-in. For
synthetic browser acceptance, run this fixture in a separate terminal:

```sh
cargo test --lib browser_fixture -- --ignored --nocapture
```

Then run `tests/browser-acceptance.cjs` with Node and Playwright installed.
`PLAYWRIGHT_MODULE` may point to an existing Playwright package;
`PLAYWRIGHT_CHANNEL` defaults to `chrome`. Tests use port 8788 and synthetic
data only, including a fixture-only SDK simulator with no Spotify audio/network.
They cover progress without navigation, export, history, edits, player controls,
scrolling and desktop/mobile layout. Simulator checks are not real DRM/autoplay
or Premium account acceptance.

Local use requires no AWS resources or other music projects. The optional
[hosting guide](deploy/README.md) describes the proposed small-server release;
the presence of deployment files does not mean a cloud instance is running.
Vendored Lucide icons
retain their license in `assets/LUCIDE-LICENSE`.
