//! 遅延読み込みの計測。実ファイルを mmap で開き、索引作成の速度とセル取得の遅延を測る。
//!
//!   cargo run --release --example bench_lazy -- <file.csv>
//!   /usr/bin/time -l cargo run --release --example bench_lazy -- <file.csv>   # ピークメモリも見る
//!
//! セルの中身は表示しない（件数と時間だけを出す）。
//! OS のページキャッシュの状態で、初回と2回目以降の速度が変わる点に注意。

#[path = "../src/csv_data.rs"]
#[allow(dead_code)]
mod csv_data;
#[path = "../src/lazy_table.rs"]
#[allow(dead_code)]
mod lazy_table;
#[path = "../src/record_index.rs"]
#[allow(dead_code)]
mod record_index;

use std::fs::File;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use lazy_table::{IndexStatus, LazyTable};
use memmap2::Mmap;

fn rss_mb() -> f64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().unwrap_or(0.0) / 1024.0
}

fn percentile(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() - 1) as f64 * p) as usize]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: bench_lazy <file.csv>");
    let file = File::open(&path).unwrap();
    // SAFETY: 計測中に他のプロセスがファイルを変更しないことを前提とする。
    let mmap = unsafe { Mmap::map(&file).unwrap() };
    let file_mb = mmap.len() as f64 / 1024.0 / 1024.0;

    let start = Instant::now();
    let (table, mut job) = LazyTable::open(Arc::new(mmap), None);
    let open_ms = ms(start.elapsed());
    let rss_open = rss_mb();

    // 索引作成は別スレッドで走らせ、こちらでは表がいつから読めるようになるかを測る。
    let cancel = Arc::new(AtomicBool::new(false));
    let handle = {
        let cancel = cancel.clone();
        std::thread::spawn(move || job.run(&cancel))
    };
    let mut first_row = None;
    let mut first_1000 = None;
    let mut max_poll_gap = Duration::ZERO;
    let mut last_poll = Instant::now();
    while table.status() == IndexStatus::Indexing {
        let rows = table.row_count();
        if rows >= 1 && first_row.is_none() {
            first_row = Some(start.elapsed());
        }
        if rows >= 1000 && first_1000.is_none() {
            first_1000 = Some(start.elapsed());
        }
        max_poll_gap = max_poll_gap.max(last_poll.elapsed());
        last_poll = Instant::now();
        std::thread::sleep(Duration::from_millis(2));
    }
    handle.join().unwrap();
    let index_total = start.elapsed();
    let rss_indexed = rss_mb();

    let rows = table.row_count();
    println!("file            : {file_mb:.0} MB");
    println!("open (mmap)     : {open_ms:.2} ms");
    println!(
        "first row       : {:.1} ms,  1000 rows: {:.1} ms",
        first_row.map_or(f64::NAN, ms),
        first_1000.map_or(f64::NAN, ms)
    );
    println!(
        "index complete  : {:.0} ms  ({:.0} MB/s)",
        ms(index_total),
        file_mb / index_total.as_secs_f64()
    );
    println!(
        "rows={rows}  columns={}  encoding={}  status={:?}",
        table.column_count(),
        table.encoding().name(),
        table.status()
    );
    println!(
        "sparse index    : ~{:.1} MB",
        (rows / record_index::STRIDE + 1) as f64 * 8.0 / 1024.0 / 1024.0
    );
    println!("rss             : after open {rss_open:.0} MB, after index {rss_indexed:.0} MB");

    // セル取得の遅延。1回ずつ別の行を読む（キャッシュに当たりにくい）。
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let cols = table.column_count().max(1);
    let mut single: Vec<Duration> = (0..20_000)
        .map(|_| {
            let row = (next() % rows.max(1) as u64) as usize;
            let t = Instant::now();
            std::hint::black_box(table.cell(row, 0));
            t.elapsed()
        })
        .collect();
    single.sort();
    println!(
        "random cell     : median {:.1} us, p99 {:.1} us, max {:.2} ms",
        percentile(&single, 0.5).as_secs_f64() * 1e6,
        percentile(&single, 0.99).as_secs_f64() * 1e6,
        ms(*single.last().unwrap())
    );

    // 画面1枚ぶん（40行 × 全列）を、ランダムな位置から描くコスト。
    let mut frames: Vec<Duration> = (0..2_000)
        .map(|_| {
            let top = (next() % rows.saturating_sub(40).max(1) as u64) as usize;
            let t = Instant::now();
            for r in top..(top + 40).min(rows) {
                for c in 0..cols {
                    std::hint::black_box(table.cell(r, c));
                }
            }
            t.elapsed()
        })
        .collect();
    frames.sort();
    println!(
        "frame (40 rows x {cols} cols) : median {:.2} ms, p99 {:.2} ms, max {:.2} ms",
        ms(percentile(&frames, 0.5)),
        ms(percentile(&frames, 0.99)),
        ms(*frames.last().unwrap())
    );
    println!("rss             : at end {:.0} MB", rss_mb());
}
