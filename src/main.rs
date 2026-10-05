mod csv_data;
mod lazy_table;
mod record_index;
mod scroll_pos;
mod scrollbar;
mod table;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    App, Application, Bounds, Context, ExternalPaths, FocusHandle, KeyBinding, Menu, MenuItem,
    MouseButton, MouseDownEvent, MouseMoveEvent, PathPromptOptions, Pixels, Point, ScrollHandle,
    ScrollWheelEvent, Task, Window, WindowBounds, WindowOptions, actions, canvas, div, point,
    prelude::*, px, rgb, rgba, size,
};

use lazy_table::{IndexJob, IndexStatus};
use scroll_pos::{Nav, ScrollPos};
use scrollbar::{Axis, AxisState};
use table::Table;

actions!(
    csv_viewer,
    [
        Quit,
        CloseWindow,
        OpenFile,
        ScrollLineUp,
        ScrollLineDown,
        ScrollPageUp,
        ScrollPageDown,
        ScrollToTop,
        ScrollToBottom,
    ]
);

const ROW_HEIGHT: f32 = 24.0;
/// 列番号の行の高さ。ヘッダー行の上に置く。
const COLNO_HEIGHT: f32 = 20.0;
/// 縦スクロールしない部分（列番号の行とヘッダー行）の高さ。
const HEADER_HEIGHT: f32 = COLNO_HEIGHT + ROW_HEIGHT;
const MIN_GUTTER_WIDTH: f32 = 40.0;
const MIN_COL_WIDTH: f32 = 60.0;
const MAX_COL_WIDTH: f32 = 400.0;
const CHAR_WIDTH: f32 = 7.5;
const CELL_PADDING: f32 = 16.0;
const WIDTH_SAMPLE_ROWS: usize = 1000;
/// 行を描く領域の大きさがまだ分からない最初の1フレームだけ使う高さ。
const DEFAULT_VIEWPORT_HEIGHT: f32 = 800.0;
/// 遅延読み込みの索引作成中に、画面を更新する間隔。
/// 最初の `FAST_POLL_PERIOD` の間は1フレーム（約16ms）ごとに更新して、最初の行をすぐ見せる。
const FAST_POLL_INTERVAL: Duration = Duration::from_millis(16);
const FAST_POLL_PERIOD: Duration = Duration::from_millis(500);
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
    /// 計算し直したら `true`。
    ///
    /// 標本を毎回デコードし直すと、1行が大きい表で更新のたびに重くなる。そこで、標本の行数が
    /// 倍になったときと、索引が完了したときだけ計算し直す（総量は最終的な標本の2倍以内）。
    fn refresh_widths(&mut self) -> bool {
        let rows = self.table.row_count().min(WIDTH_SAMPLE_ROWS);
        let indexing_done = self.table.indexing_percent().is_none();
        let doubled = rows >= self.sampled_rows.saturating_mul(2).max(1);
        let finished_with_new_rows = indexing_done && rows != self.sampled_rows;
        let columns_changed = self.table.column_count() != self.col_widths.len();
        if doubled || finished_with_new_rows || columns_changed {
            (self.col_widths, self.sampled_rows) = column_widths(&self.table);
            return true;
        }
        false
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
    /// 縦のスクロール位置（先頭に見える行の番号と、その行の中でのずれ）。
    /// 全体の座標（先頭から何px）で持つと、行数が多いほど刻み幅が粗くなり、`f32` では
    /// 約70万行を超えたあたりから行が重なって描かれる。
    scroll: ScrollPos,
    /// 行を描く領域の位置と大きさ。描画のたびに更新される。
    viewport: Rc<Cell<Bounds<Pixels>>>,
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
            scroll: ScrollPos::TOP,
            viewport: Rc::new(Cell::new(Bounds::default())),
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
                self.scroll = ScrollPos::TOP;
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
            let started = Instant::now();
            loop {
                let interval = if started.elapsed() < FAST_POLL_PERIOD {
                    FAST_POLL_INTERVAL
                } else {
                    POLL_INTERVAL
                };
                timer.timer(interval).await;
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

    fn row_count(&self) -> usize {
        self.loaded.as_ref().map_or(0, |l| l.table.row_count())
    }

    /// 行番号の欄の幅。行数の桁数で決まる。
    fn gutter_width(&self) -> f32 {
        gutter_width(self.row_count())
    }

    fn navigate(&mut self, nav: Nav, cx: &mut Context<Self>) {
        let viewport_height = f32::from(self.viewport.get().size.height);
        self.scroll = self
            .scroll
            .navigated(nav, self.max_scroll(), viewport_height);
        cx.notify();
    }

    /// 縦に動かせる上限（最後の行が領域の下端に揃う位置）。
    fn max_scroll(&self) -> ScrollPos {
        ScrollPos::max(self.row_count(), self.viewport.get().size.height.into())
    }

    fn axis_state(&self, axis: Axis) -> AxisState {
        match axis {
            Axis::Vertical => {
                let viewport = self.viewport.get();
                AxisState {
                    origin: viewport.origin.y,
                    viewport: viewport.size.height,
                    // つまみの長さの比率を求めるためだけに使うので、`f32` に丸めても問題ない。
                    content: px((self.row_count() as f64 * f64::from(ROW_HEIGHT)) as f32),
                    fraction: self.scroll.fraction(self.max_scroll()),
                }
            }
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
        let fraction = state.fraction_for_thumb_pos(thumb.len, pos);
        match axis {
            Axis::Vertical => {
                self.scroll = ScrollPos::from_fraction(fraction, self.max_scroll());
            }
            Axis::Horizontal => {
                let travel = f64::from(f32::from(state.content - state.viewport));
                let offset = px(-(travel * fraction) as f32);
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
                    .top(px(HEADER_HEIGHT) + thumb.pos)
                    .right(px(scrollbar::MARGIN))
                    .w(px(scrollbar::THICKNESS))
                    .h(thumb.len),
            ),
            Axis::Horizontal => (
                "hbar",
                div()
                    .absolute()
                    .left(px(self.gutter_width()) + thumb.pos)
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
        let colno = div()
            .flex()
            .flex_row()
            .h(px(COLNO_HEIGHT))
            .bg(rgb(0xe8e8e8))
            .text_xs()
            .text_color(rgb(0x888888))
            .children(
                loaded
                    .col_widths
                    .iter()
                    .enumerate()
                    .map(|(i, w)| cell((i + 1).to_string(), *w)),
            );
        let total_width: f32 = loaded.col_widths.iter().sum();
        let row_count = loaded.table.row_count();
        let gutter_width = gutter_width(row_count);

        // 縦は自前で仮想スクロールする。全体の座標（数億px）は使わず、見えている先頭行の番号と、
        // その行の中でのずれ（0〜行の高さ）だけで位置を持つ。各行の位置が小さな値になるので、
        // 何千万行あっても誤差が出ない。
        self.scroll = self.scroll.clamped(self.max_scroll());
        let viewport_height = match f32::from(self.viewport.get().size.height) {
            h if h > 0.0 => h,
            _ => DEFAULT_VIEWPORT_HEIGHT,
        };
        let visible = visible_rows(self.scroll, viewport_height, row_count);
        let rows = (visible.first..visible.last).map(|ix| {
            div()
                .flex()
                .flex_row()
                .flex_shrink_0()
                .h(px(ROW_HEIGHT))
                .when(ix % 2 == 1, |d| d.bg(rgb(0xfafafa)))
                .children(loaded.col_widths.iter().enumerate().map(|(i, w)| {
                    cell(loaded.table.cell(ix, i).to_string(), *w)
                }))
        });
        // 左端の行番号の欄。横にスクロールしても動かない。行の位置は、本体と同じ計算で揃える。
        let gutter_rows = (visible.first..visible.last).map(|ix| {
            div()
                .flex()
                .flex_row()
                .flex_shrink_0()
                .justify_end()
                .w(px(gutter_width))
                .h(px(ROW_HEIGHT))
                .px_2()
                .text_color(rgb(0x888888))
                .bg(rgb(if ix % 2 == 1 { 0xf0f0f0 } else { 0xf6f6f6 }))
                .child((ix + 1).to_string())
        });
        let gutter = div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(px(gutter_width))
            .h_full()
            .border_r_1()
            .border_color(rgb(0xcccccc))
            .child(
                div()
                    .w(px(gutter_width))
                    .h(px(HEADER_HEIGHT))
                    .flex_shrink_0()
                    .bg(rgb(0xe8e8e8))
                    .border_b_1()
                    .border_color(rgb(0xcccccc)),
            )
            .child(
                div().relative().flex_1().min_h_0().overflow_hidden().child(
                    div()
                        .relative()
                        .top(px(-visible.offset))
                        .flex()
                        .flex_col()
                        .children(gutter_rows),
                ),
            );

        let viewport = self.viewport.clone();
        let list = div()
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            // 行を描く領域の大きさを、次の描画のために控えておく。
            .child(
                canvas(move |bounds, _, _| viewport.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .child(
                div()
                    .relative()
                    .top(px(-visible.offset))
                    .flex()
                    .flex_col()
                    .children(rows),
            );

        // ヘッダーと本体を同じ横幅の列にまとめ、外側で横スクロールさせる。
        let table = div()
            .flex()
            .flex_col()
            .h_full()
            .min_w(px(total_width))
            .w_full()
            .child(colno)
            .child(header)
            .child(list);

        // 初回の描画ではスクロール領域の寸法がまだ0なので、確定後にもう一度描画してバーを出す。
        if self.viewport.get().size.height <= px(0.) || self.h_scroll.bounds().size.width <= px(0.)
        {
            window.request_animation_frame();
        }

        let vbar = self.thumb_element(Axis::Vertical, cx);
        let hbar = self.thumb_element(Axis::Horizontal, cx);

        // 縦は自前で処理するので、横スクロールの要素が、縦のホイールを横スクロールとして
        // 解釈しないよう、軸を制限する。
        let mut h_scroll_area = div()
            .id("table-scroll")
            .size_full()
            .overflow_x_scroll()
            .track_scroll(&self.h_scroll)
            .child(table);
        h_scroll_area.style().restrict_scroll_to_axis = Some(true);

        div()
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_row()
            // 縦のホイールは、行番号の欄の上でも効くよう、全体で受ける。
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                let dy = f64::from(f32::from(event.delta.pixel_delta(px(ROW_HEIGHT)).y));
                if dy != 0.0 {
                    // 下へ送る向き（delta.y が負）で、位置が増える。
                    this.scroll = this.scroll.scrolled_by(-dy).clamped(this.max_scroll());
                    cx.notify();
                }
            }))
            .child(gutter)
            .child(div().flex_1().min_w_0().h_full().child(h_scroll_area))
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

/// 画面に描く行の範囲と、先頭行が画面の上端より上にはみ出している量（px）。
#[derive(Debug, PartialEq)]
struct VisibleRows {
    first: usize,
    last: usize,
    offset: f32,
}

/// スクロール位置から、描く行を求める。範囲外の位置は範囲内に丸める。
/// `offset` は、先頭行の中でのずれそのもの（0以上、行の高さ未満）。
fn visible_rows(scroll: ScrollPos, viewport_height: f32, row_count: usize) -> VisibleRows {
    let scroll = scroll.clamped(ScrollPos::max(row_count, viewport_height));
    let first = scroll.row.min(row_count);
    let offset = if scroll.row >= row_count { 0.0 } else { scroll.offset };
    // 先頭の行がはみ出しているぶん、領域の高さを覆うには1行多く要る。
    let count = (viewport_height / ROW_HEIGHT).ceil() as usize + 1;
    VisibleRows {
        first,
        last: first.saturating_add(count).min(row_count),
        offset,
    }
}

/// 行番号の欄の幅。行数の桁数に合わせる（桁が増えるときだけ広がる）。
fn gutter_width(row_count: usize) -> f32 {
    let digits = row_count.max(1).to_string().len();
    (digits as f32 * CHAR_WIDTH + CELL_PADDING).max(MIN_GUTTER_WIDTH)
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
            .on_action(cx.listener(|this, _: &ScrollLineUp, _, cx| this.navigate(Nav::LineUp, cx)))
            .on_action(cx.listener(|this, _: &ScrollLineDown, _, cx| this.navigate(Nav::LineDown, cx)))
            .on_action(cx.listener(|this, _: &ScrollPageUp, _, cx| this.navigate(Nav::PageUp, cx)))
            .on_action(cx.listener(|this, _: &ScrollPageDown, _, cx| this.navigate(Nav::PageDown, cx)))
            .on_action(cx.listener(|this, _: &ScrollToTop, _, cx| this.navigate(Nav::Top, cx)))
            .on_action(cx.listener(|this, _: &ScrollToBottom, _, cx| this.navigate(Nav::Bottom, cx)))
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
        KeyBinding::new("up", ScrollLineUp, None),
        KeyBinding::new("down", ScrollLineDown, None),
        KeyBinding::new("pageup", ScrollPageUp, None),
        KeyBinding::new("pagedown", ScrollPageDown, None),
        KeyBinding::new("home", ScrollToTop, None),
        KeyBinding::new("end", ScrollToBottom, None),
        KeyBinding::new("cmd-up", ScrollToTop, None),
        KeyBinding::new("cmd-down", ScrollToBottom, None),
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

#[cfg(test)]
mod tests {
    use super::*;
    use lazy_table::LazyTable;

    fn lazy(text: &str) -> (Loaded, IndexJob) {
        let (table, job) = LazyTable::open(Arc::new(text.as_bytes().to_vec()), None);
        (Loaded::new(Table::Lazy(table)), job)
    }

    #[test]
    fn widths_are_recomputed_only_when_the_sample_doubles_or_indexing_finishes() {
        let text: String = (0..700).map(|i| format!("{i},name{i}\n")).collect();
        let (mut loaded, mut job) = lazy(&format!("id,name\n{text}"));

        let mut recomputed = 0;
        loop {
            let more = job.step(64);
            if loaded.refresh_widths() {
                recomputed += 1;
            }
            if !more {
                break;
            }
        }
        // 約700行を、64バイトずつ数百回の更新で読んでも、再計算は対数回に収まる。
        assert!(recomputed <= 14, "recomputed {recomputed} times");
        assert_eq!(loaded.sampled_rows, 700);
        // 最終的な列幅は、全行を見た場合と同じ。
        let (expected, _) = column_widths(&loaded.table);
        assert_eq!(loaded.col_widths, expected);
        assert!(!loaded.refresh_widths(), "nothing changes after indexing is done");
    }

    #[test]
    fn widths_follow_columns_that_appear_later() {
        let (mut loaded, mut job) = lazy("a\n1\n2\n3,4,5\n");
        job.step(4);
        loaded.refresh_widths();
        while job.step(4) {
            loaded.refresh_widths();
        }
        loaded.refresh_widths();
        assert_eq!(loaded.col_widths.len(), 3);
    }

    #[test]
    fn eager_tables_never_need_a_recompute() {
        let table = Table::Eager(csv_data::CsvTable::from_bytes(b"a,b\n1,2\n").unwrap());
        let mut loaded = Loaded::new(table);
        assert!(!loaded.refresh_widths());
    }

    // ---- 縦スクロールの描画範囲 ----

    fn pos(row: usize, offset: f32) -> ScrollPos {
        ScrollPos { row, offset }
    }

    #[test]
    fn visible_rows_at_the_top() {
        assert_eq!(
            visible_rows(ScrollPos::TOP, 100.0, 1000),
            VisibleRows { first: 0, last: 6, offset: 0.0 }
        );
    }

    #[test]
    fn visible_rows_with_an_offset_into_the_first_row() {
        assert_eq!(
            visible_rows(pos(2, 2.0), 100.0, 1000),
            VisibleRows { first: 2, last: 8, offset: 2.0 }
        );
    }

    #[test]
    fn visible_rows_stop_at_the_last_row() {
        let max = ScrollPos::max(10, 100.0);
        let v = visible_rows(max, 100.0, 10);
        assert_eq!(v.last, 10);
        assert!(v.first < 10);
        assert_eq!(
            visible_rows(ScrollPos::TOP, 100.0, 0),
            VisibleRows { first: 0, last: 0, offset: 0.0 }
        );
    }

    #[test]
    fn visible_rows_clamp_an_out_of_range_position() {
        let v = visible_rows(pos(5_000, 10.0), 100.0, 100);
        assert_eq!(v, visible_rows(ScrollPos::max(100, 100.0), 100.0, 100));
    }

    /// 全体の座標を `f32` で持つと、約3000万行で隣り合う行の位置が衝突する（今回の不具合）。
    #[test]
    fn global_f32_positions_collide_for_tens_of_millions_of_rows() {
        let rows = 30_466_333usize;
        let top_of = |ix: usize| (ix as f32) * ROW_HEIGHT;
        let collisions = (rows - 100..rows)
            .filter(|&ix| top_of(ix) == top_of(ix + 1))
            .count();
        assert!(collisions > 0, "f32 positions should collide near the end");
    }

    /// 先頭行からの相対位置で描けば、同じ行数でも衝突せず、行の高さちょうどに並ぶ。
    #[test]
    fn relative_positions_stay_exact_for_tens_of_millions_of_rows() {
        for rows in [1_000_000usize, 30_466_333, 500_000_000, 5_000_000_000_000] {
            for viewport in [300.0f32, 700.0, 2160.0] {
                let max = ScrollPos::max(rows, viewport);
                for scroll in [
                    ScrollPos::TOP,
                    pos(rows / 3, 7.5),
                    ScrollPos::from_fraction(0.5, max),
                    pos(max.row.saturating_sub(1), 20.0),
                    max,
                ] {
                    let v = visible_rows(scroll, viewport, rows);
                    assert!(v.last <= rows);
                    assert!((0.0..ROW_HEIGHT).contains(&v.offset), "offset {}", v.offset);
                    // 画面上の各行の上端は、行の高さちょうどの間隔で並ぶ。
                    let tops: Vec<f32> = (v.first..v.last)
                        .map(|ix| (ix - v.first) as f32 * ROW_HEIGHT - v.offset)
                        .collect();
                    for pair in tops.windows(2) {
                        assert_eq!(pair[1] - pair[0], ROW_HEIGHT);
                    }
                    // 末尾以外では、画面を覆う。
                    if v.last < rows {
                        let bottom = tops.last().unwrap() + ROW_HEIGHT;
                        assert!(bottom >= viewport, "rows must cover the viewport");
                    }
                }
                // 末尾では、最後の行の下端が、領域の下端にちょうど揃う。
                let v = visible_rows(max, viewport, rows);
                assert_eq!(v.last, rows);
                let bottom = (v.last - v.first) as f32 * ROW_HEIGHT - v.offset;
                assert!((bottom - viewport).abs() < 1e-2, "{bottom} vs {viewport}");
            }
        }
    }

    #[test]
    fn the_row_number_gutter_grows_with_the_number_of_digits() {
        assert_eq!(gutter_width(0), MIN_GUTTER_WIDTH);
        assert_eq!(gutter_width(9), gutter_width(1));
        assert_eq!(gutter_width(99), gutter_width(10));
        assert!(gutter_width(1_000_000) > gutter_width(999));
        assert_eq!(
            gutter_width(30_466_333),
            8.0 * CHAR_WIDTH + CELL_PADDING,
            "eight digits"
        );
    }
}

