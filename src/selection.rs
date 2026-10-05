//! セルの選択範囲と、コピー用のタブ区切りへの変換。画面には依存しない。
//!
//! 選択は、アンカー（最初に押した位置）と、現在位置（ドラッグや `Shift+クリック`、`Shift+矢印` で
//! 動かす位置）を対角とする、長方形の範囲。種類は3つ。
//! - セルの範囲
//! - 行全体（行番号の欄をクリック）。列の範囲は、表の列数で決まる
//! - 列全体（列番号をクリック）。行の範囲は、表の行数で決まる
//!
//! 行全体・列全体は、表の大きさが変わっても（索引作成中に行が増えても）、全体のまま追従する。

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
enum Kind {
    Cells,
    Rows,
    Columns,
}

/// キーボードによる、現在位置の動かし方。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Up,
    Down,
    Left,
    Right,
    /// 指定した行数ぶん、上へ。
    PageUp(usize),
    /// 指定した行数ぶん、下へ。
    PageDown(usize),
    Top,
    Bottom,
    /// その行の、先頭の列へ。
    LineStart,
    /// その行の、末尾の列へ。
    LineEnd,
    /// 左上のセルへ。
    First,
    /// 右下のセルへ。
    Last,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    anchor: CellPos,
    head: CellPos,
    kind: Kind,
}

impl Selection {
    pub fn cell(pos: CellPos) -> Self {
        Self {
            anchor: pos,
            head: pos,
            kind: Kind::Cells,
        }
    }

    pub fn whole_row(row: usize) -> Self {
        let pos = CellPos { row, col: 0 };
        Self {
            anchor: pos,
            head: pos,
            kind: Kind::Rows,
        }
    }

