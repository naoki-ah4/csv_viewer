//! 縦のスクロール位置。
//!
//! 全体の座標（先頭から何pxか）は、行数が増えると数億〜数兆pxになり、`f32` はもちろん
//! `f64` でも、位置が大きいほど刻み幅が粗くなる。そこで、位置を次の2つの組で持つ。
//!
//! - `row`: 先頭に見える行の番号（整数なので、どれだけ大きくても1行単位で正確）
//! - `offset`: その行の中でのずれ（0以上、行の高さ未満の `f32`）
//!
//! 数が大きくならないので、先頭付近でも末尾付近でも、スクロールの刻み幅は同じになる。

use crate::ROW_HEIGHT;

/// 位置の大小は、`row`、`offset` の順の辞書式で決まる（`offset` は行の高さ未満に保つ）。
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct ScrollPos {
    pub row: usize,
    pub offset: f32,
}

impl ScrollPos {
    pub const TOP: Self = Self {
        row: 0,
        offset: 0.0,
    };

    /// 最後の行が、領域の下端にちょうど揃う位置。全体が領域に収まるなら `TOP`。
    pub fn max(row_count: usize, viewport_height: f32) -> Self {
        let viewport_height = viewport_height.max(0.0);
        let full_rows = (viewport_height / ROW_HEIGHT).floor() as usize;
        if row_count <= full_rows {
            return Self::TOP;
        }
        // 位置 = 全行の高さ - 領域の高さ = (行数 - 全行ぶん)×行の高さ - 端数
        let remainder = viewport_height - full_rows as f32 * ROW_HEIGHT;
        let rows_above = row_count - full_rows;
        if remainder <= 0.0 {
            Self {
                row: rows_above,
                offset: 0.0,
            }
        } else {
            Self {
                row: rows_above - 1,
                offset: ROW_HEIGHT - remainder,
            }
        }
    }

    /// 範囲の上限 `max` を超えないようにする。
    pub fn clamped(self, max: Self) -> Self {
        if self > max { max } else { self }
    }

    /// `delta_px` だけ下へ動かす（負なら上へ）。先頭より上には行かない。上限は `clamped` で抑える。
    pub fn scrolled_by(self, delta_px: f64) -> Self {
        let height = f64::from(ROW_HEIGHT);
        let total = f64::from(self.offset) + delta_px;
        let carry = (total / height).floor();
        let row = self.row as i128 + carry as i128;
        if row < 0 {
            return Self::TOP;
        }
        let row = usize::try_from(row).unwrap_or(usize::MAX);
        Self::normalized(row, total - carry * height)
    }

    /// `offset` が行の高さに丸められて届いてしまう場合を、繰り上げて整える。
    fn normalized(row: usize, offset: f64) -> Self {
        let offset = offset.max(0.0) as f32;
        if offset >= ROW_HEIGHT {
            Self {
                row: row.saturating_add(1),
                offset: 0.0,
            }
        } else {
            Self { row, offset }
        }
    }

    /// 行単位に直した位置（`row + offset / 行の高さ`）。割合を求めるためだけに使う。
    fn in_rows(self) -> f64 {
        self.row as f64 + f64::from(self.offset) / f64::from(ROW_HEIGHT)
    }

    /// 上限 `max` に対する位置の割合（0〜1）。スクロールバーのつまみの位置に使う。
    pub fn fraction(self, max: Self) -> f64 {
        let total = max.in_rows();
        if total <= 0.0 {
            0.0
        } else {
            (self.in_rows() / total).clamp(0.0, 1.0)
        }
    }

