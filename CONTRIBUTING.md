# Contributing to Stet

Thanks for helping improve Stet.

## Before opening a change

- Search existing issues and pull requests.
- For substantial behavior or interface changes, open an issue first so the
  design can be discussed.
- Keep changes focused and include tests for observable behavior.

## Development setup

Stet requires Rust 1.90 or newer. On Linux, building also needs
`libxkbcommon-dev` and `libwayland-dev` (or your distribution's equivalents).

```console
git clone https://github.com/ArchAstro/stet.git
cd stet
cargo build --locked
cargo test --locked --all-targets --all-features
cargo run --release -- examples/tour.md
```

The editing core (`crates/stet-core`) has no platform code and holds most of the
tests. Anything a user can do with keys should be testable there with the key
notation harness in `crates/stet-core/src/editor/tests.rs`. Changes to markdown
analysis must keep the incremental path equal to a full parse:

```console
STET_FUZZ=20000 cargo test --release -p stet-core incremental_analysis
```

For changes you can only judge by looking, `Stet --screenshot out.png --keys
'...' file.md` renders a frame off-screen, and `Stet --drive` feeds scripted
input to a real window (see the README).

Before opening a pull request, run the same core checks as CI (`make
release-check` runs them all):

```console
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps --all-features
cargo test --locked --all-targets --all-features
cargo audit
```

Install `cargo-audit` with `cargo install cargo-audit --locked` if needed.

## Pull requests

Explain the problem, the chosen solution, and how you verified it. Update
documentation when behavior or configuration changes. By contributing, you
agree that your work is licensed under the repository's MIT license.

Be respectful and follow the [Code of Conduct](CODE_OF_CONDUCT.md).
