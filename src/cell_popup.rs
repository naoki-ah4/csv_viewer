//! セルの全文を出すポップアップの、画面に依存しない部分。

/// ポップアップに出す最大の文字数。これを超える部分は切り捨てる（全体の文字数は知らせる）。
/// 巨大なセルを、そのまま1つの文章として組版すると重いため。あとで調整できるよう、定数にしてある。
pub const MAX_CHARS: usize = 20_000;

pub const WIDTH: f32 = 420.0;
pub const MAX_HEIGHT: f32 = 320.0;
/// ポップアップと、クリックした位置・ウィンドウの端との間隔。
const MARGIN: f32 = 8.0;

/// 先頭から `max` 文字までに切り詰める。戻り値は、切り詰めた文字列と、元の全体の文字数。
pub fn truncate_chars(text: &str, max: usize) -> (String, usize) {
    let total = text.chars().count();
    if total <= max {
        (text.to_string(), total)
    } else {
        (text.chars().take(max).collect(), total)
    }
}

/// クリックした位置 `anchor` の近くで、ウィンドウからはみ出さないポップアップの左上の位置。
/// いずれも `(x, y)` / `(幅, 高さ)`。ウィンドウに収まらないほど大きいときは、左上に寄せる。
pub fn place(anchor: (f32, f32), window: (f32, f32), size: (f32, f32)) -> (f32, f32) {
    let max_x = (window.0 - size.0 - MARGIN).max(MARGIN);
    let max_y = (window.1 - size.1 - MARGIN).max(MARGIN);
    // クリックした位置の少し下・右に出す。
    let x = anchor.0.min(max_x).max(MARGIN);
    let y = (anchor.1 + MARGIN).min(max_y).max(MARGIN);
    (x, y)
}

/// クリックした位置 `anchor` の真上に出す版（ステータスバーのメニュー用）。
/// ウィンドウの上端・左右の端からはみ出さない。
pub fn place_above(anchor: (f32, f32), window: (f32, f32), size: (f32, f32)) -> (f32, f32) {
    let max_x = (window.0 - size.0 - MARGIN).max(MARGIN);
    let x = anchor.0.min(max_x).max(MARGIN);
    let y = (anchor.1 - size.1 - MARGIN).max(MARGIN);
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_kept_whole() {
        assert_eq!(truncate_chars("abc", 10), ("abc".to_string(), 3));
        assert_eq!(truncate_chars("", 10), (String::new(), 0));
        assert_eq!(
            truncate_chars("abc", 3),
            ("abc".to_string(), 3),
            "exactly at the limit"
        );
    }

    #[test]
    fn long_text_is_cut_by_characters_and_reports_the_total() {
        let (text, total) = truncate_chars("abcdef", 4);
        assert_eq!(text, "abcd");
        assert_eq!(total, 6);
    }

    #[test]
    fn truncation_never_splits_a_multibyte_character() {
        let (text, total) = truncate_chars("日本語のテキスト", 3);
        assert_eq!(text, "日本語");
        assert_eq!(total, 8);
        // 結合文字や絵文字も、`char` 単位なので、文字の途中では切れない（不正な UTF-8 にならない）。
        let (text, _) = truncate_chars("a😀b😀c", 2);
        assert_eq!(text, "a😀");
    }

    #[test]
    fn newlines_are_preserved() {
        let (text, total) = truncate_chars("一行目\n二行目", 100);
        assert_eq!(text, "一行目\n二行目");
        assert_eq!(total, 7);
    }

    #[test]
    fn the_popup_appears_just_below_the_click() {
        let (x, y) = place((100.0, 200.0), (1200.0, 800.0), (WIDTH, MAX_HEIGHT));
        assert_eq!((x, y), (100.0, 208.0));
    }

    #[test]
    fn the_popup_is_kept_inside_the_window() {
        let window = (900.0, 600.0);
        let size = (WIDTH, MAX_HEIGHT);
        // 右端に近いクリック：右にはみ出さない。
        let (x, _) = place((880.0, 100.0), window, size);
        assert_eq!(x, 900.0 - WIDTH - 8.0);
        // 下端に近いクリック：下にはみ出さない。
        let (_, y) = place((100.0, 590.0), window, size);
        assert_eq!(y, 600.0 - MAX_HEIGHT - 8.0);
        // 左上の端：間隔を空ける。
        assert_eq!(place((0.0, 0.0), window, size), (8.0, 8.0));
        assert_eq!(place((-50.0, -50.0), window, size), (8.0, 8.0));
    }

    #[test]
    fn a_popup_larger_than_the_window_sticks_to_the_top_left() {
        assert_eq!(
            place((300.0, 300.0), (200.0, 100.0), (WIDTH, MAX_HEIGHT)),
            (8.0, 8.0)
        );
    }

    #[test]
    fn a_menu_above_the_click_sits_over_it_and_stays_inside_the_window() {
        let window = (900.0, 600.0);
        let size = (200.0, 150.0);
        assert_eq!(
            place_above((300.0, 580.0), window, size),
            (300.0, 580.0 - 150.0 - 8.0)
        );
        // 右端に近いクリックは、右にはみ出さない。
        assert_eq!(
            place_above((890.0, 580.0), window, size).0,
            900.0 - 200.0 - 8.0
        );
        // 上に十分な余白がないときは、上端に寄せる。
        assert_eq!(place_above((300.0, 100.0), window, size).1, 8.0);
        assert_eq!(place_above((-5.0, 580.0), window, size).0, 8.0);
    }
}
