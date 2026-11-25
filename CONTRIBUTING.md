# Contributing

## Build

Follow [Build from source](README.md#build-from-source) for the toolchain: Rust 1.96, Zig
0.17.1 and the Linux development packages. Then:

```sh
cargo build --locked      # debug build; the first one compiles libghostty-vt
cargo run --locked        # open a window (Linux: needs a Wayland session)
```

Both commands honor `CARGO_TARGET_DIR` and `ZIG_GLOBAL_CACHE_DIR`. Point them at a directory
outside the worktree if you keep several checkouts, so they share the Zig downloads.

For a smoke test without a long-lived window, `sumie --exit-after-frames 3 --screenshot out.png`
renders three frames, writes the third and exits.

## Run the gate

```sh
scripts/install-hooks.sh     # once: runs `check-all.sh --fast` before every commit
scripts/check-all.sh --fast  # read-only checks: structure, terminfo, shell, fmt, clippy, test
scripts/check-all.sh         # the merge gate: the fast checks, then the negative controls
```

The gates need Python 3.11 or newer. CI runs `scripts/check-all.sh` on Linux x86_64, Linux
arm64 and macOS arm64, plus release-profile clippy on each. Run the full gate before you open a
pull request. [ARCHITECTURE.md](ARCHITECTURE.md#gates) lists what each gate checks.

### Negative controls

A gate that cannot fail proves nothing. `scripts/negative-controls.sh` makes a deliberate
violation for each gate (a forbidden crate dependency, a missing `forbid(unsafe_code)`, a
terminfo capability swapped for `RGB`, a shell hook that drops the command status, and so on),
confirms that the gate rejects it with the expected message, and restores the files.
It edits the worktree while it runs, so do not touch the tree meanwhile.

When you add a rule to a gate, add a control to `scripts/negative-controls.py` in the same
change, and show that the control fails the gate before you rely on the rule.

## Code boundaries

`src/main.rs` is the composition root and `crates/` holds the logic and adapters. Read
[ARCHITECTURE.md](ARCHITECTURE.md) before you change a crate boundary, add a module or add a
dependency: the table there lists what each crate owns, what it must not do and which crates it
may depend on, and the `structure` gate enforces it. Unsafe code is allowed only in the crates
that the table names, and every unsafe block needs a safety comment.

## Test on Linux without touching your desktop

Run the window in a private nested KWin, and inject keystrokes with the helper in
[scripts/nested/README.md](scripts/nested/README.md):

```sh
scripts/nested/run.sh 1.5 10 --type $'echo injected-ok\nexit\n' --after-ms 1500 -- target/debug/sumie
```

The runner creates its own `XDG_RUNTIME_DIR`, so it never touches the session bus of your
desktop. Do not replace it with `dbus-run-session` against your own runtime directory.
`scripts/perf/` holds the latency and stall measurements, and `scripts/macos-spike/` holds the
macOS runner scripts that CI uses.

## Add a configuration key

Settings live in `crates/sumie-config`, one file per table (for example `scroll.rs`).

1. Add the field to the table's struct as an `Option<T>` with a doc comment; the comment
   becomes the description in the JSON Schema. Use `#[schemars(with = "T", default = "...")]`
   so the schema shows the default, and keep `deny_unknown_fields`.
2. Add a getter that applies the default, and extend the table's `overlay` method so that a
   later file overrides an earlier include per key.
3. If a running window must react, add a flag to `Changes` in `changes.rs` and compare the
   effective values in `Config::changes`. Then handle the flag where the app applies a reload.
4. Pass the value from `Config` to the consumer in the binary. `sumie-core` does not depend on
   `sumie-config`, so the binary converts settings into core options.
5. Add a test in `crates/sumie-config/src/tests.rs` for the default, the override and
   rejection of a bad value. Check the key with `sumie config show --defaults` and
   `sumie config schema`.

## Commits and pull requests

Write the subject as a short imperative sentence in sentence case, with no prefix and no
trailing period, such as "Cache the Zig archive in CI and fetch it from community mirrors
first". Add a body when the reason is not obvious from the diff. Keep each commit to one
change that passes `scripts/check-all.sh --fast`.

Fix a bug by first writing a test that fails because of it. Until the first public release the
project keeps no backward compatibility: rename, replace or delete directly, with no shims for
old keys or formats.

## License

By contributing you agree that your work is licensed under AGPL-3.0-or-later.
