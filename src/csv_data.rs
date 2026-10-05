use std::path::Path;

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;

/// 文字コード判定に `chardetng` へ渡す最大バイト数。全体を見ると大きいファイルで数百msかかる。
const DETECT_WINDOW: usize = 64 * 1024;
/// 最初の非ASCIIバイトを探す範囲の上限。HDDでも体感に出ない量（シーケンシャル読みで約10ms）に留める。
/// 全読み込みとmmapで同じ判定結果になるよう、読み込み方式に関わらずこの範囲だけを見る。
const DETECT_SCAN_LIMIT: usize = 1024 * 1024;
const ESC: u8 = 0x1B;

pub struct CsvTable {
    pub encoding: &'static Encoding,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl CsvTable {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Self::from_bytes(&std::fs::read(path)?)
    }

    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        let (encoding, body) = detect_encoding(bytes);
        let (text, _, _) = encoding.decode(body);

        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .flexible(true)
            .from_reader(text.as_bytes());

        let mut records = reader
            .records()
            .map(|r| r.map(|r| r.iter().map(str::to_owned).collect::<Vec<_>>()));
        let headers = records.next().transpose()?.unwrap_or_default();
        let rows = records.collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            encoding,
            headers,
            rows,
        })
    }

    pub fn column_count(&self) -> usize {
        self.rows
            .iter()
            .map(Vec::len)
            .chain([self.headers.len()])
            .max()
            .unwrap_or(0)
    }
}

