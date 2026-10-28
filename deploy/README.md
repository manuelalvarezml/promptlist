# Hosted Private Beta

Status: serverless migration selected on 2026-10-28; not implemented or live.
The owner selected **under US$3/month in AWS charges**, excluding provider API
usage, and an AWS-provided HTTPS address until a domain is purchased. This
supersedes the Lightsail recommendation below. No existing AWS website,
credentials or data store is a runtime dependency.

## Selected Serverless Direction

- Rust Lambda HTTP function with a stable AWS Function URL for the application,
  OAuth callback and assets. No purchased domain, certificate renewal, API
  Gateway, always-on instance or provisioned concurrency required initially.
- Separate invoked Rust worker for sync/build/coverage, with durable queued job
  state, progress, bounded retries and an account-level lease. A submitted job
  must survive process termination; do not rely on `tokio::spawn` after an HTTP
  response. Choose and test a durable dispatch/outbox mechanism before release.
- DynamoDB on-demand for small session/job records, history indexes and bounded
  event batches. Use conditional writes for ownership, deletion generations,
  deduplication and ambiguous-export receipts. Do not fit entire libraries or
  unlimited prompt results into one item. Persist server-only credentials
  encrypted, with least-privilege IAM and expiry enforced on every read.
- Private S3 for compressed library snapshots, larger prompt/result documents
  and analytical archives. Use account-scoped opaque keys and immutable objects
  with conditional pointer updates. Do not copy/upload the SQLite file after
  every request or share writable `/tmp` state between Lambda instances.
- Keep functions outside a customer VPC: no NAT gateway, EFS, RDS, load balancer,
  container registry or separate analytics service. Package compiled Rust as a
  zip; benchmark memory and architecture before selecting their final settings.
- Cache public versioned assets in the browser; all account pages, OAuth,
  playback tokens and APIs remain private/no-store. CloudFront is optional only
  if its verified cost/abuse protection improves the simple Function URL design.

The current application still uses SQLite and in-memory sessions/workers.
The existing Dockerfile/systemd files are historical server preparation, not a
Lambda implementation. Do not deploy the current server unchanged on Lambda.

## Budget And Guardrails

Under $3 is a **light-use target, not a guaranteed AWS billing cap**. Model the
incremental cost without assuming unused account-wide free allowances or trial
credits; include requests, compute, storage, logs, transfer and retries.

Illustration using AWS's published x86 Lambda rate of $0.0000166667/GB-second:
500 builds at 512 MiB and 60 billed seconds cost about **$0.25 in worker compute**;
500 at five minutes cost about **$1.25**. These are not whole-app estimates or
measured Promptlist runtimes. Sync, coverage, HTTP/status requests, cold starts,
storage and other charges are additional. Verify the chosen region's rates and
measure actual jobs before assigning a monthly allowance.

Required before public release: explicit account allowlist, small concurrency
limits, per-account/month work admission using worst-case billed duration,
timeouts, bounded provider retries, status polling backoff, short redacted-log
retention and storage/object limits. Reject new expensive work before exhausting
the allowance while preserving history reads and existing receipts. Reserve
headroom for anonymous traffic and deletion. Billing alerts are delayed and are
not a hard spending stop; never promise protection against all public abuse.

## History And Behavior Data

Current local implementation stores submitted prompts/refinements, structured
interpretations, original/latest playlist results, outcomes and elapsed times per
account until deletion. Detailed actions expire after 90 days. Daily action-kind
counts, outcome counts and timing sums survive for 365 UTC days; no prompt text
is duplicated in these aggregates. Migration backfills existing actions once.
Account deletion removes both raw actions and aggregates. The private audit
export is schema version 2 and includes the summaries.

Tracked actions include navigation, submissions, jobs, edits, exports and play
starts. This is not a complete playback telemetry stream: SDK pause, seek and
volume interactions are not currently instrumented. No keystrokes, passwords,
OAuth credentials or raw provider bodies enter analytics; no behavior/library
data goes to an AI model or third-party analytics service.

