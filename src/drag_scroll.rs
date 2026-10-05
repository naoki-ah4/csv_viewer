//! ドラッグで選択を広げているとき、表の端の外へ出たら、自動でスクロールする。その計算。
//! 画面には依存しない。

/// 1回の更新（約16ms）あたりの、最小と最大の速さ（px）。
const MIN_SPEED: f32 = 4.0;
const MAX_SPEED: f32 = 48.0;
/// 端から離れた距離（px）に対する、速さの割合。
const GAIN: f32 = 0.25;

/// ポインタが `start..end` の外にいるとき、1回の更新で、動かす量（px）。
/// 内側なら 0。手前（上・左）の外は負、奥（下・右）の外は正。離れるほど速く、上限がある。
pub fn edge_scroll(pointer: f32, start: f32, end: f32) -> f32 {
    if pointer < start {
        -((start - pointer) * GAIN).clamp(MIN_SPEED, MAX_SPEED)
    } else if pointer > end {
        ((pointer - end) * GAIN).clamp(MIN_SPEED, MAX_SPEED)
    } else {
        0.0
    }
}

/// 値を `start..=end`（`end` の1つ手前まで）に収める。領域の外にいるポインタを、端のセルに当てる。
pub fn clamp_into(value: f32, start: f32, end: f32) -> f32 {
    value.clamp(start, (end - 1.0).max(start))
}

/// 縦の位置 `y` にある行。`rows_top` は、行を描く領域の上端、`first_row` は、先頭に見えている行、
/// `first_offset` は、その行が上端より上にはみ出している量。範囲外は、端の行に収める。
/// 行がなければ `None`。
pub fn row_at(
    y: f32,
    rows_top: f32,
    first_row: usize,
    first_offset: f32,
    row_height: f32,
    row_count: usize,
) -> Option<usize> {
    if row_count == 0 {
        return None;
    }
    let steps = ((y - rows_top + first_offset) / row_height).floor();
    let row = first_row as f64 + f64::from(steps);
    Some(row.clamp(0.0, (row_count - 1) as f64) as usize)
}

/// 横の位置 `x` にある列。`content_left` は、本体の左端（横スクロールのぶん、ずれた位置）。
/// 範囲外は、端の列に収める。列がなければ `None`。
pub fn col_at(x: f32, content_left: f32, widths: &[f32]) -> Option<usize> {
    if widths.is_empty() {
        return None;
    }
    let relative = x - content_left;
    let mut right = 0.0;
    for (col, width) in widths.iter().enumerate() {
        right += width;
        if relative < right {
            return Some(col);
        }
    }
    Some(widths.len() - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inside_the_area_there_is_no_scrolling() {
        assert_eq!(edge_scroll(50.0, 0.0, 100.0), 0.0);
        assert_eq!(edge_scroll(0.0, 0.0, 100.0), 0.0, "exactly on the edge");
        assert_eq!(edge_scroll(100.0, 0.0, 100.0), 0.0);
    }

    #[test]
    fn outside_the_area_it_scrolls_toward_the_pointer() {
        assert!(
            edge_scroll(-10.0, 0.0, 100.0) < 0.0,
            "above / left scrolls back"
        );
        assert!(
            edge_scroll(130.0, 0.0, 100.0) > 0.0,
            "below / right scrolls forward"
        );
    }

    #[test]
    fn it_speeds_up_with_distance_within_limits() {
        let near = edge_scroll(105.0, 0.0, 100.0);
        let far = edge_scroll(300.0, 0.0, 100.0);
        assert_eq!(
            near, MIN_SPEED,
            "just outside still moves at a minimum speed"
        );
        assert!(far > near);
        assert_eq!(edge_scroll(100_000.0, 0.0, 100.0), MAX_SPEED);
        assert_eq!(edge_scroll(-100_000.0, 0.0, 100.0), -MAX_SPEED);
    }

    #[test]
    fn clamping_puts_the_pointer_on_an_edge_cell() {
        assert_eq!(clamp_into(-50.0, 10.0, 110.0), 10.0);
        assert_eq!(
            clamp_into(500.0, 10.0, 110.0),
            109.0,
            "the last pixel inside"
        );
        assert_eq!(clamp_into(60.0, 10.0, 110.0), 60.0);
        assert_eq!(clamp_into(5.0, 10.0, 10.0), 10.0, "an empty area");
    }

    #[test]
    fn a_row_is_found_from_the_vertical_position() {
        // 先頭の行（120行目）が、5px はみ出している。行の高さは24px。
        let at = |y: f32| row_at(y, 100.0, 120, 5.0, 24.0, 1000);
        assert_eq!(at(100.0), Some(120));
        assert_eq!(
            at(118.0),
            Some(120),
            "the visible part of the first row ends at 119"
        );
        assert_eq!(at(119.0), Some(121));
        assert_eq!(at(100.0 + 24.0 * 3.0), Some(123));
    }

    #[test]
    fn a_row_is_clamped_into_the_table() {
        assert_eq!(row_at(-1_000_000.0, 100.0, 5, 0.0, 24.0, 1000), Some(0));
        assert_eq!(row_at(1_000_000.0, 100.0, 5, 0.0, 24.0, 1000), Some(999));
        assert_eq!(row_at(100.0, 100.0, 0, 0.0, 24.0, 0), None);
        // 巨大な行番号でも、あふれない。
        assert_eq!(
            row_at(100.0, 100.0, 30_466_330, 0.0, 24.0, 30_466_333),
            Some(30_466_330)
        );
    }

    #[test]
    fn a_column_is_found_from_the_horizontal_position() {
        let widths = [100.0, 50.0, 200.0];
        assert_eq!(col_at(0.0, 0.0, &widths), Some(0));
        assert_eq!(col_at(99.0, 0.0, &widths), Some(0));
        assert_eq!(col_at(100.0, 0.0, &widths), Some(1));
        assert_eq!(col_at(149.0, 0.0, &widths), Some(1));
        assert_eq!(col_at(150.0, 0.0, &widths), Some(2));
    }

    #[test]
    fn the_horizontal_scroll_shifts_the_columns() {
        // 本体の左端が、画面の左端より 130px 左にある（右へ 130px スクロールしている）。
        let widths = [100.0, 50.0, 200.0];
        assert_eq!(col_at(0.0, -130.0, &widths), Some(1));
        assert_eq!(col_at(30.0, -130.0, &widths), Some(2));
    }

    #[test]
    fn a_column_is_clamped_into_the_table() {
        let widths = [100.0, 50.0];
        assert_eq!(col_at(-500.0, 0.0, &widths), Some(0));
        assert_eq!(col_at(9_999.0, 0.0, &widths), Some(1));
        assert_eq!(col_at(10.0, 0.0, &[]), None);
    }
}
