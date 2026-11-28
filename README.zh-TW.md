# sumie

[English](README.md)

> **狀態：預覽版。** Linux（Wayland）版本已是可用的終端機。macOS 版本目前只開啟固定的 GPU 測試頁，不是終端機：PTY、分頁與輸入尚未接上。正式版發布前，設定檔與命令列介面不保證穩定。

## 螢幕截圖

尚未公開。

## 功能

在 Linux（Wayland）上：

- 繪製式分頁列，支援移動、循環切換與依編號選取的快捷鍵。
- 分割窗格，可依方向切換焦點、調整大小與放大。
- 捲動記錄支援平順的像素捲動（或逐行捲動），並可搜尋。
- 文字選取、複製貼上與可點擊的連結。
- 支援 Kitty 圖形協定的圖片，縮圖高度可設定。
- 以 HarfRust 進行文字排版，並透過 fontconfig 回退字型。
- 透過 `zwp_text_input_v3` 支援輸入法，並支援分數縮放。
- 依終端機跳脫序列顯示桌面通知與分頁進度（OSC 9;4）。
- bash、zsh 與 fish 的 shell 整合：向 sumie 回報工作目錄，用於分頁標籤及新分頁、新窗格。首次啟動時自動安裝，可用 `[shell] integration = false` 關閉。
- TOML 設定檔，sumie 執行時即時重新載入，並提供供編輯器使用的 JSON Schema。
- 視窗背景不透明度；合成器支援時，另有背景模糊。
- 文字對比校正（WCAG），預設開啟。

預設配色為 Cobalt2，預設字型為 JetBrains Mono。可安裝喜歡的字型並設定 `[font] family`。