/// BOMがあればそれを優先し、なければ chardetng で推定する。BOMは本文から取り除く。
fn detect_encoding(bytes: &[u8]) -> (&'static Encoding, &[u8]) {
    if let Some((encoding, bom_len)) = Encoding::for_bom(bytes) {
        return (encoding, &bytes[bom_len..]);
    }
    // 判定に効くのは非ASCIIの部分だけなので、最初の非ASCIIバイトから一定量だけ見る。
    // ISO-2022-JP は7bitのみでESCで文字集合を切り替えるため、ESCも目印にする。
    // 探す範囲には上限があり、その範囲に見つからなければASCII互換（UTF-8）とみなす。
    let scan = &bytes[..bytes.len().min(DETECT_SCAN_LIMIT)];
    let Some(start) = scan.iter().position(|&b| b >= 0x80 || b == ESC) else {
        return (encoding_rs::UTF_8, bytes);
    };
    let end = (start + DETECT_WINDOW).min(bytes.len());
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Allow);
    detector.feed(&bytes[start..end], end == bytes.len());
    (detector.guess(None, Utf8Detection::Allow), bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> CsvTable {
        CsvTable::from_bytes(bytes).unwrap()
    }

    fn encode(encoding: &'static Encoding, text: &str) -> Vec<u8> {
        let (bytes, _, had_errors) = encoding.encode(text);
        assert!(!had_errors, "test fixture is not representable in {}", encoding.name());
        bytes.into_owned()
    }

    #[test]
    fn parses_header_and_rows() {
        let t = parse(b"a,b\n1,2\n3,4\n");
        assert_eq!(t.headers, ["a", "b"]);
        assert_eq!(t.rows, [["1", "2"], ["3", "4"]]);
        assert_eq!(t.encoding, encoding_rs::UTF_8);
    }

    #[test]
    fn handles_crlf_and_missing_trailing_newline() {
        let t = parse(b"a,b\r\n1,2\r\n3,4");
        assert_eq!(t.rows, [["1", "2"], ["3", "4"]]);
    }

    #[test]
    fn keeps_quoted_comma_newline_and_quote() {
        let t = parse(b"a,b\n\"x,y\",\"line1\nline2\"\n\"say \"\"hi\"\"\",z\n");
        assert_eq!(t.rows[0], ["x,y", "line1\nline2"]);
        assert_eq!(t.rows[1], ["say \"hi\"", "z"]);
    }

    #[test]
    fn accepts_ragged_rows_and_counts_widest_column() {
        let t = parse(b"a,b\n1\n1,2,3,4\n");
        assert_eq!(t.rows[0], ["1"]);
        assert_eq!(t.rows[1].len(), 4);
        assert_eq!(t.column_count(), 4);
    }

    #[test]
    fn empty_input_has_no_headers_or_rows() {
        let t = parse(b"");
        assert!(t.headers.is_empty());
        assert!(t.rows.is_empty());
        assert_eq!(t.column_count(), 0);
    }

    #[test]
    fn header_only_input_has_no_rows() {
        let t = parse(b"a,b,c\n");
        assert_eq!(t.headers, ["a", "b", "c"]);
        assert!(t.rows.is_empty());
        assert_eq!(t.column_count(), 3);
    }

    #[test]
    fn strips_utf8_bom() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("名前,部署\n山田,営業\n".as_bytes());
        let t = parse(&bytes);
        assert_eq!(t.encoding, encoding_rs::UTF_8);
        assert_eq!(t.headers, ["名前", "部署"]);
    }

    #[test]
    fn decodes_utf16le_with_bom() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "名前,部署\n山田,営業\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let t = parse(&bytes);
        assert_eq!(t.encoding, encoding_rs::UTF_16LE);
        assert_eq!(t.rows, [["山田", "営業"]]);
    }

    #[test]
    fn detects_shift_jis() {
        let text = "id,名前,部署,備考\n\
                    1,山田太郎,営業部,東京支社に所属しています\n\
                    2,鈴木花子,開発部,新規プロジェクトの担当です\n\
                    3,佐藤健,人事部,採用活動を担当しています\n";
        let t = parse(&encode(encoding_rs::SHIFT_JIS, text));
        assert_eq!(t.encoding, encoding_rs::SHIFT_JIS);
        assert_eq!(t.headers, ["id", "名前", "部署", "備考"]);
        assert_eq!(t.rows[0][1], "山田太郎");
    }

    #[test]
    fn detects_euc_jp() {
        let text = "名前,部署,備考\n\
                    山田太郎,営業部,東京支社に所属しています\n\
                    鈴木花子,開発部,新規プロジェクトの担当です\n";
        let t = parse(&encode(encoding_rs::EUC_JP, text));
        assert_eq!(t.encoding, encoding_rs::EUC_JP);
        assert_eq!(t.rows[1][0], "鈴木花子");
    }

    #[test]
    fn ascii_only_input_is_not_misread() {
        let t = parse(b"id,name\n1,alice\n2,bob\n");
        assert_eq!(t.rows[1], ["2", "bob"]);
    }

    #[test]
    fn detects_short_shift_jis() {
        let t = parse(&encode(encoding_rs::SHIFT_JIS, "名前,部署\n山田,営業\n"));
        assert_eq!(t.encoding, encoding_rs::SHIFT_JIS);
        assert_eq!(t.rows, [["山田", "営業"]]);
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
        assert_eq!(t.rows.last().unwrap()[1], "鈴木花子");
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
        assert_eq!(t.rows.len(), n);
        assert_eq!(t.rows[n - 1][1], format!("テスト太郎{}", n - 1));
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
        assert_eq!(t.rows[0][1], "日本語");
    }

    #[test]
    fn gives_up_after_scan_limit_and_assumes_utf8() {
        // 上限までASCIIだけのファイルは、その先に日本語があってもUTF-8とみなす（既知の制約）。
        // HDDでも重くならない範囲で打ち切る設計上の割り切りで、文字コードは手動で切り替える想定。
        let mut text = String::from("id,name\n");
        let mut i = 0;
        while text.len() < DETECT_SCAN_LIMIT + 1024 {
            text.push_str(&format!("{i},user{i}\n"));
            i += 1;
        }
        text.push_str("9999,山田太郎\n");
        let t = parse(&encode(encoding_rs::SHIFT_JIS, &text));
        assert_eq!(t.encoding, encoding_rs::UTF_8);
    }

    #[test]
    fn detects_shift_jis_just_inside_scan_limit() {
        // 上限の内側に最初の日本語があれば、その後の量が多くても判定できる。
        let mut text = String::from("id,name\n");
        let mut i = 0;
        while text.len() < DETECT_SCAN_LIMIT - 4096 {
            text.push_str(&format!("{i},user{i}\n"));
            i += 1;
        }
        for k in 0..200 {
            text.push_str(&format!("{k},山田太郎,東京支社に所属しています\n"));
        }
        let t = parse(&encode(encoding_rs::SHIFT_JIS, &text));
        assert_eq!(t.encoding, encoding_rs::SHIFT_JIS);
    }
}
