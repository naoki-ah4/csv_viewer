//! 全読み込みの表の検索。全セルを順に調べる。
//! 遅延読み込みの `lazy/search.rs`（生のバイト列を先に走査する）と対になる。

use std::sync::atomic::{AtomicBool, Ordering};

use super::CsvTable;
use crate::matcher::{Direction, Matcher, Outcome};

impl CsvTable {
    /// `from_row` 以降（前向き）、または `from_row` より前（後ろ向き）で、最初に一致する行。
    /// 全セルを順に調べる。遅延読み込みの `LazySearcher::find_row` と、同じ引数・同じ戻り値の組。
    pub(crate) fn find_row(
        &self,
        matcher: &Matcher,
        from_row: usize,
        dir: Direction,
        cancel: &AtomicBool,
    ) -> Outcome<usize> {
        let columns = self.column_count();
        let row_matches =
            |row: usize| (0..columns).any(|col| matcher.is_match(self.cell(row, col)));
        let check = |row: usize, step: usize| row % step == 0 && cancel.load(Ordering::Relaxed);
        match dir {
            Direction::Forward => {
                for row in from_row..self.row_count() {
                    if check(row, 1024) {
                        return Outcome::Cancelled;
                    }
                    if row_matches(row) {
                        return Outcome::Found(row);
                    }
                }
            }
            Direction::Backward => {
                for row in (0..from_row.min(self.row_count())).rev() {
                    if check(row, 1024) {
                        return Outcome::Cancelled;
                    }
                    if row_matches(row) {
                        return Outcome::Found(row);
                    }
                }
            }
        }
        Outcome::NotFound
    }
}
