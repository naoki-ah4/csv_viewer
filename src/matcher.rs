//! 検索語の判定。表や画面には依存しない。
//!
//! - 検索語がセルの中に含まれるかを調べる（部分一致）。英字は大文字小文字を区別せず、
//!   日本語などは完全一致（ひらがな/カタカナ、全角/半角の同一視はしない）。
//! - 遅延読み込みの表のために、検索語を、ファイルの文字コードのバイト列に直して、生のバイト列を
//!   走査する正規表現も作る。

use std::cell::RefCell;
use std::sync::Arc;

use encoding_rs::Encoding;
use memchr::memmem;

/// 検索の向き。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

/// 検索の結果。
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome<T> {
    Found(T),
    NotFound,
    /// 呼び出し側が中断した。
    Cancelled,
}

/// 検索語。セルとの一致判定と、生のバイト列を走査するための準備を持つ。
pub struct Matcher {
    needle: String,
    /// 英字を小文字にした UTF-8 のバイト列。
    lower: Vec<u8>,
    has_ascii_letters: bool,
    finder: memmem::Finder<'static>,
    /// 生のバイト列の走査に使う正規表現。文字コードごとに作り、使い回す。
    raw: RefCell<Option<(&'static Encoding, Option<Arc<regex::bytes::Regex>>)>>,
}

impl Matcher {
    /// 空の検索語は、何にも一致しないので、`None`。
    pub fn new(needle: &str) -> Option<Self> {
        if needle.is_empty() {
            return None;
        }
        let lower = needle.to_ascii_lowercase().into_bytes();
        Some(Self {
            needle: needle.to_string(),
            has_ascii_letters: needle.bytes().any(|b| b.is_ascii_alphabetic()),
            finder: memmem::Finder::new(&lower).into_owned(),
            lower,
            raw: RefCell::new(None),
        })
    }

    /// セルの文字列に、検索語が含まれるか。
    pub fn is_match(&self, cell: &str) -> bool {
        if cell.len() < self.lower.len() {
            return false;
        }
        if self.has_ascii_letters {
            // 英字だけを小文字にしても、UTF-8 のほかのバイトは変わらないので、バイト列で探してよい。
            self.finder
                .find(cell.to_ascii_lowercase().as_bytes())
                .is_some()
        } else {
            self.finder.find(cell.as_bytes()).is_some()
        }
    }

    /// 検索語を `encoding` のバイト列に直して、それを探す正規表現。
    /// 文字コードで表せない文字を含むなら、どのセルにもないので `None`。
    pub fn raw_regex(&self, encoding: &'static Encoding) -> Option<Arc<regex::bytes::Regex>> {
        if let Some((cached, regex)) = self.raw.borrow().as_ref() {
            if *cached == encoding {
                return regex.clone();
            }
        }
        let regex = raw_pattern(&self.needle, encoding).map(Arc::new);
        *self.raw.borrow_mut() = Some((encoding, regex.clone()));
        regex
    }

