use std::path::Path;

use anyhow::Context;
use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;

/// 文字コード判定に `chardetng` へ渡す最大バイト数。全体を見ると大きいファイルで数百msかかる。
const DETECT_WINDOW: usize = 64 * 1024;
const ESC: u8 = 0x1B;

/// デコード済みのCSVを、セル文字列を1本に連結した形で保持する。
///
/// セルごとに `String` を持つと、ファイルの7〜8倍のメモリと大量の小さな確保が必要になる。
/// ここでは次の3つだけを持ち、ファイルの1.3倍程度に収める。
/// - `text`: 全セルの文字列を連結したもの（引用符は外し、`""` はほどいた後の中身）
/// - `cell_ends`: 各セルの終端位置（`text` 内のバイト位置。始端は1つ前のセルの終端）
/// - `row_starts`: 各行の先頭セルが `cell_ends` の何番目か（末尾に番兵を1つ持つ）
///
/// 最初のレコードをヘッダーとして扱う。内部の配置は外に出さず、読み取りは
/// `cell` などの窓口だけを使う（編集を重ねる層を後から挟めるようにするため）。
pub struct CsvTable {
    pub encoding: &'static Encoding,
    text: String,
    cell_ends: Vec<u32>,
    /// 長さは「レコード数 + 1」。ヘッダーも1レコードとして含む。
    row_starts: Vec<u32>,
    column_count: usize,
}

impl CsvTable {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        Self::from_bytes_as(bytes, None)
    }

    /// `forced` を指定すると、文字コードの自動判定をせずその文字コードで読む（手動切り替え用）。
    pub fn from_bytes_as(
        bytes: &[u8],
        forced: Option<&'static Encoding>,
    ) -> anyhow::Result<Self> {
        let (encoding, body) = match forced {
            Some(encoding) => (encoding, strip_bom(bytes, encoding)),
            None => detect_encoding(bytes),
        };
        // UTF-8 として正しければ借用のまま（コピーしない）。
        let (decoded, _) = encoding.decode_without_bom_handling(body);

        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(decoded.as_bytes());

        // 連結後の文字列は、区切りや引用符を除くので入力より小さくなる。
        let mut text = String::with_capacity(decoded.len());
        let mut cell_ends: Vec<u32> = Vec::new();
        let mut row_starts: Vec<u32> = Vec::new();
        let mut column_count = 0;

        let mut record = csv::StringRecord::new();
        while reader.read_record(&mut record)? {
            row_starts.push(u32::try_from(cell_ends.len()).context("too many cells")?);
            column_count = column_count.max(record.len());
            for field in &record {
                text.push_str(field);
                cell_ends.push(u32::try_from(text.len()).context("file is too large")?);
            }
        }
        row_starts.push(u32::try_from(cell_ends.len()).context("too many cells")?);

        text.shrink_to_fit();
        cell_ends.shrink_to_fit();
        row_starts.shrink_to_fit();

        Ok(Self {
            encoding,
            text,
            cell_ends,
            row_starts,
            column_count,
        })
    }

    /// このテーブルがヒープに確保しているバイト数（容量ベース）。
    /// アプリ本体では使わず、計測用の `examples/bench_load.rs` から呼ぶ。
    #[allow(dead_code)]
    pub fn heap_bytes(&self) -> usize {
        self.text.capacity()
            + self.cell_ends.capacity() * size_of::<u32>()
            + self.row_starts.capacity() * size_of::<u32>()
    }

    /// ヘッダーを除いたデータ行の数。
    pub fn row_count(&self) -> usize {
        (self.row_starts.len() - 1).saturating_sub(1)
    }

    /// ヘッダーを含めた中で最も広い行の列数。
    pub fn column_count(&self) -> usize {
        self.column_count
    }

    pub fn header(&self, col: usize) -> &str {
        self.record_cell(0, col)
    }

    /// データ行 `row`（0始まり、ヘッダーを除く）の `col` 列目。範囲外は空文字列。
    pub fn cell(&self, row: usize, col: usize) -> &str {
        self.record_cell(row + 1, col)
    }

    fn record_cell(&self, record: usize, col: usize) -> &str {
        let (Some(&first), Some(&next_first)) =
            (self.row_starts.get(record), self.row_starts.get(record + 1))
        else {
            return "";
        };
        let ix = first as usize + col;
        if ix >= next_first as usize {
            return "";
        }
        let start = if ix == 0 { 0 } else { self.cell_ends[ix - 1] };
        &self.text[start as usize..self.cell_ends[ix] as usize]
    }
}

