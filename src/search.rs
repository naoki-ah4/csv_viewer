//! 表の中の検索。
//!
//! - 順序は、行ごとに左から右、そして次の行へ。ヘッダー行は対象にしない。
//! - 全読み込みの表は、全セルを順に調べる。遅延読み込みの表は、生のバイト列を先に走査して
//!   候補を絞り、セルの中身で確かめる（`lazy_table::LazySearcher`）。どちらも同じ結果を返す。
//! - 検索語の判定は `matcher` にある。

use std::sync::atomic::{AtomicBool, Ordering};

use crate::csv_data::CsvTable;
use crate::matcher::{Direction, Matcher, Outcome};
use crate::table::Table;

/// 一致したセルの位置（データ行は0始まり、ヘッダーを除く）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub row: usize,
    pub col: usize,
}

impl Match {
    /// 1つ前のセル。これを `start` にして前向きに探すと、このセル自身から探したことになる。
    /// 入力しながらの検索で、今の一致が、まだ一致しているなら、そこに留まるために使う。
    pub fn previous_cell(self, columns: usize) -> Option<Match> {
        if self.col > 0 {
            Some(Match {
                row: self.row,
                col: self.col - 1,
            })
        } else if self.row > 0 {
            Some(Match {
                row: self.row - 1,
                col: columns.saturating_sub(1),
            })
        } else {
            None
        }
    }
}

