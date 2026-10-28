# Promptlist

Turn a request into a Spotify playlist made from your own music library.

Local-first Rust application with a floating prompt composer, live progress,
scrollable track queues, editable previews and private Spotify export. Library
snapshots and prompt history persist locally; syncing is always manual.

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
browser controller updates progress/results without document reloads. It has
no provider credentials or matching logic. No Node build or frontend framework
is required to run the application.

See [UI_COMPONENTS.md](UI_COMPONENTS.md) for the component map, design tokens
and the efficient workflow for consistent interface changes.

## Privacy And Safety

- Binds only to loopback. This is a local application, not a hosted service.
- `.data/` contains private account-scoped SQLite snapshots, prompt/results
  history and app-action records. Do not commit or publicly upload it.
- History remains until account deletion; action records expire after 90 days.
  No external analytics or model training pipeline is enabled.
- OpenAI receives request text and static schemas, not your Spotify library.
  Last.fm receives artist/title lookups; ReccoBeats receives candidate track IDs.
- Provider cache directives are honored. No-store numeric features are transient.
- Tokens remain in memory. Signing out retains saved local state; confirmed
  account-data deletion clears that account's records. Separate backups and
  developer exports must be deleted separately.
- CSRF/origin checks, preview nonces and ambiguous-write protections guard
  Spotify exports. The browser never automatically retries a write.

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
data only. They cover progress without navigation, export, history, edits,
scrolling and desktop/mobile layout.

No AWS resources or other music projects are required. Vendored Lucide icons
retain their license in `assets/LUCIDE-LICENSE`.
