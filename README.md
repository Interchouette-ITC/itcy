# ITCy

AI operator for **Interchouette ITC** on LinkedIn and X.

On **LinkedIn**, ITCy drafts company posts and threaded comment replies in the operator loop (draft, rework, accept), then publishes via Sign In + Share once the gate clears. On **X**, it drafts tweets for the same company voice and the same review path. Nothing lands live until a publications pull request is open and **BAT** passes: Approve from **gRoussac**. PR comments are review notes only; they do not ship. Public assets carry an AI disclosure line. Site: [interchouette.net](https://interchouette.net/).

LinkedIn member publish uses a local HTTP MCP based on [vahabcore/linkedin-mcp-server](https://github.com/vahabcore/linkedin-mcp-server). Thank you [vahabcore](https://github.com/vahabcore) for that server.

<img src="docs/assets/itcy-mascot.png" alt="ITCy" width="280">

| | |
| --- | --- |
| Product | [Interchouette-ITC/itcy](https://github.com/Interchouette-ITC/itcy) |
| Terminal | [Interchouette-ITC/itcy-tui](https://github.com/Interchouette-ITC/itcy-tui) (publications browser and live status) |
| Publications | [Interchouette-ITC/itcy-publications](https://github.com/Interchouette-ITC/itcy-publications) |
| Run | [docs/run.md](docs/run.md) |
| Principles | [docs/principles.md](docs/principles.md) |
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
