use std::borrow::Cow;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use encoding_rs::Encoding;
use memmap2::Mmap;

use crate::csv_data::CsvTable;
use crate::lazy_table::{IndexJob, IndexStatus, LazyTable};

/// これ以上のサイズのファイルは、全体を読み込まず遅延読み込みで開く。
///
/// 決め手は最初の画面が出るまでの時間。全読み込みは約1.2倍のメモリで済むが、サイズに比例して
/// 待たされる（M4 Pro で 43MB が約60ms。CPU が3倍遅くHDDなら、10〜20MB で体感できる待ちになる）。
/// 遅延読み込みは、サイズに関わらず最初の行がすぐ読める（M4 Pro で 1.8GB でも数ms）。
/// 環境変数 `CSV_VIEWER_LAZY_THRESHOLD`（バイト数）で上書きできる（動作確認・計測用）。
const DEFAULT_LAZY_THRESHOLD: u64 = 16 * 1024 * 1024;
const LAZY_THRESHOLD_ENV: &str = "CSV_VIEWER_LAZY_THRESHOLD";

fn lazy_threshold() -> u64 {
    std::env::var(LAZY_THRESHOLD_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_LAZY_THRESHOLD)
}

/// UI が見る表の窓口。読み込み方式（全読み込み / 遅延）の違いはここで隠す。
///
/// セルは `Cow<str>` で返す。全読み込み側は借用（コピーなし）、
/// 遅延側は必要な行をその場でデコードするため所有になる。
pub enum Table {
    Eager(CsvTable),
    Lazy(LazyTable),
}

impl Table {
    /// ファイルサイズで読み込み方式を選んで開く。
    /// 遅延読み込みの場合は、索引を作る `IndexJob` も返す。呼び出し側がバックグラウンドで走らせる。
    pub fn open(path: &Path) -> anyhow::Result<(Self, Option<IndexJob>)> {
        Self::open_with_threshold(path, lazy_threshold())
    }

    fn open_with_threshold(
        path: &Path,
        threshold: u64,
    ) -> anyhow::Result<(Self, Option<IndexJob>)> {
        let size = std::fs::metadata(path)?.len();
        if size > 0 && size >= threshold {
            let file = File::open(path)?;
            // SAFETY: 開いている間に他のプロセスがファイルを切り詰めると、読み出し時に
            // SIGBUS になり得る。CSV ビューアでは、その場合のクラッシュを許容する。
            let mmap = unsafe { Mmap::map(&file)? };
            let (table, job) = LazyTable::open(Arc::new(mmap), None);
            return Ok((Self::Lazy(table), Some(job)));
        }
        Ok((Self::load_eager(path)?, None))
    }

    /// サイズに関わらず、全体を読み込んで開く。
    pub fn load_eager(path: &Path) -> anyhow::Result<Self> {
        Ok(Self::Eager(CsvTable::load(path)?))
    }

