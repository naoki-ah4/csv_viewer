mod csv_data;
mod lazy_table;
mod record_index;
mod scrollbar;
mod table;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui::{
    App, Application, Bounds, Context, ExternalPaths, FocusHandle, KeyBinding, Menu, MenuItem,
    MouseButton, MouseDownEvent, MouseMoveEvent, PathPromptOptions, Pixels, Point, ScrollHandle,
    Task, UniformListScrollHandle, Window, WindowBounds, WindowOptions, actions, div, point,
    prelude::*, px, rgb, rgba, size, uniform_list,
};

use lazy_table::{IndexJob, IndexStatus};
use scrollbar::{Axis, AxisState};
use table::Table;

actions!(csv_viewer, [Quit, CloseWindow, OpenFile]);

const ROW_HEIGHT: f32 = 24.0;
const MIN_COL_WIDTH: f32 = 60.0;
const MAX_COL_WIDTH: f32 = 400.0;
const CHAR_WIDTH: f32 = 7.5;
const CELL_PADDING: f32 = 16.0;
const WIDTH_SAMPLE_ROWS: usize = 1000;
/// 遅延読み込みの索引作成中に、画面を更新する間隔。
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// 遅延読み込みできない文字コードのとき、全読み込みで開き直してよい最大サイズ。
const EAGER_FALLBACK_LIMIT: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy)]
struct Drag {
    axis: Axis,
    /// つまみの先頭から、掴んだ位置までの距離。
    grab: Pixels,
}

struct Loaded {
    table: Table,
    col_widths: Vec<f32>,
    /// 列幅の元にしたデータ行の数。
    sampled_rows: usize,
}

impl Loaded {
    fn new(table: Table) -> Self {
        let (col_widths, sampled_rows) = column_widths(&table);
        Self {
            table,
            col_widths,
            sampled_rows,
        }
    }

    /// 索引作成中は行や列が増えるので、列幅の元にした範囲が古くなったら計算し直す。
    fn refresh_widths(&mut self) {
        let rows = self.table.row_count().min(WIDTH_SAMPLE_ROWS);
        if rows != self.sampled_rows || self.table.column_count() != self.col_widths.len() {
            (self.col_widths, self.sampled_rows) = column_widths(&self.table);
        }
    }
}

struct Viewer {
    loaded: Option<Loaded>,
    /// 直近の読み込み失敗など、ステータスバーに出すメッセージ。
    error: Option<String>,
    /// 読み込み中のファイル名。
    loading: Option<String>,
    /// 実行中の読み込み。保持している間だけ動き、drop するとキャンセルされる。
    load_task: Option<Task<()>>,
    /// 遅延読み込みの索引作成。`cancel` を立てると、次のチャンクの前で止まる。
    index_task: Option<Task<()>>,
    cancel: Arc<AtomicBool>,
    /// 索引作成中に、画面を定期的に更新するタスク。
    poll_task: Option<Task<()>>,
    list_scroll: UniformListScrollHandle,
    h_scroll: ScrollHandle,
    drag: Option<Drag>,
    focus_handle: FocusHandle,
}

