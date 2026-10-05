//! CSV のレコード（行）の境界を、デコードせずに生バイトのまま探す。
//!
//! 遅延読み込みの土台。規則は `csv` クレート（既定設定、`flexible`）に合わせてある。
//! - 区切りはカンマ、引用符は `"`（引用符内の `""` は引用符1文字）
//! - 引用符で始まるフィールドだけが引用符フィールドになる。フィールド途中の `"` は普通の文字
//! - レコードの終端は `\n`、`\r`、`\r\n`（引用符の外のみ）。空行はレコードにならない
//!   （`\r\n` は「`\r` で終端、続く `\n` は空行」と読んでも結果が同じなので、特別扱いしない）
//!
//! 文字は「ユニット」単位で見る。UTF-8・Shift_JIS・EUC-JP などは1バイト、UTF-16 は2バイト。
//! これらは改行・引用符・カンマが、他の文字の一部として現れない。
//!
//! 全レコードの先頭位置は持たず、`STRIDE` レコードごとに1つだけ持つ（疎な索引）。
//! 任意のレコードは、手前のブロックの先頭から最大 `STRIDE - 1` レコード走査して見つける。

use std::ops::{ControlFlow, Range};

const QUOTE: u32 = b'"' as u32;
const DELIM: u32 = b',' as u32;
const LF: u32 = b'\n' as u32;
const CR: u32 = b'\r' as u32;

/// 疎な索引の間隔（レコード数）。
pub const STRIDE: usize = 64;

pub trait Unit {
    const WIDTH: usize;
    /// `pos` から1ユニット読む。`pos + WIDTH <= bytes.len()` が前提。
    fn read(bytes: &[u8], pos: usize) -> u32;

    /// `from..to` で最初に現れる引用符の位置。
    fn find_quote(bytes: &[u8], from: usize, to: usize) -> Option<usize> {
        Self::find_where(bytes, from, to, |c| c == QUOTE)
    }

    /// `from..to` で最初に現れる、区切りか改行（`,` `\n` `\r`）の位置。
    fn find_separator(bytes: &[u8], from: usize, to: usize) -> Option<usize> {
        Self::find_where(bytes, from, to, |c| c == DELIM || c == LF || c == CR)
    }

    /// 1ユニットずつ調べる素朴な実装。1バイト単位の `Byte` は、これを SIMD に置き換える。
    fn find_where(bytes: &[u8], from: usize, to: usize, found: impl Fn(u32) -> bool) -> Option<usize> {
        let mut pos = from;
        while pos + Self::WIDTH <= to {
            if found(Self::read(bytes, pos)) {
                return Some(pos);
            }
            pos += Self::WIDTH;
        }
        None
    }
}

pub struct Byte;
pub struct Utf16Le;
pub struct Utf16Be;

impl Unit for Byte {
    const WIDTH: usize = 1;
    #[inline(always)]
    fn read(bytes: &[u8], pos: usize) -> u32 {
        bytes[pos] as u32
    }

    // memchr は、実行時に CPU の機能を調べて、使える最速の命令（AVX2、SSE2、NEON など）を選び、
    // どれも使えなければ移植可能な実装に戻る。特定の CPU は要らない。
    fn find_quote(bytes: &[u8], from: usize, to: usize) -> Option<usize> {
        memchr::memchr(b'"', &bytes[from..to]).map(|i| from + i)
    }

    fn find_separator(bytes: &[u8], from: usize, to: usize) -> Option<usize> {
        memchr::memchr3(b',', b'\n', b'\r', &bytes[from..to]).map(|i| from + i)
    }
}

impl Unit for Utf16Le {
    const WIDTH: usize = 2;
    #[inline(always)]
    fn read(bytes: &[u8], pos: usize) -> u32 {
        u16::from_le_bytes([bytes[pos], bytes[pos + 1]]) as u32
    }
}

impl Unit for Utf16Be {
    const WIDTH: usize = 2;
    #[inline(always)]
    fn read(bytes: &[u8], pos: usize) -> u32 {
        u16::from_be_bytes([bytes[pos], bytes[pos + 1]]) as u32
    }
}

/// 1レコードの位置。`end` は終端記号を含まない。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record {
    pub start: usize,
    pub end: usize,
    pub fields: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    StartField,
    InField,
    InQuoted,
    /// 引用符の中で `"` を読んだ直後。次が `"` なら引用符1文字、それ以外なら引用符の終わり。
    QuoteInQuoted,
}