    pub fn encoding(&self) -> &'static Encoding {
        match self {
            Self::Eager(t) => t.encoding,
            Self::Lazy(t) => t.encoding(),
        }
    }

    /// ヘッダーを除いたデータ行の数。遅延読み込みの索引作成中は、見つかったぶんだけ。
    pub fn row_count(&self) -> usize {
        match self {
            Self::Eager(t) => t.row_count(),
            Self::Lazy(t) => t.row_count(),
        }
    }

    /// ヘッダーを含めた中で最も広い行の列数。遅延読み込みの索引作成中は、増えることがある。
    pub fn column_count(&self) -> usize {
        match self {
            Self::Eager(t) => t.column_count(),
            Self::Lazy(t) => t.column_count(),
        }
    }

    pub fn header(&self, col: usize) -> Cow<'_, str> {
        match self {
            Self::Eager(t) => Cow::Borrowed(t.header(col)),
            Self::Lazy(t) => t.header(col),
        }
    }

    /// データ行 `row`（0始まり、ヘッダーを除く）の `col` 列目。範囲外は空文字列。
    pub fn cell(&self, row: usize, col: usize) -> Cow<'_, str> {
        match self {
            Self::Eager(t) => Cow::Borrowed(t.cell(row, col)),
            Self::Lazy(t) => t.cell(row, col),
        }
    }

    /// 遅延読み込みの索引の状態。全読み込みなら `None`。
    pub fn index_status(&self) -> Option<IndexStatus> {
        match self {
            Self::Eager(_) => None,
            Self::Lazy(t) => Some(t.status()),
        }
    }

    /// 索引作成中なら進捗（0〜100%）。
    pub fn indexing_percent(&self) -> Option<u8> {
        match self {
            Self::Lazy(t) if t.status() == IndexStatus::Indexing => {
                let (scanned, total) = t.progress();
                Some((scanned.saturating_mul(100) / total.max(1)).min(100) as u8)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eager(csv: &str) -> Table {
        Table::Eager(CsvTable::from_bytes(csv.as_bytes()).unwrap())
    }

    #[test]
    fn delegates_to_the_eager_table() {
        let t = eager("名前,部署\n山田,営業\n鈴木,開発\n");
        assert_eq!(t.row_count(), 2);
        assert_eq!(t.column_count(), 2);
        assert_eq!(t.header(1), "部署");
        assert_eq!(t.cell(1, 0), "鈴木");
        assert_eq!(t.cell(9, 9), "");
        assert_eq!(t.encoding(), encoding_rs::UTF_8);
    }

    #[test]
    fn eager_cells_are_borrowed_not_copied() {
        let t = eager("a,b\n1,2\n");
        assert!(matches!(t.header(0), Cow::Borrowed(_)));
        assert!(matches!(t.cell(0, 1), Cow::Borrowed(_)));
        // 範囲外も借用（'static の空文字列）で、確保が発生しない。
        assert!(matches!(t.cell(5, 5), Cow::Borrowed("")));
    }

    fn same_cells(a: &Table, b: &Table) {
        assert_eq!(a.encoding(), b.encoding());
        assert_eq!(a.row_count(), b.row_count());
        assert_eq!(a.column_count(), b.column_count());
        for c in 0..=a.column_count() {
            assert_eq!(a.header(c), b.header(c));
        }
        for r in 0..=a.row_count() {
            for c in 0..=a.column_count() {
                assert_eq!(a.cell(r, c), b.cell(r, c), "cell ({r},{c})");
            }
        }
    }

    #[test]
    fn files_at_or_above_the_threshold_open_lazily_via_mmap() {
        use std::sync::atomic::AtomicBool;

        for name in ["utf8", "sjis", "wide"] {
            let path = std::path::PathBuf::from(format!("samples/{name}.csv"));
            let (lazy, job) = Table::open_with_threshold(&path, 1).unwrap();
            assert!(matches!(lazy, Table::Lazy(_)), "{name}");
            job.expect("a lazy table needs an index job")
                .run(&AtomicBool::new(false));
            assert_eq!(lazy.index_status(), Some(IndexStatus::Done), "{name}");
            assert_eq!(lazy.indexing_percent(), None, "{name}");
            same_cells(&lazy, &Table::load_eager(&path).unwrap());
        }
    }

    #[test]
    fn files_below_the_threshold_open_eagerly() {
        let path = std::path::Path::new("samples/utf8.csv");
        let (table, job) = Table::open_with_threshold(path, u64::MAX).unwrap();
        assert!(matches!(table, Table::Eager(_)));
        assert!(job.is_none());
        assert_eq!(table.index_status(), None);
    }

    #[test]
    fn indexing_percent_reports_progress_and_clears_when_done() {
        let path = std::path::Path::new("samples/wide.csv");
        let (table, job) = Table::open_with_threshold(path, 1).unwrap();
        let mut job = job.unwrap();
        assert_eq!(table.indexing_percent(), Some(0));
        assert!(job.step(4096));
        let mid = table.indexing_percent().unwrap();
        assert!(mid > 0 && mid < 100, "progress after one small step: {mid}");
        while job.step(4096) {}
        assert_eq!(table.indexing_percent(), None);
    }
}
