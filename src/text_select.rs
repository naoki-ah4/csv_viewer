//! ポップアップの文章の、文字の選択。画面には依存しない。
//! 位置は、文字列の UTF-8 のバイト位置（文字の境界）。

use std::ops::Range;

/// 文章の中の選択範囲。アンカー（押した位置）と、現在位置（ドラッグで動かす位置）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSelection {
    anchor: usize,
    head: usize,
}

impl TextSelection {
    /// 位置 `at` で押した。まだ、何も選ばれていない。
    pub fn at(at: usize) -> Self {
        Self {
            anchor: at,
            head: at,
        }
    }

    /// 全体を選ぶ。
    pub fn all(len: usize) -> Self {
        Self {
            anchor: 0,
            head: len,
        }
    }

    /// 現在位置を動かして、範囲を広げる。
    pub fn extended_to(self, head: usize) -> Self {
        Self { head, ..self }
    }

    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }
}

/// `range` の部分の文字列。範囲が文字列の外にはみ出していたり、文字の途中を指していたりしても、
/// 安全に切り出す。文字の途中を指しているときは、その文字を、範囲に含める（範囲を、外側の
/// 文字の境界まで広げる）。実際の選択では、位置は文字の境界なので、これは、念のための備え。
pub fn slice<'a>(text: &'a str, range: &Range<usize>) -> &'a str {
    let start = floor_boundary(text, range.start);
    let end = ceil_boundary(text, range.end);
    if start >= end {
        return "";
    }
    &text[start..end]
}

/// `index` 以下で、いちばん近い文字の境界。文字列の長さを超えていれば、末尾。
fn floor_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// `index` 以上で、いちばん近い文字の境界。文字列の長さを超えていれば、末尾。
fn ceil_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressing_selects_nothing_until_dragged() {
        let sel = TextSelection::at(5);
        assert!(sel.is_empty());
        assert_eq!(sel.range(), 5..5);
    }

    #[test]
    fn dragging_forward_and_backward_gives_the_same_range() {
        let forward = TextSelection::at(3).extended_to(9);
        let backward = TextSelection::at(9).extended_to(3);
        assert_eq!(forward.range(), 3..9);
        assert_eq!(backward.range(), 3..9);
        assert!(!forward.is_empty());
        // 押した位置に戻すと、空になる。
        assert!(forward.extended_to(3).is_empty());
    }

    #[test]
    fn select_all_covers_the_whole_text() {
        assert_eq!(TextSelection::all(10).range(), 0..10);
        assert!(TextSelection::all(0).is_empty());
    }

    #[test]
    fn slicing_returns_the_selected_part() {
        assert_eq!(slice("hello world", &(6..11)), "world");
        assert_eq!(slice("hello", &(0..5)), "hello");
        assert_eq!(slice("hello", &(2..2)), "");
    }

    #[test]
    fn slicing_works_on_japanese_and_multi_line_text() {
        let text = "一行目\n二行目\n三行目";
        // 「二行目」は、バイト位置 10..19。
        assert_eq!(slice(text, &(10..19)), "二行目");
        assert_eq!(slice(text, &(0..text.len())), text);
    }

    #[test]
    fn slicing_is_safe_for_out_of_range_and_mid_character_positions() {
        assert_eq!(slice("abc", &(1..999)), "bc");
        assert_eq!(slice("abc", &(999..1000)), "");
        // 「あ」は3バイト。文字の途中を指しても、パニックせず、その文字を、範囲に含める。
        assert_eq!(slice("あい", &(1..4)), "あい");
        assert_eq!(slice("あい", &(2..5)), "あい");
        assert_eq!(slice("あい", &(4..6)), "い");
        assert_eq!(slice("あい", &(0..1)), "あ", "a range inside one character");
        // 絵文字（4バイト）。
        assert_eq!(slice("a😀b", &(0..3)), "a😀");
        assert_eq!(slice("a😀b", &(2..3)), "😀");
    }

    #[test]
    fn an_inverted_range_is_empty() {
        #[allow(clippy::reversed_empty_ranges)]
        let inverted = 5..2;
        assert_eq!(slice("hello world", &inverted), "");
    }
}
