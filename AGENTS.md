This file takes precedence over any other repository file in case of conflict.

- Read the [design book](https://claude.ai/artifact/A3KR8SWWJouwinJsSZUje8) and [ARCHITECTURE.md](ARCHITECTURE.md) before changing a crate boundary, module, or dependency.
- `src/main.rs` is the composition root; `crates/` contains the logic and adapters.
- `scripts/check-all.sh` runs the merge gate; `--fast` runs the read-only pre-commit checks.
- Read the design book's invariants before implementing a feature. In its first change, wire the relevant check and show that a deliberate violation fails it.
- Read the architecture limits before writing unsafe code or adding a dependency.
