# sumie

[English](README.md)

sumie 是以 Rust 編寫的 GPU 終端機模擬器，採用 libghostty-vt 與 wgpu。在 Linux Wayland 上，按住滑鼠左鍵拖曳即可選取文字，放開時會將文字複製到剪貼簿。按 Ctrl+Shift+C 可再次複製選取的文字，按 Ctrl+Shift+V 可貼上。macOS 支援尚在規劃中。

## 建置與檢查

建置骨架需要 Rust 1.96 以上版本。接入 libghostty-vt 後，建置該函式庫還需要 Zig 0.17。在儲存庫根目錄執行 `cargo build --workspace --locked` 建置骨架。

安裝 Git、Python 3.11 以上版本、rustfmt 與 Clippy 後，在儲存庫根目錄執行 `scripts/check-all.sh`。加上 `--fast` 可略過會暫時修改原始碼的反向驗證。crate 邊界與 Git hook 安裝方式見 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 設定 Linux 字型

建立 `$XDG_CONFIG_HOME/sumie/sumie.toml`；未設定 `XDG_CONFIG_HOME` 時，使用 `~/.config/sumie/sumie.toml`。

```toml
#:schema ./sumie.schema.json
include = []

[font]
family = "JetBrains Mono"
size = 15.0
hinting = "light"
```

`size` 以 96 DPI 下的點數為單位。執行 `sumie config show --defaults` 可查看所有選項與預設值。
要啟用編輯器自動補全，請在設定目錄執行 `sumie config schema > sumie.schema.json`。
比對字型時，`SUMIE_FONT`、`SUMIE_FONT_PX`（邏輯像素）、`SUMIE_FONT_WEIGHT` 和 `SUMIE_FONT_HINTING` 優先於設定檔。

## 授權

[GNU Affero 通用公眾授權條款第 3 版或更新版本](LICENSE)（`AGPL-3.0-or-later`）。