/// `encoding` と同じ文字コードのBOMが先頭にあれば取り除く。
fn strip_bom<'a>(bytes: &'a [u8], encoding: &'static Encoding) -> &'a [u8] {
    match Encoding::for_bom(bytes) {
        Some((bom_encoding, bom_len)) if bom_encoding == encoding => &bytes[bom_len..],
        _ => bytes,
    }
}

/// BOMがあればそれを優先し、なければ chardetng で推定する。BOMは本文から取り除く。
fn detect_encoding(bytes: &[u8]) -> (&'static Encoding, &[u8]) {
    if let Some((encoding, bom_len)) = Encoding::for_bom(bytes) {
        return (encoding, &bytes[bom_len..]);
    }
    match find_first_non_ascii(bytes) {
        None => (encoding_rs::UTF_8, bytes),
        Some(start) => (classify_from(bytes, start), bytes),
    }
}

/// 文字コードの判断材料になる最初のバイトの位置。
///
/// 判定に効くのは非ASCIIの部分だけで、ASCIIだけの区間はどの文字コードでも同じに読める。
/// ISO-2022-JP は7bitのみでESCにより文字集合を切り替えるため、ESCも目印にする。
/// 見つからなければ全体がASCII（UTF-8として読んでよい）。
pub fn find_first_non_ascii(bytes: &[u8]) -> Option<usize> {
    bytes.iter().position(|&b| b >= 0x80 || b == ESC)
}