/// 途中で止めて再開できる状態機械。位置は常にバイト単位の絶対位置。
pub struct Scanner {
    state: State,
    delims: usize,
    has_content: bool,
    start: usize,
}

impl Scanner {
    /// レコードの先頭（直前の終端記号の直後、または本文の先頭）から始める。
    pub fn new(start: usize) -> Self {
        Self {
            state: State::StartField,
            delims: 0,
            has_content: false,
            start,
        }
    }

    fn reset(&mut self) {
        self.state = State::StartField;
        self.delims = 0;
        self.has_content = false;
    }

    /// `bytes[from..to]` を処理し、レコードが終わるたびに `on_record` を呼ぶ。
    /// `on_record` が `Break` を返したら、そのレコードの終端の直後で止める。
    /// 戻り値は、処理を終えた位置（再開時の `from`）。
    pub fn feed<U: Unit>(
        &mut self,
        bytes: &[u8],
        from: usize,
        to: usize,
        on_record: &mut impl FnMut(Record) -> ControlFlow<()>,
    ) -> usize {
        let mut pos = from;
        while pos + U::WIDTH <= to {
            // 引用符の中と、フィールドの途中は、次に意味を持つ文字まで一気に読み飛ばす。
            // フィールドの途中の引用符は、ただの文字なので、探さなくてよい。
            match self.state {
                State::InQuoted => match U::find_quote(bytes, pos, to) {
                    Some(found) => {
                        self.state = State::QuoteInQuoted;
                        pos = found + U::WIDTH;
                        continue;
                    }
                    None => {
                        pos = to - (to - pos) % U::WIDTH;
                        break;
                    }
                },
                State::InField => match U::find_separator(bytes, pos, to) {
                    Some(found) => pos = found,
                    None => {
                        pos = to - (to - pos) % U::WIDTH;
                        break;
                    }
                },
                State::StartField | State::QuoteInQuoted => {}
            }
            let c = U::read(bytes, pos);
            let next = pos + U::WIDTH;
            match c {
                LF | CR => {
                    let record = self.has_content.then(|| Record {
                        start: self.start,
                        end: pos,
                        fields: self.delims + 1,
                    });
                    self.reset();
                    self.start = next;
                    pos = next;
                    if let Some(record) = record {
                        if on_record(record).is_break() {
                            return pos;
                        }
                    }
                }
                DELIM => {
                    self.delims += 1;
                    self.state = State::StartField;
                    self.has_content = true;
                    pos = next;
                }
                QUOTE => {
                    if self.state != State::InField {
                        self.state = State::InQuoted;
                    }
                    self.has_content = true;
                    pos = next;
                }
                _ => {
                    self.state = State::InField;
                    self.has_content = true;
                    pos = next;
                }
            }
        }
        pos
    }

    /// 入力の終わりに、終端記号のない最後のレコードを取り出す。
    pub fn finish(&mut self, end: usize) -> Option<Record> {
        let record = self.has_content.then(|| Record {
            start: self.start,
            end,
            fields: self.delims + 1,
        });
        self.reset();
        record
    }
}

/// 疎なレコード索引。
#[derive(Default)]
pub struct RecordIndex {
    /// `block_starts[i]` は、レコード `i * STRIDE` の先頭位置。
    block_starts: Vec<u64>,
    record_count: usize,
    max_fields: usize,
}

impl RecordIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// レコードを順番どおりに追加する。
    pub fn push(&mut self, record: Record) {
        if self.record_count % STRIDE == 0 {
            self.block_starts.push(record.start as u64);
        }
        self.record_count += 1;
        self.max_fields = self.max_fields.max(record.fields);
    }

    pub fn record_count(&self) -> usize {
        self.record_count
    }

    /// 全レコードの中で最も多いフィールド数。
    pub fn max_fields(&self) -> usize {
        self.max_fields
    }

    /// 位置 `offset` を含む（その位置を過ぎてから始まる最初の）レコードがあるブロック。
    /// 戻り値は（ブロックの先頭レコードの番号、ブロックの先頭の位置）。索引が空なら `None`。
    pub fn block_containing(&self, offset: usize) -> Option<(usize, usize)> {
        if self.block_starts.is_empty() {
            return None;
        }
        let block = self
            .block_starts
            .partition_point(|&start| start as usize <= offset)
            .saturating_sub(1);
        Some((block * STRIDE, self.block_starts[block] as usize))
    }

    /// レコード `record` を含むブロックの先頭位置。レコード数の範囲外なら `None`。
    pub fn block_start(&self, record: usize) -> Option<usize> {
        (record < self.record_count).then(|| self.block_starts[record / STRIDE] as usize)
    }
}

