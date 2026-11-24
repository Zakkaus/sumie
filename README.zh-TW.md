# sumie

[English](README.md)

sumie 是以 Rust 編寫的 GPU 終端機模擬器，採用 libghostty-vt 與 wgpu。在 Linux Wayland 上，按住滑鼠左鍵拖曳可選取文字，雙擊可選取字詞或完整網址，三擊可選取整個邏輯行（包含自動折行的部分）。雙擊或三擊後繼續拖曳，會以字詞或邏輯行為單位擴大選取範圍。放開左鍵時會複製選取的文字。按 Ctrl+Shift+C 可再次複製，按 Ctrl+Shift+V 可貼上。

在 Wayland 上，OSC 8 連結預設顯示淡色點線；終端機程式若指定底線樣式，則沿用該樣式。自動辨識的網址只在滑鼠停留時顯示點線。按住 Ctrl 時，整段連結（包含自動換行的部分）改用實線底線，滑鼠指標也會變成手形。按住 Ctrl 點擊連結，放開左鍵時會透過 `xdg-open` 開啟，並保留原有選取範圍；拖曳則重新選取文字。即使程式開啟滑鼠追蹤，這兩種操作仍然有效。sumie 不會開啟裸路徑、遠端 `file:` 網址、不在允許清單內的 URI，或含有控制字元或雙向文字控制符號的 URI。

按 Ctrl+Shift+F 可搜尋畫面與捲動記錄。搜尋會接續自動換行的文字，只有搜尋字串含大寫字母時才區分大小寫。結果由舊到新編號。按 Enter 或 F3 可跳到較舊的結果，同時按住 Shift 則跳到較新的結果。按 Escape 會關閉搜尋列，畫面留在目前的位置。
畫面在底部時才會跟隨新輸出。在終端機輸入會回到底部。

在搜尋欄按 Ctrl+A 可選取整段搜尋文字。輸入或貼上會取代選取的文字；Backspace 會刪除選取的文字，未選取時則刪除一個字素叢集。Home 和 End 分別移至開頭和結尾，Ctrl+Backspace 刪除前一個字詞。Ctrl+V 或 Shift+Insert 從剪貼簿貼上，每個換行會轉成一個空格。

在 Wayland 上，按 Ctrl+Shift+T 可新增分頁。按 Ctrl+Shift+E 可在目前窗格右側開啟新窗格，按 Ctrl+Shift+O 則在下方開啟。新分頁和新窗格沿用目前窗格透過 OSC 7 回報的本機目錄；若沒有可用的目錄，則從 `$HOME` 開始。新視窗一律從 `$HOME` 開始。

點選窗格即可輸入，也可按 Ctrl+Alt+方向鍵切換到相鄰窗格。拖曳分隔線或按 Ctrl+Shift+Alt+方向鍵可調整大小。按 Ctrl+Shift+Z 可讓目前窗格填滿分頁，再按一次即可還原。按 Ctrl+Shift+W 或結束 shell 會關閉目前窗格，原來的空間由同一次分割的另一側接手。關閉最後一個窗格時會關閉分頁，關閉最後一個分頁時會關閉視窗。

按 Ctrl+Tab 或 Ctrl+PageDown 切換到下一個分頁，按 Ctrl+Shift+Tab 或 Ctrl+PageUp 切換到上一個。Alt+1 至 Alt+8 依位置選取分頁，Alt+9 選取最後一個。按 Ctrl+Shift+PageUp 或 Ctrl+Shift+PageDown 可將目前分頁向左或向右移動。開啟第二個分頁後才會顯示分頁列。按左鍵選取分頁，按中鍵關閉分頁，在分頁列上捲動滾輪可切換分頁。

按 Ctrl+= 或 Ctrl++ 可放大終端機字型，按 Ctrl+- 可縮小，每次調整 1 pt，範圍為 4 至 200 pt。同一視窗的所有分頁共用字型大小；字元網格會重新排列，視窗尺寸不變。按 Ctrl+0 或修改設定檔中的 `[font].size` 可還原為設定值。

在 macOS 13 以上版本，sumie 會開啟 Metal 測試頁，不啟動 shell。執行 `cargo run --locked -- --exit-after-frames 3 --screenshot smoke.png` 可將第三個畫格存為 PNG，隨後關閉視窗。

