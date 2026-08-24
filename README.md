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

## License

BUSL-1.1 (Interchouette-ITC). See [LICENSE](LICENSE). Source files carry SPDX headers.