/// `bytes[body_start..]` 全体を走査して索引を作る（テストで `csv` と突き合わせるための基準）。
#[cfg(test)]
pub fn build<U: Unit>(bytes: &[u8], body_start: usize) -> RecordIndex {
    let end = aligned_end::<U>(bytes);
    let mut index = RecordIndex::new();
    let mut scanner = Scanner::new(body_start);
    scanner.feed::<U>(bytes, body_start, end, &mut |record| {
        index.push(record);
        ControlFlow::Continue(())
    });
    if let Some(record) = scanner.finish(end) {
        index.push(record);
    }
    index
}

/// レコード `record`（0始まり、ヘッダーも1レコード）の `bytes` 上の範囲。
#[cfg(test)]
pub fn locate<U: Unit>(bytes: &[u8], index: &RecordIndex, record: usize) -> Option<Range<usize>> {
    let start = index.block_start(record)?;
    locate_in_block::<U>(bytes, start, record % STRIDE)
}

/// ブロックの先頭 `start` から走査して、`to_skip` 個先のレコードの範囲を返す。
/// 索引のロックを持たずに走査できるよう、`locate` から分けてある。
pub fn locate_in_block<U: Unit>(
    bytes: &[u8],
    start: usize,
    to_skip: usize,
) -> Option<Range<usize>> {
    let end = aligned_end::<U>(bytes);
    let mut seen = 0;
    let mut found = None;
    let mut scanner = Scanner::new(start);
    scanner.feed::<U>(bytes, start, end, &mut |r| {
        if seen == to_skip {
            found = Some(r.start..r.end);
            ControlFlow::Break(())
        } else {
            seen += 1;
            ControlFlow::Continue(())
        }
    });
    if found.is_none() && seen == to_skip {
        found = scanner.finish(end).map(|r| r.start..r.end);
    }
    found
}

/// ブロックの先頭 `block_start`（先頭レコードの番号が `first_record`）から走査して、
/// 終端が `offset` より後ろにある最初のレコードを返す。`offset` が、レコードの内側にあっても、
/// レコードの間（終端記号や空行）にあっても、その位置以降の最初のレコードになる。
/// 戻り値は（レコードの番号、バイト範囲）。
pub fn record_containing<U: Unit>(
    bytes: &[u8],
    first_record: usize,
    block_start: usize,
    offset: usize,
) -> Option<(usize, Range<usize>)> {
    let end = aligned_end::<U>(bytes);
    let mut skipped = 0;
    let mut found = None;
    let mut scanner = Scanner::new(block_start);
    scanner.feed::<U>(bytes, block_start, end, &mut |r| {
        if r.end > offset {
            found = Some((first_record + skipped, r.start..r.end));
            ControlFlow::Break(())
        } else {
            skipped += 1;
            ControlFlow::Continue(())
        }
    });
    if found.is_none() {
        if let Some(r) = scanner.finish(end) {
            if r.end > offset {
                found = Some((first_record + skipped, r.start..r.end));
            }
        }
    }
    found
}

/// 末尾の中途半端なユニット（UTF-16 の奇数バイト）を除いた終端位置。
pub fn aligned_end<U: Unit>(bytes: &[u8]) -> usize {
    bytes.len() - bytes.len() % U::WIDTH
}

#[cfg(test)]
mod tests {
    use super::*;
    use encoding_rs::Encoding;

    /// `csv` クレートによる基準の結果。
    fn reference(text: &str) -> Vec<Vec<String>> {
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(text.as_bytes());
        reader
            .records()
            .map(|r| r.unwrap().iter().map(str::to_owned).collect())
            .collect()
    }

    /// 索引と `locate` で切り出したレコードを、デコードして `csv` で読んだ結果。
    fn via_index<U: Unit>(
        bytes: &[u8],
        encoding: &'static Encoding,
    ) -> (Vec<Vec<String>>, RecordIndex) {
        let index = build::<U>(bytes, 0);
        let rows = (0..index.record_count())
            .map(|i| {
                let range = locate::<U>(bytes, &index, i).expect("record must be locatable");
                let (text, _) = encoding.decode_without_bom_handling(&bytes[range]);
                let mut reader = csv::ReaderBuilder::new()
                    .has_headers(false)
                    .flexible(true)
                    .from_reader(text.as_bytes());
                let mut records = reader.records();
                let record = records.next().expect("a record must contain a record");
                assert!(records.next().is_none(), "a record must not split further");
                record.unwrap().iter().map(str::to_owned).collect()
            })
            .collect();
        (rows, index)
    }

