# Third-party patches

`[patch.crates-io]` in the root `Cargo.toml` pins these forks by full rev. Each `.patch` file is the reviewed diff, regenerated with `git diff <upstream tag> <rev>` in a clone of the fork.

| Patch | Fork and rev | Upstream tag | Change |
| --- | --- | --- | --- |
| `wgpu/0001-drawable-timeout.patch` | [Zakkaus/wgpu](https://github.com/Zakkaus/wgpu) `sumie/30.0.2`, `391fb61f4c0bec28c8c2a368a5a90f525c2fd1eb` | `wgpu-hal-v30.0.2` | On macOS, `CAMetalLayer.allowsNextDrawableTimeout` is `true`, so `nextDrawable` returns nil after about 1 s and wgpu reports `Timeout` instead of blocking. |

The wgpu fork changes only `wgpu-hal`. `naga`, `naga-types`, `wgpu-types` and `wgpu-naga-bridge` are pinned to the same rev because the fork's `wgpu-hal` resolves them from its own checkout; their sources equal the crates.io 30.0.2 releases.
