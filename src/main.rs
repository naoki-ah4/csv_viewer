mod csv_data;

use std::path::PathBuf;

use gpui::{
    App, Application, Bounds, Context, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    rgb, size, uniform_list,
};

use csv_data::CsvTable;

const ROW_HEIGHT: f32 = 24.0;
const MIN_COL_WIDTH: f32 = 60.0;
const MAX_COL_WIDTH: f32 = 400.0;
const CHAR_WIDTH: f32 = 7.5;
const CELL_PADDING: f32 = 16.0;
const WIDTH_SAMPLE_ROWS: usize = 1000;

struct Viewer {
    table: CsvTable,
    col_widths: Vec<f32>,
}

impl Viewer {
    fn new(table: CsvTable) -> Self {
        let col_widths = column_widths(&table);
        Self { table, col_widths }
    }
}

/// 全角は半角の2倍幅として概算する。先頭の一部の行だけを見て列幅を決める。
fn column_widths(table: &CsvTable) -> Vec<f32> {
    let mut widths = vec![0usize; table.column_count()];
    let sampled = table.rows.iter().take(WIDTH_SAMPLE_ROWS);
    for row in std::iter::once(&table.headers).chain(sampled) {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(display_width(cell));
        }
    }
    widths
        .into_iter()
        .map(|w| (w as f32 * CHAR_WIDTH + CELL_PADDING).clamp(MIN_COL_WIDTH, MAX_COL_WIDTH))
        .collect()
}

fn display_width(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

fn cell(text: String, width: f32) -> impl IntoElement {
    div()
        .w(px(width))
        .flex_shrink_0()
        .px_2()
        .overflow_hidden()
        .whitespace_nowrap()
        .child(single_line(text))
}

/// セル内の改行は表示上だけ記号に置き換える。元データは変えない。
fn single_line(text: String) -> String {
    if text.contains(['\r', '\n']) {
        text.replace("\r\n", "⏎").replace(['\r', '\n'], "⏎")
    } else {
        text
    }
}

impl Render for Viewer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let widths = self.col_widths.clone();
        let header = div()
            .flex()
            .flex_row()
            .h(px(ROW_HEIGHT))
            .bg(rgb(0xf0f0f0))
            .border_b_1()
            .border_color(rgb(0xcccccc))
            .font_weight(gpui::FontWeight::BOLD)
            .children(
                widths
                    .iter()
                    .enumerate()
                    .map(|(i, w)| cell(self.table.headers.get(i).cloned().unwrap_or_default(), *w)),
            );

        let list = uniform_list(
            "rows",
            self.table.rows.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _window, _cx| {
                range
                    .map(|ix| {
                        let row = &this.table.rows[ix];
                        div()
                            .flex()
                            .flex_row()
                            .h(px(ROW_HEIGHT))
                            .when(ix % 2 == 1, |d| d.bg(rgb(0xfafafa)))
                            .children(this.col_widths.iter().enumerate().map(|(i, w)| {
                                cell(row.get(i).cloned().unwrap_or_default(), *w)
                            }))
                    })
                    .collect()
            }),
        )
        .flex_1();

        let status = div()
            .h(px(ROW_HEIGHT))
            .px_2()
            .bg(rgb(0xf0f0f0))
            .border_t_1()
            .border_color(rgb(0xcccccc))
            .text_sm()
            .child(format!(
                "{} 行 / {} 列 / {}",
                self.table.rows.len(),
                self.col_widths.len(),
                self.table.encoding.name()
            ));

        // ヘッダーと本体を同じ横幅の列にまとめ、外側で横スクロールさせる。
        let table = div()
            .flex()
            .flex_col()
            .h_full()
            .min_w(px(self.col_widths.iter().sum::<f32>()))
            .w_full()
            .child(header)
            .child(list);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x222222))
            .text_sm()
            .child(
                div()
                    .id("table-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_x_scroll()
                    .child(table),
            )
            .child(status)
    }
}

fn main() -> anyhow::Result<()> {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: csv_viewer <file.csv>");
        std::process::exit(2);
    };
    let table = CsvTable::load(&path)?;

    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Viewer::new(table)),
        )
        .unwrap();
        cx.activate(true);
    });
    Ok(())
}
