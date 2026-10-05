//! セルの選択範囲と、コピー用のタブ区切りへの変換。画面には依存しない。
//!
//! 選択は、アンカー（最初に押した位置）と、現在位置（ドラッグや `Shift+クリック` で動かす位置）を
//! 対角とする、長方形の範囲。行番号の欄をクリックすると、行全体（すべての列）の選択になる。

use std::borrow::Cow;
use std::ops::RangeInclusive;

/// コピーできる最大のセル数。巨大な選択で、メモリや時間を使い切らないための上限。
pub const COPY_CAP_CELLS: usize = 100_000;

/// セルの位置。行は、ヘッダーを除くデータ行の0始まり。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellPos {
    pub row: usize,
    pub col: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    anchor: CellPos,
    head: CellPos,
    /// 行全体の選択。列の範囲は、表の列数で決まる。
    whole_rows: bool,
}

impl Selection {
    pub fn cell(pos: CellPos) -> Self {
        Self {
            anchor: pos,
            head: pos,
            whole_rows: false,
        }
    }

    pub fn whole_row(row: usize) -> Self {
        Self {
            anchor: CellPos { row, col: 0 },
            head: CellPos { row, col: 0 },
            whole_rows: true,
        }
    }

    /// 全行の選択。行がなければ `None`。
    pub fn all_rows(row_count: usize) -> Option<Self> {
        (row_count > 0).then(|| Self {
            anchor: CellPos { row: 0, col: 0 },
            head: CellPos {
                row: row_count - 1,
                col: 0,
            },
            whole_rows: true,
        })
    }

    /// アンカーはそのままに、現在位置を `pos` に動かす。
    pub fn extended_to(self, pos: CellPos) -> Self {
        Self { head: pos, ..self }
    }

    /// 現在位置のセル。ポップアップを開く対象になる。
    pub fn head(&self) -> CellPos {
        self.head
    }

    pub fn rows(&self) -> RangeInclusive<usize> {
        self.anchor.row.min(self.head.row)..=self.anchor.row.max(self.head.row)
    }

    pub fn cols(&self, column_count: usize) -> RangeInclusive<usize> {
        if self.whole_rows {
            0..=column_count.saturating_sub(1)
        } else {
            self.anchor.col.min(self.head.col)..=self.anchor.col.max(self.head.col)
        }
    }

    pub fn contains(&self, row: usize, col: usize, column_count: usize) -> bool {
        self.rows().contains(&row) && self.cols(column_count).contains(&col)
    }

    /// 選ばれている行数と列数。
    pub fn dimensions(&self, column_count: usize) -> (usize, usize) {
        (range_len(&self.rows()), range_len(&self.cols(column_count)))
    }

    pub fn is_single_cell(&self, column_count: usize) -> bool {
        self.dimensions(column_count) == (1, 1)
    }

    /// 選ばれているセルの数。桁あふれしない。
    pub fn cell_count(&self, column_count: usize) -> usize {
        let (rows, cols) = self.dimensions(column_count);
        rows.saturating_mul(cols)
    }
}

fn range_len(range: &RangeInclusive<usize>) -> usize {
    (range.end() - range.start()).saturating_add(1)
}

/// タブ区切りの1セル。タブ・改行・引用符を含むセルは、引用符で囲み、中の引用符は2つ重ねる
/// （Excel や Google スプレッドシートに貼ったときに、1つのセルとして読まれる形）。
pub fn tsv_cell(text: &str) -> Cow<'_, str> {
    if text.contains(['\t', '\n', '\r', '"']) {
        Cow::Owned(format!("\"{}\"", text.replace('"', "\"\"")))
    } else {
        Cow::Borrowed(text)
    }
}

