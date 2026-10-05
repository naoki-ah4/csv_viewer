//! 「行へ移動」の入力欄の状態と、移動先の決め方。画面には依存しない。
//!
//! 行番号は、ヘッダーを除いたデータ行の1始まり（画面左端の行番号の欄と同じ）。

/// 入力欄に受け付ける最大の桁数。`u64` に必ず収まる。
const MAX_DIGITS: usize = 18;

/// 入力欄に届いたキーの分類。
#[derive(Debug, PartialEq, Eq)]
pub enum GotoKey {
    Digit(char),
    Backspace,
    Enter,
    Escape,
    /// 入力欄では使わないキー。
    Ignore,
}

/// キーを分類する。`key` は押されたキー、`key_char` は入力された文字。
/// 全角の数字（IMEが確定した文字）も数字として受け付ける。
/// コマンドやコントロールなどの修飾キーが押されているときは、無視する。
pub fn classify(key: &str, key_char: Option<&str>, has_command_modifier: bool) -> GotoKey {
    if has_command_modifier {
        return GotoKey::Ignore;
    }
    match key {
        "backspace" => return GotoKey::Backspace,
        "enter" | "return" => return GotoKey::Enter,
        "escape" => return GotoKey::Escape,
        _ => {}
    }
    let typed = key_char
        .and_then(|s| s.chars().next())
        .or_else(|| key.chars().next().filter(|_| key.chars().count() == 1));
    match typed.and_then(ascii_digit) {
        Some(digit) => GotoKey::Digit(digit),
        None => GotoKey::Ignore,
    }
}

/// 数字なら、ASCII の数字に直す（全角の `０`〜`９` も受け付ける）。
fn ascii_digit(c: char) -> Option<char> {
    match c {
        '0'..='9' => Some(c),
        '０'..='９' => char::from_u32(c as u32 - '０' as u32 + '0' as u32),
        _ => None,
    }
}

/// 入力中の行番号。
#[derive(Debug, Default)]
pub struct GotoInput {
    text: String,
}

impl GotoInput {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn push_digit(&mut self, digit: char) {
        if self.text.len() < MAX_DIGITS {
            self.text.push(digit);
        }
    }

    pub fn backspace(&mut self) {
        self.text.pop();
    }

    /// 入力された行番号。空なら `None`。
    pub fn value(&self) -> Option<u64> {
        self.text.parse().ok()
    }
}

/// 移動先と、利用者に知らせる内容。
#[derive(Debug, PartialEq, Eq)]
pub struct Resolved {
    /// 移動先のデータ行（0始まり）。
    pub row: usize,
    pub notice: Option<String>,
}

