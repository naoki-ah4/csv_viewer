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
        let bytes = std::fs::read(path)?;
        let (encoding, body) = detect_encoding(&bytes);
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