    pub fn whole_column(col: usize) -> Self {
        let pos = CellPos { row: 0, col };
        Self {
            anchor: pos,
            head: pos,
            kind: Kind::Columns,
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
            kind: Kind::Rows,
        })
    }

    /// アンカーはそのままに、現在位置を `pos` に動かす。
    /// 行全体の選択では行だけ、列全体の選択では列だけが、意味を持つ。
    pub fn extended_to(self, pos: CellPos) -> Self {
        Self { head: pos, ..self }
    }

    /// 現在位置のセル。ポップアップを開く対象になる。
    pub fn head(&self) -> CellPos {
        self.head
    }

    pub fn rows(&self, row_count: usize) -> RangeInclusive<usize> {
        match self.kind {
            Kind::Columns => 0..=row_count.saturating_sub(1),
            _ => self.anchor.row.min(self.head.row)..=self.anchor.row.max(self.head.row),
        }
    }

    pub fn cols(&self, column_count: usize) -> RangeInclusive<usize> {
        match self.kind {
            Kind::Rows => 0..=column_count.saturating_sub(1),
            _ => self.anchor.col.min(self.head.col)..=self.anchor.col.max(self.head.col),
        }
    }

    pub fn contains(&self, row: usize, col: usize, row_count: usize, column_count: usize) -> bool {
        self.rows(row_count).contains(&row) && self.cols(column_count).contains(&col)
    }

    /// 選ばれている行数と列数。
    pub fn dimensions(&self, row_count: usize, column_count: usize) -> (usize, usize) {
        (
            range_len(&self.rows(row_count)),
            range_len(&self.cols(column_count)),
        )
    }

    pub fn is_single_cell(&self, row_count: usize, column_count: usize) -> bool {
        self.dimensions(row_count, column_count) == (1, 1)
    }

    /// 選ばれているセルの数。桁あふれしない。
    pub fn cell_count(&self, row_count: usize, column_count: usize) -> usize {
        let (rows, cols) = self.dimensions(row_count, column_count);
        rows.saturating_mul(cols)
    }

    /// キーボードで、現在位置を `step` だけ動かして、範囲を広げる（アンカーは動かさない）。
    /// 表の外へは出ない。行全体の選択では上下だけ、列全体の選択では左右だけが効く。
    pub fn extended_by(self, step: Step, row_count: usize, column_count: usize) -> Self {
        if row_count == 0 || column_count == 0 {
            return self;
        }
        let last_row = row_count - 1;
        let last_col = column_count - 1;
        let mut head = CellPos {
            row: self.head.row.min(last_row),
            col: self.head.col.min(last_col),
        };
        let vertical = self.kind != Kind::Columns;
        let horizontal = self.kind != Kind::Rows;
        match step {
            Step::Up if vertical => head.row = head.row.saturating_sub(1),
            Step::Down if vertical => head.row = (head.row + 1).min(last_row),
            Step::PageUp(n) if vertical => head.row = head.row.saturating_sub(n),
            Step::PageDown(n) if vertical => head.row = head.row.saturating_add(n).min(last_row),
            Step::Top if vertical => head.row = 0,
            Step::Bottom if vertical => head.row = last_row,
            Step::Left if horizontal => head.col = head.col.saturating_sub(1),
            Step::Right if horizontal => head.col = (head.col + 1).min(last_col),
            Step::LineStart if horizontal => head.col = 0,
            Step::LineEnd if horizontal => head.col = last_col,
            Step::First => {
                if vertical {
                    head.row = 0;
                }
                if horizontal {
                    head.col = 0;
                }
            }
            Step::Last => {
                if vertical {
                    head.row = last_row;
                }
                if horizontal {
                    head.col = last_col;
                }
            }
            _ => {}
        }
        Self { head, ..self }
    }

    /// スプレッドシートの矢印キーのように、現在位置を `step` だけ動かして、選択を、その1つの
    /// セルにする（範囲は広げない）。行全体・列全体の選択からは、現在位置のセルから動く。
    pub fn moved_by(self, step: Step, row_count: usize, column_count: usize) -> Self {
        let moved = Self::cell(self.head).extended_by(step, row_count, column_count);
        Self::cell(moved.head)
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
        assert_eq!(sel.rows(100), 3..=3);
        assert_eq!(sel.cols(10), 2..=2);
        assert_eq!(sel.dimensions(100, 10), (1, 1));
        assert!(sel.is_single_cell(100, 10));
        assert_eq!(sel.head(), pos(3, 2));
    }

    #[test]
    fn dragging_makes_a_rectangle_in_any_direction() {
        let down_right = Selection::cell(pos(2, 1)).extended_to(pos(5, 4));
        assert_eq!(down_right.rows(100), 2..=5);
        assert_eq!(down_right.cols(10), 1..=4);
        assert_eq!(down_right.dimensions(100, 10), (4, 4));
        // 左上へ向かってドラッグしても、同じ長方形になる。
        let up_left = Selection::cell(pos(5, 4)).extended_to(pos(2, 1));
        assert_eq!(up_left.rows(100), down_right.rows(100));
        assert_eq!(up_left.cols(10), down_right.cols(10));
        // アンカーは動かず、現在位置だけが動く。
        assert_eq!(up_left.head(), pos(2, 1));
        assert_eq!(up_left.extended_to(pos(9, 9)).rows(100), 5..=9);
    }

    #[test]
    fn contains_follows_the_rectangle() {
        let sel = Selection::cell(pos(2, 1)).extended_to(pos(4, 3));
        assert!(sel.contains(2, 1, 100, 10));
        assert!(sel.contains(4, 3, 100, 10));
        assert!(sel.contains(3, 2, 100, 10));
        assert!(!sel.contains(1, 2, 100, 10));
        assert!(!sel.contains(3, 4, 100, 10));
        assert!(!sel.contains(5, 1, 100, 10));
    }

    #[test]
    fn a_whole_row_covers_every_column_and_can_be_extended_by_rows() {
        let sel = Selection::whole_row(7);
        assert_eq!(sel.rows(100), 7..=7);
        assert_eq!(sel.cols(5), 0..=4);
        assert_eq!(sel.dimensions(100, 5), (1, 5));
        assert!(sel.contains(7, 4, 100, 5));
        let more = sel.extended_to(pos(9, 0));
        assert_eq!(more.rows(100), 7..=9);
        assert_eq!(more.cols(5), 0..=4, "still every column");
        // 列数が変わっても（索引作成中に列が増えても）、行全体のまま。
        assert_eq!(more.cols(8), 0..=7);
    }

    #[test]
    fn a_whole_column_covers_every_row_and_can_be_extended_by_columns() {
        let sel = Selection::whole_column(2);
        assert_eq!(sel.cols(5), 2..=2);
        assert_eq!(sel.rows(1000), 0..=999);
        assert_eq!(sel.dimensions(1000, 5), (1000, 1));
        assert!(sel.contains(999, 2, 1000, 5));
        assert!(!sel.contains(0, 3, 1000, 5));
        let more = sel.extended_to(pos(0, 4));
        assert_eq!(more.cols(5), 2..=4);
        assert_eq!(more.rows(1000), 0..=999, "still every row");
        // 行数が増えても（索引作成中）、列全体のまま。
        assert_eq!(more.rows(5000), 0..=4999);
    }

    #[test]
    fn a_selection_of_an_empty_table_does_not_underflow() {
        assert_eq!(Selection::whole_row(0).cols(0), 0..=0);
        assert_eq!(Selection::whole_column(0).rows(0), 0..=0);
    }

    #[test]
    fn select_all_covers_every_row() {
        let all = Selection::all_rows(1000).unwrap();
        assert_eq!(all.rows(1000), 0..=999);
        assert_eq!(all.cols(3), 0..=2);
        assert_eq!(all.cell_count(1000, 3), 3000);
        assert!(Selection::all_rows(0).is_none());
    }

    #[test]
    fn the_cell_count_does_not_overflow() {
        let huge = Selection::all_rows(usize::MAX).unwrap();
        assert_eq!(huge.cell_count(usize::MAX, usize::MAX), usize::MAX);
        assert_eq!(huge.dimensions(usize::MAX, 2).0, usize::MAX);
        let column = Selection::whole_column(0);
        assert_eq!(column.cell_count(usize::MAX, 5), usize::MAX);
    }

    #[test]
    fn the_keyboard_moves_the_head_and_keeps_the_anchor() {
        let sel = Selection::cell(pos(5, 3));
        let down = sel.extended_by(Step::Down, 100, 10);
        assert_eq!(down.head(), pos(6, 3));
        assert_eq!(down.rows(100), 5..=6);
        let right = down.extended_by(Step::Right, 100, 10);
        assert_eq!(right.dimensions(100, 10), (2, 2));
        // 戻すと、範囲が縮む。アンカーの位置を、越えて動かすと、反対側へ広がる。
        let back = right
            .extended_by(Step::Up, 100, 10)
            .extended_by(Step::Left, 100, 10);
        assert!(back.is_single_cell(100, 10));
        let past = back.extended_by(Step::Up, 100, 10);
        assert_eq!(past.rows(100), 4..=5);
    }

    #[test]
    fn the_keyboard_never_leaves_the_table() {
        let corner = Selection::cell(pos(0, 0));
        assert_eq!(corner.extended_by(Step::Up, 10, 4).head(), pos(0, 0));
        assert_eq!(corner.extended_by(Step::Left, 10, 4).head(), pos(0, 0));
        let far = Selection::cell(pos(9, 3));
        assert_eq!(far.extended_by(Step::Down, 10, 4).head(), pos(9, 3));
        assert_eq!(far.extended_by(Step::Right, 10, 4).head(), pos(9, 3));
        assert_eq!(
            corner.extended_by(Step::PageDown(1000), 10, 4).head(),
            pos(9, 0)
        );
        assert_eq!(far.extended_by(Step::PageUp(1000), 10, 4).head(), pos(0, 3));
    }

    #[test]
    fn paging_and_jumping_move_by_rows() {
        let sel = Selection::cell(pos(50, 2));
        assert_eq!(
            sel.extended_by(Step::PageDown(20), 1000, 5).head(),
            pos(70, 2)
        );
        assert_eq!(
            sel.extended_by(Step::PageUp(20), 1000, 5).head(),
            pos(30, 2)
        );
        assert_eq!(sel.extended_by(Step::Top, 1000, 5).head(), pos(0, 2));
        assert_eq!(sel.extended_by(Step::Bottom, 1000, 5).head(), pos(999, 2));
    }

    #[test]
    fn a_stale_head_is_pulled_back_into_the_table() {
        // 表が小さくなった（別のファイルに替わった）あと、はみ出した位置からでも、安全に動く。
        let sel = Selection::cell(pos(500, 50));
        assert_eq!(sel.extended_by(Step::Up, 10, 4).head(), pos(8, 3));
    }

    #[test]
    fn whole_rows_only_respond_to_up_and_down_and_whole_columns_to_left_and_right() {
        let rows = Selection::whole_row(5);
        assert_eq!(rows.extended_by(Step::Down, 100, 10).rows(100), 5..=6);
        assert_eq!(rows.extended_by(Step::Right, 100, 10), rows, "no effect");
        let cols = Selection::whole_column(5);
        assert_eq!(cols.extended_by(Step::Right, 100, 10).cols(10), 5..=6);
        assert_eq!(cols.extended_by(Step::Down, 100, 10), cols, "no effect");
    }

    #[test]
    fn an_empty_table_ignores_keyboard_steps() {
        let sel = Selection::cell(pos(0, 0));
        assert_eq!(sel.extended_by(Step::Down, 0, 5), sel);
        assert_eq!(sel.extended_by(Step::Right, 5, 0), sel);
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

    #[test]
    fn moving_collapses_the_selection_to_the_new_cell() {
        // 範囲を選んでいても、動かすと、現在位置から1つのセルになる。
        let range = Selection::cell(pos(2, 1)).extended_to(pos(5, 4));
        let moved = range.moved_by(Step::Down, 100, 10);
        assert!(moved.is_single_cell(100, 10));
        assert_eq!(moved.head(), pos(6, 4));
        assert_eq!(moved.moved_by(Step::Left, 100, 10).head(), pos(6, 3));
    }

    #[test]
    fn moving_stays_inside_the_table_like_a_spreadsheet() {
        let corner = Selection::cell(pos(0, 0));
        assert_eq!(corner.moved_by(Step::Up, 10, 4).head(), pos(0, 0));
        assert_eq!(corner.moved_by(Step::Left, 10, 4).head(), pos(0, 0));
        let far = Selection::cell(pos(9, 3));
        assert_eq!(far.moved_by(Step::Down, 10, 4).head(), pos(9, 3));
        assert_eq!(far.moved_by(Step::Right, 10, 4).head(), pos(9, 3));
        assert_eq!(
            corner.moved_by(Step::PageDown(1000), 10, 4).head(),
            pos(9, 0)
        );
    }

    #[test]
    fn moving_from_a_whole_row_or_column_starts_at_the_current_cell() {
        let row = Selection::whole_row(5);
        let down = row.moved_by(Step::Down, 100, 10);
        assert!(down.is_single_cell(100, 10));
        assert_eq!(down.head(), pos(6, 0));
        let right = row.moved_by(Step::Right, 100, 10);
        assert_eq!(
            right.head(),
            pos(5, 1),
            "a whole row can move sideways once collapsed"
        );
        let column = Selection::whole_column(3);
        assert_eq!(column.moved_by(Step::Down, 100, 10).head(), pos(1, 3));
        assert_eq!(column.moved_by(Step::Right, 100, 10).head(), pos(0, 4));
    }

    #[test]
    fn home_end_and_the_corners_work_like_a_spreadsheet() {
        let sel = Selection::cell(pos(50, 2));
        assert_eq!(sel.moved_by(Step::LineStart, 100, 6).head(), pos(50, 0));
        assert_eq!(sel.moved_by(Step::LineEnd, 100, 6).head(), pos(50, 5));
        assert_eq!(sel.moved_by(Step::First, 100, 6).head(), pos(0, 0));
        assert_eq!(sel.moved_by(Step::Last, 100, 6).head(), pos(99, 5));
        assert_eq!(sel.moved_by(Step::Top, 100, 6).head(), pos(0, 2));
        assert_eq!(sel.moved_by(Step::Bottom, 100, 6).head(), pos(99, 2));
    }

    #[test]
    fn extending_to_the_line_ends_and_the_corners_keeps_the_anchor() {
        let sel = Selection::cell(pos(50, 2));
        let to_start = sel.extended_by(Step::LineStart, 100, 6);
        assert_eq!(to_start.cols(6), 0..=2);
        assert_eq!(to_start.rows(100), 50..=50);
        let to_last = sel.extended_by(Step::Last, 100, 6);
        assert_eq!((to_last.rows(100), to_last.cols(6)), (50..=99, 2..=5));
        let to_first = sel.extended_by(Step::First, 100, 6);
        assert_eq!((to_first.rows(100), to_first.cols(6)), (0..=50, 0..=2));
    }

    #[test]
    fn line_steps_do_nothing_for_whole_rows_and_first_last_respect_the_kind() {
        let row = Selection::whole_row(5);
        assert_eq!(row.extended_by(Step::LineEnd, 100, 6), row);
        assert_eq!(row.extended_by(Step::Last, 100, 6).rows(100), 5..=99);
        let column = Selection::whole_column(3);
        assert_eq!(column.extended_by(Step::Bottom, 100, 6), column);
        assert_eq!(column.extended_by(Step::Last, 100, 6).cols(6), 3..=5);
    }
}
