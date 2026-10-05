//! 表示の設定と、その保存・読み込み。
//!
//! 設定は `~/Library/Application Support/<識別子>/settings.jsonc` に、JSON で書く。
//! コメントと末尾のカンマは書いてよい（ただし、アプリが保存するときは、コメントなしで書き直す）。
//! ファイルがない・読めない・壊れているときは、既定値で動く（落ちない）。
//! 識別子は、`scripts/bundle.sh` の `BUNDLE_ID` と同じにすること。

use std::path::{Path, PathBuf};

pub(crate) const APP_ID: &str = "net.naoki-workspace.csv-viewer";
const FILE_NAME: &str = "settings.jsonc";

/// 表示の設定。すべてのウィンドウで共通。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ViewSettings {
    /// 左端の行番号の欄を出すか。
    pub(crate) show_row_numbers: bool,
    /// 上端の列番号の行を出すか。
    pub(crate) show_col_numbers: bool,
}

impl gpui::Global for ViewSettings {}

impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            show_row_numbers: true,
            show_col_numbers: true,
        }
    }
}

impl ViewSettings {
    /// 設定ファイルの中身を読む。キーごとに読み、知らないキーや型の違う値は、無視する
    /// （新旧の版が混ざっても、手で書き間違えても、他のキーは生きるように）。
    pub(crate) fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        let Ok(value) = jsonc_parser::parse_to_serde_value::<serde_json::Value>(
            text,
            &jsonc_parser::ParseOptions::default(),
        ) else {
            return settings;
        };
        let flag = |key: &str| value.get(key).and_then(serde_json::Value::as_bool);
        if let Some(v) = flag("show_row_numbers") {
            settings.show_row_numbers = v;
        }
        if let Some(v) = flag("show_col_numbers") {
            settings.show_col_numbers = v;
        }
        settings
    }

    pub(crate) fn serialize(&self) -> String {
        let value = serde_json::json!({
            "show_row_numbers": self.show_row_numbers,
            "show_col_numbers": self.show_col_numbers,
        });
        let mut text = serde_json::to_string_pretty(&value).unwrap_or_default();
        text.push('\n');
        text
    }

    /// 既定の場所から読む。
    pub(crate) fn load() -> Self {
        settings_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    /// 既定の場所へ書く。失敗しても、アプリの動きは止めない（標準エラー出力に出すだけ）。
    pub(crate) fn save(&self) {
        let Some(path) = settings_path() else {
            return;
        };
        if let Err(e) = self.save_to(&path) {
            eprintln!("csv_viewer: could not save settings: {e}");
        }
    }

    /// 一時ファイルに書いてから置き換える。途中で落ちても、設定ファイルが半端にならない。
    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("jsonc.tmp");
        std::fs::write(&tmp, self.serialize())?;
        std::fs::rename(&tmp, path)
    }
}

fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support")
            .join(APP_ID)
            .join(FILE_NAME),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_shows_both_numbers() {
        assert_eq!(ViewSettings::parse(""), ViewSettings::default());
        assert!(ViewSettings::default().show_row_numbers);
        assert!(ViewSettings::default().show_col_numbers);
    }

    #[test]
    fn settings_round_trip_for_every_combination() {
        for row in [true, false] {
            for col in [true, false] {
                let s = ViewSettings {
                    show_row_numbers: row,
                    show_col_numbers: col,
                };
                assert_eq!(ViewSettings::parse(&s.serialize()), s);
            }
        }
    }

    #[test]
    fn comments_and_trailing_commas_are_accepted() {
        let text = "// memo\n{\n  /* 行番号は不要 */\n  \"show_row_numbers\": false,\n  \"show_col_numbers\": true,\n}\n";
        assert_eq!(
            ViewSettings::parse(text),
            ViewSettings {
                show_row_numbers: false,
                show_col_numbers: true
            }
        );
    }

    #[test]
    fn unknown_keys_and_wrong_types_are_ignored_per_key() {
        let text =
            r#"{"future_option": 3, "show_row_numbers": false, "show_col_numbers": "maybe"}"#;
        assert_eq!(
            ViewSettings::parse(text),
            ViewSettings {
                show_row_numbers: false,
                show_col_numbers: true
            },
            "an unreadable value keeps the default"
        );
    }

    #[test]
    fn broken_or_unexpected_files_fall_back_to_the_defaults() {
        for text in ["", "   ", "{", "nonsense", "[1, 2]", "42", "null"] {
            assert_eq!(
                ViewSettings::parse(text),
                ViewSettings::default(),
                "{text:?}"
            );
        }
    }

    #[test]
    fn saving_creates_the_directory_and_replaces_the_file() {
        let dir = std::env::temp_dir().join(format!("csv_viewer_settings_{}", std::process::id()));
        let path = dir.join("nested").join(FILE_NAME);
        let hidden = ViewSettings {
            show_row_numbers: false,
            show_col_numbers: false,
        };
        hidden.save_to(&path).unwrap();
        assert_eq!(
            ViewSettings::parse(&std::fs::read_to_string(&path).unwrap()),
            hidden
        );
        ViewSettings::default().save_to(&path).unwrap();
        assert_eq!(
            ViewSettings::parse(&std::fs::read_to_string(&path).unwrap()),
            ViewSettings::default()
        );
        assert!(!path.with_extension("jsonc.tmp").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
