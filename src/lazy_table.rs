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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use encoding_rs::Encoding;

use crate::csv_data::{classify_from, find_first_non_ascii};
use crate::record_index::{
    Byte, Record, RecordIndex, Scanner, STRIDE, Unit, Utf16Be, Utf16Le, aligned_end,
    locate_in_block,
};

/// 元のバイト列。mmap でも `Vec<u8>` でもよい。
pub type Bytes = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// 索引を1回の `feed` で進める最大バイト数。
const CHUNK: usize = 4 * 1024 * 1024;
/// 最初の1回だけは小さくして、最初の行をすぐ読めるようにする。HDD や遅い CPU では、
/// 4MiB の読み出しと走査だけで数十ms かかる（M4 Pro＋NVMe の実測で約8ms → 約2.5ms）。
const FIRST_CHUNK: usize = 256 * 1024;
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

/// 索引を作る側。別スレッドへ渡して `run` を呼ぶ。1チャンクずつ進める `step` も使える。
pub struct IndexJob {
    shared: Arc<Shared>,
    /// 次に走査するバイト位置。
    pos: usize,
    scanner: Scanner,
    /// 見つけたが、まだ索引に追加していないレコード。
    batch: Vec<Record>,
    /// まだ最初のステップを踏んでいないか。
    first_step: bool,
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
        let job = IndexJob {
            shared,
            pos: body_start,
            scanner: Scanner::new(body_start),
            batch: Vec::new(),
            first_step: true,
        };
        (table, job)
    }

    pub fn encoding(&self) -> &'static Encoding {
        self.shared.encoding.lock().unwrap().encoding
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

impl IndexJob {
    /// 索引を最後まで作る。`cancel` が立ったら、次のチャンクの前で止まる。
    pub fn run(&mut self, cancel: &AtomicBool) {
        self.run_chunked(cancel, CHUNK);
    }

    /// チャンクの大きさを指定して実行する（テストで境界をまたがせるため）。
    pub fn run_chunked(&mut self, cancel: &AtomicBool, chunk: usize) {
        loop {
            if cancel.load(Ordering::Relaxed) {
                if *self.shared.status.lock().unwrap() == IndexStatus::Indexing {
                    self.set_status(IndexStatus::Cancelled);
                }
                return;
            }
            if !self.step(chunk) {
                return;
            }
        }
    }

    /// 最大 `chunk` バイトぶん進める。まだ続きがあれば `true`。
    pub fn step(&mut self, chunk: usize) -> bool {
        if *self.shared.status.lock().unwrap() != IndexStatus::Indexing {
            return false;
        }
        match self.shared.kind {
            UnitKind::Byte => self.step_with::<Byte>(chunk),
            UnitKind::Utf16Le => self.step_with::<Utf16Le>(chunk),
            UnitKind::Utf16Be => self.step_with::<Utf16Be>(chunk),
        }
    }

    fn set_status(&self, status: IndexStatus) {
        *self.shared.status.lock().unwrap() = status;
    }

    fn step_with<U: Unit>(&mut self, chunk: usize) -> bool {
        let shared = self.shared.clone();
        let bytes = shared.bytes();
        let end = aligned_end::<U>(bytes);
        let chunk = if std::mem::take(&mut self.first_step) {
            chunk.min(FIRST_CHUNK)
        } else {
            chunk
        };
        let chunk = chunk.max(U::WIDTH) / U::WIDTH * U::WIDTH;

        if self.pos >= end {
            if let Some(record) = self.scanner.finish(end) {
                self.batch.push(record);
            }
            self.publish(end);
            self.set_status(IndexStatus::Done);
            return false;
        }
        let to = (self.pos + chunk).min(end);

        if !shared.encoding.lock().unwrap().locked {
            if let Some(offset) = find_first_non_ascii(&bytes[self.pos..to]) {
                let encoding = classify_from(bytes, self.pos + offset);
                *shared.encoding.lock().unwrap() = EncodingState {
                    encoding,
                    locked: true,
                };
                if encoding == encoding_rs::ISO_2022_JP {
                    self.set_status(IndexStatus::Unsupported(encoding));
                    return false;
                }
            }
        }

        let batch = &mut self.batch;
        self.pos = self.scanner.feed::<U>(bytes, self.pos, to, &mut |record| {
            batch.push(record);
            std::ops::ControlFlow::Continue(())
        });
        self.publish(self.pos);
        true
    }

