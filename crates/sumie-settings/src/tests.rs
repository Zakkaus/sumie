use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use sumie_config::{CursorStyle, load};

use crate::{
    input::{Decoder, Key},
    log::Log,
    model::{Mode, Model},
    screen::{Layout, Screen},
};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static ID: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "sumie-settings-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn press(model: &mut Model, keys: impl IntoIterator<Item = Key>) {
    for key in keys {
        assert!(model.key(key, 2));
    }
}

fn type_text(model: &mut Model, text: &str) {
    press(model, text.chars().map(Key::Char));
}

fn selected(model: &Model) -> &str {
    &model.options[model.current().unwrap()].info.path
}

fn model(main: &Path) -> Model {
    Model::new(main.to_owned(), Log::default())
}

#[test]
fn changes_write_immediately_to_the_defining_file() {
    let dir = Scratch::new();
    let earlier = dir.file("earlier.toml", "[font]\nsize = 12.0\n");
    let later = dir.file(
        "later.toml",
        "# Keep this\n[font]\nsize = 13.0 # points\nfamily = 'Fixed'\n",
    );
    let main = dir.file(
        "sumie.toml",
        "include = ['earlier.toml', 'later.toml'] # order\n",
    );
    let mut model = model(&main);
    press(&mut model, [Key::Char('2'), Key::Down, Key::Right]);
    assert_eq!(
        fs::read_to_string(&later).unwrap(),
        "# Keep this\n[font]\nsize = 13.1 # points\nfamily = 'Fixed'\n"
    );
    assert_eq!(
        fs::read_to_string(&earlier).unwrap(),
        "[font]\nsize = 12.0\n"
    );
    press(&mut model, [Key::Char('r'), Key::End, Key::Right]);
    assert_eq!(
        fs::read_to_string(&main).unwrap(),
        "include = ['earlier.toml', 'later.toml'] # order\n\n[font]\nhinting = \"normal\"\n"
    );
    assert_eq!(
        fs::read_to_string(&later).unwrap(),
        "# Keep this\n[font]\n# points\nfamily = 'Fixed'\n"
    );
    assert_font_size(&main, 12.0);
    assert_eq!(
        fs::read_to_string(&earlier).unwrap(),
        "[font]\nsize = 12.0\n"
    );
    press(&mut model, [Key::Up, Key::Up, Key::Char('r')]);
    assert_font_size(&main, 15.0);
    assert_eq!(fs::read_to_string(&earlier).unwrap(), "[font]\n");
    assert!(!model.key(Key::Char('q'), 2));
}

#[test]
fn browse_and_search_handle_pages_and_committed_utf8() {
    let dir = Scratch::new();
    let main = dir.file("sumie.toml", "language = 'zh-TW'\n");
    let mut model = model(&main);
    press(&mut model, [Key::Tab, Key::PageDown]);
    assert_eq!(selected(&model), "font.weight");
    press(
        &mut model,
        [Key::PageUp, Key::Char('j'), Key::Char('k'), Key::Char('5')],
    );
    assert_eq!(selected(&model), "font.family");
    let mut decoder = Decoder::default();
    for chunk in [
        b"/\xe6".as_slice(),
        b"\xb8\xb8\xe6\xa8",
        b"\x99\xe9\x96\x83\xe7\x88\x8d",
        b"\r",
    ] {
        press(
            &mut model,
            chunk.iter().filter_map(|&byte| decoder.push(byte)),
        );
    }
    assert_eq!(selected(&model), "cursor.blink");
    press(&mut model, [Key::Char('h')]);
    assert!(!load(&main).unwrap().cursor.blink());
    press(&mut model, [Key::Char('k'), Key::Left]);
    assert_eq!(load(&main).unwrap().cursor.style(), CursorStyle::Underline);
    assert!(!model.key(Key::Esc, 2));
}