/// 入力された行番号を、移動先に解決する。範囲外のときは、行ける所まで行って、理由を知らせる。
///
/// - `row_count`: 今ある（索引ができている）データ行の数
/// - `indexing`: 索引作成の途中か。途中なら、これから増える行もあるので、言い方を変える
pub fn resolve(input: u64, row_count: usize, indexing: bool) -> Resolved {
    if row_count == 0 {
        let notice = if indexing {
            "行はまだ読み込み中です"
        } else {
            "移動できる行がありません"
        };
        return Resolved {
            row: 0,
            notice: Some(notice.to_string()),
        };
    }
    if input == 0 {
        return Resolved {
            row: 0,
            notice: Some("行番号は1から始まります。1行目へ移動しました".to_string()),
        };
    }
    let wanted = usize::try_from(input - 1).unwrap_or(usize::MAX);
    if wanted < row_count {
        return Resolved {
            row: wanted,
            notice: None,
        };
    }
    let last = row_count - 1;
    let notice = if indexing {
        format!(
            "{input} 行目はまだ索引にありません（索引作成中、現在 {row_count} 行まで）。{row_count} 行目へ移動しました"
        )
    } else {
        format!("{input} 行目は存在しません（全 {row_count} 行）。最終行へ移動しました")
    };
    Resolved {
        row: last,
        notice: Some(notice),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_are_recognised_from_the_key_or_the_typed_character() {
        assert_eq!(classify("5", Some("5"), false), GotoKey::Digit('5'));
        assert_eq!(classify("5", None, false), GotoKey::Digit('5'));
        assert_eq!(classify("0", Some("0"), false), GotoKey::Digit('0'));
    }

    #[test]
    fn full_width_digits_from_an_ime_are_accepted() {
        assert_eq!(classify("1", Some("１"), false), GotoKey::Digit('1'));
        assert_eq!(classify("x", Some("９"), false), GotoKey::Digit('9'));
    }

    #[test]
    fn control_keys_are_classified() {
        assert_eq!(classify("backspace", None, false), GotoKey::Backspace);
        assert_eq!(classify("enter", Some("\n"), false), GotoKey::Enter);
        assert_eq!(classify("escape", None, false), GotoKey::Escape);
    }

    #[test]
    fn other_keys_and_command_shortcuts_are_ignored() {
        assert_eq!(classify("a", Some("a"), false), GotoKey::Ignore);
        assert_eq!(classify("あ", Some("あ"), false), GotoKey::Ignore);
        assert_eq!(classify("tab", None, false), GotoKey::Ignore);
        assert_eq!(classify("up", None, false), GotoKey::Ignore);
        // cmd-1 のような、修飾キーつきのショートカットは数字として扱わない。
        assert_eq!(classify("1", Some("1"), true), GotoKey::Ignore);
        assert_eq!(classify("enter", None, true), GotoKey::Ignore);
    }

    #[test]
    fn the_input_collects_digits_and_supports_backspace() {
        let mut input = GotoInput::default();
        assert_eq!(input.value(), None);
        for c in "1234".chars() {
            input.push_digit(c);
        }
        assert_eq!(input.text(), "1234");
        assert_eq!(input.value(), Some(1234));
        input.backspace();
        assert_eq!(input.value(), Some(123));
        for _ in 0..10 {
            input.backspace();
        }
        assert_eq!(input.value(), None, "backspace on an empty input is harmless");
    }

    #[test]
    fn the_input_stops_accepting_digits_at_the_limit() {
        let mut input = GotoInput::default();
        for _ in 0..40 {
            input.push_digit('9');
        }
        assert_eq!(input.text().len(), MAX_DIGITS);
        assert_eq!(input.value(), Some(999_999_999_999_999_999));
    }

    #[test]
    fn leading_zeros_are_allowed() {
        let mut input = GotoInput::default();
        for c in "007".chars() {
            input.push_digit(c);
        }
        assert_eq!(input.value(), Some(7));
    }

    #[test]
    fn a_row_inside_the_range_moves_there_without_a_notice() {
        assert_eq!(resolve(1, 100, false), Resolved { row: 0, notice: None });
        assert_eq!(resolve(50, 100, false), Resolved { row: 49, notice: None });
        assert_eq!(resolve(100, 100, false), Resolved { row: 99, notice: None });
    }

    #[test]
    fn a_row_past_the_end_of_a_finished_table_goes_to_the_last_row() {
        let r = resolve(10_000, 100, false);
        assert_eq!(r.row, 99);
        let notice = r.notice.unwrap();
        assert!(notice.contains("10000") && notice.contains("100") && notice.contains("最終行"));
    }

    #[test]
    fn a_row_not_yet_indexed_goes_as_far_as_possible_and_says_so() {
        let r = resolve(5_000_000, 1_200, true);
        assert_eq!(r.row, 1_199);
        let notice = r.notice.unwrap();
        assert!(notice.contains("索引作成中") && notice.contains("1200"), "{notice}");
        // 索引ができている範囲なら、索引作成中でも、そのまま移動できる。
        assert_eq!(resolve(1_000, 1_200, true), Resolved { row: 999, notice: None });
    }

    #[test]
    fn row_zero_goes_to_the_first_row_with_a_notice() {
        let r = resolve(0, 100, false);
        assert_eq!(r.row, 0);
        assert!(r.notice.unwrap().contains("1から"));
    }

    #[test]
    fn an_empty_table_has_nowhere_to_go() {
        assert_eq!(resolve(5, 0, false).row, 0);
        assert!(resolve(5, 0, false).notice.unwrap().contains("ありません"));
        assert!(resolve(5, 0, true).notice.unwrap().contains("読み込み中"));
    }

    #[test]
    fn a_huge_row_number_does_not_overflow() {
        let r = resolve(u64::MAX, 100, false);
        assert_eq!(r.row, 99);
        assert!(r.notice.is_some());
    }
}