/// 範囲のセルを、タブ区切り（行は改行）にする。最後に改行は付けない（1つのセルを貼るとき、
/// 余計な改行が入らないように）。
pub fn to_tsv<'a>(
    rows: RangeInclusive<usize>,
    cols: RangeInclusive<usize>,
    cell: impl Fn(usize, usize) -> Cow<'a, str>,
) -> String {
    let mut out = String::new();
    for (i, row) in rows.enumerate() {
        if i > 0 {
            out.push('\n');
        }
        for (j, col) in cols.clone().enumerate() {
            if j > 0 {
                out.push('\t');
            }
            out.push_str(&tsv_cell(&cell(row, col)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(row: usize, col: usize) -> CellPos {
        CellPos { row, col }
    }

    #[test]
    fn a_single_cell_selection_is_one_by_one() {
        let sel = Selection::cell(pos(3, 2));
        assert_eq!(sel.rows(), 3..=3);
        assert_eq!(sel.cols(10), 2..=2);
        assert_eq!(sel.dimensions(10), (1, 1));
        assert!(sel.is_single_cell(10));
        assert_eq!(sel.head(), pos(3, 2));
    }

    #[test]
    fn dragging_makes_a_rectangle_in_any_direction() {
        let down_right = Selection::cell(pos(2, 1)).extended_to(pos(5, 4));
        assert_eq!(down_right.rows(), 2..=5);
        assert_eq!(down_right.cols(10), 1..=4);
        assert_eq!(down_right.dimensions(10), (4, 4));
        // 左上へ向かってドラッグしても、同じ長方形になる。
        let up_left = Selection::cell(pos(5, 4)).extended_to(pos(2, 1));
        assert_eq!(up_left.rows(), down_right.rows());
        assert_eq!(up_left.cols(10), down_right.cols(10));
        // アンカーは動かず、現在位置だけが動く。
        assert_eq!(up_left.head(), pos(2, 1));
        assert_eq!(up_left.extended_to(pos(9, 9)).rows(), 5..=9);
    }

    #[test]
    fn contains_follows_the_rectangle() {
        let sel = Selection::cell(pos(2, 1)).extended_to(pos(4, 3));
        assert!(sel.contains(2, 1, 10));
        assert!(sel.contains(4, 3, 10));
        assert!(sel.contains(3, 2, 10));
        assert!(!sel.contains(1, 2, 10));
        assert!(!sel.contains(3, 4, 10));
        assert!(!sel.contains(5, 1, 10));
    }

    #[test]
    fn a_whole_row_covers_every_column_and_can_be_extended_by_rows() {
        let sel = Selection::whole_row(7);
        assert_eq!(sel.rows(), 7..=7);
        assert_eq!(sel.cols(5), 0..=4);
        assert_eq!(sel.dimensions(5), (1, 5));
        assert!(sel.contains(7, 4, 5));
        let more = sel.extended_to(pos(9, 0));
        assert_eq!(more.rows(), 7..=9);
        assert_eq!(more.cols(5), 0..=4, "still every column");
        // 列数が変わっても（索引作成中に列が増えても）、行全体のまま。
        assert_eq!(more.cols(8), 0..=7);
    }

    #[test]
    fn a_whole_row_of_a_table_without_columns_does_not_underflow() {
        assert_eq!(Selection::whole_row(0).cols(0), 0..=0);
    }

    #[test]
    fn select_all_covers_every_row() {
        let all = Selection::all_rows(1000).unwrap();
        assert_eq!(all.rows(), 0..=999);
        assert_eq!(all.cols(3), 0..=2);
        assert_eq!(all.cell_count(3), 3000);
        assert!(Selection::all_rows(0).is_none());
    }

    #[test]
    fn the_cell_count_does_not_overflow() {
        let huge = Selection::all_rows(usize::MAX).unwrap();
        assert_eq!(huge.cell_count(usize::MAX), usize::MAX);
        assert_eq!(huge.dimensions(2).0, usize::MAX);
    }

    #[test]
    fn plain_cells_are_not_quoted() {
        assert_eq!(tsv_cell("abc"), "abc");
        assert_eq!(tsv_cell("山田 太郎"), "山田 太郎");
        assert_eq!(tsv_cell(""), "");
        assert!(matches!(tsv_cell("abc"), Cow::Borrowed(_)));
    }

    #[test]
    fn cells_with_tabs_newlines_or_quotes_are_quoted() {
        assert_eq!(tsv_cell("a\tb"), "\"a\tb\"");
        assert_eq!(tsv_cell("a\nb"), "\"a\nb\"");
        assert_eq!(tsv_cell("a\r\nb"), "\"a\r\nb\"");
        assert_eq!(tsv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(tsv_cell("\""), "\"\"\"\"");
    }

    #[test]
    fn a_range_becomes_tab_separated_rows_without_a_trailing_newline() {
        let grid = [["a", "b", "c"], ["d", "", "f"], ["g", "h", "i"]];
        let cell = |r: usize, c: usize| Cow::Borrowed(grid[r][c]);
        assert_eq!(to_tsv(0..=2, 0..=2, cell), "a\tb\tc\nd\t\tf\ng\th\ti");
        assert_eq!(to_tsv(1..=2, 1..=2, cell), "\tf\nh\ti");
        // 1つのセルは、タブも改行も付かない。
        assert_eq!(to_tsv(0..=0, 1..=1, cell), "b");
    }

    #[test]
    fn quoting_applies_inside_a_range() {
        let grid = [["x", "line1\nline2"], ["say \"hi\"", "ok"]];
        let cell = |r: usize, c: usize| Cow::Borrowed(grid[r][c]);
        assert_eq!(
            to_tsv(0..=1, 0..=1, cell),
            "x\t\"line1\nline2\"\n\"say \"\"hi\"\"\"\tok"
        );
    }
}
