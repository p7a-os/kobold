# Contributing to Kobold

Thank you for your interest in contributing to **Kobold**!

Kobold is an open-source autonomous agent harness and supervisor designed with uncompromising performance, reliability, and architectural cleanliness. We welcome contributions, bug reports, and enhancements from the community.

---

## Code of Conduct

We are committed to providing a welcoming, inclusive, and harassment-free environment for everyone. Please treat all contributors and users with respect, professionalism, and kindness.

---

## Development Prerequisites

* **Rust**: Toolchain 1.93.0 or newer (managed via `rustup` per `rust-toolchain.toml`).
* **C Compiler**: `clang` or `gcc` (required by `ring` crypto).
* **CMake** *(optional)*: Only required if building `kobold-tts` with SentencePiece C++ bindings.

---

## Workspace Architecture

Kobold is structured as a modular Cargo workspace:

* **`kobold`**: Interactive Ratatui Terminal UI and one-shot CLI entrypoint.
* **`koboldd`** (`src/bin/koboldd.rs`): Headless supervisor daemon, multi-lane state machine, and UDS/WebSocket server.
* **`kobold-core`**: Core kernel, session registry, transcript DAG, single-writer lease engine, and sandbox broker.
* **`kobold-proto`**: Shared protocol definitions for Northbound (AG-UI) and Southbound (commands and frames).
* **`kobold-adapter-acp`**: Southbound adapter for external agents speaking the Agent Client Protocol (Claude Code, Antigravity, Grok Build).
* **`kobold-adapter-tmux`**: Southbound bridge for headless tmux sessions and persistent PTYs.
* **`kobold-openai`**: Southbound WebSocket adapter for OpenAI Responses API streaming.
* **`kobold-tts`**: High-efficiency local neural speech synthesizer powered by Pocket-TTS.

---

## Development Workflow

### 1. Clone & Build

```sh
git clone https://github.com/p7a-os/kobold.git
cd kobold
cargo build
```

### 2. Running Tests

Kobold maintains a 5-tier test taxonomy (unit, integration, contract, property, and end-to-end tests). All tests are hermetic and run completely offline:

```sh
# Run the entire test suite
cargo test --workspace --all-targets

# Run documentation tests
cargo test --doc
```

### 3. Code Formatting & Linting

All code must be formatted and pass Clippy without warnings:

```sh
# Check formatting
cargo fmt --check

# Format code
cargo fmt

# Run Clippy with warnings treated as errors
cargo clippy --workspace --all-targets -- -D warnings
```

---

## Git Conventions & Pull Requests

* **Branch Naming**: Use descriptive branch names like `feat/acp-tools`, `fix/tui-wrap`, `docs/seams`.
* **Commit Messages**: Follow [Conventional Commits](https://www.conventionalcommits.org/):
  * `feat(...)`: A new user-visible feature or adapter capability.
  * `fix(...)`: A bug fix.
  * `docs(...)`: Documentation additions or revisions.
  * `test(...)`: Adding or refactoring tests.
  * `perf(...)`: Performance optimizations.
  * `refactor(...)`: Code changes that neither fix bugs nor add features.
* **PR Checklist**:
  * [ ] All tests pass (`cargo test --workspace --all-targets`).
  * [ ] Clippy is clean (`cargo clippy --workspace --all-targets -- -D warnings`).
  * [ ] Code is formatted (`cargo fmt --check`).
  * [ ] Documentation is updated if changes affect user-facing behavior or protocol schemas.

---

## Documentation

Documentation is maintained under `docs/` using [docmd](https://docmd.io). To preview documentation locally with hot-reloading:

```sh
# Start the local docmd preview server
bunx @docmd/core dev

# Or build static documentation
bunx @docmd/core build
```

---

## License

By contributing to Kobold, you agree that your contributions will be licensed under the [GNU Affero General Public License v3.0 (AGPL-3.0)](LICENSE).
