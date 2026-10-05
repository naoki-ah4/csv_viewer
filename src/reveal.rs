//! 検索で見つけたセルを、画面の中に見えるようにするための、スクロール位置の計算。

use std::ops::Range;

use crate::scroll_pos::ScrollPos;

/// 行 `row` が見えるように、縦のスクロール位置を決める。
///
/// `fully_visible` は、いま全体が見えている行の範囲。そこに入っていれば、動かさない。
/// 見えていなければ、その行の `context_rows` 行上から表示する（前の行も見えるように）。
/// 上限の丸めは、呼び出し側で行う。
pub fn reveal_row(
    scroll: ScrollPos,
    fully_visible: Range<usize>,
    row: usize,
    context_rows: usize,
) -> ScrollPos {
    if fully_visible.contains(&row) {
        scroll
    } else {
        ScrollPos {
            row: row.saturating_sub(context_rows),
            offset: 0.0,
        }
    }
}

/// 横方向で、`left..right`（本体の左端からの距離）の列が見えるように、スクロール量を決める。
/// スクロール量は、右へ送るほど増える。領域より広い列は、左端を合わせる。
pub fn reveal_column(
    scroll_x: f32,
    viewport_width: f32,
    left: f32,
    right: f32,
    max_scroll_x: f32,
) -> f32 {
    let target = if left < scroll_x {
        left
    } else if right > scroll_x + viewport_width {
        if right - left >= viewport_width {
            left
        } else {
            right - viewport_width
        }
    } else {
        scroll_x
    };
    target.clamp(0.0, max_scroll_x.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(row: usize, offset: f32) -> ScrollPos {
        ScrollPos { row, offset }
    }

    #[test]
    fn a_row_that_is_already_visible_does_not_move_the_view() {
        let scroll = pos(100, 7.0);
        assert_eq!(reveal_row(scroll, 101..120, 101, 3), scroll);
        assert_eq!(reveal_row(scroll, 101..120, 119, 3), scroll);
    }

    #[test]
    fn a_row_outside_the_view_is_brought_in_with_some_rows_above_it() {
        let scroll = pos(100, 7.0);
        assert_eq!(reveal_row(scroll, 101..120, 5_000, 3), pos(4_997, 0.0));
        assert_eq!(
            reveal_row(scroll, 101..120, 120, 3),
            pos(117, 0.0),
            "just past the end"
        );
        assert_eq!(
            reveal_row(scroll, 101..120, 100, 3),
            pos(97, 0.0),
            "the half-hidden first row"
        );
    }

    #[test]
    fn rows_near_the_top_do_not_underflow() {
        assert_eq!(reveal_row(pos(500, 0.0), 500..520, 1, 3), pos(0, 0.0));
        assert_eq!(reveal_row(pos(500, 0.0), 500..520, 0, 3), pos(0, 0.0));
    }

    #[test]
    fn a_visible_column_does_not_move_the_view() {
        assert_eq!(reveal_column(100.0, 400.0, 150.0, 250.0, 1_000.0), 100.0);
        assert_eq!(
            reveal_column(100.0, 400.0, 100.0, 500.0, 1_000.0),
            100.0,
            "exactly fits"
        );
    }

    #[test]
    fn a_column_to_the_left_is_aligned_to_the_left_edge() {
        assert_eq!(reveal_column(300.0, 400.0, 120.0, 220.0, 1_000.0), 120.0);
    }

    #[test]
    fn a_column_to_the_right_is_aligned_to_the_right_edge() {
        assert_eq!(reveal_column(0.0, 400.0, 600.0, 700.0, 1_000.0), 300.0);
    }

    #[test]
    fn a_column_wider_than_the_view_is_aligned_to_its_left_edge() {
        assert_eq!(reveal_column(0.0, 300.0, 500.0, 900.0, 2_000.0), 500.0);
    }

    #[test]
    fn the_result_stays_within_the_scrollable_range() {
        assert_eq!(reveal_column(0.0, 400.0, 900.0, 1_000.0, 450.0), 450.0);
        assert_eq!(reveal_column(50.0, 400.0, 0.0, 100.0, 450.0), 0.0);
        assert_eq!(
            reveal_column(0.0, 400.0, 900.0, 1_000.0, 0.0),
            0.0,
            "nothing to scroll"
        );
    }
}
