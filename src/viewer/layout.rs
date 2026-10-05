//! 描画の位置や列幅など、画面に依存しない計算。

use super::*;

/// 画面に描く行の範囲と、先頭行が画面の上端より上にはみ出している量（px）。
#[derive(Debug, PartialEq)]
pub(super) struct VisibleRows {
    pub(super) first: usize,
    pub(super) last: usize,
    pub(super) offset: f32,
}

/// スクロール位置から、描く行を求める。範囲外の位置は範囲内に丸める。
/// `offset` は、先頭行の中でのずれそのもの（0以上、行の高さ未満）。
pub(super) fn visible_rows(
    scroll: ScrollPos,
    viewport_height: f32,
    row_count: usize,
) -> VisibleRows {
    let scroll = scroll.clamped(ScrollPos::max(row_count, viewport_height));
    let first = scroll.row.min(row_count);
    let offset = if scroll.row >= row_count {
        0.0
    } else {
        scroll.offset
    };
    // 先頭の行がはみ出しているぶん、領域の高さを覆うには1行多く要る。
    let count = (viewport_height / ROW_HEIGHT).ceil() as usize + 1;
    VisibleRows {
        first,
        last: first.saturating_add(count).min(row_count),
        offset,
    }
}

/// 行番号の欄の幅。行数の桁数に合わせる（桁が増えるときだけ広がる）。
pub(super) fn gutter_width(row_count: usize) -> f32 {
    let digits = row_count.max(1).to_string().len();
    (digits as f32 * CHAR_WIDTH + CELL_PADDING).max(MIN_GUTTER_WIDTH)
}

pub(super) fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy())
        .into_owned()
}

pub(super) fn axis_value(axis: Axis, p: Point<Pixels>) -> Pixels {
    match axis {
        Axis::Vertical => p.y,
        Axis::Horizontal => p.x,
    }
}

/// 全角は半角の2倍幅として概算する。先頭の一部の行だけを見て列幅を決める。
pub(super) fn column_widths(table: &Table) -> (Vec<f32>, usize) {
    let sampled_rows = table.row_count().min(WIDTH_SAMPLE_ROWS);
    let mut widths = vec![0usize; table.column_count()];
    for (col, w) in widths.iter_mut().enumerate() {
        let sampled = (0..sampled_rows).map(|row| table.cell(row, col));
        *w = std::iter::once(table.header(col))
            .chain(sampled)
            .map(|text| display_width(&text))
            .max()
            .unwrap_or(0);
    }
    let widths = widths
        .into_iter()
        .map(|w| (w as f32 * CHAR_WIDTH + CELL_PADDING).clamp(MIN_COL_WIDTH, MAX_COL_WIDTH))
        .collect();
    (widths, sampled_rows)
}

pub(super) fn display_width(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

pub(super) fn cell(text: String, width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .flex_shrink_0()
        .px_2()
        .overflow_hidden()
        .whitespace_nowrap()
        .child(single_line(text))
}

/// セル内の改行は表示上だけ記号に置き換える。元データは変えない。
pub(super) fn single_line(text: String) -> String {
    if text.contains(['\r', '\n']) {
        text.replace("\r\n", "⏎").replace(['\r', '\n'], "⏎")
    } else {
        text
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    // ---- 縦スクロールの描画範囲 ----

    fn pos(row: usize, offset: f32) -> ScrollPos {
        ScrollPos { row, offset }
    }

    #[test]
    fn visible_rows_at_the_top() {
        assert_eq!(
            visible_rows(ScrollPos::TOP, 100.0, 1000),
            VisibleRows {
                first: 0,
                last: 6,
                offset: 0.0
            }
        );
    }

    #[test]
    fn visible_rows_with_an_offset_into_the_first_row() {
        assert_eq!(
            visible_rows(pos(2, 2.0), 100.0, 1000),
            VisibleRows {
                first: 2,
                last: 8,
                offset: 2.0
            }
        );
    }

    #[test]
    fn visible_rows_stop_at_the_last_row() {
        let max = ScrollPos::max(10, 100.0);
        let v = visible_rows(max, 100.0, 10);
        assert_eq!(v.last, 10);
        assert!(v.first < 10);
        assert_eq!(
            visible_rows(ScrollPos::TOP, 100.0, 0),
            VisibleRows {
                first: 0,
                last: 0,
                offset: 0.0
            }
        );
    }

    #[test]
    fn visible_rows_clamp_an_out_of_range_position() {
        let v = visible_rows(pos(5_000, 10.0), 100.0, 100);
        assert_eq!(v, visible_rows(ScrollPos::max(100, 100.0), 100.0, 100));
    }

    /// 全体の座標を `f32` で持つと、約3000万行で隣り合う行の位置が衝突する（今回の不具合）。
    #[test]
    fn global_f32_positions_collide_for_tens_of_millions_of_rows() {
        let rows = 30_466_333usize;
        let top_of = |ix: usize| (ix as f32) * ROW_HEIGHT;
        let collisions = (rows - 100..rows)
            .filter(|&ix| top_of(ix) == top_of(ix + 1))
            .count();
        assert!(collisions > 0, "f32 positions should collide near the end");
    }

    /// 先頭行からの相対位置で描けば、同じ行数でも衝突せず、行の高さちょうどに並ぶ。
    #[test]
    fn relative_positions_stay_exact_for_tens_of_millions_of_rows() {
        for rows in [1_000_000usize, 30_466_333, 500_000_000, 5_000_000_000_000] {
            for viewport in [300.0f32, 700.0, 2160.0] {
                let max = ScrollPos::max(rows, viewport);
                for scroll in [
                    ScrollPos::TOP,
                    pos(rows / 3, 7.5),
                    ScrollPos::from_fraction(0.5, max),
                    pos(max.row.saturating_sub(1), 20.0),
                    max,
                ] {
                    let v = visible_rows(scroll, viewport, rows);
                    assert!(v.last <= rows);
                    assert!((0.0..ROW_HEIGHT).contains(&v.offset), "offset {}", v.offset);
                    // 画面上の各行の上端は、行の高さちょうどの間隔で並ぶ。
                    let tops: Vec<f32> = (v.first..v.last)
                        .map(|ix| (ix - v.first) as f32 * ROW_HEIGHT - v.offset)
                        .collect();
                    for pair in tops.windows(2) {
                        assert_eq!(pair[1] - pair[0], ROW_HEIGHT);
                    }
                    // 末尾以外では、画面を覆う。
                    if v.last < rows {
                        let bottom = tops.last().unwrap() + ROW_HEIGHT;
                        assert!(bottom >= viewport, "rows must cover the viewport");
                    }
                }
                // 末尾では、最後の行の下端が、領域の下端にちょうど揃う。
                let v = visible_rows(max, viewport, rows);
                assert_eq!(v.last, rows);
                let bottom = (v.last - v.first) as f32 * ROW_HEIGHT - v.offset;
                assert!((bottom - viewport).abs() < 1e-2, "{bottom} vs {viewport}");
            }
        }
    }

    #[test]
    fn the_row_number_gutter_grows_with_the_number_of_digits() {
        assert_eq!(gutter_width(0), MIN_GUTTER_WIDTH);
        assert_eq!(gutter_width(9), gutter_width(1));
        assert_eq!(gutter_width(99), gutter_width(10));
        assert!(gutter_width(1_000_000) > gutter_width(999));
        assert_eq!(
            gutter_width(30_466_333),
            8.0 * CHAR_WIDTH + CELL_PADDING,
            "eight digits"
        );
    }
}