#[test]
fn typed_values_are_validated_and_foreign_saves_are_not_overwritten() {
    let dir = Scratch::new();
    let main = dir.file("sumie.toml", "language = 'en'\n[font]\nsize = 16.0\n");
    let mut model = model(&main);
    press(
        &mut model,
        [
            Key::Char('2'),
            Key::Down,
            Key::Enter,
            Key::Backspace,
            Key::Backspace,
        ],
    );
    type_text(&mut model, "-1");
    press(&mut model, [Key::Enter]);
    assert!(matches!(model.mode, Mode::Edit(_)));
    assert_eq!(model.notice().as_deref(), Some("Expected: Number · > 0"));
    assert_font_size(&main, 16.0);
    fs::write(&main, "language = 'en'\n[font]\nsize = 18.0\n").unwrap();
    press(&mut model, [Key::Backspace, Key::Backspace]);
    type_text(&mut model, "20.5");
    press(&mut model, [Key::Enter]);
    assert_eq!(
        model.notice().as_deref(),
        Some("The setting changed on disk. Review it and try again.")
    );
    assert_font_size(&main, 18.0);
    press(&mut model, [Key::Enter, Key::Backspace, Key::Backspace]);
    type_text(&mut model, "20.5");
    press(&mut model, [Key::Enter]);
    assert_font_size(&main, 20.5);
    press(&mut model, [Key::Up, Key::Enter, Key::Home]);
    type_text(&mut model, "新字型 ");
    press(&mut model, [Key::Enter]);
    assert_eq!(load(&main).unwrap().font.family(), "新字型 JetBrains Mono");
}

fn assert_font_size(main: &Path, points: f32) {
    let expected_px = points * sumie_config::POINT_TO_PX;
    assert_eq!(
        load(main).unwrap().font.logical_px().to_bits(),
        expected_px.to_bits()
    );
}

#[test]
fn reset_leaves_lower_priority_overrides_untouched() {
    let dir = Scratch::new();
    let included = dir.file("fonts.toml", "[font]\nweight = 500\nfamily = 'Fixed'\n");
    let main = dir.file(
        "sumie.toml",
        "include = ['fonts.toml']\n[font]\nweight = 600\n",
    );
    let mut model = model(&main);
    press(&mut model, [Key::Char('2'), Key::PageDown, Key::Char('r')]);
    assert_eq!(load(&main).unwrap().font.weight(), Some(500));
    assert_eq!(
        fs::read_to_string(included).unwrap(),
        "[font]\nweight = 500\nfamily = 'Fixed'\n"
    );
    assert_eq!(
        fs::read_to_string(&main).unwrap(),
        "include = ['fonts.toml']\n[font]\n"
    );
}

#[test]
fn key_sequences_do_not_turn_into_text_or_accidental_quits() {
    let mut decoder = Decoder::default();
    let keys: Vec<_> = b"\x1b[6~\x1b[5~\x1bOB\x1b[A\x1b[C\x1bOD\t\x1bq\x1b"
        .iter()
        .filter_map(|&byte| decoder.push(byte))
        .collect();
    assert_eq!(
        keys,
        [
            Key::PageDown,
            Key::PageUp,
            Key::Down,
            Key::Up,
            Key::Right,
            Key::Left,
            Key::Tab
        ]
    );
    assert_eq!(decoder.escape_timeout(), Some(Key::Esc));
    assert_eq!(decoder.push(3), Some(Key::Interrupt));
}

#[test]
fn bracketed_paste_is_text_only_and_never_submits() {
    let dir = Scratch::new();
    let main = dir.file("sumie.toml", "language = 'en'\n");
    let mut model = model(&main);
    let mut decoder = Decoder::default();
    for chunk in [
        b"\x1b[200~q\r2\x1b[201~2\r\x1b[20".as_slice(),
        "0~甲\n乙\x1b[2".as_bytes(),
        b"01~",
    ] {
        press(
            &mut model,
            chunk.iter().filter_map(|&byte| decoder.push(byte)),
        );
    }
    assert_eq!(selected(&model), "font.family");
    assert_eq!(fs::read_to_string(&main).unwrap(), "language = 'en'\n");
    press(&mut model, [decoder.push(b'\r').unwrap()]);
    assert_eq!(load(&main).unwrap().font.family(), "JetBrains Mono甲 乙");
}

