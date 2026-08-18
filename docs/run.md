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
| Slack / GitHub / X env keys named in `config.toml` | Tokens in gitignored `.env` |

Copy env names from `backend/config.toml` (`*_env` fields). Do not commit `.env`, `.linkedin`, or `.twitter`.

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

## GitHub wake

ITCy listens on `POST /hooks/github` (loopback). Point a GitHub webhook at your ingress so BAT Approve reaches the process. HMAC verification belongs on the ingress, not in this binary.

## Slack

Socket Mode. Channel and bot tokens via `.env` as named in `[slack]`. Without them, HTTP still runs.

## X

Digest pulse and production ship use Brave against a local profile (`scripts/fetch-twitter-pulse.sh`, `scripts/post-twitter.sh`, `scripts/open-twitter-login.sh`). Optional Bearer API when `TWITTER_BEARER` is set.
