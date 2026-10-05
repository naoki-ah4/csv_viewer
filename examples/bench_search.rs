//! 遅延読み込みの検索の計測。実ファイルを mmap で開き、索引を作り終えてから、
//! いくつかの検索語で、検索にかかる時間を測る。セルの中身は表示しない（時間と行番号だけ）。
//!
//!   cargo run --release --example bench_search -- <file.csv>

#[path = "../src/matcher.rs"]
#[allow(dead_code)]
mod matcher;
#[path = "../src/search.rs"]
#[allow(dead_code)]
mod search;
#[path = "../src/table/mod.rs"]
#[allow(dead_code)]
mod table;

use std::fs::File;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use matcher::{Direction, Matcher, Outcome};
use memchr::memchr;
use memmap2::Mmap;
use table::lazy::LazyTable;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: bench_search <file.csv>");
    let file = File::open(&path).unwrap();
    // SAFETY: 計測中に他のプロセスがファイルを変更しないことを前提とする。
    let mmap = unsafe { Mmap::map(&file).unwrap() };
    let file_mb = mmap.len() as f64 / 1024.0 / 1024.0;
    let (table, mut job) = LazyTable::open(Arc::new(mmap), None);
    let t = Instant::now();
    job.run(&AtomicBool::new(false));
    let rows = table.row_count();
    println!(
        "file {file_mb:.0} MB, {rows} rows, indexed in {:.0} ms",
        t.elapsed().as_secs_f64() * 1000.0
    );

    let searcher = table.searcher();
    let never = AtomicBool::new(false);
    let describe = |o: &Outcome<usize>| match o {
        Outcome::Found(row) => format!("found row {row}"),
        Outcome::NotFound => "not found".to_string(),
        Outcome::Cancelled => "cancelled".to_string(),
    };
    let cases = [
        (
            "absent, lowercase, forward (full scan)",
            "zzzz-not-here-zzzz",
            Direction::Forward,
            0,
        ),
        (
            "absent, UPPERCASE letters (case-insensitive), forward",
            "ZZZZ-NOT-HERE-ZZZZ",
            Direction::Forward,
            0,
        ),
        (
            "absent, backward from the end (full scan)",
            "zzzz-not-here-zzzz",
            Direction::Backward,
            rows,
        ),
        (
            "absent, Japanese, forward (full scan)",
            "あいうえおかきくけこ",
            Direction::Forward,
            0,
        ),
        (
            "common digit, first match forward",
            "2",
            Direction::Forward,
            0,
        ),
        (
            "common digit, first match backward from the end",
            "2",
            Direction::Backward,
            rows,
        ),
        (
            "absent, with a quote in the needle, forward",
            "zz\"zz",
            Direction::Forward,
            0,
        ),
    ];
    for (label, needle, dir, from) in cases {
        let matcher = Matcher::new(needle).unwrap();
        let mut best = f64::MAX;
        let mut outcome = Outcome::NotFound;
        for _ in 0..3 {
            let t = Instant::now();
            outcome = searcher.find_row(&matcher, from, dir, &never);
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        }
        let gbps = if matches!(outcome, Outcome::NotFound) {
            format!(" ({:.1} GB/s)", file_mb / 1024.0 / (best / 1000.0))
        } else {
            String::new()
        };
        println!("{label:<56} {best:>9.1} ms  {}{gbps}", describe(&outcome));
    }

    // 一致する行の数（上限つき）。前向きに、1行ずつ、次の一致を探す。
    for (needle, cap) in [("2", 100_000usize), ("zzzz-not-here-zzzz", 100_000)] {
        let matcher = Matcher::new(needle).unwrap();
        let t = Instant::now();
        let mut count = 0usize;
        let mut next = 0usize;
        while count < cap {
            match searcher.find_row(&matcher, next, Direction::Forward, &never) {
                Outcome::Found(row) => {
                    count += 1;
                    next = row + 1;
                }
                _ => break,
            }
        }
        let _ = memchr(b'x', b"");
        println!(
            "count rows containing the needle (cap {cap}): {count}{}  in {:.0} ms",
            if count >= cap { "+" } else { "" },
            t.elapsed().as_secs_f64() * 1000.0
        );
    }
}
