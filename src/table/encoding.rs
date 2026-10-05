//! 文字コードの自動判定。全読み込み（`eager`）と遅延読み込み（`lazy`）の、どちらも使う。
//!
//! BOMがあればそれを優先し、なければ、最初の非ASCIIのバイトから一定量だけを `chardetng` で推定する。

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::Encoding;

/// 文字コード判定に `chardetng` へ渡す最大バイト数。全体を見ると大きいファイルで数百msかかる。
pub(super) const DETECT_WINDOW: usize = 64 * 1024;
const ESC: u8 = 0x1B;

/// `encoding` と同じ文字コードのBOMが先頭にあれば取り除く。
pub(super) fn strip_bom<'a>(bytes: &'a [u8], encoding: &'static Encoding) -> &'a [u8] {
    match Encoding::for_bom(bytes) {
        Some((bom_encoding, bom_len)) if bom_encoding == encoding => &bytes[bom_len..],
        _ => bytes,
    }
}

/// BOMがあればそれを優先し、なければ chardetng で推定する。BOMは本文から取り除く。
pub(super) fn detect_encoding(bytes: &[u8]) -> (&'static Encoding, &[u8]) {
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
pub(super) fn find_first_non_ascii(bytes: &[u8]) -> Option<usize> {
    bytes.iter().position(|&b| b >= 0x80 || b == ESC)
}

/// `start`（`find_first_non_ascii` の結果）から最大 `DETECT_WINDOW` バイトを見て推定する。
pub(super) fn classify_from(bytes: &[u8], start: usize) -> &'static Encoding {
    let end = (start + DETECT_WINDOW).min(bytes.len());
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Allow);
    detector.feed(&bytes[start..end], end == bytes.len());
    detector.guess(None, Utf8Detection::Allow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_first_non_ascii_marks_high_bytes_and_esc() {
        assert_eq!(find_first_non_ascii(b""), None);
        assert_eq!(find_first_non_ascii(b"id,name\n1,alice\n"), None);
        assert_eq!(find_first_non_ascii(b"ab\x80cd"), Some(2));
        assert_eq!(find_first_non_ascii(b"abc\x1b$B"), Some(3));
        assert_eq!(find_first_non_ascii("ab名".as_bytes()), Some(2));
    }
}
