# Serverless Private Beta

Implemented and provisioned October 28, 2026. Live acceptance and exact resource
identities belong in private operational checkpoints, never public fixtures.
Spotify hosted OAuth and genuine Premium playback require separate acceptance;
an HTTP health response is not proof of either.

## Architecture

- Rust HTTP Lambda, 256 MB, 30-second timeout, reserved concurrency 6, AWS HTTPS
  Function URL, with an optional Regional HTTP API for a custom domain.
- Rust worker, 512 MB, 600-second timeout, reserved concurrency 1. DynamoDB
  Streams dispatch queued jobs durably. Batch size 1, three delivery retries,
  one-hour maximum stream record age and a private encrypted failure queue.
- Private on-demand DynamoDB stores sessions, job progress and logical account
  rows. Values over 120 KB are compressed into immutable private S3 objects;
  fenced transactions change their pointers before old objects are removed.
- SQLite is an in-memory projection during each cloud invocation, not shared
  writable disk. Only changed rows persist. The ordinary local runtime remains.
- Conditional account leases, job IDs and account-deletion generations prevent
  concurrent writes and duplicate export replay. The worker stays within its
  invocation while background matching runs, flushing progress/session state.
- Private SSE-S3 bucket, TLS-only access, Block Public Access, scoped IAM.
  Hosted session tokens are private/encrypted, expire after 24 hours and never
  enter analytics. Cookies are Secure/HttpOnly, responses private/no-store.
- No VPC/NAT, EFS, RDS, load balancer, ECR, fixed-price host,
  provisioned concurrency or external analytics service.

## History And Behavior

Prompt/refinement text, interpretations, original/latest result selections and
outcomes persist per account until deletion. Raw app events expire after 90 days;
daily action-kind/outcome/count/timing summaries last 365 UTC days. Large records
use S3 only when useful; normal analytics writes share the existing request.
No invocation per click, keystroke capture or separate analytics SaaS.

Navigation, submissions, jobs, edits, exports and playback starts are recorded.
SDK pause/seek/volume interactions are not yet instrumented. No library or activity
data goes to OpenAI. Private developer exports are pseudonymous, not anonymous.

Expiry is enforced on hydration, independent of eventual DynamoDB TTL cleanup.
Transient S3 objects expire after two days. Confirmed account deletion removes
active/expired rows, orphaned objects and linked sessions. Local source databases
and separately created developer exports are independent copies: delete those
separately when satisfying a full erasure request.

## Model