impl Viewer {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            loaded: None,
            error: None,
            loading: None,
            load_task: None,
            index_task: None,
            cancel: Arc::new(AtomicBool::new(false)),
            poll_task: None,
            list_scroll: UniformListScrollHandle::new(),
            h_scroll: ScrollHandle::new(),
            drag: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// CSVをバックグラウンドで読み込み、終わったら表示を差し替える。
    /// 読み込み中は直前の表示を残す。別のファイルを開くと、前の読み込みは破棄される。
    fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.open_with(path, false, window, cx);
    }

    /// `force_eager` なら、サイズに関わらず全体を読み込んで開く。
    fn open_with(
        &mut self,
        path: PathBuf,
        force_eager: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading = Some(file_name(&path));
        self.error = None;

        let load = cx.background_spawn({
            let path = path.clone();
            async move {
                let (table, job) = if force_eager {
                    (Table::load_eager(&path)?, None)
                } else {
                    Table::open(&path)?
                };
                anyhow::Ok((Loaded::new(table), job))
            }
        });
        // 古い Task は代入時に drop され、キャンセルされる。
        self.load_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = load.await;
            this.update_in(cx, |this, window, cx| {
                this.finish_load(&path, result, window, cx)
            })
            .ok();
        }));
        cx.notify();
    }

    /// 読み込み結果を反映する。失敗時は直前の表示を残し、メッセージだけ出す。
    fn finish_load(
        &mut self,
        path: &Path,
        result: anyhow::Result<(Loaded, Option<IndexJob>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading = None;
        match result {
            Ok((loaded, job)) => {
                self.stop_indexing();
                self.loaded = Some(loaded);
                self.list_scroll
                    .0
                    .borrow()
                    .base_handle
                    .set_offset(point(px(0.), px(0.)));
                self.h_scroll.set_offset(point(px(0.), px(0.)));
                window.set_window_title(&file_name(path));
                if let Some(job) = job {
                    self.start_indexing(path.to_path_buf(), job, window, cx);
                }
            }
            Err(e) => {
                self.error = Some(format!("開けませんでした: {} ({e})", path.display()));
            }
        }
        cx.notify();
    }

    /// 動いている索引作成と画面更新を止める。
    fn stop_indexing(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.index_task = None;
        self.poll_task = None;
    }

    /// 索引作成をバックグラウンドで走らせ、終わるまで画面を定期的に更新する。
    fn start_indexing(
        &mut self,
        path: PathBuf,
        job: IndexJob,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        self.index_task = Some(cx.background_spawn(async move {
            let mut job = job;
            job.run(&cancel);
        }));

        let timer = cx.background_executor().clone();
        self.poll_task = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                timer.timer(POLL_INTERVAL).await;
                let keep_going = this
                    .update_in(cx, |this, window, cx| this.poll_index(&path, window, cx))
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }

    /// 索引作成の途中経過を画面に反映する。続けて呼ぶ必要があれば `true`。
    fn poll_index(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(loaded) = self.loaded.as_mut() else {
            return false;
        };
        loaded.refresh_widths();
        let status = loaded.table.index_status();
        cx.notify();
        match status {
            Some(IndexStatus::Indexing) => true,
            Some(IndexStatus::Unsupported(encoding)) => {
                self.fall_back_to_eager(path, encoding, window, cx);
                false
            }
            Some(IndexStatus::Done | IndexStatus::Cancelled) | None => false,
        }
    }

    /// 遅延読み込みできない文字コード（ISO-2022-JP など）だったので、全読み込みで開き直す。
    /// 巨大なファイルは、メモリを使い切らないよう断る。
    fn fall_back_to_eager(
        &mut self,
        path: &Path,
        encoding: &'static encoding_rs::Encoding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(u64::MAX);
        if size > EAGER_FALLBACK_LIMIT {
            self.error = Some(format!(
                "{} は文字コード {} のため、この大きさでは開けません",
                file_name(path),
                encoding.name()
            ));
            cx.notify();
            return;
        }
        self.open_with(path.to_path_buf(), true, window, cx);
    }

    fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update_in(cx, |this, window, cx| this.open_path(path, window, cx))
                .ok();
        })
        .detach();
    }

    fn axis_state(&self, axis: Axis) -> AxisState {
        match axis {
            Axis::Vertical => AxisState::vertical(&self.list_scroll),
            Axis::Horizontal => AxisState::horizontal(&self.h_scroll),
        }
    }

    fn start_drag(&mut self, axis: Axis, mouse: Point<Pixels>) {
        let state = self.axis_state(axis);
        let Some(thumb) = state.thumb() else { return };
        let mouse = axis_value(axis, mouse);
        self.drag = Some(Drag {
            axis,
            grab: mouse - state.origin - thumb.pos,
        });
    }

    fn drag_to(&mut self, mouse: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(Drag { axis, grab }) = self.drag else {
            return;
        };
        let state = self.axis_state(axis);
        let Some(thumb) = state.thumb() else { return };
        let pos = axis_value(axis, mouse) - state.origin - grab;
        let offset = state.offset_for_thumb_pos(thumb.len, pos);
        match axis {
            Axis::Vertical => {
                let handle = self.list_scroll.0.borrow().base_handle.clone();
                handle.set_offset(point(handle.offset().x, offset));
            }
            Axis::Horizontal => {
                self.h_scroll
                    .set_offset(point(offset, self.h_scroll.offset().y));
            }
        }
        cx.notify();
    }

    fn thumb_element(
        &self,
        axis: Axis,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let thumb = self.axis_state(axis).thumb()?;
        let (id, base) = match axis {
            Axis::Vertical => (
                "vbar",
                div()
                    .absolute()
                    .top(px(ROW_HEIGHT) + thumb.pos)
                    .right(px(scrollbar::MARGIN))
                    .w(px(scrollbar::THICKNESS))
                    .h(thumb.len),
            ),
            Axis::Horizontal => (
                "hbar",
                div()
                    .absolute()
                    .left(thumb.pos)
                    .bottom(px(scrollbar::MARGIN))
                    .h(px(scrollbar::THICKNESS))
                    .w(thumb.len),
            ),
        };
        let dragging = self.drag.as_ref().is_some_and(|d| d.axis == axis);
        Some(
            base.id(id)
                .rounded_full()
                .bg(rgba(if dragging { 0x00000099 } else { 0x00000055 }))
                .hover(|d| d.bg(rgba(0x00000088)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                        this.start_drag(axis, ev.position);
                        cx.notify();
                    }),
                ),
        )
    }

    fn render_empty(&self) -> impl IntoElement {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap_2()
            .justify_center()
            .items_center()
            .text_color(rgb(0x888888))
            .child("CSVファイルをここにドロップ")
            .child("または Cmd+O で開く")
    }

    fn render_table(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loaded = self.loaded.as_ref().expect("render_table requires a loaded CSV");
        let header = div()
            .flex()
            .flex_row()
            .h(px(ROW_HEIGHT))
            .bg(rgb(0xf0f0f0))
            .border_b_1()
            .border_color(rgb(0xcccccc))
            .font_weight(gpui::FontWeight::BOLD)
            .children(
                loaded.col_widths.iter().enumerate().map(|(i, w)| {
                    cell(loaded.table.header(i).to_string(), *w)
                }),
            );
        let total_width: f32 = loaded.col_widths.iter().sum();
        let row_count = loaded.table.row_count();

        let list = uniform_list(
            "rows",
            row_count,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, _cx| {
                let Some(loaded) = &this.loaded else {
                    return Vec::new();
                };
                range
                    .map(|ix| {
                        div()
                            .flex()
                            .flex_row()
                            .h(px(ROW_HEIGHT))
                            .when(ix % 2 == 1, |d| d.bg(rgb(0xfafafa)))
                            .children(loaded.col_widths.iter().enumerate().map(|(i, w)| {
                                cell(loaded.table.cell(ix, i).to_string(), *w)
                            }))
                    })
                    .collect()
            }),
        )
        .track_scroll(self.list_scroll.clone())
        .flex_1();

        // ヘッダーと本体を同じ横幅の列にまとめ、外側で横スクロールさせる。
        let table = div()
            .flex()
            .flex_col()
            .h_full()
            .min_w(px(total_width))
            .w_full()
            .child(header)
            .child(list);

        // 初回の描画ではスクロール領域の寸法がまだ0なので、確定後にもう一度描画してバーを出す。
        if self.list_scroll.0.borrow().base_handle.bounds().size.height <= px(0.)
            || self.h_scroll.bounds().size.width <= px(0.)
        {
            window.request_animation_frame();
        }

        let vbar = self.thumb_element(Axis::Vertical, cx);
        let hbar = self.thumb_element(Axis::Horizontal, cx);

        div()
            .relative()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .id("table-scroll")
                    .size_full()
                    .overflow_x_scroll()
                    .track_scroll(&self.h_scroll)
                    .child(table),
            )
            .children(vbar)
            .children(hbar)
    }

    fn render_status(&self) -> impl IntoElement {
        let text = match (&self.error, &self.loading, &self.loaded) {
            (Some(error), _, _) => error.clone(),
            (None, Some(name), _) => format!("読み込み中… {name}"),
            (None, None, Some(l)) => {
                let percent = l.table.indexing_percent();
                let mut text = format!(
                    "{}{} 行 / {} 列 / {}",
                    l.table.row_count(),
                    if percent.is_some() { "+" } else { "" },
                    l.table.column_count(),
                    l.table.encoding().name()
                );
                if let Some(percent) = percent {
                    text.push_str(&format!(" / 索引作成中 {percent}%"));
                }
                text
            }
            (None, None, None) => String::new(),
        };
        div()
            .h(px(ROW_HEIGHT))
            .px_2()
            .bg(rgb(0xf0f0f0))
            .border_t_1()
            .border_color(rgb(0xcccccc))
            .text_sm()
            .when(self.error.is_some(), |d| d.text_color(rgb(0xb00020)))
            .child(text)
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy())
        .into_owned()
}

