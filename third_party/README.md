# Third-party patches

`[patch.crates-io]` in the root `Cargo.toml` pins these forks by full rev, which must match `Cargo.lock`. Each `.patch` file is the reviewed diff, regenerated with `git diff <upstream tag> <rev>` in a clone of the fork; the `structure` gate rejects debug prints in added lines.

| Patch | Fork and rev | Upstream tag | Change |
| --- | --- | --- | --- |
| `wgpu/0001-drawable-timeout.patch` | [Zakkaus/wgpu](https://github.com/Zakkaus/wgpu) `sumie/30.0.2`, `391fb61f4c0bec28c8c2a368a5a90f525c2fd1eb` | `wgpu-hal-v30.0.2` | On macOS, `CAMetalLayer.allowsNextDrawableTimeout` is `true`, so `nextDrawable` returns nil after about 1 s and wgpu reports `Timeout` instead of blocking. |
| `winit/0001-macos.patch` | [Zakkaus/winit](https://github.com/Zakkaus/winit) `sumie/0.30.13-ime`, `2e4aace2ca68f68d210125f991581239232a7891` | `v0.30.13` | macOS `run_after_handler`, the Quit hook (`intercept_quit`, `finish_quit`), and quits that carry a reason (log out, restart, shut down) answered `NSTerminateLater`. Input methods: disallowing them discards the input method's marked text; Command chords skip the input method, and their key-ups pair by key code; an input-source change leaves the composition at once. |

The wgpu fork changes only `wgpu-hal`. `naga`, `naga-types`, `wgpu-types` and `wgpu-naga-bridge` are pinned to the same rev because the fork's `wgpu-hal` resolves them from its own checkout; their sources equal the crates.io 30.0.2 releases.

`scripts/macos-spike/quit.sh` checks that `winit/0001-macos.patch` equals the diff for the pinned rev before it builds the fixture from the same commit.