Default `gpt-6.1-sol`, medium reasoning, 4,000 output tokens, 45-second timeout.
Only requests and prompt-derived preferences are sent. Deterministic local
requests avoid inference; provider failure falls back to local matching.
Two bounded synthetic prompt/refinement calls took about 4.8/3.5 seconds and
cost approximately $0.00195/$0.00233 at the documented $2/M input, $10/M output
rates. These are examples, not a quality or production latency guarantee.
[Official model](https://developers.openai.com/api/docs/models/gpt-6.1-sol),
[pricing](https://developers.openai.com/api/docs/pricing).

## Cost Controls

Under $3/month is a light-use target, **not a billing guarantee or hard cap**.
Global admission allows at most 200 expensive jobs per UTC month, including
sync/coverage/build/refinement/export. At the maximum 600 billed seconds and
512 MB, 200 worker invocations cost about $1 of x86 compute before free tier.
HTTP, failed/redelivered invocations, storage, requests, logs and transfer add
cost. Anonymous new sessions are limited to 100/day; this does not eliminate
cost from public traffic. Logs retain three days; release artifacts 30 days.

For a few users and short jobs, these controls leave headroom below $3 without
assuming free credits. Large histories, slow jobs or abusive traffic can exceed
it. No paid monitoring subscription or billing alert is configured. Inspect
actual billing and workload before inviting more users or raising limits.
[Lambda pricing](https://aws.amazon.com/lambda/pricing/),
[DynamoDB pricing](https://aws.amazon.com/dynamodb/pricing/),
[S3 pricing](https://aws.amazon.com/s3/pricing/).

## Operator Workflow

Requires Rust 1.89+, Docker, AWS CLI, Node with `util.parseEnv`, zip and sqlite3.
Use only CLI profile `manueldev`, verified as IAM `manuel-web`, region us-east-2.
Never use default/Reevalua. Create an ignored mode-0600 `.deploy/operator.json`
containing the expected `account` and full `arn`, confirmed with STS. Release
scripts refuse a different identity. `.env.local` supplies only this project's
keys. The local database supplies the bounded owner allowlist.

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
docker build --platform linux/amd64 -f deploy/Dockerfile.lambda \
  --target artifact --output type=local,dest=.deploy/lambda .
node deploy/release.cjs deploy
node deploy/release.cjs configure
node deploy/run-cloud.cjs --container cloud-smoke
node deploy/run-cloud.cjs --container cloud-smoke --model
node deploy/run-cloud.cjs --container cloud-import .data/catalog.sqlite
node deploy/release.cjs finalize
```

`deploy/configure` temporarily allow the synthetic test account; `finalize`
removes it. Always finalize after acceptance. No public endpoint can sign in
as that fixture account. Linux operator containers avoid host-specific native
network issues and mount the source database read-only. Native Rust operator
commands are supported without `--container` where networking works.

For routine production releases use `node deploy/release.cjs deploy-final`:
it uploads the rebuilt artifact without temporarily enabling the test account.

Deploy creates only dedicated Promptlist resources and records stable identities
in ignored `.deploy/aws-release.json`. Secret-bearing CloudFormation inputs use
0600 temporary files which are removed; parameters are NoEcho. Keys/tokens/data
never belong in GitHub or command arguments. Licensed theme binaries are added
only to the private release zip, not redistributed in the repository.

Cloud import uses SQLite's consistent backup API from a read-only connection,
copies logical account records, does not copy tokens, and refuses any nonempty
destination. A partial failed import needs inspection/recovery, not blind rerun.
Synthetic smoke verifies private S3, durable session/job/result/history,
Origin rejection, stale-lease fencing, duplicate delivery and account deletion.
The optional model smoke adds a paid synthetic-only request from the real worker.

Register the exact output URL plus `callback` in Spotify while preserving
localhost. Keep Web API/Web Playback SDK enabled. Reconnect Premium and accept
the requested scopes. Friends require both Spotify Development Mode access and
the app account allowlist. A public HTTPS URL does not enable general signup.

## Custom Domain

The optional `domain.json` stack adds a Regional API Gateway HTTP API, root/www
domain mappings and scoped Lambda invocation permission. The HTTPv2 integration
uses the existing API function and private data; no migration or extra worker.
The default execute-api endpoint is disabled. Stage limits are 10 requests/second
and a burst of 20, not a billing cap.

Use a non-exportable public ACM certificate in us-east-2 covering the root and
www names. Keep its DNS-validation CNAMEs permanently for automatic renewal.
Save domain, certificateArn and stack in ignored `.deploy/domain-state.json`.
`node deploy/domain.cjs` verifies the AWS identity and issued certificate before
creating the dedicated stack and saving its DNS targets. Namecheap BasicDNS can
route the apex with an ALIAS and www with a CNAME, without a Route53 hosted zone.
Replace only old parking/URL-routing records; preserve mail records/settings.

Register `https://promptlist.lat/callback` in Spotify before cutover, preserving
localhost. Set private release receipt `publicUrl` to `https://promptlist.lat/`
and `redirectHosts` to the legacy Function URL hostname and `www.promptlist.lat`.
Deploy the rebuilt binary with `deploy-final`, then apply canonical configuration
with `finalize`. New-domain cookies require reconnecting, not another library
sync. Approved aliases redirect GET/HEAD to the canonical root; query strings,
OAuth codes and POST bodies are never forwarded. Unknown hosts remain rejected.

HTTP API requests start around $1/million ($0.01 per 10,000), with no fixed
monthly API/custom-domain fee. Non-exportable integrated ACM certificates have
no extra certificate charge. Lambda, storage and transfer still apply; domain
registration/renewal is separate from the light-use AWS budget.
[HTTP API domains](https://docs.aws.amazon.com/apigateway/latest/developerguide/http-api-custom-domain-names.html),
[API pricing](https://aws.amazon.com/api-gateway/pricing/),
[ACM pricing](https://aws.amazon.com/certificate-manager/pricing/),
[Namecheap ALIAS](https://www.namecheap.com/support/knowledgebase/article.aspx/10128/2237/how-to-create-an-alias-record/).

## Recovery And Limits

- Do not automatically replay interrupted/ambiguous Spotify exports. Inspect
  the durable write phase and known playlist ID first. Completed delivery IDs
  are ignored; crash interruption is not proof that Spotify did nothing.
- Inspect the dedicated failure queue and sanitized project logs after failures.
  An active account lease expires after 45 seconds (HTTP) or 650 seconds (worker).
- Source edits require rebuild/deploy. Do not rerun import to update production.
  Manual Spotify sync reconciles the library; sign-in does not resync it.
- Roll back code by using a previously recorded release key in the private
  receipt, then `configure` and `finalize`. Never overwrite current user data
  from an old local database as part of code rollback.
- State table and data bucket are retained on stack deletion. They are durable
  primary storage, not an independent backup. Point-in-time recovery/versioning
  is not enabled. The original local snapshot survives migration, but does not
  protect new cloud-only history. Record this tradeoff before wider rollout.
- Genuine Spotify playback, owner listening quality, a live explicit export,
  chaos/failure-injection coverage and actual month-end billing remain distinct
  acceptance items; synthetic tests cannot establish them.

Official runtime references: [Function URLs](https://docs.aws.amazon.com/lambda/latest/dg/urls-configuration.html),
[Lambda lifecycle](https://docs.aws.amazon.com/lambda/latest/dg/lambda-runtime-environment.html),
[DynamoDB TTL](https://docs.aws.amazon.com/amazondynamodb/latest/developerguide/TTL.html).
The old fixed-server templates are preserved only as historical alternatives.
