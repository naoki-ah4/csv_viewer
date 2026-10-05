use super::index_job::{CHUNK, FIRST_CHUNK};
use super::*;
use crate::table::eager::CsvTable;

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
    assert_eq!(
        lazy.column_count(),
        eager.column_count(),
        "{label}: column_count"
    );
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
        ("utf8", &include_bytes!("../../../samples/utf8.csv")[..]),
        ("sjis", &include_bytes!("../../../samples/sjis.csv")[..]),
        ("wide", &include_bytes!("../../../samples/wide.csv")[..]),
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
    bytes.extend_from_slice(&[
        0x1B, 0x24, 0x42, 0x46, 0x7C, 0x4B, 0x5C, 0x38, 0x6C, 0x1B, 0x28, 0x42,
    ]);
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
        let len = if round % 60 == 0 {
            700
        } else {
            (next() % 40) as usize
        };
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
    assert!(
        table.row_count() > 0,
        "rows are readable right after the first step"
    );

    assert!(job.step(CHUNK));
    assert!(
        table.progress().0 > FIRST_CHUNK * 2,
        "later steps use the full chunk"
    );
}