fn axis_value(axis: Axis, p: Point<Pixels>) -> Pixels {
    match axis {
        Axis::Vertical => p.y,
        Axis::Horizontal => p.x,
    }
}

/// 全角は半角の2倍幅として概算する。先頭の一部の行だけを見て列幅を決める。
fn column_widths(table: &Table) -> (Vec<f32>, usize) {
    let sampled_rows = table.row_count().min(WIDTH_SAMPLE_ROWS);
    let mut widths = vec![0usize; table.column_count()];
    for (col, w) in widths.iter_mut().enumerate() {
        let sampled = (0..sampled_rows).map(|row| table.cell(row, col));
        *w = std::iter::once(table.header(col))
            .chain(sampled)
            .map(|text| display_width(&text))
            .max()
            .unwrap_or(0);
    }
    let widths = widths
        .into_iter()
        .map(|w| (w as f32 * CHAR_WIDTH + CELL_PADDING).clamp(MIN_COL_WIDTH, MAX_COL_WIDTH))
        .collect();
    (widths, sampled_rows)
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = if self.loaded.is_some() {
            self.render_table(window, cx).into_any_element()
        } else if self.loading.is_some() {
            // 初回の読み込み中は、案内文を出さない（小さいファイルだとちらつくため）。
            div().flex_1().into_any_element()
        } else {
            self.render_empty().into_any_element()
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x222222))
            .text_sm()
            .track_focus(&self.focus_handle)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(cx.listener(|this, _: &OpenFile, window, cx| this.prompt_open(window, cx)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if let Some(path) = paths.paths().first() {
                    this.open_path(path.clone(), window, cx);
                }
            }))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(rgb(0xe8f0fe)))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if ev.dragging() {
                    this.drag_to(ev.position, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.drag = None;
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.drag = None;
                    cx.notify();
                }),
            )
            .child(body)
            .child(self.render_status())
    }
}

fn main() {
    let path = std::env::args_os().nth(1).map(PathBuf::from);

    Application::new().run(move |cx: &mut App| {
        setup_app(cx);

        let bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                cx.new(|cx| {
                    let mut viewer = Viewer::new(cx);
                    if let Some(path) = &path {
                        viewer.open_path(path.clone(), window, cx);
                    }
                    viewer.focus_handle.focus(window);
                    viewer
                })
            },
        )
        .unwrap();
        cx.activate(true);
    });
}

/// 終了まわりの設定。GPUIは最後のウィンドウが閉じてもアプリを終了しないため、自前で扱う。
fn setup_app(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    // CloseWindow / OpenFile はウィンドウ側（Viewer のルート要素）で受ける。
    // アクション処理中は当該ウィンドウが取り出されており、ここから window.update() はできない。
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-o", OpenFile, None),
    ]);
    cx.set_menus(vec![
        Menu {
            name: "csv_viewer".into(),
            items: vec![MenuItem::action("Quit", Quit)],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Open…", OpenFile),
                MenuItem::action("Close Window", CloseWindow),
            ],
        },
    ]);
    cx.on_window_closed(|cx| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
}