    /// 溜めたレコードを索引へ追加する。ロックはチャンクごとに1回だけ取る。
    fn publish(&mut self, scanned: usize) {
        {
            let mut index = self.shared.index.lock().unwrap();
            for record in self.batch.drain(..) {
                index.push(record);
            }
        }
        self.shared.scanned.store(scanned, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csv_data::CsvTable;

    fn open_indexed(bytes: &[u8], chunk: usize) -> LazyTable {
        let (table, mut job) = LazyTable::open(Arc::new(bytes.to_vec()), None);
        job.run_chunked(&AtomicBool::new(false), chunk);
        table
    }

    /// 全読み込みと、行数・列数・文字コード・全セル（範囲外を含む）が一致すること。
    fn assert_same_as_eager(bytes: &[u8], chunk: usize, label: &str) {
        let eager = CsvTable::from_bytes(bytes).unwrap();
        let lazy = open_indexed(bytes, chunk);
        assert_eq!(lazy.status(), IndexStatus::Done, "{label}");
        assert_eq!(lazy.encoding(), eager.encoding, "{label}: encoding");
        assert_eq!(lazy.row_count(), eager.row_count(), "{label}: row_count");
        assert_eq!(lazy.column_count(), eager.column_count(), "{label}: column_count");
        let cols = eager.column_count() + 1;
        for c in 0..cols {
            assert_eq!(lazy.header(c), eager.header(c), "{label}: header {c}");
        }
        for r in 0..eager.row_count() + 1 {
            for c in 0..cols {
                assert_eq!(lazy.cell(r, c), eager.cell(r, c), "{label}: cell ({r},{c})");
            }
        }
    }

    fn with_bom(bom: &[u8], body: &[u8]) -> Vec<u8> {
        let mut v = bom.to_vec();
        v.extend_from_slice(body);
        v
    }

    fn utf16(text: &str, le: bool) -> Vec<u8> {
        let bom: &[u8] = if le { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] };
        let body: Vec<u8> = text
            .encode_utf16()
            .flat_map(|u| if le { u.to_le_bytes() } else { u.to_be_bytes() })
            .collect();
        with_bom(bom, &body)
    }

    #[test]
    fn matches_eager_on_the_sample_files() {
        for (name, bytes) in [
            ("utf8", &include_bytes!("../samples/utf8.csv")[..]),
            ("sjis", &include_bytes!("../samples/sjis.csv")[..]),
            ("wide", &include_bytes!("../samples/wide.csv")[..]),
        ] {
            for chunk in [8, 64, CHUNK] {
                assert_same_as_eager(bytes, chunk, &format!("{name} chunk={chunk}"));
            }
        }
    }

    #[test]
    fn matches_eager_for_each_encoding_and_bom() {
        let text = "名前,部署,備考\n山田太郎,営業部,\"東京,\n支社\"\n鈴木花子,開発部,新規\n\n佐藤,,\"\"\"引用\"\"\"\n";
        let inputs = [
            ("utf8", text.as_bytes().to_vec()),
            ("utf8 bom", with_bom(&[0xEF, 0xBB, 0xBF], text.as_bytes())),
            ("sjis", encoding_rs::SHIFT_JIS.encode(text).0.into_owned()),
            ("euc-jp", encoding_rs::EUC_JP.encode(text).0.into_owned()),
            ("utf16le", utf16(text, true)),
            ("utf16be", utf16(text, false)),
            ("empty", Vec::new()),
            ("bom only", vec![0xEF, 0xBB, 0xBF]),
            ("header only", b"a,b,c\n".to_vec()),
            ("no trailing newline", b"a,b\n1,2".to_vec()),
        ];
        for (name, bytes) in &inputs {
            for chunk in [2, 8, 100, CHUNK] {
                assert_same_as_eager(bytes, chunk, &format!("{name} chunk={chunk}"));
            }
        }
    }

    #[test]
    fn matches_eager_across_many_index_blocks() {
        let mut text = String::from("id,名前,メモ\n");
        for i in 0..(STRIDE * 6 + 5) {
            if i % 13 == 0 {
                text.push_str(&format!("{i},テスト{i},\"改\n行{i}\"\n"));
            } else {
                text.push_str(&format!("{i},テスト{i},memo {i}\n"));
            }
        }
        for (name, bytes) in [
            ("utf8", text.as_bytes().to_vec()),
            ("sjis", encoding_rs::SHIFT_JIS.encode(&text).0.into_owned()),
            ("utf16le", utf16(&text, true)),
        ] {
            assert_same_as_eager(&bytes, 1000, name);
        }
    }

    #[test]
    fn detects_the_encoding_late_when_the_first_non_ascii_is_far_in() {
        let mut text = String::from("id,name\n");
        for i in 0..400 {
            text.push_str(&format!("{i},user{i}\n"));
        }
        text.push_str("9999,山田太郎,東京支社に所属しています\n");
        text.push_str("10000,鈴木花子,新規プロジェクトの担当です\n");
        let bytes = encoding_rs::SHIFT_JIS.encode(&text).0.into_owned();

        let (table, mut job) = LazyTable::open(Arc::new(bytes.clone()), None);
        // 最初のチャンクにはASCIIしかないので、まだ判定されない。
        assert_eq!(table.encoding(), encoding_rs::UTF_8);
        job.run_chunked(&AtomicBool::new(false), 64);
        assert_eq!(table.encoding(), encoding_rs::SHIFT_JIS);
        assert_eq!(table.cell(table.row_count() - 1, 1), "鈴木花子");
        assert_same_as_eager(&bytes, 64, "late detection");
    }

    #[test]
    fn rows_are_readable_while_indexing_is_in_progress() {
        let mut text = String::from("id\n");
        for i in 0..1000 {
            text.push_str(&format!("{i}\n"));
        }
        let bytes = text.into_bytes();
        let (table, mut job) = LazyTable::open(Arc::new(bytes), None);
        assert_eq!(table.row_count(), 0);

        // 少しずつ進めて、その時点で見える行が正しく読めることを確かめる。
        let mut last = 0;
        for _ in 0..100 {
            let more = job.step(200);
            let rows = table.row_count();
            assert!(rows >= last, "row count must not shrink");
            last = rows;
            if rows > 0 {
                assert_eq!(table.cell(rows - 1, 0), (rows - 1).to_string());
            }
            if !more {
                break;
            }
        }
        assert_eq!(table.status(), IndexStatus::Done);
        assert_eq!(table.row_count(), 1000);
        let (scanned, total) = table.progress();
        assert_eq!(scanned, total);
    }

    #[test]
    fn cancel_stops_indexing_and_keeps_what_was_indexed() {
        let text: String = (0..500).map(|i| format!("{i}\n")).collect();
        let (table, mut job) = LazyTable::open(Arc::new(text.into_bytes()), None);
        let cancel = AtomicBool::new(true);
        job.run_chunked(&cancel, 16);
        assert_eq!(table.status(), IndexStatus::Cancelled);
        assert!(table.row_count() < 500);
    }

    #[test]
    fn iso_2022_jp_is_reported_as_unsupported() {
        let mut bytes = b"a,b\n1,".to_vec();
        bytes.extend_from_slice(&[0x1B, 0x24, 0x42, 0x46, 0x7C, 0x4B, 0x5C, 0x38, 0x6C, 0x1B, 0x28, 0x42]);
        bytes.push(b'\n');
        let (table, mut job) = LazyTable::open(Arc::new(bytes.clone()), None);
        job.run(&AtomicBool::new(false));
        assert_eq!(
            table.status(),
            IndexStatus::Unsupported(encoding_rs::ISO_2022_JP)
        );
        // 全読み込みなら開ける。
        assert_eq!(CsvTable::from_bytes(&bytes).unwrap().cell(0, 1), "日本語");
    }

    #[test]
    fn forced_encoding_is_used_without_detection() {
        let text = "名前,部署\n山田太郎,営業部\n鈴木花子,開発部\n";
        let sjis = encoding_rs::SHIFT_JIS.encode(text).0.into_owned();
        let (table, mut job) = LazyTable::open(Arc::new(sjis), Some(encoding_rs::SHIFT_JIS));
        assert_eq!(table.encoding(), encoding_rs::SHIFT_JIS);
        job.run(&AtomicBool::new(false));
        assert_eq!(table.cell(0, 0), "山田太郎");
    }

    #[test]
    fn out_of_range_cells_are_empty_and_borrowed() {
        let table = open_indexed(b"a,b\n1,2\n", 8);
        assert!(matches!(table.cell(0, 9), Cow::Borrowed("")));
        assert!(matches!(table.cell(9, 0), Cow::Borrowed("")));
        assert!(matches!(table.header(9), Cow::Borrowed("")));
    }

    /// 乱数の入力で、全読み込みと全セルが一致することを確かめる。
    #[test]
    fn matches_eager_on_random_inputs() {
        const TOKENS: [&str; 14] = [
            "a", "b", "1", ",", ",", " ", "\"", "\"", "\"\"", "\n", "\n", "\r", "\r\n", "あ漢",
        ];
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..1500 {
            let len = if round % 60 == 0 { 700 } else { (next() % 40) as usize };
            let text: String = (0..len)
                .map(|_| TOKENS[(next() % TOKENS.len() as u64) as usize])
                .collect();
            let chunk = 2 + 2 * (next() % 40) as usize;
            assert_same_as_eager(text.as_bytes(), chunk, &format!("utf8 {text:?}"));
            let sjis = encoding_rs::SHIFT_JIS.encode(&text).0.into_owned();
            assert_same_as_eager(&sjis, chunk, &format!("sjis {text:?}"));
            assert_same_as_eager(&utf16(&text, true), chunk, &format!("utf16le {text:?}"));
        }
    }

    #[test]
    fn the_first_step_is_small_and_later_steps_use_the_given_chunk() {
        let text: String = (0..60_000).map(|i| format!("{i},row{i}\n")).collect();
        assert!(text.len() > FIRST_CHUNK * 2);
        let (table, mut job) = LazyTable::open(Arc::new(text.into_bytes()), None);

        assert!(job.step(CHUNK));
        assert_eq!(table.progress().0, FIRST_CHUNK, "first step is capped");
        assert!(table.row_count() > 0, "rows are readable right after the first step");

        assert!(job.step(CHUNK));
        assert!(table.progress().0 > FIRST_CHUNK * 2, "later steps use the full chunk");
    }
}