/// 全読み込みの表で、`from_row` 以降（前向き）、または `from_row` より前（後ろ向き）の、
/// 最初に一致する行を探す。全セルを順に調べる。
pub fn find_row_in_csv(
    table: &CsvTable,
    matcher: &Matcher,
    from_row: usize,
    dir: Direction,
    cancel: &AtomicBool,
) -> Outcome<usize> {
    let columns = table.column_count();
    let row_matches =
        |row: usize| (0..columns).any(|col| matcher.is_match(table.cell(row, col)));
    let check = |row: usize, step: usize| row % step == 0 && cancel.load(Ordering::Relaxed);
    match dir {
        Direction::Forward => {
            for row in from_row..table.row_count() {
                if check(row, 1024) {
                    return Outcome::Cancelled;
                }
                if row_matches(row) {
                    return Outcome::Found(row);
                }
            }
        }
        Direction::Backward => {
            for row in (0..from_row.min(table.row_count())).rev() {
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

/// 行 `row` で、最初に（後ろ向きなら最後に）一致するセルの列。
pub fn match_col_in_row(
    table: &Table,
    matcher: &Matcher,
    row: usize,
    dir: Direction,
) -> Option<usize> {
    let columns = table.column_count();
    let matches = |col: &usize| matcher.is_match(&table.cell(row, *col));
    match dir {
        Direction::Forward => (0..columns).find(matches),
        Direction::Backward => (0..columns).rev().find(matches),
    }
}

/// `start` と同じ行の中で、`start` の次（後ろ向きなら前）のセルから探して、一致するセル。
pub fn same_row_step(
    table: &Table,
    matcher: &Matcher,
    start: Match,
    dir: Direction,
) -> Option<Match> {
    let hit = |col: &usize| matcher.is_match(&table.cell(start.row, *col));
    let col = match dir {
        Direction::Forward => (start.col + 1..table.column_count()).find(hit),
        Direction::Backward => (0..start.col).rev().find(hit),
    }?;
    Some(Match {
        row: start.row,
        col,
    })
}

/// `start` の行を調べ終えたあと、行ごとの検索を始める行。`find_row` の `from_row` に渡す。
/// `start` が `None` なら、前向きは先頭、後ろ向きは末尾から。
pub fn first_row_to_search(start: Option<Match>, dir: Direction, row_count: usize) -> usize {
    match (start, dir) {
        (Some(m), Direction::Forward) => m.row + 1,
        (Some(m), Direction::Backward) => m.row,
        (None, Direction::Forward) => 0,
        (None, Direction::Backward) => row_count,
    }
}

/// `start` の次（後ろ向きなら前）に一致するセルを探す。`start` が `None` なら、先頭（末尾）から。
/// 折り返しはしない。呼び出し側が、見つからなかったときに `None` から探し直す。
pub fn find_match(
    table: &Table,
    matcher: &Matcher,
    start: Option<Match>,
    dir: Direction,
    cancel: &AtomicBool,
) -> Outcome<Match> {
    // まず、同じ行の中で、`start` の隣のセルから探す。
    if let Some(found) = start.and_then(|m| same_row_step(table, matcher, m, dir)) {
        return Outcome::Found(found);
    }
    // 次に、行ごとに探す。行が見つかったのに、その中に一致するセルがなければ（索引作成中に
    // 行が増えたときなど）、その先を探し直す。
    let mut next_row = first_row_to_search(start, dir, table.row_count());
    loop {
        let found = match table.find_row(matcher, next_row, dir, cancel) {
            Outcome::Found(row) => row,
            Outcome::NotFound => return Outcome::NotFound,
            Outcome::Cancelled => return Outcome::Cancelled,
        };
        if let Some(col) = match_col_in_row(table, matcher, found, dir) {
            return Outcome::Found(Match { row: found, col });
        }
        next_row = match dir {
            Direction::Forward => found + 1,
            Direction::Backward => found,
        };
    }
}

/// 一致する行の数。`cap` に達したら、そこで止める（巨大なファイルで、数を数えきるのに、
/// 時間がかかりすぎないように）。戻り値は（数、`cap` で打ち切ったか）。
pub fn count_rows(
    table: &Table,
    matcher: &Matcher,
    cap: usize,
    cancel: &AtomicBool,
) -> Outcome<(usize, bool)> {
    let mut count = 0;
    let mut next_row = 0;
    loop {
        match table.find_row(matcher, next_row, Direction::Forward, cancel) {
            Outcome::Found(row) => {
                count += 1;
                if count >= cap {
                    return Outcome::Found((count, true));
                }
                next_row = row + 1;
            }
            Outcome::NotFound => return Outcome::Found((count, false)),
            Outcome::Cancelled => return Outcome::Cancelled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lazy_table::{LazySearcher, LazyTable};
    use encoding_rs::Encoding;
    use std::sync::Arc;

    const NEVER: AtomicBool = AtomicBool::new(false);

    /// 同じバイト列から作った、全読み込みの表と、遅延読み込みの表（索引を最後まで作ったもの）。
    struct Fixture {
        eager: Table,
        lazy_table: LazyTable,
        lazy: Table,
    }

    fn fixture(bytes: &[u8], encoding: &'static Encoding) -> Fixture {
        // 文字コードは、自動判定に任せず指定する。短い入力では、判定が揺れるため。
        let eager = Table::Eager(CsvTable::from_bytes_as(bytes, Some(encoding)).unwrap());
        let (lazy_table, mut job) = LazyTable::open(Arc::new(bytes.to_vec()), Some(encoding));
        job.run(&AtomicBool::new(false));
        // 同じ表を、`Table` としても使えるよう、もう1つ開く（`LazyTable` は複製できない）。
        let (lazy, mut job) = LazyTable::open(Arc::new(bytes.to_vec()), Some(encoding));
        job.run(&AtomicBool::new(false));
        Fixture {
            eager,
            lazy_table,
            lazy: Table::Lazy(lazy),
        }
    }

    /// 基準：元の文字列を `csv` クレートで読んで、素朴に調べた、一致するデータ行と、一致するセル。
    struct Oracle {
        rows: usize,
        matching_rows: Vec<usize>,
        matching_cells: Vec<Match>,
    }

    fn oracle(text: &str, needle: &str) -> Oracle {
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(text.as_bytes());
        let records: Vec<Vec<String>> = reader
            .records()
            .map(|r| r.unwrap().iter().map(str::to_owned).collect())
            .collect();
        let needle = needle.to_ascii_lowercase();
        let mut matching_rows = Vec::new();
        let mut matching_cells = Vec::new();
        let data = records.get(1..).unwrap_or(&[]);
        for (row, cells) in data.iter().enumerate() {
            let mut any = false;
            for (col, cell) in cells.iter().enumerate() {
                if cell.to_ascii_lowercase().contains(&needle) {
                    any = true;
                    matching_cells.push(Match { row, col });
                }
            }
            if any {
                matching_rows.push(row);
            }
        }
        Oracle {
            rows: data.len(),
            matching_rows,
            matching_cells,
        }
    }

    fn expected_row(o: &Oracle, from_row: usize, dir: Direction) -> Outcome<usize> {
        let found = match dir {
            Direction::Forward => o.matching_rows.iter().copied().find(|&r| r >= from_row),
            Direction::Backward => o.matching_rows.iter().copied().rev().find(|&r| r < from_row),
        };
        found.map_or(Outcome::NotFound, Outcome::Found)
    }

    /// 全読み込み、遅延読み込み（通常の走査）、遅延読み込み（極端に短い走査）が、基準と同じか。
    fn check(text: &str, bytes: &[u8], encoding: &'static Encoding, needle: &str, label: &str) {
        let Some(matcher) = Matcher::new(needle) else {
            return;
        };
        let o = oracle(text, needle);
        let f = fixture(bytes, encoding);
        let tiny = f.lazy_table.searcher().with_limits(5, 9);
        let normal = f.lazy_table.searcher();

        for from_row in 0..=o.rows + 1 {
            for dir in [Direction::Forward, Direction::Backward] {
                let want = expected_row(&o, from_row, dir);
                let ctx = format!("{label} {needle:?} from {from_row} {dir:?}");
                assert_eq!(f.eager.find_row(&matcher, from_row, dir, &NEVER), want, "eager: {ctx}");
                assert_eq!(normal.find_row(&matcher, from_row, dir, &NEVER), want, "lazy: {ctx}");
                assert_eq!(tiny.find_row(&matcher, from_row, dir, &NEVER), want, "lazy tiny: {ctx}");
            }
        }

        // セル単位で、前向きにたどった順序と、後ろ向きにたどった順序が、基準と同じ。
        for table in [&f.eager, &f.lazy] {
            let mut forward = Vec::new();
            let mut at = None;
            while let Outcome::Found(m) = find_match(table, &matcher, at, Direction::Forward, &NEVER) {
                forward.push(m);
                at = Some(m);
                assert!(forward.len() <= o.matching_cells.len() + 1, "{label} {needle:?}: runaway");
            }
            assert_eq!(forward, o.matching_cells, "forward cells: {label} {needle:?}");

            let mut backward = Vec::new();
            let mut at = None;
            while let Outcome::Found(m) = find_match(table, &matcher, at, Direction::Backward, &NEVER) {
                backward.push(m);
                at = Some(m);
                assert!(backward.len() <= o.matching_cells.len() + 1, "{label} {needle:?}: runaway");
            }
            backward.reverse();
            assert_eq!(backward, o.matching_cells, "backward cells: {label} {needle:?}");

            let want = (o.matching_rows.len(), false);
            assert_eq!(
                count_rows(table, &matcher, usize::MAX, &NEVER),
                Outcome::Found(want),
                "count: {label} {needle:?}"
            );
        }
    }

    fn encode(encoding: &'static Encoding, text: &str) -> Vec<u8> {
        if encoding == encoding_rs::UTF_16LE {
            text.encode_utf16().flat_map(u16::to_le_bytes).collect()
        } else if encoding == encoding_rs::UTF_16BE {
            text.encode_utf16().flat_map(u16::to_be_bytes).collect()
        } else {
            let (bytes, _, unmappable) = encoding.encode(text);
            assert!(!unmappable, "fixture text must be representable");
            bytes.into_owned()
        }
    }

    const ENCODINGS: [&Encoding; 5] = [
        encoding_rs::UTF_8,
        encoding_rs::SHIFT_JIS,
        encoding_rs::EUC_JP,
        encoding_rs::UTF_16LE,
        encoding_rs::UTF_16BE,
    ];

    const SAMPLE: &str = "id,名前,備考\n\
        1,山田太郎,Tokyo Office\n\
        2,鈴木花子,\"a,b\"\n\
        3,佐藤 Ken,\"line1\nline2\"\n\
        4,ア,\"say \"\"Hi\"\"\"\n\
        5,アイウ,HELLO world\n\
        6,あ,hello\n\
        7,,\n\
        8,山田,山田\n";

    #[test]
    fn matches_the_oracle_on_a_handwritten_table_in_every_encoding() {
        for encoding in ENCODINGS {
            let bytes = encode(encoding, SAMPLE);
            for needle in [
                "hello", "HELLO", "Tokyo", "tokyo office", "山田", "山", "太郎", "あ", "ア", "アイ",
                "A", "a", "ken", "line1\nline2", "\n", "a,b", "\"", "Hi", "say \"Hi\"", ",",
                "zzz", "名前", "id",
            ] {
                check(SAMPLE, &bytes, encoding, needle, encoding.name());
            }
        }
    }

    #[test]
    fn raw_byte_coincidences_are_rejected_by_the_verification() {
        // 「a,b」は、生のバイト列では、別々の2つのセル「a」「b」の境界にも現れる。
        let text = "x,y\na,b\n1,2\n\"a,b\",3\n";
        for encoding in ENCODINGS {
            let bytes = encode(encoding, text);
            let o = oracle(text, "a,b");
            assert_eq!(o.matching_rows, vec![2], "only the quoted cell contains a,b");
            check(text, &bytes, encoding, "a,b", encoding.name());
        }
        // Shift_JIS では、「ア」(0x83 0x41) の2バイト目が「A」と同じ値になる。
        // 生のバイト列では一致しても、セルの文字には、英字の A はない。
        let text = "x\nア\nァ\nイ\n";
        let bytes = encode(encoding_rs::SHIFT_JIS, text);
        assert!(bytes.contains(&0x41), "the trail byte looks like an ASCII A");
        let o = oracle(text, "A");
        assert!(o.matching_rows.is_empty());
        check(text, &bytes, encoding_rs::SHIFT_JIS, "A", "sjis trail byte");
        check(text, &bytes, encoding_rs::SHIFT_JIS, "a", "sjis trail byte");
    }

    #[test]
    fn the_header_row_is_not_searched() {
        let text = "needle,other\n1,2\n3,4\n";
        for encoding in ENCODINGS {
            let bytes = encode(encoding, text);
            check(text, &bytes, encoding, "needle", encoding.name());
            let f = fixture(&bytes, encoding);
            let matcher = Matcher::new("needle").unwrap();
            assert_eq!(
                f.lazy.find_row(&matcher, 0, Direction::Forward, &NEVER),
                Outcome::NotFound
            );
        }
    }

    #[test]
    fn a_needle_the_encoding_cannot_hold_never_matches() {
        let text = "a\nb\n";
        let bytes = encode(encoding_rs::SHIFT_JIS, text);
        let f = fixture(&bytes, encoding_rs::SHIFT_JIS);
        let matcher = Matcher::new("😀").unwrap();
        assert_eq!(f.lazy.find_row(&matcher, 0, Direction::Forward, &NEVER), Outcome::NotFound);
        assert_eq!(f.eager.find_row(&matcher, 0, Direction::Forward, &NEVER), Outcome::NotFound);
    }

    #[test]
    fn an_empty_table_has_nothing_to_find() {
        for encoding in ENCODINGS {
            let f = fixture(&encode(encoding, ""), encoding);
            let matcher = Matcher::new("a").unwrap();
            for dir in [Direction::Forward, Direction::Backward] {
                assert_eq!(f.eager.find_row(&matcher, 0, dir, &NEVER), Outcome::NotFound);
                assert_eq!(f.lazy.find_row(&matcher, 0, dir, &NEVER), Outcome::NotFound);
            }
        }
    }

    #[test]
    fn a_requested_cancel_stops_both_kinds_of_search() {
        let text: String = (0..50).map(|i| format!("{i},row\n")).collect();
        let bytes = encode(encoding_rs::UTF_8, &text);
        let f = fixture(&bytes, encoding_rs::UTF_8);
        let matcher = Matcher::new("zzz").unwrap();
        let cancel = AtomicBool::new(true);
        assert_eq!(f.eager.find_row(&matcher, 0, Direction::Forward, &cancel), Outcome::Cancelled);
        assert_eq!(f.lazy.find_row(&matcher, 0, Direction::Forward, &cancel), Outcome::Cancelled);
        assert_eq!(f.lazy.find_row(&matcher, 50, Direction::Backward, &cancel), Outcome::Cancelled);
        assert_eq!(count_rows(&f.lazy, &matcher, 10, &cancel), Outcome::Cancelled);
    }

    #[test]
    fn counting_stops_at_the_cap() {
        let text: String = (0..100).map(|i| format!("{i},hit\n")).collect();
        let bytes = encode(encoding_rs::UTF_8, &text);
        let f = fixture(&bytes, encoding_rs::UTF_8);
        let matcher = Matcher::new("hit").unwrap();
        // 先頭の行（ヘッダー）は数えないので、一致するのは99行。
        assert_eq!(count_rows(&f.eager, &matcher, 1000, &NEVER), Outcome::Found((99, false)));
        assert_eq!(count_rows(&f.lazy, &matcher, 1000, &NEVER), Outcome::Found((99, false)));
        assert_eq!(count_rows(&f.eager, &matcher, 10, &NEVER), Outcome::Found((10, true)));
        assert_eq!(count_rows(&f.lazy, &matcher, 10, &NEVER), Outcome::Found((10, true)));
    }

    #[test]
    fn only_the_indexed_part_is_searched_while_indexing_is_in_progress() {
        let text: String = (0..2000).map(|i| format!("{i},{}\n", if i == 1900 { "needle" } else { "x" })).collect();
        let bytes = encode(encoding_rs::UTF_8, &text);
        let (table, mut job) = LazyTable::open(Arc::new(bytes), Some(encoding_rs::UTF_8));
        let searcher = table.searcher();
        let matcher = Matcher::new("needle").unwrap();

        job.step(2_000);
        assert_eq!(searcher.find_row(&matcher, 0, Direction::Forward, &NEVER), Outcome::NotFound);
        while job.step(2_000) {}
        assert_eq!(searcher.find_row(&matcher, 0, Direction::Forward, &NEVER), Outcome::Found(1899));
        // 索引ができたあとなら、後ろ向きでも見つかる。
        assert_eq!(
            searcher.find_row(&matcher, 2000, Direction::Backward, &NEVER),
            Outcome::Found(1899)
        );
    }

    #[test]
    fn matches_the_oracle_on_random_tables() {
        const TOKENS: [&str; 18] = [
            "a", "A", "b", "ab", "AB", "あ", "ア", "漢", "山田", "x", " ", ",", ",", "\"", "\"\"",
            "\n", "\n", "\r\n",
        ];
        const NEEDLES: [&str; 12] = [
            "a", "ab", "AB", "b", "山田", "あ", "ア", "a,b", "b\n", "\"", ",", "漢あ",
        ];
        let mut seed = 0xDEAD_BEEF_CAFE_F00Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..120 {
            // たまに長い表を混ぜて、疎な索引の複数のブロックと、走査の窓を、またがせる。
            let len = if round % 20 == 0 { 700 } else { (next() % 60) as usize };
            let text: String = (0..len)
                .map(|_| TOKENS[(next() % TOKENS.len() as u64) as usize])
                .collect();
            for encoding in [encoding_rs::UTF_8, encoding_rs::SHIFT_JIS, encoding_rs::UTF_16LE] {
                let bytes = encode(encoding, &text);
                for needle in NEEDLES {
                    check(&text, &bytes, encoding, needle, &format!("round {round} {}", encoding.name()));
                }
            }
        }
    }

    // `LazySearcher` が、別スレッドへ渡せること（コンパイル時の確認）。
    #[allow(dead_code)]
    fn lazy_searcher_is_send_and_sync() {
        fn assert_send<T: Send + Sync>() {}
        assert_send::<LazySearcher>();
    }

    #[test]
    fn previous_cell_steps_back_across_rows() {
        assert_eq!(Match { row: 3, col: 2 }.previous_cell(5), Some(Match { row: 3, col: 1 }));
        assert_eq!(Match { row: 3, col: 0 }.previous_cell(5), Some(Match { row: 2, col: 4 }));
        assert_eq!(Match { row: 0, col: 1 }.previous_cell(5), Some(Match { row: 0, col: 0 }));
        assert_eq!(Match { row: 0, col: 0 }.previous_cell(5), None);
        // 列がない表でも、あふれない。
        assert_eq!(Match { row: 1, col: 0 }.previous_cell(0), Some(Match { row: 0, col: 0 }));
    }

    #[test]
    fn searching_from_the_cell_before_includes_the_cell_itself() {
        // 入力しながらの検索：今の一致が、まだ一致しているなら、そこに留まる。
        let text = "a,b\nx,hello\nhello,y\n";
        let f = fixture(&encode(encoding_rs::UTF_8, text), encoding_rs::UTF_8);
        let matcher = Matcher::new("hel").unwrap();
        let current = Match { row: 1, col: 0 };
        let start = current.previous_cell(f.eager.column_count());
        for table in [&f.eager, &f.lazy] {
            assert_eq!(
                find_match(table, &matcher, start, Direction::Forward, &NEVER),
                Outcome::Found(current)
            );
        }
    }

    #[test]
    fn the_first_row_to_search_follows_the_start_and_direction() {
        let m = Some(Match { row: 7, col: 3 });
        assert_eq!(first_row_to_search(m, Direction::Forward, 100), 8);
        assert_eq!(first_row_to_search(m, Direction::Backward, 100), 7);
        assert_eq!(first_row_to_search(None, Direction::Forward, 100), 0);
        assert_eq!(first_row_to_search(None, Direction::Backward, 100), 100);
    }
}

