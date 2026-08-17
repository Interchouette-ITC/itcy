# ITCy

AI operator for **Interchouette ITC** company LinkedIn and X.

ITCy drafts posts and tweets, opens a publications pull request, and ships only after **BAT** (Approve from **gRoussac**). Public assets carry an AI disclosure line. Site: [interchouette.net](https://interchouette.net/).

![ITCy](docs/assets/itcy-mascot.png)

| | |
| --- | --- |
| Product | [Interchouette-ITC/itcy](https://github.com/Interchouette-ITC/itcy) |
| Status UI | [Interchouette-ITC/itcy-tui](https://github.com/Interchouette-ITC/itcy-tui) |
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
