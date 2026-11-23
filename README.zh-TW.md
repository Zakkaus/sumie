# sumie

[English](README.md)

sumie 是以 Rust 編寫的 GPU 終端機模擬器，採用 libghostty-vt 與 wgpu。在 Linux Wayland 上，按住滑鼠左鍵拖曳可選取文字，雙擊可選取字詞或完整網址，三擊可選取整個邏輯行（包含自動折行的部分）。雙擊或三擊後繼續拖曳，會以字詞或邏輯行為單位擴大選取範圍。放開左鍵時會複製選取的文字。按 Ctrl+Shift+C 可再次複製，按 Ctrl+Shift+V 可貼上。

按 Ctrl+Shift+F 可搜尋畫面與捲動記錄。搜尋會接續自動換行的文字，只有搜尋字串含大寫字母時才區分大小寫。結果由舊到新編號。按 Enter 或 F3 可跳到較舊的結果，同時按住 Shift 則跳到較新的結果。按 Escape 會回到開啟搜尋列前的位置；若原本在底部，則繼續跟隨輸出。

在 Wayland 上，按 Ctrl+Shift+T 可新增分頁。新分頁沿用目前 shell 透過 OSC 7 回報的本機目錄；若沒有可用的目錄，則從 `$HOME` 開始。新視窗一律從 `$HOME` 開始。按 Ctrl+Shift+W 可關閉目前分頁，子程序結束時也會關閉分頁。最後一個分頁關閉時，視窗會一併關閉。

按 Ctrl+Tab 或 Ctrl+PageDown 切換到下一個分頁，按 Ctrl+Shift+Tab 或 Ctrl+PageUp 切換到上一個。Alt+1 至 Alt+8 依位置選取分頁，Alt+9 選取最後一個。按 Ctrl+Shift+PageUp 或 Ctrl+Shift+PageDown 可將目前分頁向左或向右移動。開啟第二個分頁後才會顯示分頁列。按左鍵選取分頁，按中鍵關閉分頁，在分頁列上捲動滾輪可切換分頁。

在 macOS 13 以上版本，sumie 會開啟 Metal 測試頁，不啟動 shell。執行 `cargo run --locked -- --exit-after-frames 3 --screenshot smoke.png` 可將第三個畫格存為 PNG，隨後關閉視窗。

在 Linux 上首次啟動時，sumie 使用 ncurses 的 `tic` 將 `xterm-sumie` 安裝到 `$XDG_DATA_HOME/sumie/terminfo`（預設為 `~/.local/share/sumie/terminfo`）。內附的原始碼有變更時才重新編譯；若無法安裝，sumie 會改用 `TERM=xterm-256color` 啟動 shell。

滾輪每格捲動三行歷史記錄。程式若開啟滑鼠追蹤，滾輪事件會送給程式。`less` 等使用替代螢幕的程式若未追蹤滑鼠，且已開啟模式 1007（預設開啟），滾輪每格會送出三次方向鍵。滑鼠追蹤開啟時，按住 Shift 開始選取文字；拖曳途中放開 Shift，移動事件也不會送給程式。捲動會清除選取範圍，輸入文字會回到底部。

## 建置與檢查

建置骨架需要 Rust 1.96 以上版本。接入 libghostty-vt 後，建置該函式庫還需要 Zig 0.17。在儲存庫根目錄執行 `cargo build --workspace --locked` 建置骨架。

安裝 Git、Python 3.11 以上版本、rustfmt 與 Clippy 後，在儲存庫根目錄執行 `scripts/check-all.sh`。加上 `--fast` 可略過會暫時修改原始碼的反向驗證。crate 邊界與 Git hook 安裝方式見 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 在 Linux 上設定 sumie

建立 `$XDG_CONFIG_HOME/sumie/sumie.toml`；未設定 `XDG_CONFIG_HOME` 時，使用 `~/.config/sumie/sumie.toml`。

```toml
#:schema ./sumie.schema.json
include = []
minimum-contrast = 1.1 # 設為 1.0 可停用文字對比校正
scrollback-limit = 50000000

[font]
family = "JetBrains Mono"
size = 15.0
hinting = "light"

[colors]
foreground = "#cccccc"
background = "#1e1e1e"

[cursor]
style = "block"
blink = true
```

在 Linux 上，儲存主設定檔或 `include` 引入的檔案即可套用變更，不必重新啟動 sumie。設定有誤時，sumie 會沿用上一份有效設定，並在視窗頂端顯示檔名、行號與欄號，直到儲存有效的設定為止。色碼採用 `#rrggbb` 格式；若設定 `palette`，須依索引順序列出全部 16 個 ANSI 色碼。

`scrollback-limit` 以位元組設定捲動記錄的近似上限。調低上限可能立即刪除較舊的記錄。設為 `0` 會清除並停用捲動記錄。

粗體和斜體優先使用同一字型家族的對應字體，不改變字元格大小。SGR 2 將前景的不透明度降至 50%。底線支援單線、雙線、波浪線、點線和虛線，可用 SGR 58 單獨指定顏色。刪除線、頂線和底線可同時顯示。

`cursor.style` 可設為 `block`、`bar` 或 `underline`。終端機內的程式可用 DECSCUSR 覆寫形狀與閃爍設定，直到重設為止。視窗失焦時，可見的游標改為不閃爍的空心方框。

`size` 以 96 DPI 下的點數為單位。執行 `sumie config show --defaults` 可查看所有選項與預設值。
要啟用編輯器自動補全，請在設定目錄執行 `sumie config schema > sumie.schema.json`。
比對字型時，`SUMIE_FONT`、`SUMIE_FONT_PX`（邏輯像素）、`SUMIE_FONT_WEIGHT` 和 `SUMIE_FONT_HINTING` 優先於設定檔。

## 授權

[GNU Affero 通用公眾授權條款第 3 版或更新版本](LICENSE)（`AGPL-3.0-or-later`）。
