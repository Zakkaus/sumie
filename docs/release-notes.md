This is a preview release. Expect rough edges and changes between versions.

Checksums are in `SHA256SUMS`.

## macOS

- arm64 (Apple silicon) only.
- Drag `sumie.app` to Applications. The app is signed ad hoc and not notarized. If Gatekeeper
  blocks the first launch, dismiss the warning, then go to System Settings → Privacy & Security
  and choose **Open Anyway**. Or run `xattr -dr com.apple.quarantine /Applications/sumie.app`.
- The app bundles JetBrains Mono, the default font, under the SIL Open Font License 1.1; the
  license is in `sumie.app/Contents/Resources/Fonts/OFL.txt`.
- The declared minimum is macOS 13. Only macOS 26 and 27 are tested.

## Linux

- Wayland only; there is no X11 backend.
- x86_64 and aarch64 tarballs. The runtime libraries are listed in the README.