For serverless, retain the same account-scoped contract. Persist bounded event
batches as part of existing requests, not a separate Lambda invocation per
click. Flush before completing an invocation, not on a background timer.
Deduplicate batches and job outcomes so retries cannot inflate daily counts.
Use DynamoDB for active records/indexes and compact private S3 batches for
longer retention; benchmark batching rather than assuming fewer HTTP calls mean
fewer billed DynamoDB write units. Prefer S3 Standard at beta scale; tiny-object
archive transitions can cost more than they save.

Cloud objects must use Block Public Access, bucket-owner enforcement, TLS-only
access, SSE-S3 and project-specific least-privilege roles. Enforce expiry in reads
and exports because DynamoDB TTL/S3 lifecycle deletion is asynchronous. Account
deletion must invalidate workers and purge indexes, objects, versions and queued
events, with tombstones preventing restoration from old recovery backups.
Validate this before enabling any cloud archive. Pseudonymous exports are still
personal data, not public fixtures. **No cloud analytics or backup exists yet.**

## Migration Acceptance

1. Separate HTTP/session/job persistence from the existing matching code, keeping
   the local runtime working. Move auth state and refresh coordination out of
   process memory without exposing tokens to logs or browser storage.
2. Implement durable jobs, idempotent action history and account deletion; test
   cold starts, concurrent sessions, worker termination, duplicate delivery,
   lost responses and ambiguous Spotify writes. Never replay a playlist export
   just because a worker retried.
3. Package/test Linux Rust functions and isolated infrastructure with measured
   memory/time/storage, a cost model and least-privilege policies. Verify the
   intended AWS identity immediately before provisioning; never use default.
4. Register the generated HTTPS callback while preserving localhost. Reconnect
   the owner account, validate private state restoration, build/refine/playback
   and explicit export, then record actual cost and rollback evidence.

