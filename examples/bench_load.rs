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

/// BENCH_RUNS=1 にすると1回だけ実行する（ピークメモリを測るときは、前回の結果が残らないようこちらを使う）。
fn runs() -> usize {
    std::env::var("BENCH_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(5)
}

/// 現在のRSS(MB)。`ps` 経由の簡易計測（診断用）。
fn rss_mb() -> f64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().parse::<f64>().unwrap_or(0.0) / 1024.0
}

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
    for _ in 0..runs() {
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

    // 生成に使った文字列は、計測前に解放する（ピークメモリに混ざらないように）。
    let bytes = {
        let text = generate((mb * 1024.0 * 1024.0) as usize, enc == "ascii");
        match enc.as_str() {
            "sjis" => encoding_rs::SHIFT_JIS.encode(&text).0.into_owned(),
            _ => text.into_bytes(),
        }
    };

    let rss_input = rss_mb();
    let (detect_ms, _) = best_of(|| {
        let mut d = EncodingDetector::new(Iso2022JpDetection::Allow);
        d.feed(&bytes, true);
        d.guess(None, Utf8Detection::Allow)
    });
    let (total_ms, table) = best_of(|| CsvTable::from_bytes(&bytes).unwrap());

    eprintln!("rss: after input built={rss_input:.0}MB  after load={:.0}MB", rss_mb());
    println!(
        "{enc:>5} {:>6.1}MB  rows={:>8}  full_scan_detect(旧方式の参考値)={detect_ms:>7.1}ms  from_bytes(total)={total_ms:>7.1}ms  table_heap={:>6.1}MB",
        bytes.len() as f64 / 1024.0 / 1024.0,
        table.row_count(),
        table.heap_bytes() as f64 / 1024.0 / 1024.0,
    );
}