    /// 生のバイト列で、検索語に一致し得る最大のバイト数。窓の重なりの計算に使う。
    /// 文字コードで表せない文字があれば `None`。
    pub fn raw_len(&self, encoding: &'static Encoding) -> Option<usize> {
        let mut total = 0;
        for c in self.needle.chars() {
            let len = encode_char(c, encoding)?.len();
            // 引用符は、生のバイト列では、2つ重なって現れ得る（下の `raw_pattern` を参照）。
            total += if c == '"' { len * 2 } else { len };
        }
        Some(total)
    }
}

/// 検索語を、ファイルの文字コードのバイト列に直す。表せない文字があれば `None`。
fn encode_needle(needle: &str, encoding: &'static Encoding) -> Option<Vec<u8>> {
    if encoding == encoding_rs::UTF_16LE {
        return Some(needle.encode_utf16().flat_map(u16::to_le_bytes).collect());
    }
    if encoding == encoding_rs::UTF_16BE {
        return Some(needle.encode_utf16().flat_map(u16::to_be_bytes).collect());
    }
    if encoding == encoding_rs::UTF_8 {
        return Some(needle.as_bytes().to_vec());
    }
    let (bytes, _, unmappable) = encoding.encode(needle);
    (!unmappable).then(|| bytes.into_owned())
}

/// 1文字を、ファイルの文字コードのバイト列にする。表せない文字なら `None`。
fn encode_char(c: char, encoding: &'static Encoding) -> Option<Vec<u8>> {
    let mut buf = [0u8; 4];
    encode_needle(c.encode_utf8(&mut buf), encoding)
}

/// 検索語を、生のバイト列を走査する正規表現にする。英字だけ、大文字小文字を区別しない。
///
/// 英数字以外のバイトは、`\xHH` で、そのままの値として一致させる。
/// ただし `"` は、**1つか2つ**の `"` に一致させる。CSV では、セルの中の `"` は、引用符で囲まれた
/// フィールドなら `""` と書かれ、囲まれていないフィールドの途中なら、1つのまま書かれるため。
/// 偶然の一致は、あとでセルの中身を確かめてはじくので、取りこぼさないことが大事。
fn raw_pattern(needle: &str, encoding: &'static Encoding) -> Option<regex::bytes::Regex> {
    use std::fmt::Write;
    let mut pattern = String::new();
    for c in needle.chars() {
        let mut literal = String::new();
        for b in encode_char(c, encoding)? {
            if b.is_ascii_alphanumeric() {
                literal.push(b as char);
            } else {
                write!(literal, "\\x{b:02X}").ok()?;
            }
        }
        if c == '"' {
            write!(pattern, "(?:{literal}){{1,2}}").ok()?;
        } else {
            pattern.push_str(&literal);
        }
    }
    regex::bytes::RegexBuilder::new(&pattern)
        .unicode(false)
        .case_insensitive(true)
        .build()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(needle: &str) -> Matcher {
        Matcher::new(needle).expect("non-empty needle")
    }

    #[test]
    fn an_empty_needle_is_not_a_matcher() {
        assert!(Matcher::new("").is_none());
    }

    #[test]
    fn english_letters_ignore_case() {
        assert!(m("abc").is_match("xxABCxx"));
        assert!(m("ABC").is_match("xxabcxx"));
        assert!(m("a-1").is_match("pre A-1 post"));
        assert!(!m("abc").is_match("a b c"));
    }

    #[test]
    fn japanese_must_match_exactly() {
        assert!(m("山田").is_match("山田太郎"));
        assert!(!m("山田").is_match("山 田"));
        // ひらがなとカタカナ、全角と半角は、同一視しない。
        assert!(!m("やまだ").is_match("ヤマダ"));
        assert!(!m("ＡＢＣ").is_match("abc"));
        assert!(!m("abc").is_match("ＡＢＣ"));
    }

    #[test]
    fn only_ascii_letters_are_case_folded() {
        assert!(!m("é").is_match("É"));
        assert!(m("Name:山").is_match("my name:山田"));
    }

    #[test]
    fn needles_at_the_edges_and_longer_than_the_cell() {
        assert!(m("ab").is_match("ab"));
        assert!(m("ab").is_match("abxx"));
        assert!(m("ab").is_match("xxab"));
        assert!(!m("abc").is_match("ab"));
        assert!(!m("a").is_match(""));
    }

    #[test]
    fn a_needle_with_a_newline_matches_a_multi_line_cell() {
        assert!(m("a\nb").is_match("xa\nbx"));
        assert!(!m("a\nb").is_match("a b"));
    }

    fn raw_matches(needle: &str, encoding: &'static Encoding, haystack: &[u8]) -> Vec<usize> {
        let regex = m(needle).raw_regex(encoding).expect("representable");
        let mut found = Vec::new();
        let mut pos = 0;
        while let Some(hit) = regex.find_at(haystack, pos) {
            found.push(hit.start());
            pos = hit.start() + 1;
        }
        found
    }

    #[test]
    fn the_raw_regex_finds_the_needle_in_every_supported_encoding() {
        let text = "xx山田yyHelloあ";
        for encoding in [
            encoding_rs::UTF_8,
            encoding_rs::SHIFT_JIS,
            encoding_rs::EUC_JP,
        ] {
            let bytes = encoding.encode(text).0.into_owned();
            let at = encode_needle("xx", encoding).unwrap().len(); // 「xx」の直後が「山田」
            assert_eq!(
                raw_matches("山田", encoding, &bytes),
                vec![at],
                "{}",
                encoding.name()
            );
            // 英字は大文字小文字を区別しない。
            assert_eq!(
                raw_matches("hello", encoding, &bytes).len(),
                1,
                "{}",
                encoding.name()
            );
            assert_eq!(
                raw_matches("HELLO", encoding, &bytes).len(),
                1,
                "{}",
                encoding.name()
            );
        }
        for (encoding, to_bytes) in [
            (
                encoding_rs::UTF_16LE,
                u16::to_le_bytes as fn(u16) -> [u8; 2],
            ),
            (
                encoding_rs::UTF_16BE,
                u16::to_be_bytes as fn(u16) -> [u8; 2],
            ),
        ] {
            let bytes: Vec<u8> = text.encode_utf16().flat_map(to_bytes).collect();
            assert_eq!(
                raw_matches("山田", encoding, &bytes),
                vec![4],
                "{}",
                encoding.name()
            );
            assert_eq!(
                raw_matches("hello", encoding, &bytes).len(),
                1,
                "{}",
                encoding.name()
            );
        }
    }

    #[test]
    fn regex_metacharacters_in_the_needle_are_matched_literally() {
        let utf8 = encoding_rs::UTF_8;
        assert_eq!(raw_matches("a.b*c(", utf8, b"xx a.b*c( yy"), vec![3]);
        assert!(raw_matches("a.b", utf8, b"axb").is_empty());
        assert!(raw_matches("a|b", utf8, b"a b").is_empty());
        assert_eq!(raw_matches("[x]", utf8, b"[x]"), vec![0]);
        assert_eq!(raw_matches("a\\b", utf8, b"a\\b"), vec![0]);
    }

    #[test]
    fn a_needle_the_encoding_cannot_represent_has_no_raw_regex() {
        let emoji = m("😀");
        assert!(emoji.raw_regex(encoding_rs::UTF_8).is_some());
        assert!(emoji.raw_regex(encoding_rs::SHIFT_JIS).is_none());
        assert!(emoji.raw_len(encoding_rs::EUC_JP).is_none());
    }

    #[test]
    fn raw_length_depends_on_the_encoding() {
        let needle = m("山田");
        assert_eq!(needle.raw_len(encoding_rs::UTF_8), Some(6));
        assert_eq!(needle.raw_len(encoding_rs::SHIFT_JIS), Some(4));
        assert_eq!(needle.raw_len(encoding_rs::UTF_16LE), Some(4));
    }

    #[test]
    fn the_raw_regex_is_cached_per_encoding() {
        let needle = m("abc");
        let a = needle.raw_regex(encoding_rs::UTF_8).unwrap();
        let b = needle.raw_regex(encoding_rs::UTF_8).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "same encoding reuses the regex");
        let c = needle.raw_regex(encoding_rs::SHIFT_JIS).unwrap();
        assert!(!Arc::ptr_eq(&a, &c));
        // 切り替えたあと、もう一度 UTF-8 に戻しても、正しく作り直される。
        assert!(needle.raw_regex(encoding_rs::UTF_8).is_some());
    }

    #[test]
    fn a_quote_in_the_needle_matches_both_the_doubled_and_the_single_form() {
        let needle = m("say \"Hi\"");
        let regex = needle.raw_regex(encoding_rs::UTF_8).unwrap();
        // 引用符で囲まれたフィールドの中では、`""` と書かれる。
        assert!(regex.is_match(b"x \"say \"\"Hi\"\"\" y"));
        // 囲まれていないフィールドの途中では、1つのまま。
        assert!(regex.is_match(b"say \"Hi\""));
        // 引用符がないものには、一致しない。
        assert!(!regex.is_match(b"say Hi"));
        // 最大の長さは、引用符を2つとして数える。
        assert_eq!(
            needle.raw_len(encoding_rs::UTF_8),
            Some("say ".len() + 2 + "Hi".len() + 2)
        );
    }

    #[test]
    fn a_quote_in_the_needle_works_in_utf16_too() {
        let needle = m("\"a");
        let regex = needle.raw_regex(encoding_rs::UTF_16LE).unwrap();
        let doubled: Vec<u8> = "\"\"a".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let single: Vec<u8> = "\"a".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert!(regex.is_match(&doubled));
        assert!(regex.is_match(&single));
    }
}