快捷鍵、選取、連結、搜尋、窗格與分頁的說明見[在 Linux 上使用 sumie](#在-linux-上使用-sumie)。

## 在 Linux 上使用 sumie

sumie 是以 Rust 編寫的 GPU 終端機模擬器，採用 libghostty-vt 與 wgpu。在 Linux Wayland 上，按住滑鼠左鍵拖曳可選取文字，雙擊可選取字詞或完整網址，三擊可選取整個邏輯行（包含自動折行的部分）。雙擊或三擊後繼續拖曳，會以字詞或邏輯行為單位擴大選取範圍。放開左鍵時會複製選取的文字。按 Ctrl+Shift+C 可再次複製，按 Ctrl+Shift+V 可貼上。

在 Wayland 上，OSC 8 連結預設顯示淡色點線。自動辨識的網址只在滑鼠停留時顯示點線。按住 Ctrl 時，滑鼠指標變成手形，連結提示改為實線，換行處也適用。終端機程式指定的底線樣式與顏色不變。按住 Ctrl 點擊連結，放開左鍵時會透過 `xdg-open` 開啟，並保留原有選取範圍；拖曳則重新選取文字。即使程式開啟滑鼠追蹤，這兩種操作仍然有效。sumie 不會開啟裸路徑、遠端 `file:` 網址、不在允許清單內的 URI，或含有控制字元或雙向文字控制符號的 URI。

按 Ctrl+Shift+F 可搜尋畫面與捲動記錄。搜尋會接續自動換行的文字，只有搜尋字串含大寫字母時才區分大小寫。結果由舊到新編號。按 Enter 或 F3 可跳到較舊的結果，同時按住 Shift 則跳到較新的結果。按 Escape 會關閉搜尋列，畫面留在目前的位置。
畫面在底部時才會跟隨新輸出。在終端機輸入會回到底部。

在搜尋欄按 Ctrl+A 可選取整段搜尋文字。輸入或貼上會取代選取的文字；Backspace 會刪除選取的文字，未選取時則刪除一個字素叢集。左右方向鍵每次移動一個字素叢集，Home 和 End 分別移至開頭和結尾。Ctrl+Backspace 刪除前一個字詞。Ctrl+V 或 Shift+Insert 從剪貼簿貼上，每個換行會轉成一個空格。

在 Wayland 上，按 Ctrl+Shift+T 可新增分頁。按 Ctrl+Shift+E 可在目前窗格右側開啟新窗格，按 Ctrl+Shift+O 則在下方開啟。新分頁和新窗格沿用目前窗格透過 OSC 7 回報的本機目錄；若沒有可用的目錄，則從 `$HOME` 開始。新視窗一律從 `$HOME` 開始。

點選窗格即可輸入，也可按 Ctrl+Alt+方向鍵切換到相鄰窗格。拖曳分隔線或按 Ctrl+Shift+Alt+方向鍵可調整大小。按 Ctrl+Shift+Z 可讓目前窗格填滿分頁，再按一次即可還原。按 Ctrl+Shift+W 或結束 shell 會關閉目前窗格，原來的空間由同一次分割的另一側接手。關閉最後一個窗格時會關閉分頁，關閉最後一個分頁時會關閉視窗。

按 Ctrl+Tab 或 Ctrl+PageDown 切換到下一個分頁，按 Ctrl+Shift+Tab 或 Ctrl+PageUp 切換到上一個。Alt+1 至 Alt+8 依位置選取分頁，Alt+9 選取最後一個。按 Ctrl+Shift+PageUp 或 Ctrl+Shift+PageDown 可將目前分頁向左或向右移動。

開啟第二個分頁後才會顯示分頁列。點選分頁或在分頁列上捲動滾輪可切換分頁。空間不足以維持標題可讀時，分頁列會捲動，讓目前分頁保持可見。按最後一個可見分頁旁的 **+** 可新增分頁。標題旁的圓點表示該分頁在背景收到輸出，切換到該分頁後圓點便會消失。按中鍵可關閉分頁，也可按目前分頁或指標所在分頁上的 **×**。

按 Ctrl+= 或 Ctrl++ 可放大終端機字型，按 Ctrl+- 可縮小，每次調整 1 pt，範圍為 4 至 200 pt。同一視窗的所有分頁共用字型大小；字元網格會重新排列，視窗尺寸不變。按 Ctrl+0 或修改設定檔中的 `[font].size` 可還原為設定值。

在 macOS 13 以上版本，sumie 會開啟 Metal 測試頁，不啟動 shell。執行 `cargo run --locked -- --exit-after-frames 3 --screenshot smoke.png` 可將第三個畫格存為 PNG，隨後關閉視窗。

在 Linux 上首次啟動時，sumie 使用 ncurses 的 `tic` 將 `xterm-sumie` 安裝到 `$XDG_DATA_HOME/sumie/terminfo`（預設為 `~/.local/share/sumie/terminfo`）。內附的原始碼有變更時才重新編譯；若無法安裝，sumie 會改用 `TERM=xterm-256color` 啟動 shell。

在 bash、zsh 與 fish 登入 shell 中，sumie 會把指令稿安裝到 `$XDG_DATA_HOME/sumie/shell-integration`，不修改你的啟動檔。這些指令稿標記提示符與指令（OSC 133），並回報工作目錄（OSC 7）。`ssh` 包裝函式只對 SSH 設定 `TERM=xterm-256color`，並要求轉送 `COLORTERM` 與 `TERM_PROGRAM`；伺服器須在 `AcceptEnv` 中允許這些變數。在 `[shell]` 設定 `integration = false` 可略過注入，`ssh` 也維持原樣。子行程另會取得 `FORCE_HYPERLINK=1`（已自行設定時不覆寫），讓 Claude Code 這類依終端機名稱判斷是否支援超連結的工具送出 OSC 8 連結。

在 Wayland 上，視窗取得鍵盤焦點時，終端機程式可用 OSC 52 向標準剪貼簿寫入最多 1 MiB 的解碼資料。sumie 拒絕 OSC 52 讀取，也不支援主選取區（primary selection）。OSC 0 與 OSC 2 會更新視窗標題；sumie 不支援 CSI 22/23 標題堆疊。

滾輪與觸控板會以裝置像素為單位平順捲動歷史記錄，觸控板在手指離開後還會順勢滑行一小段。程式若追蹤滑鼠、位於替代螢幕、開啟搜尋列，或設定檔提示列取得焦點時，仍維持逐行捲動，滾輪每格三行。在 `[scroll]` 設定 `smooth = false`，可一律改為逐行捲動（預設為 `true`）。程式若開啟滑鼠追蹤，滾輪事件會送給程式。`less` 等使用替代螢幕的程式若未追蹤滑鼠，且已開啟模式 1007（預設開啟），滾輪每格會送出三次方向鍵。滑鼠追蹤開啟時，按住 Shift 開始選取文字；拖曳途中放開 Shift，移動事件也不會送給程式。畫面移動滿一整行時會清除選取範圍，輸入文字會回到底部。

在 Linux 上，程式可透過 Kitty 圖形協定顯示 PNG、RGB 與 RGBA 圖片，也可透過 SSH 直接傳送圖片資料。本機檔案傳輸（`t=f`、`t=t`）只接受 `/tmp` 或 `/dev/shm` 下位於 tmpfs、ramfs 或 devtmpfs 的一般檔案，每個檔案上限為 320 MiB。路徑必須是絕對路徑，不能經過符號連結，檔案也不能有多個硬連結。使用 `t=t` 時，sumie 只在路徑包含 `tty-graphics-protocol` 且仍指向開啟的檔案時嘗試刪檔。若圖片在其他位置，或 `/tmp` 位於磁碟上，請直接傳送圖片資料。

程式可透過 Kitty 圖形協定傳送動畫影格、合成影格及控制播放。縮圖與放大檢視都會顯示目前的影格。播放影片可執行 `mpv --vo=kitty FILE`，使用 mpv 預設的直接傳輸模式。sumie 不支援共享記憶體傳輸（`t=s`）。

在主螢幕上，跟隨文字排列且高度超過三列的圖片會顯示為縮圖。使用替代螢幕時，或程式指定圖片尺寸、自行移動游標、使用相對定位或 Unicode 佔位字元時，終端機會保留圖片大小。點選縮圖即可在窗格內放大檢視；若程式追蹤滑鼠，請按住 Shift 再點選。按 Esc 或再點一次即可關閉。

在 `[images]` 中設定 `thumbnail-rows` 可調整新圖片的高度上限。使用 `timg --grid` 時請設為 `0`：它在主螢幕上依原圖大小計算格線位置，縮圖會打亂排列。

## 安裝

### 從發行版安裝

從[發行頁面](https://github.com/Zakkaus/sumie/releases)下載封存檔，並以 `SHA256SUMS` 核對。

**Linux。** `sumie-VERSION-x86_64-linux.tar.gz` 與 `sumie-VERSION-aarch64-linux.tar.gz` 內含執行檔、terminfo 原始碼、shell 整合指令稿、桌面項目與圖示。解壓縮後可在該目錄執行 `./sumie`。執行檔需要 Wayland 合成器，沒有 X11 後端。

若要在應用程式選單啟動 sumie，請於解壓縮後的目錄執行：

```sh
install -Dm755 sumie "$HOME/.local/bin/sumie"
mkdir -p "${XDG_DATA_HOME:-$HOME/.local/share}"
cp -R share/. "${XDG_DATA_HOME:-$HOME/.local/share}/"
```

桌面工作階段的 `PATH` 必須包含 `~/.local/bin`。若發行版會在登入時加入該目錄，建立目錄後請登出再登入；否則請自行將它加入工作階段的 `PATH`。

**macOS。** `sumie-VERSION-aarch64-macos.dmg` 僅適用於 Apple 晶片。開啟後將 `sumie.app` 拖曳到「應用程式」。此 App 使用臨時簽章（ad-hoc），並非 Developer ID 簽章，也未經公證，因此下載後會被 macOS 隔離。請擇一處理：

- 先開啟 App 一次並關閉警告，再到「系統設定」的「隱私權與安全性」，在 sumie 旁選擇「仍要打開」。
- 或移除隔離屬性：`xattr -dr com.apple.quarantine /Applications/sumie.app`。

宣告的最低版本為 macOS 13，但只在 macOS 26 與 27 上測試過。如開頭所述，macOS 版目前只顯示 GPU 測試頁。

### 發行版套件

AUR 與 Gentoo 套件已列入計畫，將在本專案內維護。其他發行版請見 [docs/packaging.md](docs/packaging.md)。

## 從原始碼建置

需要：

- Rust 1.96 以上版本。
- [Zig](https://ziglang.org/download/) 0.17.1。建置時以 Zig 編譯 libghostty-vt，並檢查 `zig version` 以 `0.17.` 開頭；Zig 不在 `PATH` 時，用 `ZIG` 指定路徑。
- `curl`、`tar` 與 `shasum`（建置會下載並驗證固定版本的 Ghostty 原始碼與 Zig 套件；離線建置見 [docs/packaging.md](docs/packaging.md)）。
- Linux：`pkg-config`，以及 `wayland` 與 `xkbcommon` 的開發檔（Debian 與 Ubuntu 為 `libwayland-dev libxkbcommon-dev`）。
- macOS：Xcode 命令列工具。

```sh
cargo build --release --locked
target/release/sumie
```

首次建置會編譯 libghostty-vt，需要數分鐘。設定 `ZIG_GLOBAL_CACHE_DIR` 可在乾淨重建之間保留 Zig 套件的下載內容。執行檢查見 [CONTRIBUTING.md](CONTRIBUTING.md)，crate 邊界見 [ARCHITECTURE.md](ARCHITECTURE.md)。

### Linux 執行階段需求

執行檔只連結 libc、libgcc 與 `libxkbcommon.so.0`。下列函式庫在執行時才載入，缺少時程式仍可啟動，但無法開啟視窗或繪圖：

| 函式庫 | 用途 |
| --- | --- |
| `libwayland-client.so.0` 與 Wayland 合成器 | 視窗、輸入與剪貼簿 |
| `libEGL.so.1`、`libwayland-egl.so.1` | GL ES 3 繪圖 |
| `libvulkan.so.1` 與 Vulkan 驅動程式 | Vulkan 繪圖 |
| `libfontconfig.so.1` | 尋找字型 |
| `tic`（ncurses） | 首次啟動時編譯內附的 `xterm-sumie` terminfo 項目 |

Vulkan 與 GL ES 3 至少需要一套，且驅動程式可正常運作。Debian 與 Ubuntu 可安裝 `libwayland-client0 libwayland-egl1 libegl1 libvulkan1 libfontconfig1 ncurses-bin`，再加上 Mesa 驅動程式（`mesa-vulkan-drivers libgl1-mesa-dri`）或顯示卡廠商的驅動程式。

## 設定 sumie

建立 `$XDG_CONFIG_HOME/sumie/sumie.toml`；未設定 `XDG_CONFIG_HOME` 時，使用 `~/.config/sumie/sumie.toml`。檔案不存在時使用預設值。sumie 會監看該檔案與其 `include = [...]` 引入的檔案（路徑相對於引入它的檔案），並將變更套用到執行中的視窗；檔案有誤時，沿用上一份有效設定。macOS 使用相同的路徑。

```toml
#:schema ./sumie.schema.json
include = []
minimum-contrast = 1.1 # 設為 1.0 可停用文字對比校正
scrollback-limit = 50000000

[window]
padding-x = 8
padding-y = 6

[font]
family = "JetBrains Mono"
size = 15.0 # 96 DPI 下的點數
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

| `[window]` 設定 | 預設值 | 說明 |
| --- | ---: | --- |
| `padding-x` | `8` | 各窗格左右兩側的內距，以邏輯像素為單位；設為 `0` 即取消。 |
| `padding-y` | `6` | 各窗格上下兩側的內距，以邏輯像素為單位；設為 `0` 即取消。 |
| `background-opacity` | `1.0` | 預設背景、窗格內距與窗格間隙的不透明度，範圍從 `0.0`（完全透明）到 `1.0`（完全不透明）。文字、分頁列及程式指定的背景色不受影響。 |
| `background-blur` | `false` | 若 Wayland 合成器透過 `ext-background-effect-v1` 提供模糊效果，開啟此設定即可模糊半透明視窗後方的內容。不支援時仍可使用透明背景。 |
粗體和斜體優先使用同一字型家族的對應字體，不改變字元格大小。SGR 2 將前景的不透明度降至 50%。底線支援單線、雙線、波浪線、點線和虛線，可用 SGR 58 單獨指定顏色。刪除線、頂線和底線可同時顯示。

`cursor.style` 可設為 `block`、`bar` 或 `underline`。終端機內的程式可用 DECSCUSR 覆寫形狀與閃爍設定，直到重設為止。視窗失焦時，可見的游標改為不閃爍的空心方框。

執行 `sumie config show --defaults` 可查看所有選項。要啟用編輯器自動補全，請在設定目錄執行 `sumie config schema > sumie.schema.json`。比對字型時，`SUMIE_FONT`、`SUMIE_FONT_PX`（邏輯像素）、`SUMIE_FONT_WEIGHT` 和 `SUMIE_FONT_HINTING` 優先於設定檔。`SUMIE_FRAME_LOG=PATH` 會寫入各畫格的階段時間戳記與重繪的窗格 ID（不含終端機內容），供多窗格畫格預算測試使用。

## 專案文件

- [CONTRIBUTING.md](CONTRIBUTING.md)：建置、測試，以及每項變更必須通過的檢查。
- [ARCHITECTURE.md](ARCHITECTURE.md)：各 crate 的職責與允許的依賴關係。
- [docs/packaging.md](docs/packaging.md)：打包、離線建置與安裝位置。

## 授權

[GNU Affero 通用公眾授權條款第 3 版或更新版本](LICENSE)（`AGPL-3.0-or-later`）。

圖示取自 Yuji Syuku 字型的輪廓設計（SIL Open Font License 1.1），見 [assets/icons/README.md](assets/icons/README.md)。

macOS 版應用程式內含 JetBrains Mono 2.304（SIL Open Font License 1.1，Copyright 2020 The JetBrains Mono Project Authors），因為 macOS 沒有預設字型。授權文字在 `sumie.app/Contents/Resources/Fonts/OFL.txt`。
