# sumie

[English](README.md)

sumie 是以 Rust 編寫的 GPU 終端機模擬器，目標平台為 Linux Wayland 與 macOS，採用 libghostty-vt 與 wgpu 的架構。狀態：pre-alpha，P0。目前可建置工作區骨架，但尚不能開啟視窗或啟動終端機工作階段。

## 建置與檢查

建置骨架需要 Rust 1.96 以上版本。接入 libghostty-vt 後，建置該函式庫還需要 Zig 0.17。在儲存庫根目錄執行 `cargo build --workspace --locked` 建置骨架。

安裝 Git、Python 3.11 以上版本、rustfmt 與 Clippy 後，在儲存庫根目錄執行 `scripts/check-all.sh`。加上 `--fast` 可略過會暫時修改原始碼的反向驗證。crate 邊界與 Git hook 安裝方式見 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 授權

[GNU Affero 通用公眾授權條款第 3 版或更新版本](LICENSE)（`AGPL-3.0-or-later`）。
