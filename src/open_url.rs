//! Finder などから「開く」イベントで届く URL を、ファイルのパスに直す。
//!
//! macOS は、ダブルクリックや「このアプリケーションで開く」を、コマンド引数ではなく
//! `file:///Users/.../a%20b.csv` のような URL で渡す。空白や日本語はパーセントエンコードされている。

use std::path::PathBuf;

/// `file://` の URL、または `/` で始まるパスを、`PathBuf` にする。それ以外は `None`。
pub fn path_from_url(url: &str) -> Option<PathBuf> {
    let url = url.trim();
    if url.starts_with('/') {
        return Some(PathBuf::from(url));
    }
    url::Url::parse(url).ok()?.to_file_path().ok()
}

/// 届いた URL の中から、最初に開けそうなパスを取り出す。
pub fn first_path(urls: &[String]) -> Option<PathBuf> {
    urls.iter().find_map(|u| path_from_url(u))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_plain_file_url_becomes_a_path() {
        assert_eq!(
            path_from_url("file:///Users/me/data.csv"),
            Some(PathBuf::from("/Users/me/data.csv"))
        );
    }

    #[test]
    fn percent_encoded_spaces_and_japanese_are_decoded() {
        assert_eq!(
            path_from_url("file:///Users/me/my%20data.csv"),
            Some(PathBuf::from("/Users/me/my data.csv"))
        );
        assert_eq!(
            path_from_url("file:///Users/me/%E3%83%87%E3%83%BC%E3%82%BF.csv"),
            Some(PathBuf::from("/Users/me/データ.csv"))
        );
    }

    #[test]
    fn localhost_is_accepted_as_the_host() {
        assert_eq!(
            path_from_url("file://localhost/Users/me/data.csv"),
            Some(PathBuf::from("/Users/me/data.csv"))
        );
    }

    #[test]
    fn a_plain_absolute_path_is_kept_as_is() {
        assert_eq!(
            path_from_url("/Users/me/data.csv"),
            Some(PathBuf::from("/Users/me/data.csv"))
        );
        // パスは、パーセントを含んでいても、デコードしない。
        assert_eq!(path_from_url("/tmp/100%.csv"), Some(PathBuf::from("/tmp/100%.csv")));
    }

    #[test]
    fn other_schemes_and_junk_are_rejected() {
        assert_eq!(path_from_url("https://example.com/data.csv"), None);
        assert_eq!(path_from_url(""), None);
        assert_eq!(path_from_url("   "), None);
        assert_eq!(path_from_url("data.csv"), None, "relative paths are not accepted");
        assert_eq!(path_from_url("file://remote-host/share/data.csv"), None);
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(
            path_from_url("  file:///Users/me/data.csv\n"),
            Some(PathBuf::from("/Users/me/data.csv"))
        );
    }

    #[test]
    fn the_first_usable_url_wins() {
        let urls = vec![
            "https://example.com/x.csv".to_string(),
            "file:///a/one.csv".to_string(),
            "file:///a/two.csv".to_string(),
        ];
        assert_eq!(first_path(&urls).as_deref(), Some(Path::new("/a/one.csv")));
        assert_eq!(first_path(&[]), None);
    }
}
