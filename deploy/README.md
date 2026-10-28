# Hosted Private Beta

Status: production configuration and Linux build preparation, not a live release.
Choose the cost envelope and public HTTPS hostname before provisioning. No
existing AWS website, credentials or data store is a runtime dependency.

## Smallest Practical Server

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
