# Contributing to Moments

Thank you for your interest in contributing to Moments! This guide will help you get started.

## Reporting Bugs

Open a [GitHub issue](https://github.com/justinf555/Moments/issues/new?template=bug_report.md) with:

- Steps to reproduce the problem
- What you expected to happen
- What actually happened
- Your system info (distro, GNOME version, Flatpak version)

## Suggesting Features

Open a [GitHub issue](https://github.com/justinf555/Moments/issues/new?template=feature_request.md) describing:

- The problem you want to solve
- Your proposed solution
- Any alternatives you considered

## Development Setup

### Using GNOME Builder (recommended)

1. Clone the repository:
   ```bash
   git clone https://github.com/justinf555/Moments.git
   ```
2. Open the project in GNOME Builder
3. Click **Run** to build and launch via Flatpak

### Using the command line

```bash
git clone https://github.com/justinf555/Moments.git
cd Moments
make run    # builds and runs via Flatpak
make clean  # cleans the Flatpak build directory
```

### Running unit tests

Unit tests run outside Flatpak using `cargo test`. You need the system development libraries installed — see [README.md](README.md#system-dependencies-for-cargo-test-outside-flatpak) for the package list.

```bash
cargo test
```

## Code Style

- **Rust edition 2021** with standard `rustfmt` formatting
- **Module naming**: use `src/foo/bar.rs`, never `src/foo/bar/mod.rs` (Rust 2018+ style)
- **Logging**: use the `tracing` crate (`info!`, `debug!`, `warn!`, `error!`), never `println!` or `eprintln!`
- **Instrumentation**: add `#[instrument]` to functions worth timing, with `skip()` for large or sensitive parameters
- **Error handling**: return `Result<T, LibraryError>` from library operations; use `thiserror` for error types
- **GTK/GObject**: follow the split `mod imp` pattern used throughout the codebase

## Architecture

Read [ARCHITECTURE.md](ARCHITECTURE.md) for a detailed overview of the codebase, including the two-executor model (GTK + Tokio), the library trait abstraction, and the widget hierarchy.

Design documents for specific features live in the `docs/` directory.

## Pull Request Process

1. **Create a feature branch** from `main` — never commit directly to `main`
2. **Keep PRs focused** — one feature or fix per pull request
3. **Write tests** for new functionality using `#[cfg(test)]` modules and `#[tokio::test]` for async code
4. **Ensure `cargo test` passes** before submitting
5. **Write a clear PR description** explaining what changed and why

### Database changes

If your change modifies the SQLite schema:

1. Add a numbered migration in `src/library/db/migrations/`
2. Regenerate the offline query snapshot:
   ```bash
   cargo sqlx database create
   cargo sqlx migrate run
   cargo sqlx prepare
   ```
3. Commit the updated `.sqlx/` directory with your PR

## Code of Conduct

This project follows the [Contributor Covenant Code of Conduct](CODE_OF_CONDUCT.md). By participating, you are expected to uphold this code.
