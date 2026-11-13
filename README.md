# ITCy

<p align="center">
  <img src="docs/assets/itcy-mascot.png" alt="ITCy owl mascot" width="320" />
</p>

<p align="center">
  <strong>AI operator for Interchouette ITC</strong><br />
  Company voice on LinkedIn and X - draft, rework, accept, then ship.
</p>

<p align="center">
  <a href="https://interchouette.net/">interchouette.net</a>
  ·
  <a href="docs/run.md">Run</a>
  ·
  <a href="docs/principles.md">Principles</a>
  ·
  <a href="LICENSE">License</a>
</p>

---

ITCy drafts LinkedIn company posts and threaded comment replies, and tweets in the same operator loop. Nothing goes live until a publications pull request is open and **BAT** passes: Approve from **gRoussac**. PR comments are review notes only; they do not ship. Public assets carry an AI disclosure line.

LinkedIn member publish uses a local HTTP MCP based on [vahabcore/linkedin-mcp-server](https://github.com/vahabcore/linkedin-mcp-server). Thank you [vahabcore](https://github.com/vahabcore) for that server.

## Repos

| | |
| --- | --- |
| Product | [Interchouette-ITC/itcy](https://github.com/Interchouette-ITC/itcy) |
| Terminal | [Interchouette-ITC/itcy-tui](https://github.com/Interchouette-ITC/itcy-tui) |
| Publications | [Interchouette-ITC/itcy-publications](https://github.com/Interchouette-ITC/itcy-publications) |
| SQLite | [docs/sql.md](docs/sql.md) |

## Run

```bash
make lint
make test
make run          # http://127.0.0.1:4700/health → ok
```

Defaults live in `backend/config.toml`. Secrets and overrides go in a gitignored `.env` at the repo root. Without Slack credentials the binary still serves `/health`.

## Publication text guarantees

Drafts retain sentence punctuation before paragraph-closing emoji, with a blank
line before the next sentence. Leading and inline emoji remain supported.

X publishing accepts at most **two complete tweets**, each at most **280 weighted
characters**, including emoji, handles, hashtags and links (t.co weighting).
Layout preserves every sentence: it never clips a suffix, splits a filename, or
drops prose to fit. Generated tweets get a bounded rewrite attempt; impossible
layouts fail visibly. Rework, URL changes, persistence, BAT and publishing all
validate the final payload. Invalid previously approved content needs rework;
publishing does not silently rewrite it.
`/show` displays the checked tweet parts and their weighted lengths without
adding preview labels to the stored or published text.

Playground uses the same text gate as production. Browser publishing checks the
entire composer text and reads both payload files before posting. A failed
self-reply is an audited partial-thread failure, not success. A persisted partial
thread blocks automatic root retries; recover the reply explicitly using its
reported root status instead of republishing the root.

Audit existing records without modifying the database or published posts:

```bash
cd backend
cargo run -p itcy --bin audit-publication-text -- ../sql/runtime.db
```

The audit emits affected IDs, reasons and normalized part lengths as JSON lines.
It examines stored text, which does not establish what a platform actually
published. Always-on regressions cover the reported November draft/tweet failures,
content conservation, weighted limits, full composer matching and rewrite failure.

## License

BUSL-1.1 (Interchouette-ITC). See [LICENSE](LICENSE). Source files carry SPDX headers.