在 Linux 上首次啟動時，sumie 使用 ncurses 的 `tic` 將 `xterm-sumie` 安裝到 `$XDG_DATA_HOME/sumie/terminfo`（預設為 `~/.local/share/sumie/terminfo`）。內附的原始碼有變更時才重新編譯；若無法安裝，sumie 會改用 `TERM=xterm-256color` 啟動 shell。

滾輪每格捲動三行歷史記錄。程式若開啟滑鼠追蹤，滾輪事件會送給程式。`less` 等使用替代螢幕的程式若未追蹤滑鼠，且已開啟模式 1007（預設開啟），滾輪每格會送出三次方向鍵。滑鼠追蹤開啟時，按住 Shift 開始選取文字；拖曳途中放開 Shift，移動事件也不會送給程式。捲動會清除選取範圍，輸入文字會回到底部。

在 Linux 上，程式可透過 Kitty 圖形協定顯示 PNG、RGB 與 RGBA 圖片，也可透過 SSH 直接傳送圖片資料。本機檔案傳輸（`t=f`、`t=t`）只接受 `/tmp` 或 `/dev/shm` 下位於 tmpfs、ramfs 或 devtmpfs 的一般檔案，每個檔案上限為 320 MiB。路徑必須是絕對路徑，不能經過符號連結，檔案也不能有多個硬連結。使用 `t=t` 時，sumie 只在路徑包含 `tty-graphics-protocol` 且仍指向開啟的檔案時嘗試刪檔。若圖片在其他位置，或 `/tmp` 位於磁碟上，請直接傳送圖片資料。目前不支援動畫與影片播放。

在主螢幕上，跟隨文字排列且高度超過三列的圖片會顯示為縮圖。使用替代螢幕時，或程式指定圖片尺寸、自行移動游標、使用相對定位或 Unicode 佔位字元時，終端機會保留圖片大小。點選縮圖即可在窗格內放大檢視；若程式追蹤滑鼠，請按住 Shift 再點選。按 Esc 或再點一次即可關閉。

在 `[images]` 中設定 `thumbnail-rows` 可調整新圖片的高度上限。使用 `timg --grid` 時請設為 `0`：它在主螢幕上依原圖大小計算格線位置，縮圖會打亂排列。

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
foreground = "#ffffff"
background = "#122738"

[cursor]
style = "block"
blink = true
```

在 Linux 上，儲存主設定檔或 `include` 引入的檔案即可套用變更，不必重新啟動 sumie。設定有誤時，sumie 會沿用上一份有效設定，並在視窗頂端顯示檔名、行號與欄號，直到儲存有效的設定為止。

預設配色為 Cobalt2。色碼採用 `#rrggbb` 格式；若設定 `palette`，須依索引順序列出全部 16 個 ANSI 色碼。

`scrollback-limit` 以位元組設定捲動記錄的近似上限。調低上限可能立即刪除較舊的記錄。設為 `0` 會清除並停用捲動記錄。

粗體和斜體優先使用同一字型家族的對應字體，不改變字元格大小。SGR 2 將前景的不透明度降至 50%。底線支援單線、雙線、波浪線、點線和虛線，可用 SGR 58 單獨指定顏色。刪除線、頂線和底線可同時顯示。

`cursor.style` 可設為 `block`、`bar` 或 `underline`。終端機內的程式可用 DECSCUSR 覆寫形狀與閃爍設定，直到重設為止。視窗失焦時，可見的游標改為不閃爍的空心方框。

`size` 以 96 DPI 下的點數為單位。執行 `sumie config show --defaults` 可查看所有選項與預設值。
要啟用編輯器自動補全，請在設定目錄執行 `sumie config schema > sumie.schema.json`。
比對字型時，`SUMIE_FONT`、`SUMIE_FONT_PX`（邏輯像素）、`SUMIE_FONT_WEIGHT` 和 `SUMIE_FONT_HINTING` 優先於設定檔。

## 授權

[GNU Affero 通用公眾授權條款第 3 版或更新版本](LICENSE)（`AGPL-3.0-or-later`）。
