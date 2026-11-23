# sumie

[English](README.md)

sumie 是以 Rust 編寫的 GPU 終端機模擬器，採用 libghostty-vt 與 wgpu。在 Linux Wayland 上，按住滑鼠左鍵拖曳即可選取文字，放開時會將文字複製到剪貼簿。按 Ctrl+Shift+C 可再次複製選取的文字，按 Ctrl+Shift+V 可貼上。

在 macOS 13 以上版本，sumie 會開啟 Metal 測試頁，不啟動 shell。執行 `cargo run --locked -- --exit-after-frames 3 --screenshot smoke.png` 可將第三個畫格存為 PNG，隨後關閉視窗。

在 Linux 上首次啟動時，sumie 使用 ncurses 的 `tic` 將 `xterm-sumie` 安裝到 `$XDG_DATA_HOME/sumie/terminfo`（預設為 `~/.local/share/sumie/terminfo`）。內附的原始碼有變更時才重新編譯；若無法安裝，sumie 會改用 `TERM=xterm-256color` 啟動 shell。
滾輪每格捲動三行歷史記錄。程式若開啟滑鼠追蹤，滾輪事件會送給程式；此時按一下左鍵可點擊，拖曳可選取終端機文字，按住 Alt 拖曳則交由程式處理。`less` 等使用替代螢幕的程式若未追蹤滑鼠，且已開啟模式 1007（預設開啟），滾輪每格會送出三次方向鍵。捲動會清除選取範圍，輸入文字會回到底部。

## 建置與檢查

建置骨架需要 Rust 1.96 以上版本。接入 libghostty-vt 後，建置該函式庫還需要 Zig 0.17。在儲存庫根目錄執行 `cargo build --workspace --locked` 建置骨架。

安裝 Git、Python 3.11 以上版本、rustfmt 與 Clippy 後，在儲存庫根目錄執行 `scripts/check-all.sh`。加上 `--fast` 可略過會暫時修改原始碼的反向驗證。crate 邊界與 Git hook 安裝方式見 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 設定 Linux 字型與配色

建立 `$XDG_CONFIG_HOME/sumie/sumie.toml`；未設定 `XDG_CONFIG_HOME` 時，使用 `~/.config/sumie/sumie.toml`。

```toml
#:schema ./sumie.schema.json
include = []
minimum-contrast = 1.1 # 設為 1.0 可停用文字對比校正

[font]
family = "JetBrains Mono"
size = 15.0
hinting = "light"

[colors]
foreground = "#cccccc"
background = "#1e1e1e"
```

在 Linux 上，儲存主設定檔或 `include` 引入的檔案即可套用變更，不必重新啟動 sumie。設定有誤時，sumie 會沿用上一份有效設定，並在視窗頂端顯示檔名、行號與欄號，直到儲存有效的設定為止。色碼採用 `#rrggbb` 格式；若設定 `palette`，須依索引順序列出全部 16 個 ANSI 色碼。

`size` 以 96 DPI 下的點數為單位。執行 `sumie config show --defaults` 可查看所有選項與預設值。
要啟用編輯器自動補全，請在設定目錄執行 `sumie config schema > sumie.schema.json`。
比對字型時，`SUMIE_FONT`、`SUMIE_FONT_PX`（邏輯像素）、`SUMIE_FONT_WEIGHT` 和 `SUMIE_FONT_HINTING` 優先於設定檔。

## 授權

[GNU Affero 通用公眾授權條款第 3 版或更新版本](LICENSE)（`AGPL-3.0-or-later`）。
