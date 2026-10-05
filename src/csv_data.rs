use std::path::Path;

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;

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
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Allow);
    detector.feed(bytes, true);
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
}