Sources checked 2026-10-28:
[Function URL/HTTPS](https://docs.aws.amazon.com/lambda/latest/dg/urls-configuration.html),
[Lambda lifecycle](https://docs.aws.amazon.com/lambda/latest/dg/lambda-runtime-environment.html),
[Lambda pricing](https://aws.amazon.com/lambda/pricing/),
[DynamoDB pricing](https://aws.amazon.com/dynamodb/pricing/),
[TTL delay](https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/TTL.html),
[S3 pricing](https://aws.amazon.com/s3/pricing/),
[SSE-S3](https://docs.aws.amazon.com/AmazonS3/latest/userguide/default-encryption-faq.html).

## Historical Server Alternative (Not Selected)

One Lightsail Linux `nano_3_0` instance in us-east-2: 512 MB RAM, 20 GB disk,
public IPv4 and 1 TB transfer for **US$5/month**, before tax and overages.
Use Ubuntu 24.04 LTS, one non-root Rust process, SQLite on persistent disk,
systemd supervision, and Nginx for TLS and request limits. No load balancer,
NAT gateway, managed database, container registry or paid CDN is needed.

Verified against the AWS bundle API and [AWS bundle table](https://docs.aws.amazon.com/lightsail/latest/userguide/amazon-lightsail-bundles.html)
on 2026-10-28. IPv6-only is $3.50, but provider reachability and client support
must be proven before choosing it. Public AAAA DNS records were found for all
five current API providers on 2026-10-28; this is not an actual egress test.
[CloudFront supports IPv6 origins](https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/cloudfront-enable-ipv6.html),
so it can preserve IPv4 viewer access, subject to an origin hostname, trusted
origin TLS and verified Free-plan eligibility. Do not assume a trial/free credit.
Backups, monitoring upgrades, excess traffic, domains and provider API usage
are separate costs. The base price is not a hard total-bill ceiling.

For a user-owned domain, use automated public TLS. Without a domain, a stable
public IP can have a trusted short-lived certificate: [Certbot/Let's Encrypt
IP certificate instructions](https://letsencrypt.org/2026/03/11/shorter-certs-certbot).
Certbot >=5.4 supports webroot IP validation. Renew automatically and reload the
proxy after renewal; these certificates last six days. Do not serve OAuth over
plain HTTP or tell users to bypass certificate warnings.

Serverless can have a lower idle bill, but is not a drop-in packaging change:
current sessions/tokens and background workers are in memory, while SQLite is
writable local state. A Lambda design needs durable encrypted auth state,
asynchronous jobs, account serialization and durable writes. A single Lambda
container with `/tmp` SQLite would lose state and freeze jobs after responses.

## Configuration

The application accepts named environment variables, overriding `.env.local`.
Use a root-owned mode-0600 systemd environment file outside the release tree:

```dotenv
SPOTIFY_CLIENT_ID=your_client_id
SPOTIFY_REDIRECT_URI=https://your-host/callback
LASTFM_API_KEY=your_key
OPENAI_API_KEY=your_key
PROMPTLIST_ALLOWED_USERS=your_spotify_account_id
```

`promptlist.service` sets hosted mode, the internal loopback port, and private
data directory. HTTPS is mandatory in hosted mode, along with a nonempty account
allowlist. Cookies become Secure and HSTS is enabled. Host and Origin checks
use the configured public origin; forwarded headers cannot override it. Configure
the reverse proxy to preserve the exact public Host. Never publish port 8787.

Add the exact HTTPS callback to the Spotify developer app, preserving the local
callback. Retain Web API and Web Playback SDK. The app remains subject to
[Spotify Development Mode limits](https://developer.spotify.com/documentation/web-api/concepts/quota-modes),
including the user allowlist; a public URL is not approval for general signup.

## Build And Release

```sh
docker build --platform linux/amd64 -f deploy/Dockerfile --target artifact \
  --output type=local,dest=.deploy/release .
```

The Docker context is allowlisted and excludes local secrets, SQLite, Git,
operational documents and licensed theme binaries. The output is a Linux binary,
not a cloud deployment. Build away from the 512 MB production server.

Install versioned releases under `/opt/promptlist/releases/` and point
`/opt/promptlist/current` at the selected release. Install optional authorized
theme assets into each release's `assets/` directory separately. Run under the
dedicated `promptlist` OS user using the supplied hardened service unit.
Data belongs at `/var/lib/promptlist`, outside every release. Do not upload the
entire local project folder or automatically overwrite an existing cloud database.

Only 80/443 should be public. Limit SSH to the operator's IP and verify host keys.
Keep access logs off or explicitly redact callback query strings, cookies and
authorization headers. Add proxy request/body/connection limits; maintain the
application's Origin/CSRF/preview/write protections. Disable password SSH and
enable unattended security updates. Keep cloud credentials off the host.

## Acceptance And Recovery

- Verify the exact AWS identity before every provisioning/deployment operation.
- Check Linux startup, HTTPS certificate, health, Secure cookies and host/origin
  denials. Confirm tokens, database, environment and deployment files are not URLs.
- Reconnect the allowed Premium account, sync once in a fresh hosted installation,
  and test build/refine, playback, export, sign-out and cached-state restoration.
- Check denied/unlisted accounts and concurrent access. Never claim actual
  playback from the SDK simulator or an HTTP health response.
- Back up SQLite consistently with its backup API, store backups privately,
  test restore and record retention/cost before enabling paid backup resources.
  Local-only backup files do not protect against loss of the server/disk.
- Roll back the release symlink; leave the private data directory intact. A
  process restart requires Spotify reconnect, not another completed library sync.
- Never automatically repeat an ambiguous playlist export after a restart.

Cloud acceptance and resource identities belong in private operational records,
not in this public template. No production credentials belong in GitHub.