    /// 割合（0〜1）から位置を求める。スクロールバーのつまみのドラッグに使う。
    pub fn from_fraction(fraction: f64, max: Self) -> Self {
        let rows = fraction.clamp(0.0, 1.0) * max.in_rows();
        let row = rows.floor();
        let offset = (rows - row) * f64::from(ROW_HEIGHT);
        Self::normalized(row as usize, offset).clamped(max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: f64 = ROW_HEIGHT as f64;

    fn px_of(pos: ScrollPos) -> f64 {
        pos.row as f64 * H + f64::from(pos.offset)
    }

    #[test]
    fn max_aligns_the_last_row_with_the_bottom_edge() {
        // 10行（240px）を高さ100pxの領域で見る。位置は 240 - 100 = 140px。
        let max = ScrollPos::max(10, 100.0);
        assert_eq!(max, ScrollPos { row: 5, offset: 20.0 });
        assert_eq!(px_of(max), 140.0);
        // 領域の高さが行の高さの整数倍のとき。240 - 96 = 144px。
        assert_eq!(ScrollPos::max(10, 96.0), ScrollPos { row: 6, offset: 0.0 });
    }

    #[test]
    fn max_is_top_when_everything_fits() {
        assert_eq!(ScrollPos::max(0, 600.0), ScrollPos::TOP);
        assert_eq!(ScrollPos::max(5, 600.0), ScrollPos::TOP);
        assert_eq!(ScrollPos::max(25, 600.0), ScrollPos::TOP); // ちょうど収まる
        assert_eq!(ScrollPos::max(10, 0.0), ScrollPos { row: 10, offset: 0.0 });
    }

    #[test]
    fn scrolled_by_carries_into_the_row_number() {
        assert_eq!(
            ScrollPos::TOP.scrolled_by(50.0),
            ScrollPos { row: 2, offset: 2.0 }
        );
        let pos = ScrollPos { row: 2, offset: 2.0 };
        assert_eq!(pos.scrolled_by(-26.0), ScrollPos { row: 1, offset: 0.0 });
        assert_eq!(pos.scrolled_by(-1.0), ScrollPos { row: 2, offset: 1.0 });
    }

    #[test]
    fn scrolled_by_never_goes_above_the_top() {
        assert_eq!(ScrollPos::TOP.scrolled_by(-1.0), ScrollPos::TOP);
        assert_eq!(ScrollPos::TOP.scrolled_by(-1e12), ScrollPos::TOP);
        let pos = ScrollPos { row: 3, offset: 5.0 };
        assert_eq!(pos.scrolled_by(-1e9), ScrollPos::TOP);
    }

    #[test]
    fn clamped_stops_at_the_maximum() {
        let max = ScrollPos::max(100, 600.0);
        assert_eq!(ScrollPos::TOP.scrolled_by(1e9).clamped(max), max);
        let inside = ScrollPos { row: 3, offset: 1.0 };
        assert_eq!(inside.clamped(max), inside);
    }

    #[test]
    fn the_step_size_is_the_same_at_every_position() {
        // どの行数でも、0.001px 動かすと、ずれがちょうど 0.001px 増える。
        for row in [0usize, 1_000, 30_466_333, 5_000_000_000_000] {
            let pos = ScrollPos { row, offset: 10.0 };
            let moved = pos.scrolled_by(0.001);
            assert_eq!(moved.row, row);
            assert!((f64::from(moved.offset) - 10.001).abs() < 1e-5, "row {row}");
        }
    }

    #[test]
    fn a_single_global_f64_loses_small_steps_at_extreme_positions() {
        // 比較用：5兆行ぶん（約1.2×10^14 px）の位置を1つの f64 で持つと、0.001px の動きが消える。
        let global = 5_000_000_000_000f64 * H + 10.0;
        assert_eq!(global + 0.001, global);
    }

    #[test]
    fn carry_stays_exact_for_large_row_numbers() {
        let pos = ScrollPos { row: 30_466_000, offset: 0.0 };
        let moved = pos.scrolled_by(H * 333.0 + 5.0);
        assert_eq!(moved.row, 30_466_333);
        assert!((moved.offset - 5.0).abs() < 1e-3);
    }

    #[test]
    fn offset_always_stays_below_the_row_height() {
        let mut seed = 0x1234_5678_9ABC_DEF1u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut pos = ScrollPos { row: 1_000_000, offset: 3.0 };
        for _ in 0..100_000 {
            let delta = (next() % 20_001) as f64 / 10.0 - 1_000.0; // -1000.0〜+1000.0 px
            pos = pos.scrolled_by(delta);
            assert!((0.0..ROW_HEIGHT).contains(&pos.offset), "{pos:?}");
        }
    }

    #[test]
    fn fraction_and_from_fraction_round_trip() {
        for (rows, viewport) in [(30_466_333usize, 700.0f32), (100, 600.0), (5_000_000_000_000, 2160.0)] {
            let max = ScrollPos::max(rows, viewport);
            for fraction in [0.0, 0.25, 0.5, 0.999_999, 1.0] {
                let pos = ScrollPos::from_fraction(fraction, max);
                assert!(pos <= max);
                let back = pos.fraction(max);
                assert!((back - fraction).abs() < 1e-9, "{rows} rows, {fraction}: {back}");
            }
            assert_eq!(ScrollPos::from_fraction(1.0, max), max);
            assert_eq!(ScrollPos::from_fraction(0.0, max), ScrollPos::TOP);
            assert_eq!(ScrollPos::from_fraction(2.0, max), max);
            assert_eq!(ScrollPos::from_fraction(-1.0, max), ScrollPos::TOP);
        }
    }

    #[test]
    fn fraction_is_zero_when_there_is_nothing_to_scroll() {
        let max = ScrollPos::max(5, 600.0);
        assert_eq!(ScrollPos::TOP.fraction(max), 0.0);
        assert_eq!(ScrollPos::from_fraction(0.7, max), ScrollPos::TOP);
    }
}
