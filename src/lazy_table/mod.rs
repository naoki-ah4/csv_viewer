//! 巨大なファイル向けの遅延読み込みの表。
//!
//! 元のバイト列（mmap など）をそのまま参照し、行の先頭位置だけを疎な索引で持つ。
//! 表示するセルだけを、その場でデコードして `csv` でパースする。
//!
//! 索引の作成（`IndexJob`）は別スレッドで進められる。表は作成の途中でも読める。
//! 文字コードは、最初の非ASCIIバイトが見つかった時点で判定する（それまでの行はASCIIだけで、
//! どの文字コードでも同じに読めるため）。

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use encoding_rs::Encoding;

use crate::csv_data::{classify_from, find_first_non_ascii};
use crate::record_index::{
    Byte, Record, RecordIndex, Scanner, STRIDE, Unit, Utf16Be, Utf16Le, aligned_end,
    locate_in_block, record_containing,
};
use crate::matcher::{Direction, Matcher, Outcome};

mod index_job;
mod searcher;
#[cfg(test)]
mod tests;

pub use index_job::IndexJob;
pub use searcher::LazySearcher;

/// 元のバイト列。mmap でも `Vec<u8>` でもよい。
pub type Bytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// 最近読んだ行をいくつ覚えておくか。画面に出る行数より十分大きくする。
const CACHE_ROWS: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UnitKind {
    Byte,
    Utf16Le,
    Utf16Be,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IndexStatus {
    Indexing,
    Done,
    Cancelled,
    /// 遅延読み込みでは扱えない文字コード（ISO-2022-JP など）。全読み込みで開き直す必要がある。
    Unsupported(&'static Encoding),
}

struct EncodingState {
    encoding: &'static Encoding,
    /// BOM・手動指定・判定のいずれかで確定済みか。確定後は判定しない。
    locked: bool,
}

struct Shared {
    data: Bytes,
    kind: UnitKind,
    index: Mutex<RecordIndex>,
    encoding: Mutex<EncodingState>,
    status: Mutex<IndexStatus>,
    scanned: AtomicUsize,
}

impl Shared {
    fn bytes(&self) -> &[u8] {
        (*self.data).as_ref()
    }
}

pub struct LazyTable {
    shared: Arc<Shared>,
    cache: RefCell<RowCache>,
}

struct RowCache {
    encoding: &'static Encoding,
    rows: VecDeque<(usize, Arc<Vec<String>>)>,
}

impl LazyTable {
    /// `forced` を指定すると、文字コードを自動判定せずその文字コードで読む。
    pub fn open(data: Bytes, forced: Option<&'static Encoding>) -> (LazyTable, IndexJob) {
        let bytes = (*data).as_ref();
        let (encoding, body_start, locked) = match (Encoding::for_bom(bytes), forced) {
            (Some((bom, len)), None) => (bom, len, true),
            (Some((bom, len)), Some(forced)) if bom == forced => (forced, len, true),
            (_, Some(forced)) => (forced, 0, true),
            (None, None) => (encoding_rs::UTF_8, 0, false),
        };
        let kind = if encoding == encoding_rs::UTF_16LE {
            UnitKind::Utf16Le
        } else if encoding == encoding_rs::UTF_16BE {
            UnitKind::Utf16Be
        } else {
            UnitKind::Byte
        };
        let status = if encoding == encoding_rs::ISO_2022_JP {
            IndexStatus::Unsupported(encoding)
        } else {
            IndexStatus::Indexing
        };
        let shared = Arc::new(Shared {
            data,
            kind,
            index: Mutex::new(RecordIndex::new()),
            encoding: Mutex::new(EncodingState { encoding, locked }),
            status: Mutex::new(status),
            scanned: AtomicUsize::new(body_start),
        });
        let table = LazyTable {
            shared: shared.clone(),
            cache: RefCell::new(RowCache {
                encoding,
                rows: VecDeque::new(),
            }),
        };
        let job = IndexJob::new(shared, body_start);
        (table, job)
    }

    pub fn encoding(&self) -> &'static Encoding {
        self.shared.encoding.lock().unwrap().encoding
    }

    /// 別スレッドから検索するための窓口。行のキャッシュは持たず、`Send` で、安く複製できる。
    pub fn searcher(&self) -> LazySearcher {
        LazySearcher::new(self.shared.clone())
    }

    pub fn status(&self) -> IndexStatus {
        *self.shared.status.lock().unwrap()
    }

    /// 索引を作り終えたバイト数と、ファイルの全体バイト数。
    pub fn progress(&self) -> (usize, usize) {
        (
            self.shared.scanned.load(Ordering::Relaxed),
            aligned_end_for(self.shared.kind, self.shared.bytes()),
        )
    }

    /// 索引ができたぶんのデータ行の数（ヘッダーを除く）。作成中は増えていく。
    pub fn row_count(&self) -> usize {
        self.shared
            .index
            .lock()
            .unwrap()
            .record_count()
            .saturating_sub(1)
    }

    /// これまでに見つかった中で最も広い行の列数。作成中は増えることがある。
    pub fn column_count(&self) -> usize {
        self.shared.index.lock().unwrap().max_fields()
    }

    pub fn header(&self, col: usize) -> Cow<'_, str> {
        self.record_cell(0, col)
    }

    /// データ行 `row`（0始まり、ヘッダーを除く）の `col` 列目。範囲外は空文字列。
    pub fn cell(&self, row: usize, col: usize) -> Cow<'_, str> {
        self.record_cell(row + 1, col)
    }

    fn record_cell(&self, record: usize, col: usize) -> Cow<'_, str> {
        match self.fields(record).and_then(|f| f.get(col).cloned()) {
            Some(text) => Cow::Owned(text),
            None => Cow::Borrowed(""),
        }
    }

    fn fields(&self, record: usize) -> Option<Arc<Vec<String>>> {
        let encoding = self.encoding();
        {
            let mut cache = self.cache.borrow_mut();
            if cache.encoding != encoding {
                // 文字コードが確定・変更されたら、古いデコード結果は使えない。
                cache.encoding = encoding;
                cache.rows.clear();
            }
            if let Some((_, fields)) = cache.rows.iter().find(|(r, _)| *r == record) {
                return Some(fields.clone());
            }
        }
        // 索引のロックは、ブロック先頭を読む間だけ持つ。走査とデコードはロックの外で行う。
        let block_start = self.shared.index.lock().unwrap().block_start(record)?;
        let bytes = self.shared.bytes();
        let to_skip = record % STRIDE;
        let range = match self.shared.kind {
            UnitKind::Byte => locate_in_block::<Byte>(bytes, block_start, to_skip),
            UnitKind::Utf16Le => locate_in_block::<Utf16Le>(bytes, block_start, to_skip),
            UnitKind::Utf16Be => locate_in_block::<Utf16Be>(bytes, block_start, to_skip),
        }?;
        let (text, _) = encoding.decode_without_bom_handling(&bytes[range]);
        let fields = Arc::new(parse_record(&text));

        let mut cache = self.cache.borrow_mut();
        if cache.rows.len() >= CACHE_ROWS {
            cache.rows.pop_front();
        }
        cache.rows.push_back((record, fields.clone()));
        Some(fields)
    }
}

/// 1レコード分のテキストを `csv` で読んでフィールドに分ける。
fn parse_record(text: &str) -> Vec<String> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .buffer_capacity(1024)
        .from_reader(text.as_bytes());
    let mut record = csv::StringRecord::new();
    match reader.read_record(&mut record) {
        Ok(true) => record.iter().map(str::to_owned).collect(),
        _ => Vec::new(),
    }
}

fn aligned_end_for(kind: UnitKind, bytes: &[u8]) -> usize {
    match kind {
        UnitKind::Byte => aligned_end::<Byte>(bytes),
        UnitKind::Utf16Le | UnitKind::Utf16Be => aligned_end::<Utf16Le>(bytes),
    }
}
