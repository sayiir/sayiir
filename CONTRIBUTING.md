# Contributing to Sayiir

Thanks for your interest in contributing to Sayiir! Whether it's a bug fix, new feature, documentation improvement, or storage backend — all contributions are welcome.

---

## Getting Started

### Prerequisites

- **Rust** stable (edition 2024)
- **Python** 3.10+
- **maturin** — for building Python bindings
- **uv** — for Python dependency management

### Clone and Build

```bash
git clone https://github.com/sayiir/sayiir.git
cd sayiir
cargo build --workspace --all-features
```

### Python Bindings

```bash
cd sayiir-python
uv venv
source .venv/bin/activate
maturin develop
pip install -e ".[dev]"
```

---

## Running Tests

### Rust

```bash
cargo test --workspace --all-features
```

### Python

```bash
pytest sayiir-python/tests/ -v
```

---

## Code Style

### Rust

```bash
cargo fmt --all
cargo clippy -- -D warnings
```

### Python

```bash
uvx ruff check sayiir-python/
uvx ruff format sayiir-python/
uvx pyright --project sayiir-python/
```

---

## CI Checks

Every pull request runs the following checks automatically:

| Check | What it does |
|---|---|
| `cargo deny` | License and dependency audit |
| `cargo fmt --all -- --check` | Rust formatting |
| `cargo clippy -- -D warnings` | Rust lints (warnings = errors) |
| `cargo test --workspace --all-features` | Rust tests |
| `ruff check` | Python linting |
| `ruff format --check` | Python formatting |
| `pyright` | Python type checking |
| `pytest` (Python 3.10–3.13) | Python tests across all supported versions |

All checks must pass before a PR can be merged.

---

## Commit Messages

We use emoji-prefixed messages. Follow the style from the git history:

| Emoji | Meaning |
|---|---|
| `✨` | New feature |
| `🐛` | Bug fix |
| `♻️` | Refactor |
| `⚡️` | Performance |
| `🚨` | Tests |
| `👷` | CI / build |
| `📝` | Documentation |

Example: `✨ add durable delay (#39)`

---

## Pull Requests

1. **Fork** the repository
2. **Create a branch** from `main` (`git checkout -b feature/my-change`)
3. **Make your changes** — keep commits focused
4. **Run tests and linters** locally before pushing
5. **Open a PR** against `main`
6. **Wait for CI** — all checks must pass

---

## Where to Contribute

See the [Roadmap](ROADMAP.md) for what's planned. Areas where contributions are especially welcome:

- **Storage backends** — PostgreSQL, SQLite, Redis
- **Language bindings** — TypeScript, Go
- **Documentation** — examples, tutorials, guides
- **Testing** — edge cases, benchmarks, coverage

Issues labeled `good first issue` are a great starting point.

---

## Community

- [Discord](https://discord.gg/MWSzsHeg) — questions, feedback, discussion
- [GitHub Issues](https://github.com/sayiir/sayiir/issues) — bugs and feature requests
