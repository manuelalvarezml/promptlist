# Persistence, Cookies and Product Measurement

Decision date: 2026-11-02. Scope: standalone Promptlist, not other music projects.

## Decision

Reuse the existing private Lambda, DynamoDB and S3 deployment. Keep Spotify PKCE
authentication and an opaque first-party session cookie. No Cognito migration,
analytics vendor, advertising SDK, tracking pixel or new always-on service.

Hosted authenticated sessions last up to 90 days from login, independently per
browser. The server enforces expiry and account-generation revocation; the cookie
is HttpOnly, Secure, SameSite=Lax and host-only. OAuth tokens stay server-side in
encrypted-at-rest AWS storage. They are not analytics fields. Anonymous sessions
remain bounded to one day. Local developer tokens still live only in memory.
Sign out removes that session; account deletion invalidates all linked sessions
and deletes library, evidence, history, device records and preferences.

Returning authenticated devices read the saved snapshot immediately. A newly
authorized device resolves the same Spotify account and loads the same private
snapshot. Only an account without a completed snapshot auto-syncs. Existing
clients with already expired cookies must connect once to obtain the new lifetime.
Cookie clearing, browser privacy policies and Spotify revocation can still require
reconnection; persistent does not mean permanent.

## Why Not Check Everything On Login