    fn check_all(text: &str) {
        let expected = reference(text);
        let max_fields = expected.iter().map(Vec::len).max().unwrap_or(0);

        let (rows, index) = via_index::<Byte>(text.as_bytes(), encoding_rs::UTF_8);
        assert_eq!(rows, expected, "utf-8: {text:?}");
        assert_eq!(index.max_fields(), max_fields, "utf-8 max_fields: {text:?}");

        let sjis = encoding_rs::SHIFT_JIS.encode(text).0.into_owned();
        let (rows, _) = via_index::<Byte>(&sjis, encoding_rs::SHIFT_JIS);
        assert_eq!(rows, expected, "shift_jis: {text:?}");

        let le: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let (rows, index) = via_index::<Utf16Le>(&le, encoding_rs::UTF_16LE);
        assert_eq!(rows, expected, "utf-16le: {text:?}");
        assert_eq!(index.max_fields(), max_fields, "utf-16le max_fields: {text:?}");

        let be: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        let (rows, _) = via_index::<Utf16Be>(&be, encoding_rs::UTF_16BE);
        assert_eq!(rows, expected, "utf-16be: {text:?}");
    }

    #[test]
    fn matches_csv_on_handpicked_inputs() {
        for text in [
            "",
            "\n",
            "\r\n\r\n",
            "a",
            "a\n",
            "a,b\n1,2\n",
            "a,b\r\n1,2\r\n",
            "a,b\r1,2\r",
            "a,b\n\n\n1,2\n\n",
            "a,\n,b\n,\n",
            ",",
            "\"x,y\",\"line1\nline2\"\n\"say \"\"hi\"\"\",z\n",
            "\"unterminated\nstill inside",
            "\"\"\n",
            "\"\",\"\"\n",
            "ab\"cd,e\n",
            "\"abc\"def,g\n",
            "\"abc\" ,g\n",
            "a,\"b\r\nc\",d\r\ne\n",
            "名前,部署\n山田,営業\n",
            "\"名\n前\",あ\n",
            "x\r\r\ny\n",
            "x\n\ry\n",
            " \n  \n",
        ] {
            check_all(text);
        }
    }

    #[test]
    fn records_cross_many_stride_blocks() {
        let mut text = String::from("id,memo\n");
        for i in 0..(STRIDE * 5 + 7) {
            // 引用符内の改行を混ぜて、行数とバイト数がずれるようにする。
            if i % 11 == 0 {
                text.push_str(&format!("{i},\"a\nb\"\n"));
            } else {
                text.push_str(&format!("{i},m{i}\n"));
            }
        }
        check_all(&text);
        let (_, index) = via_index::<Byte>(text.as_bytes(), encoding_rs::UTF_8);
        assert_eq!(index.record_count(), STRIDE * 5 + 8);
    }

    #[test]
    fn locate_out_of_range_is_none() {
        let index = build::<Byte>(b"a\nb\n", 0);
        assert_eq!(index.record_count(), 2);
        assert!(locate::<Byte>(b"a\nb\n", &index, 2).is_none());
        let empty = build::<Byte>(b"", 0);
        assert_eq!(empty.record_count(), 0);
        assert!(locate::<Byte>(b"", &empty, 0).is_none());
    }

    #[test]
    fn utf16_ignores_a_dangling_odd_byte() {
        let mut le: Vec<u8> = "a,b\n1,2\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
        le.push(0x41);
        let index = build::<Utf16Le>(&le, 0);
        assert_eq!(index.record_count(), 2);
    }

    #[test]
    fn index_can_start_after_a_bom() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"a,b\n1,2\n");
        let index = build::<Byte>(&bytes, 3);
        assert_eq!(index.record_count(), 2);
        assert_eq!(locate::<Byte>(&bytes, &index, 0), Some(3..6));
    }

    /// 乱数の入力で `csv` クレートと突き合わせる。失敗時は入力を表示する。
    #[test]
    fn matches_csv_on_random_inputs() {
        const TOKENS: [&str; 14] = [
            "a", "b", "1", ",", ",", " ", "\"", "\"", "\"\"", "\n", "\n", "\r", "\r\n", "あ漢",
        ];
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..6000 {
            // たまに長い入力を混ぜて、疎な索引のブロックをまたがせる。
            let len = if round % 50 == 0 { 900 } else { (next() % 40) as usize };
            let text: String = (0..len)
                .map(|_| TOKENS[(next() % TOKENS.len() as u64) as usize])
                .collect();
            check_all(&text);
        }
    }
}
