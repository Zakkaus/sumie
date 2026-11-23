# sumie

[English](README.md)

sumie 是以 Rust 編寫的 GPU 終端機模擬器，採用 libghostty-vt 與 wgpu。在 Linux Wayland 上可開啟終端機，按 Ctrl+Shift+V 即可貼上剪貼簿文字。macOS 支援尚在規劃中。

## 建置與檢查

建置骨架需要 Rust 1.96 以上版本。接入 libghostty-vt 後，建置該函式庫還需要 Zig 0.17。在儲存庫根目錄執行 `cargo build --workspace --locked` 建置骨架。

安裝 Git、Python 3.11 以上版本、rustfmt 與 Clippy 後，在儲存庫根目錄執行 `scripts/check-all.sh`。加上 `--fast` 可略過會暫時修改原始碼的反向驗證。crate 邊界與 Git hook 安裝方式見 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 在 Linux 比較字型

用 `SUMIE_FONT` 指定字型家族，並用 `SUMIE_FONT_PX` 指定邏輯像素字級。
`SUMIE_FONT_HINTING` 接受 `off`（預設）、`light` 或 `normal`。
用 `SUMIE_FONT_WEIGHT` 設定主要字型的字重，接受 100 至 900 的整數；未設定時沿用平台的預設配對。

## 授權

[GNU Affero 通用公眾授權條款第 3 版或更新版本](LICENSE)（`AGPL-3.0-or-later`）。
