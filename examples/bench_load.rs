//! 読み込み速度の計測。
//!
//!   cargo run --release --example bench_load -- <size_mb> [utf8|sjis|ascii]
//!   /usr/bin/time -l cargo run --release --example bench_load -- 10 utf8   # ピークメモリも見る
//!
//! full_scan_detect は、ファイル全体を chardetng に通した場合の参考値（現在の実装は先頭の一部だけ）。
//! 生成した入力データ（size_mb 分）もメモリに載るため、ピークRSSからその分を引いて読むこと。

#[path = "../src/csv_data.rs"]
#[allow(dead_code)]
mod csv_data;

use std::time::Instant;

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use csv_data::CsvTable;

const RUNS: usize = 5;

fn generate(target_bytes: usize, ascii: bool) -> String {
    let mut s = String::with_capacity(target_bytes + 128);
    s.push_str(if ascii { "id,name,dept,memo,amount\n" } else { "id,名前,部署,メモ,金額\n" });
    let mut i = 0usize;
    while s.len() < target_bytes {
        use std::fmt::Write;
        if ascii {
            writeln!(s, "{i},test taro{i},sales,\"row {i} memo, quoted\",{}", i * 37 % 100_000)
        } else {
            writeln!(
                s,
                "{i},テスト太郎{i},営業部,\"row {i} のメモ, 引用符つき\",{}",
                i * 37 % 100_000
            )
        }
        .unwrap();
        i += 1;
    }
    s
}

fn best_of<T>(mut f: impl FnMut() -> T) -> (f64, T) {
    let mut best = f64::MAX;
    let mut last = None;
    for _ in 0..RUNS {
        let t = Instant::now();
        let v = f();
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        last = Some(v);
    }
    (best, last.unwrap())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mb: f64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(10.0);
    let enc = args.next().unwrap_or_else(|| "utf8".into());

    let text = generate((mb * 1024.0 * 1024.0) as usize, enc == "ascii");
    let bytes = match enc.as_str() {
        "sjis" => encoding_rs::SHIFT_JIS.encode(&text).0.into_owned(),
        _ => text.into_bytes(),
    };

    let (detect_ms, _) = best_of(|| {
        let mut d = EncodingDetector::new(Iso2022JpDetection::Allow);
        d.feed(&bytes, true);
        d.guess(None, Utf8Detection::Allow)
    });
    let (total_ms, table) = best_of(|| CsvTable::from_bytes(&bytes).unwrap());

    println!(
        "{enc:>4} {:>6.1}MB  rows={:>8}  full_scan_detect(旧方式の参考値)={detect_ms:>7.1}ms  from_bytes(total)={total_ms:>7.1}ms",
        bytes.len() as f64 / 1024.0 / 1024.0,
        table.rows.len(),
    );
}