#[test]
fn invalid_number_reports_localized_inclusive_range() {
    let dir = Scratch::new();
    let main = dir.file("sumie.toml", "language = 'zh-TW'\n");
    let mut model = model(&main);
    press(&mut model, [Key::Char('2'), Key::PageDown, Key::Enter]);
    type_text(&mut model, "99");
    press(&mut model, [Key::Enter]);
    assert_eq!(model.notice().as_deref(), Some("請輸入：整數 · 100–900"));
    assert_eq!(load(&main).unwrap().font.weight(), None);
}

#[test]
fn render_wide_and_narrow_in_en_and_zh_tw() {
    use unicode_width::UnicodeWidthStr as _;

    let dir = Scratch::new();
    for (language, width, expected) in [
        ("en", 100, EN_WIDE),
        ("en", 40, EN_NARROW),
        ("zh-TW", 100, ZH_WIDE),
        ("zh-TW", 40, ZH_NARROW),
    ] {
        let main = dir.file(
            "sumie.toml",
            &format!("language = '{language}'\n[font]\nfamily = '甲乙丙丁戊己庚辛壬癸'\n"),
        );
        let mut model = model(&main);
        press(&mut model, [Key::Char('2')]);
        let mut screen = Screen::default();
        let layout = Layout::new(&model, width, 8);
        screen.render(&model, width, 8, &layout);
        assert!(screen.rows.iter().all(|row| row.width() <= width));
        assert_eq!(
            screen.rows.join("\n"),
            expected,
            "{language} at {width} columns"
        );
    }
}

const EN_WIDE: &str = concat!(
    "1 Appearance             2 [Font]                 3 Colors                 4 Behavior\n",
    ">* Family                   甲乙丙丁戊己庚辛壬癸   Choose a different installed font family for the…\n",
    "   Size                     15                     Default: JetBrains Mono\n",
    "   Weight                   Automatic              Allowed: Text\n",
    "   Hinting                  Light\n\n\n",
    "↑↓ Select  ←→ Change  Enter Edit  / Search  r Reset  Esc Quit",
);
const EN_NARROW: &str = concat!(
    "1 Appear… 2 [Font]  3 Colors  4 Behavior\n",
    ">* Family               甲乙丙丁戊己庚…\n",
    "   Size                 15\n",
    "   Weight               Automatic\n",
    "Choose a different installed font famil…\n",
    "Default: JetBrains Mono\n",
    "Allowed: Text\n",
    "↑↓ Select  ←→ Change  Enter Edit  / Sea…",
);
const ZH_WIDE: &str = concat!(
    "1 外觀                   2 [字型]                 3 配色                   4 行為\n",
    ">* 字型家族                 甲乙丙丁戊己庚辛壬癸   選擇已安裝的字型家族，用於終端機文字。\n",
    "   字級                     15                     預設值: JetBrains Mono\n",
    "   字重                     自動                   可用值: 文字\n",
    "   字形微調                 輕度\n\n\n",
    "↑↓ 選取  ←→ 修改  Enter 編輯  / 搜尋  r 還原  Esc 離開",
);
const ZH_NARROW: &str = concat!(
    "1 外觀    2 [字型]  3 配色    4 行為\n",
    ">* 字型家族             甲乙丙丁戊己庚…\n",
    "   字級                 15\n",
    "   字重                 自動\n",
    "選擇已安裝的字型家族，用於終端機文字。\n",
    "預設值: JetBrains Mono\n",
    "可用值: 文字\n",
    "↑↓ 選取  ←→ 修改  Enter 編輯  / 搜尋  r…",
);
