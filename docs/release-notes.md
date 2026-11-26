This is a preview release. Expect rough edges and changes between versions.

Checksums are in `SHA256SUMS`.

## macOS

- arm64 (Apple silicon) only.
- The app is ad-hoc signed and not notarized. Gatekeeper blocks the first launch: right-click
  `sumie.app` and choose Open, or run `xattr -dr com.apple.quarantine /Applications/sumie.app`.
- The declared minimum is macOS 13. Only macOS 26 and 27 are tested.

## Linux

- Wayland only; there is no X11 backend.
- x86_64 and aarch64 tarballs. The runtime libraries are listed in the README.