/// `start`（`find_first_non_ascii` の結果）から最大 `DETECT_WINDOW` バイトを見て推定する。
pub fn classify_from(bytes: &[u8], start: usize) -> &'static Encoding {
    let end = (start + DETECT_WINDOW).min(bytes.len());
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Allow);
    detector.feed(&bytes[start..end], end == bytes.len());
    detector.guess(None, Utf8Detection::Allow)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> CsvTable {
        CsvTable::from_bytes(bytes).unwrap()
    }

    fn headers(t: &CsvTable) -> Vec<String> {
        (0..t.column_count()).map(|c| t.header(c).to_string()).collect()
    }

    /// 各行の実際のセル数は取れないため、列数ぶん読んで末尾の空セルを落とす。
    /// （テストの入力に末尾が空のセルを含めないこと）
    fn rows(t: &CsvTable) -> Vec<Vec<String>> {
        (0..t.row_count())
            .map(|r| {
                let mut row: Vec<String> =
                    (0..t.column_count()).map(|c| t.cell(r, c).to_string()).collect();
                while row.last().is_some_and(String::is_empty) {
                    row.pop();
                }
                row
            })
            .collect()
    }

    fn encode(encoding: &'static Encoding, text: &str) -> Vec<u8> {
        let (bytes, _, had_errors) = encoding.encode(text);
        assert!(!had_errors, "test fixture is not representable in {}", encoding.name());
        bytes.into_owned()
    }

    #[test]
    fn parses_header_and_rows() {
        let t = parse(b"a,b\n1,2\n3,4\n");
        assert_eq!(headers(&t), ["a", "b"]);
        assert_eq!(rows(&t), [["1", "2"], ["3", "4"]]);
        assert_eq!(t.encoding, encoding_rs::UTF_8);
    }

    #[test]
    fn handles_crlf_and_missing_trailing_newline() {
        let t = parse(b"a,b\r\n1,2\r\n3,4");
        assert_eq!(rows(&t), [["1", "2"], ["3", "4"]]);
    }

    #[test]
    fn keeps_quoted_comma_newline_and_quote() {
        let t = parse(b"a,b\n\"x,y\",\"line1\nline2\"\n\"say \"\"hi\"\"\",z\n");
        assert_eq!(rows(&t)[0], ["x,y", "line1\nline2"]);
        assert_eq!(rows(&t)[1], ["say \"hi\"", "z"]);
    }

    #[test]
    fn accepts_ragged_rows_and_counts_widest_column() {
        let t = parse(b"a,b\n1\n1,2,3,4\n");
        assert_eq!(rows(&t)[0], ["1"]);
        assert_eq!(rows(&t)[1].len(), 4);
        assert_eq!(t.column_count(), 4);
    }

    #[test]
    fn empty_input_has_no_headers_or_rows() {
        let t = parse(b"");
        assert!(headers(&t).is_empty());
        assert!(rows(&t).is_empty());
        assert_eq!(t.column_count(), 0);
    }

    #[test]
    fn header_only_input_has_no_rows() {
        let t = parse(b"a,b,c\n");
        assert_eq!(headers(&t), ["a", "b", "c"]);
        assert!(rows(&t).is_empty());
        assert_eq!(t.column_count(), 3);
    }

    #[test]
    fn strips_utf8_bom() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("名前,部署\n山田,営業\n".as_bytes());
        let t = parse(&bytes);
        assert_eq!(t.encoding, encoding_rs::UTF_8);
        assert_eq!(headers(&t), ["名前", "部署"]);
    }

    #[test]
    fn decodes_utf16le_with_bom() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "名前,部署\n山田,営業\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let t = parse(&bytes);
        assert_eq!(t.encoding, encoding_rs::UTF_16LE);
        assert_eq!(rows(&t), [["山田", "営業"]]);
    }

    #[test]
    fn detects_shift_jis() {
        let text = "id,名前,部署,備考\n\
                    1,山田太郎,営業部,東京支社に所属しています\n\
                    2,鈴木花子,開発部,新規プロジェクトの担当です\n\
                    3,佐藤健,人事部,採用活動を担当しています\n";
        let t = parse(&encode(encoding_rs::SHIFT_JIS, text));
        assert_eq!(t.encoding, encoding_rs::SHIFT_JIS);
        assert_eq!(headers(&t), ["id", "名前", "部署", "備考"]);
        assert_eq!(rows(&t)[0][1], "山田太郎");
    }

    #[test]
    fn detects_euc_jp() {
        let text = "名前,部署,備考\n\
                    山田太郎,営業部,東京支社に所属しています\n\
                    鈴木花子,開発部,新規プロジェクトの担当です\n";
        let t = parse(&encode(encoding_rs::EUC_JP, text));
        assert_eq!(t.encoding, encoding_rs::EUC_JP);
        assert_eq!(rows(&t)[1][0], "鈴木花子");
    }

    #[test]
    fn ascii_only_input_is_not_misread() {
        let t = parse(b"id,name\n1,alice\n2,bob\n");
        assert_eq!(rows(&t)[1], ["2", "bob"]);
    }

    #[test]
    fn detects_short_shift_jis() {
        let t = parse(&encode(encoding_rs::SHIFT_JIS, "名前,部署\n山田,営業\n"));
        assert_eq!(t.encoding, encoding_rs::SHIFT_JIS);
        assert_eq!(rows(&t), [["山田", "営業"]]);
    }

    #[test]
    fn detects_shift_jis_after_long_ascii_prefix() {
        // 判定窓より長いASCIIだけの区間のあとに日本語が現れても、取りこぼさない。
        let mut text = String::from("id,name,memo\n");
        let mut i = 0;
        while text.len() < DETECT_WINDOW * 2 {
            text.push_str(&format!("{i},user{i},plain ascii memo\n"));
            i += 1;
        }
        text.push_str("9999,山田太郎,東京支社に所属しています\n");
        text.push_str("10000,鈴木花子,新規プロジェクトの担当です\n");
        let t = parse(&encode(encoding_rs::SHIFT_JIS, &text));
        assert_eq!(t.encoding, encoding_rs::SHIFT_JIS);
        assert_eq!(rows(&t).last().unwrap()[1], "鈴木花子");
    }

    #[test]
    fn utf8_larger_than_window_decodes_every_row() {
        // 判定窓がマルチバイト文字の途中で切れても、全体のデコードには影響しない。
        let mut text = String::from("id,名前\n");
        let n = DETECT_WINDOW; // 窓を確実に超える行数
        for i in 0..n {
            text.push_str(&format!("{i},テスト太郎{i}\n"));
        }
        let t = parse(text.as_bytes());
        assert_eq!(t.encoding, encoding_rs::UTF_8);
        assert_eq!(rows(&t).len(), n);
        assert_eq!(rows(&t)[n - 1][1], format!("テスト太郎{}", n - 1));
    }

    #[test]
    fn detects_iso_2022_jp() {
        // 「日本語」を ISO-2022-JP で表した固定のバイト列。
        let mut bytes = b"a,b\n1,".to_vec();
        bytes.extend_from_slice(&[
            0x1B, 0x24, 0x42, 0x46, 0x7C, 0x4B, 0x5C, 0x38, 0x6C, 0x1B, 0x28, 0x42,
        ]);
        bytes.push(b'\n');
        let t = parse(&bytes);
        assert_eq!(t.encoding, encoding_rs::ISO_2022_JP);
        assert_eq!(rows(&t)[0][1], "日本語");
    }

    #[test]
    fn detects_shift_jis_after_very_long_ascii_prefix() {
        // 先頭に何MBもASCIIだけが続いても、その先の日本語を取りこぼさない。
        let mut text = String::from("id,name\n");
        let mut i = 0;
        while text.len() < 1536 * 1024 {
            text.push_str(&format!("{i},user{i}\n"));
            i += 1;
        }
        text.push_str("9999,山田太郎,東京支社に所属しています\n");
        text.push_str("10000,鈴木花子,新規プロジェクトの担当です\n");
        let t = parse(&encode(encoding_rs::SHIFT_JIS, &text));
        assert_eq!(t.encoding, encoding_rs::SHIFT_JIS);
        assert_eq!(t.cell(t.row_count() - 1, 1), "鈴木花子");
    }

    #[test]
    fn find_first_non_ascii_marks_high_bytes_and_esc() {
        assert_eq!(find_first_non_ascii(b""), None);
        assert_eq!(find_first_non_ascii(b"id,name\n1,alice\n"), None);
        assert_eq!(find_first_non_ascii(b"ab\x80cd"), Some(2));
        assert_eq!(find_first_non_ascii(b"abc\x1b$B"), Some(3));
        assert_eq!(find_first_non_ascii("ab名".as_bytes()), Some(2));
    }

    #[test]
    fn forced_encoding_overrides_detection() {
        let text = "名前,部署\n山田太郎,営業部\n鈴木花子,開発部\n";
        let sjis = encode(encoding_rs::SHIFT_JIS, text);
        let t = CsvTable::from_bytes_as(&sjis, Some(encoding_rs::SHIFT_JIS)).unwrap();
        assert_eq!(t.encoding, encoding_rs::SHIFT_JIS);
        assert_eq!(t.cell(0, 0), "山田太郎");
        // 間違った文字コードを指定しても、クラッシュせず文字化けとして読める。
        let wrong = CsvTable::from_bytes_as(&sjis, Some(encoding_rs::UTF_8)).unwrap();
        assert_eq!(wrong.encoding, encoding_rs::UTF_8);
        assert_ne!(wrong.cell(0, 0), "山田太郎");
    }

    #[test]
    fn forced_encoding_strips_matching_bom_only() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("名前\n山田\n".as_bytes());
        let t = CsvTable::from_bytes_as(&bytes, Some(encoding_rs::UTF_8)).unwrap();
        assert_eq!(t.header(0), "名前");
    }

    #[test]
    fn cell_access_is_exact_for_ragged_and_empty_cells() {
        let t = parse(b"h1,h2,h3\na,,c\n,,\nx\n,y\n");
        assert_eq!(t.row_count(), 4);
        assert_eq!(t.column_count(), 3);
        assert_eq!((t.cell(0, 0), t.cell(0, 1), t.cell(0, 2)), ("a", "", "c"));
        assert_eq!((t.cell(1, 0), t.cell(1, 1), t.cell(1, 2)), ("", "", ""));
        assert_eq!((t.cell(2, 0), t.cell(2, 1), t.cell(2, 2)), ("x", "", ""));
        assert_eq!((t.cell(3, 0), t.cell(3, 1), t.cell(3, 2)), ("", "y", ""));
    }

    #[test]
    fn out_of_range_access_returns_empty() {
        let t = parse(b"a,b\n1,2\n");
        assert_eq!(t.cell(0, 2), "");
        assert_eq!(t.cell(1, 0), "");
        assert_eq!(t.cell(99, 99), "");
        assert_eq!(t.header(5), "");
        assert_eq!(parse(b"").header(0), "");
        assert_eq!(parse(b"").cell(0, 0), "");
    }

    #[test]
    fn row_count_excludes_header() {
        assert_eq!(parse(b"").row_count(), 0);
        assert_eq!(parse(b"a,b\n").row_count(), 0);
        assert_eq!(parse(b"a,b\n1,2\n").row_count(), 1);
    }

    #[test]
    fn many_rows_keep_every_cell_at_the_right_place() {
        let mut csv = String::from("id,text,empty,last\n");
        let n = 20_000;
        for i in 0..n {
            // 長さの違うセルと空セルを混ぜて、オフセットのずれを検出しやすくする。
            csv.push_str(&format!("{i},{},,end{i}\n", "x".repeat(i % 7)));
        }
        let t = parse(csv.as_bytes());
        assert_eq!(t.row_count(), n);
        for i in (0..n).step_by(97).chain([0, n - 1]) {
            assert_eq!(t.cell(i, 0), i.to_string());
            assert_eq!(t.cell(i, 1), "x".repeat(i % 7));
            assert_eq!(t.cell(i, 2), "");
            assert_eq!(t.cell(i, 3), format!("end{i}"));
        }
    }

    #[test]
    fn stores_unescaped_text_for_quoted_cells() {
        let t = parse(b"a\n\"say \"\"hi\"\", ok\"\n");
        assert_eq!(t.cell(0, 0), "say \"hi\", ok");
    }
}
