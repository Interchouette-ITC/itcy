# Run and configure

## Build

From the repository root:

```bash
make lint
make test
make run     # debug binary on 127.0.0.1:4700
make build   # release: itcy + linkedin-itcy-gr-mcp
```

`GET /health` returns `ok` when the process is up. `GET /status` is JSON (routes, Tor, enrich queue, webhook wake).

The terminal UI is a separate repo: [itcy-tui](https://github.com/Interchouette-ITC/itcy-tui). It lists publications from GitHub and, when this binary is up, shows live health and saved drafts.

Writer prompts are not in this tree. The build copies operator prompt files when present; otherwise it embeds short stubs so `cargo` still succeeds.

## Config

Committed defaults: [`backend/config.toml`](../backend/config.toml).

| Override | Role |
| --- | --- |
| `ITCY_CONFIG` | Path to an alternate TOML file |
| `ITCY_BIND` | Listen address (default `127.0.0.1:4700`) |
| `ITCY_STATE_DB` | SQLite path (default `sql/runtime.db` relative to `make run` cwd) |
| `ITCY_LINKEDIN_EXPORT_DIR` | Official LinkedIn export directory (zip or unzipped). Never commit it. |
| `ITCY_TOR_BIN` | Tor daemon binary for `/enrich` (SOCKS `9050`, control `9051`) |
| `ITCY_PUBLIC_FETCH_CMD` | Optional HTML fetch helper (default `scripts/fetch-public-page.sh`) |
| `ITCY_PW_BROWSER` | Ingest thin-page fetch only: `brave`, `chromium`, or opt-in `obscura` (default unset = legacy Brave/Chromium launch) |
| `ITCY_BROWSER_EXECUTABLE` | Override browser or Obscura binary path (`tools/obscura/obscura` when `ITCY_PW_BROWSER=obscura` and unset) |
| `ITCY_OBSCURA_CDP_PORT` | CDP port for `obscura serve` (default `9222`) when using Obscura ingest fetch |
| Slack / GitHub / X env keys named in `config.toml` | Tokens in gitignored `.env` |

Copy env names from `backend/config.toml` (`*_env` fields). Do not commit `.env`, `.linkedin`, or `.twitter`.

Production LinkedIn ship calls the local MCP at `http://127.0.0.1:4780/mcp` when `[linkedin] publish_mode = "production"`. Start it with `.cursor/scripts/linkedin-vahabcore-up.sh` after `LINKEDIN_ACCESS_TOKEN` is set. That MCP is [vahabcore/linkedin-mcp-server](https://github.com/vahabcore/linkedin-mcp-server) (thanks Abdulvahab Shaikh). Slack `/reply_comment` drafts a LinkedIn or X reply (`CREPLY-` / `XREPLY-`); `/accept` ships (LinkedIn playground = notice; LinkedIn production = MCP `reply_to_comment`; X = threaded Brave/API reply). No publications BAT PR for replies.

## Tor

`/enrich` fetches personal LinkedIn post URLs over Tor.

```bash
export ITCY_TOR_BIN=/path/to/tor
make tor-up     # SOCKS 9050 + control 9051
make tor-down
```

Data directory: `sql/tor-data/` (gitignored). Template: [`docker/torrc`](../docker/torrc).

## LinkedIn export

Place an official export under `linkedin-export/` (gitignored). Point `linkedin_export_dir` or `ITCY_LINKEDIN_EXPORT_DIR` at that folder, then:

```bash
cd backend && cargo run -p itcy --bin import-linkedin-export
```

Ongoing adds: Slack `/enrich` (Tor, post URLs) and `/ingest` (public pages).

## Obscura ingest (opt-in)

Thin-page `/ingest` fallback only (`scripts/fetch-public-page.sh`). Default unchanged. Set `ITCY_PW_BROWSER=obscura` in `.env` to try Obscura CDP; install binary under `tools/obscura/`. Compare baseline vs Obscura: `make obscura-parity`. Draft research (`playwright-mcp.sh`) and X ship are unchanged.

## GitHub wake

ITCy listens on `POST /hooks/github` (loopback). Point a GitHub webhook at your ingress so BAT Approve reaches the process. HMAC verification belongs on the ingress, not in this binary.

## Slack

Socket Mode. Channel and bot tokens via `.env` as named in `[slack]`. Without them, HTTP still runs.

`/draft_about` creates a LinkedIn `DRAFT-` reference; `/tweet_about` creates an X
`TWEET-` reference. `/accept` routes by that reference, not by the word "draft"
in the message. Each command has isolated research identity and tool policy,
including concurrent Slack events and localhost injections. A new item in a
digest batch cannot reuse the previous item's session ID. "Saved as open" and
acceptance hints appear only after persistence succeeds with the allocated ID;
save errors are returned explicitly instead.

BAT PR titles stay within 80 characters, including the prefix and publication ID
(well below GitHub's 256-character limit). Long subjects are shortened only in the
PR title; the full subject and publication body are preserved. If `/accept`
previously failed because the title was too long, retry `/accept` with the same
reference after updating ITCy.

BAT file uploads make up to five attempts for transient GitHub transport, HTTP
5xx, SHA conflicts, and HTTP 409 repository-rule evaluation timeouts, with
1/2/4/8-second delays. Each attempt refreshes the file SHA and checks whether the
exact content is already present, avoiding duplicate writes when GitHub committed
a write before returning an error. Actual repository-rule violations are not
bypassed or retried as timeouts. Upload errors
include the HTTP status and GitHub request ID when available; an empty or
unreadable error body is reported explicitly instead of a blank `put ...:` error.
PR creation also reports HTTP status and request ID, retries transient failures
with backoff, and checks the exact head/base for an existing open PR before and
after every attempt. A PR created despite a failed response is recovered rather
than duplicated; validation failures are not blindly retried.

`/accept` trusts the selected publisher URL for both LinkedIn drafts and tweets:
it does not fetch the article or block acceptance on HTTP errors, bot walls, or
network failures. A cite is still required. Research/SERP link filtering remains
unchanged; `/change_url` can override its selection before acceptance.

## X

X ship supports both the legacy test-ID controls and the redesigned
`/compose/post` sidebar link, textarea, and dialog Post/Reply buttons. It checks
both native and ARIA disabled states before submitting. A missing control does
not by itself mean the account is logged out; failure screenshots and HTML are
saved under `pw/screenshots/x-ship/`.
Reply textareas use the distinct `Post your reply` label/placeholder; root-post
textareas use `What's happening?`. Both are recognized, including when an inline
reply textarea and the reply dialog are visible together.
Root posts, overflow replies, and standalone replies use the visible composer
dialog when present, keeping text entry and submission in the same scope rather
than mixing the background inline reply box with the dialog's Reply button.
Disabled-submit failures also save a screenshot and HTML; they do not assume the
text exceeded the character limit.
Previous partial-thread failures remain in the audit log but do not block
`/retry_bat`. A retry runs the normal publish flow and may create a new root
tweet even if an earlier attempt already posted one.
Status resolution uses the highest own status ID (including replies), not the
first profile card. After submission, the selected post must match the complete
submitted prose. Overflow replies verify the parent's status ID and root text
before opening its reply composer, preventing attachment to an unrelated thread.
After submission, the reply's own permalink must show the intended parent
immediately before it in the conversation before ship success is recorded.

Offline browser fixtures (no login or publishing) can be run with the existing
Playwright installation:

```bash
PLAYWRIGHT_REQUIRE_FROM=/absolute/path/to/playwright/package.json \
ITCY_BROWSER_EXECUTABLE=/usr/bin/brave-browser \
node --test scripts/x-ship-ui.test.mjs
```

Digest pulse and production ship use Brave against a local profile (`scripts/fetch-twitter-pulse.sh`, `scripts/post-twitter.sh`, `scripts/open-twitter-login.sh`). Optional Bearer API when `TWITTER_BEARER` is set.
