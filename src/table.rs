use std::borrow::Cow;
use std::path::Path;

use encoding_rs::Encoding;

use crate::csv_data::CsvTable;

/// UI が見る表の窓口。読み込み方式（全読み込み / 遅延）の違いはここで隠す。
///
/// セルは `Cow<str>` で返す。全読み込み側は借用（コピーなし）、
/// 遅延側は必要な行をその場でデコードするため所有になる。
pub enum Table {
    Eager(CsvTable),
}

impl Table {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        Ok(Self::Eager(CsvTable::load(path)?))
    }

    pub fn encoding(&self) -> &'static Encoding {
        match self {
            Self::Eager(t) => t.encoding,
        }
    }

    /// ヘッダーを除いたデータ行の数。
    pub fn row_count(&self) -> usize {
        match self {
            Self::Eager(t) => t.row_count(),
        }
    }

    /// ヘッダーを含めた中で最も広い行の列数。
    pub fn column_count(&self) -> usize {
        match self {
            Self::Eager(t) => t.column_count(),
        }
    }

    pub fn header(&self, col: usize) -> Cow<'_, str> {
        match self {
            Self::Eager(t) => Cow::Borrowed(t.header(col)),
        }
    }

    /// データ行 `row`（0始まり、ヘッダーを除く）の `col` 列目。範囲外は空文字列。
    pub fn cell(&self, row: usize, col: usize) -> Cow<'_, str> {
        match self {
            Self::Eager(t) => Cow::Borrowed(t.cell(row, col)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eager(csv: &str) -> Table {
        Table::Eager(CsvTable::from_bytes(csv.as_bytes()).unwrap())
    }

    #[test]
    fn delegates_to_the_eager_table() {
        let t = eager("名前,部署\n山田,営業\n鈴木,開発\n");
        assert_eq!(t.row_count(), 2);
        assert_eq!(t.column_count(), 2);
        assert_eq!(t.header(1), "部署");
        assert_eq!(t.cell(1, 0), "鈴木");
        assert_eq!(t.cell(9, 9), "");
        assert_eq!(t.encoding(), encoding_rs::UTF_8);
    }

    #[test]
    fn eager_cells_are_borrowed_not_copied() {
        let t = eager("a,b\n1,2\n");
        assert!(matches!(t.header(0), Cow::Borrowed(_)));
        assert!(matches!(t.cell(0, 1), Cow::Borrowed(_)));
        // 範囲外も借用（'static の空文字列）で、確保が発生しない。
        assert!(matches!(t.cell(5, 5), Cow::Borrowed("")));
    }
}