Spotify playlist snapshot IDs support reuse of unchanged playlists. Liked Songs
uses offset pagination of at most 50 items, without a documented complete delta
feed. A head-page or count comparison cannot prove absence of removals or a
balanced addition/deletion. Therefore a cheap probe is not a reliable full update.
Do not block login/build on such a probe. Resync library performs the existing
incremental playlist reconciliation and complete liked-track reconciliation.
[Playlist snapshots](https://developer.spotify.com/documentation/web-api/concepts/playlists),
[saved tracks pagination](https://developer.spotify.com/documentation/web-api/reference/get-users-saved-tracks).

## Retention Contract

| Data | Storage and lifecycle |
| --- | --- |
| Library snapshot, prompt/result history | Account-scoped private records, until deletion |
| Permitted evidence and historical match counts | Durable evidence record; individual feature freshness still enforced |
| Last.fm tags | Existing seven-day default if headers omit freshness; restrictive headers override |
| ReccoBeats no-store responses | Request memory only; values are not archived or reused |
| Device records | Random independent ID, coarse browser/platform/class, first/last login UTC; at most 20 recent records, 90-day pruning on login |
| Detailed operational and allowed optional events | 90 days |
| Daily outcome/count/timing summaries | 365 days |
| Privacy choice | Account-scoped, timestamped, until changed or deleted |
| Transient draft/preview/coverage | Existing bounded lifetime, not permanent acoustic evidence |

New providers must supply stable recording identity, source attribution, freshness
and retention permission before their values can enter reusable evidence. Unknown,
expired and forbidden-to-store values are not treated as valid numeric features.
Do not claim a historical match count means feature values remain cached.
Live legacy cloud evidence envelopes upgrade on their next leased account load;
already expired or deleted historical records cannot be recovered by this change.

## Login And Behavior Records

The existing internal account mapping and random review user ID connect operational
events to a user. Login records add UTC time, an independent random device/session
record ID, coarse browser family, platform and mobile/tablet/desktop-or-unknown
classification. These labels are inferred from ordinary request headers and are
not verified hardware identity. Raw user-agent, IP, screen fingerprint, passwords,
OAuth tokens and keystrokes are excluded from application analytics.

Prompt/result history and minimal operation receipts support the service and
debugging. Optional navigation and player-action measurement defaults off, with
an account-wide opt-in/withdrawal in Settings & privacy. Withdrawal stops future
optional records; account deletion removes previous records. Existing data is not
silently reclassified as advertising consent. The developer-only audit export
remains private and contains personal data despite pseudonymous IDs.

No new analytics cookie is needed. The necessary session cookie authenticates the
service; the local-only display cookie remembers Web/Local presentation. Spotify's
SDK and authorization pages are separate provider surfaces. Their browser behavior
is not a claim that Promptlist installs no third-party technology at all.

## Advertising Boundary

Spotify prohibits profiling users from Spotify content for advertising/marketing.
Its policy also prohibits selling ads on a Streaming SDA. Promptlist uses the Web
Playback SDK, so adding in-app ads needs a separate policy/business-model review.
Do not send playlists, songs, prompts, Spotify IDs or device records to ad platforms,
and do not build music-taste audiences. [Spotify Developer Policy, III.13 and IV](https://developer.spotify.com/policy).

Buying ads to promote Promptlist is separate from serving ads inside it. Future
campaign attribution should use a separately designed, disclosed and consented
first-party acquisition measurement system, not retroactive reuse of operational
logs or Spotify data. No audience upload or advertising integration is implemented.

UK ICO guidance distinguishes necessary/statistical exceptions from advertising
tracking; statistical exceptions have narrow conditions and do not authorize
advertising reuse. Requirements vary by audience jurisdiction. The implementation
uses a conservative optional opt-in instead of assuming that every server-side
measurement is consent-exempt. This is engineering guidance, not a legal clearance.
[ICO exceptions](https://ico.org.uk/for-organisations/direct-marketing-and-privacy-and-electronic-communications/guidance-on-the-use-of-storage-and-access-technologies/what-are-the-exceptions/),
[ICO advertising](https://ico.org.uk/for-organisations/direct-marketing-and-privacy-and-electronic-communications/guidance-on-the-use-of-storage-and-access-technologies/how-do-the-rules-apply-to-online-advertising/).

## AWS Options And Cost

| Option | Fit |
| --- | --- |
| Current application cookie plus DynamoDB/S3 | Selected: already deployed, no new fixed service fee, supports Spotify PKCE directly |
| Cognito Lite/Essentials | Managed authentication, not a cookie-consent or product-analytics service; its documented native social integrations omit Spotify, so it would not simply replace this flow |
| Consent-management platform | Reconsider before external ads/trackers; not needed merely to set the current essential session cookie |
| Third-party analytics or event-stream stack | Unnecessary complexity at the current owner/friends scale |

Cognito lists a 10,000 MAU free tier for direct/social users in Lite/Essentials,
with different federation terms. Free pricing alone does not make it simpler for
Spotify. [Cognito pricing](https://aws.amazon.com/cognito/pricing/),
[supported social providers](https://docs.aws.amazon.com/cognito/latest/developerguide/cognito-user-pools-social-idp.html).

DynamoDB on-demand is usage-based. AWS's US East example uses $0.625 per million
write units; 10,000 additional one-unit writes would be about $0.00625 before
transaction/size amplification. This is an illustrative unit calculation, not
the app's total bill or a regional quote. Lambda time, whole-account state reads,
S3 requests/storage and API requests also count. Preserve the under-$3 light-use
target and existing limits; no hard budget guarantee. Batch removals reduce HTTP
round trips and eliminate full-page downloads for successful edits.
[DynamoDB pricing](https://aws.amazon.com/dynamodb/pricing/).

## Implementation And Acceptance

1. Extend server-enforced and cookie lifetimes, retaining deletion-generation checks.
2. Restore shared snapshots before deciding whether first sync is needed.
3. Persist reusable evidence and privacy/device records with explicit retention.
4. Record coarse logins, gate optional views/playback, preserve operational receipts.
5. Add mobile resume reconciliation and bounded requests; never replay writes.
6. Use one fixed painting, bottom mobile radio, compact name editor and instant removals.
7. Test sessions, isolation, nonce conflicts, privacy choices, rapid edit failure and
   mobile layouts. Deploy only through the existing guarded Promptlist release.

Tests live in Rust modules and `tests/mobile-persistence.cjs`. Visual emulation is
not physical-iPhone rubber-band acceptance. Actual device testing remains useful
for Safari viewport/keyboard behavior. Runtime and deployment receipts belong in
the maintained private project checkpoint, not in public analytics exports.
