mod cell_popup;
mod csv_data;
mod encodings;
mod goto;
mod lazy_table;
mod matcher;
mod open_url;
mod record_index;
mod reveal;
mod scroll_pos;
mod scrollbar;
mod search;
mod selection;
mod table;
mod text_input;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    App, Application, Bounds, ClipboardItem, Context, ExternalPaths, FocusHandle, KeyBinding, KeyDownEvent, Menu, MenuItem,
    MouseButton, MouseDownEvent, MouseMoveEvent, PathPromptOptions, Pixels, Point, ScrollHandle,
    Entity, Rgba, ScrollWheelEvent, Subscription, Task, Window, WindowBounds, WindowOptions, actions, canvas, div, point,
    prelude::*, px, rgb, rgba, size,
};

use lazy_table::{IndexJob, IndexStatus};
use goto::{GotoInput, GotoKey};
use matcher::{Direction, Matcher, Outcome};
use scroll_pos::{Nav, ScrollPos};
use search::Match;
use selection::{CellPos, Selection};
use scrollbar::{Axis, AxisState};
use table::Table;
use text_input::{TextInput, TextInputEvent};

actions!(
    csv_viewer,
    [
        Quit,
        CloseWindow,
        OpenFile,
        GotoRow,
        FindInTable,
        FindNext,
        FindPrevious,
        CopySelection,
        SelectAllCells,
        ScrollLineUp,
        ScrollLineDown,
        ScrollPageUp,
        ScrollPageDown,
        ScrollToTop,
        ScrollToBottom,
    ]
);

const ROW_HEIGHT: f32 = 24.0;
const MENU_WIDTH: f32 = 200.0;
/// 検索で一致したセル。画面に見えているものだけを、薄い黄色にする。
const SEARCH_HIT_BG: Rgba = Rgba {
    r: 1.0,
    g: 0.95,
    b: 0.69,
    a: 1.0,
};
/// 検索の現在の一致。
const SEARCH_CURRENT_BG: Rgba = Rgba {
    r: 1.0,
    g: 0.70,
    b: 0.28,
    a: 1.0,
};
/// 一致する行を数える上限。巨大なファイルで、数えきるのに、時間がかかりすぎないように。
const COUNT_CAP: usize = 100_000;
const MENU_ITEM_HEIGHT: f32 = 26.0;
/// ポップアップを出しているセルの背景色。
const SELECTED_CELL_BG: Rgba = Rgba {
    r: 0.81,
    g: 0.89,
    b: 0.98,
    a: 1.0,
};
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

/// クリックしたセルの全文を出すポップアップ。
struct CellPopup {
    /// データ行（0始まり）。ヘッダーのセルなら `None`。
    row: Option<usize>,
    col: usize,
    /// クリックした位置（ウィンドウ座標）。ポップアップはこの近くに出す。
    anchor: Point<Pixels>,
    /// 表示する文字列（長すぎる場合は切り詰め済み）。
    text: String,
    /// 切り詰める前の文字数。
    total_chars: usize,
}

/// 検索の状態。検索語が入力されているあいだ、持つ。
struct SearchState {
    /// 画面に見えているセルの強調に使う。
    matcher: Matcher,
    needle: String,
    /// 現在の一致。
    current: Option<Match>,
    /// 一致する行の数と、上限で打ち切ったか。数えおわるまでは `None`。
    count: Option<(usize, bool)>,
    /// 背景の検索の結果を、待っている。
    busy: bool,
}

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
    /// 「行へ移動」の入力欄。開いているあいだだけ `Some`。
    goto: Option<GotoInput>,
    /// クリックしたセルの全文を出すポップアップ。
    popup: Option<CellPopup>,
    /// 選択範囲。
    selection: Option<Selection>,
    /// マウスのボタンを押して、選択範囲を広げているところ。
    selecting: bool,
    /// 検索バーの入力欄。最初に開いたときに作る。
    search_input: Option<Entity<TextInput>>,
    search_subscription: Option<Subscription>,
    search_open: bool,
    search_state: Option<SearchState>,
    /// 実行中の検索を中断するための旗。新しい検索を始めるたびに、前の旗を立てて、作り直す。
    search_cancel: Arc<AtomicBool>,
    search_task: Option<Task<()>>,
    count_cancel: Arc<AtomicBool>,
    count_task: Option<Task<()>>,
    /// 文字コードの切り替えメニュー。開いているあいだ、クリックした位置を持つ。
    encoding_menu: Option<Point<Pixels>>,
    /// いま開いているファイルのパス。文字コードを変えて開き直すときに使う。
    path: Option<PathBuf>,
    /// 手動で指定した文字コード。`None` は自動判定。
    forced_encoding: Option<&'static encoding_rs::Encoding>,
    /// ステータスバーに出す通知（範囲外の行への移動など）。次の操作で消える。
    notice: Option<String>,
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
            goto: None,
            popup: None,
            selection: None,
            selecting: false,
            search_input: None,
            search_subscription: None,
            search_open: false,
            search_state: None,
            search_cancel: Arc::new(AtomicBool::new(false)),
            search_task: None,
            count_cancel: Arc::new(AtomicBool::new(false)),
            count_task: None,
            encoding_menu: None,
            path: None,
            forced_encoding: None,
            notice: None,
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
        // 新しいファイルは、自動判定で開く。
        self.open_with(path, false, None, window, cx);
    }

    /// `force_eager` なら、サイズに関わらず全体を読み込んで開く。
    /// `forced` を指定すると、文字コードの自動判定をせずその文字コードで読む。
    fn open_with(
        &mut self,
        path: PathBuf,
        force_eager: bool,
        forced: Option<&'static encoding_rs::Encoding>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading = Some(file_name(&path));
        self.error = None;
        self.notice = None;

        let load = cx.background_spawn({
            let path = path.clone();
            async move {
                let (table, job) = if force_eager {
                    (Table::load_eager(&path, forced)?, None)
                } else {
                    Table::open(&path, forced)?
                };
                anyhow::Ok((Loaded::new(table), job))
            }
        });
        // 古い Task は代入時に drop され、キャンセルされる。
        self.load_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = load.await;
            this.update_in(cx, |this, window, cx| {
                this.finish_load(&path, forced, result, window, cx)
            })
            .ok();
        }));
        cx.notify();
    }

    /// 読み込み結果を反映する。失敗時は直前の表示を残し、メッセージだけ出す。
    fn finish_load(
        &mut self,
        path: &Path,
        forced: Option<&'static encoding_rs::Encoding>,
        result: anyhow::Result<(Loaded, Option<IndexJob>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading = None;
        match result {
            Ok((loaded, job)) => {
                self.stop_indexing();
                self.goto = None;
                self.popup = None;
                self.encoding_menu = None;
                self.clear_search();
                self.search_open = false;
                self.selection = None;
                self.selecting = false;
                self.notice = None;
                self.path = Some(path.to_path_buf());
                self.forced_encoding = forced;
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
        self.open_with(path.to_path_buf(), true, self.forced_encoding, window, cx);
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
        self.notice = None;
        self.popup = None;
        let viewport_height = f32::from(self.viewport.get().size.height);
        self.scroll = self
            .scroll
            .navigated(nav, self.max_scroll(), viewport_height);
        cx.notify();
    }

    /// 文字コードを `forced` に変えて、同じファイルを開き直す。`None` は自動判定に戻す。
    fn reopen_with_encoding(
        &mut self,
        forced: Option<&'static encoding_rs::Encoding>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.encoding_menu = None;
        let Some(path) = self.path.clone() else {
            return;
        };
        self.open_with(path, false, forced, window, cx);
    }

    /// 文字コードの切り替えメニューを開く。開いているときは閉じる。
    fn toggle_encoding_menu(&mut self, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        self.popup = None;
        self.encoding_menu = match self.encoding_menu {
            Some(_) => None,
            None => Some(anchor),
        };
        cx.notify();
    }

    /// セルのクリックを受ける。同じセルをもう一度クリックすると閉じる。
    fn open_cell_popup(
        &mut self,
        row: Option<usize>,
        col: usize,
        anchor: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        // 全体のクリックで閉じる処理（ルート要素）には、渡さない。
        cx.stop_propagation();
        if self.popup.as_ref().is_some_and(|p| p.row == row && p.col == col) {
            self.popup = None;
            cx.notify();
            return;
        }
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let (text, total_chars) = {
            let full = match row {
                Some(row) => loaded.table.cell(row, col),
                None => loaded.table.header(col),
            };
            cell_popup::truncate_chars(&full, cell_popup::MAX_CHARS)
        };
        self.popup = Some(CellPopup {
            row,
            col,
            anchor,
            text,
            total_chars,
        });
        cx.notify();
    }

    /// 本体のセルを押した。クリックで選択、`Shift+クリック`で範囲を広げる、ダブルクリックで全文を出す。
    fn press_cell(
        &mut self,
        row: usize,
        col: usize,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        // 全体のクリックで閉じる処理（ルート要素）には、渡さない。
        cx.stop_propagation();
        self.popup = None;
        self.encoding_menu = None;
        self.notice = None;
        let pos = CellPos { row, col };
        if event.click_count >= 2 {
            self.selection = Some(Selection::cell(pos));
            self.open_cell_popup(Some(row), col, event.position, cx);
            return;
        }
        self.selecting = true;
        self.selection = Some(match (event.modifiers.shift, self.selection) {
            (true, Some(current)) => current.extended_to(pos),
            _ => Selection::cell(pos),
        });
        cx.notify();
    }

    /// ボタンを押したまま、セルの上を通った。選択範囲を広げる。
    fn drag_cell(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        let Some(current) = self.selection else {
            return;
        };
        let extended = current.extended_to(CellPos { row, col });
        if extended != current {
            self.selection = Some(extended);
            cx.notify();
        }
    }

    /// 行番号を押した。その行全体を選ぶ。`Shift+クリック`で、行の範囲を広げる。
    fn press_row_number(&mut self, row: usize, event: &MouseDownEvent, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.popup = None;
        self.encoding_menu = None;
        self.notice = None;
        self.selecting = true;
        self.selection = Some(match (event.modifiers.shift, self.selection) {
            (true, Some(current)) => current.extended_to(CellPos { row, col: 0 }),
            _ => Selection::whole_row(row),
        });
        cx.notify();
    }

    fn select_all_cells(&mut self, cx: &mut Context<Self>) {
        let rows = self.row_count();
        self.selection = Selection::all_rows(rows);
        self.notice = None;
        cx.notify();
    }

    /// 選択範囲を、タブ区切りでコピーする。ポップアップが開いているときは、そのセルの全文を
    /// コピーする（ポップアップに出ている、切り詰めた文字列ではなく）。
    fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        if let Some(popup) = self.popup.as_ref() {
            let text = match popup.row {
                Some(row) => loaded.table.cell(row, popup.col),
                None => loaded.table.header(popup.col),
            }
            .into_owned();
            let chars = text.chars().count();
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.notice = Some(format!("セルの全文をコピーしました（{chars} 文字）"));
            cx.notify();
            return;
        }
        let Some(selection) = self.selection else {
            return;
        };
        let columns = loaded.table.column_count();
        let cells = selection.cell_count(columns);
        if cells > selection::COPY_CAP_CELLS {
            self.notice = Some(format!(
                "選択が大きすぎて、コピーできません（{cells} セル。上限は {} セル）",
                selection::COPY_CAP_CELLS
            ));
            cx.notify();
            return;
        }
        let text = selection::to_tsv(selection.rows(), selection.cols(columns), |row, col| {
            loaded.table.cell(row, col)
        });
        let (rows, cols) = selection.dimensions(columns);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.notice = Some(format!("{rows} 行 × {cols} 列をコピーしました"));
        cx.notify();
    }

    /// `Enter`：選択している（現在位置の）セルの全文を、ポップアップで出す。
    fn open_popup_for_selection(&mut self, cx: &mut Context<Self>) {
        let Some(selection) = self.selection else {
            return;
        };
        let head = selection.head();
        // 見えない位置にあるなら、見える位置までスクロールする。
        self.reveal(Match {
            row: head.row,
            col: head.col,
        });
        let Some(anchor) = self.cell_anchor(head.row, head.col) else {
            return;
        };
        self.popup = None;
        self.open_cell_popup(Some(head.row), head.col, anchor, cx);
    }

    /// 画面に見えているセルの中心の位置（ウィンドウ座標）。ポップアップを出す位置に使う。
    fn cell_anchor(&self, row: usize, col: usize) -> Option<Point<Pixels>> {
        let loaded = self.loaded.as_ref()?;
        let bounds = self.viewport.get();
        let viewport_height = match f32::from(bounds.size.height) {
            h if h > 0.0 => h,
            _ => DEFAULT_VIEWPORT_HEIGHT,
        };
        let visible = visible_rows(self.scroll, viewport_height, loaded.table.row_count());
        if row < visible.first || row >= visible.last {
            return None;
        }
        let widths = &loaded.col_widths;
        let left: f32 = widths.iter().take(col).sum();
        let width = widths.get(col).copied().unwrap_or(0.0);
        // 行を描く領域の左端は、横スクロールのぶん、ずれた位置にあるので、そのまま足してよい。
        let x = bounds.origin.x + px(left + width / 2.0);
        let y = bounds.origin.y
            + px((row - visible.first) as f32 * ROW_HEIGHT - visible.offset + ROW_HEIGHT / 2.0);
        Some(point(x, y))
    }

    fn cell_press(
        cx: &Context<Self>,
        row: usize,
        col: usize,
    ) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
            this.press_cell(row, col, event, cx)
        })
    }

    fn cell_drag(
        cx: &Context<Self>,
        row: usize,
        col: usize,
    ) -> impl Fn(&MouseMoveEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
            if event.dragging() {
                this.drag_cell(row, col, cx);
            }
        })
    }

    fn row_number_press(
        cx: &Context<Self>,
        row: usize,
    ) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
            this.press_row_number(row, event, cx)
        })
    }

    fn row_number_drag(
        cx: &Context<Self>,
        row: usize,
    ) -> impl Fn(&MouseMoveEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
            if event.dragging() {
                this.drag_cell(row, 0, cx);
            }
        })
    }

    /// セルのクリックを `open_cell_popup` へつなぐリスナー。
    fn cell_click(
        cx: &Context<Self>,
        row: Option<usize>,
        col: usize,
    ) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
            this.open_cell_popup(row, col, event.position, cx)
        })
    }

    /// 検索バーを開いて、入力欄にフォーカスを移す。
    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loaded.is_none() {
            return;
        }
        self.popup = None;
        self.encoding_menu = None;
        self.goto = None;
        let input = match &self.search_input {
            Some(input) => input.clone(),
            None => {
                let input = cx.new(|cx| TextInput::new("検索", cx));
                self.search_subscription = Some(cx.subscribe_in(
                    &input,
                    window,
                    |this, _, event: &TextInputEvent, window, cx| {
                        this.on_search_event(event, window, cx)
                    },
                ));
                self.search_input = Some(input.clone());
                input
            }
        };
        self.search_open = true;
        input.read(cx).focus(window);
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = false;
        self.clear_search();
        self.notice = None;
        self.focus_handle.focus(window);
        cx.notify();
    }

    /// 検索の状態と、実行中の検索を、すべて捨てる。
    fn clear_search(&mut self) {
        self.search_cancel.store(true, Ordering::Relaxed);
        self.count_cancel.store(true, Ordering::Relaxed);
        self.search_task = None;
        self.count_task = None;
        self.search_state = None;
    }

    fn on_search_event(
        &mut self,
        event: &TextInputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TextInputEvent::Cancel => self.close_search(window, cx),
            TextInputEvent::Changed => self.on_search_text_changed(window, cx),
            TextInputEvent::Confirm { backward } => {
                let dir = if *backward {
                    Direction::Backward
                } else {
                    Direction::Forward
                };
                self.search_step(dir, window, cx);
            }
        }
    }

    /// 検索語が変わった（文字が確定した）。今の一致が、まだ一致していれば、そこに留まる。
    fn on_search_text_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let needle = self
            .search_input
            .as_ref()
            .map(|input| input.read(cx).text().to_string())
            .unwrap_or_default();
        let kept = self.search_state.as_ref().and_then(|s| s.current);
        self.clear_search();
        self.notice = None;
        let Some(matcher) = Matcher::new(&needle) else {
            cx.notify();
            return;
        };
        self.search_state = Some(SearchState {
            matcher,
            needle,
            current: kept,
            count: None,
            busy: false,
        });
        let columns = self
            .loaded
            .as_ref()
            .map_or(0, |l| l.table.column_count());
        // 「今の一致の1つ前から」前向きに探すと、今の一致自身も、探す対象に入る。
        let start = match kept {
            Some(current) => current.previous_cell(columns),
            None => self.top_of_view().previous_cell(columns),
        };
        self.run_search(start, Direction::Forward, window, cx);
        self.start_count(window, cx);
    }

    /// 画面の先頭に見えている行の、先頭のセル。
    fn top_of_view(&self) -> Match {
        Match {
            row: self.scroll.row,
            col: 0,
        }
    }

    /// 次（または前）の一致へ。一致がまだなければ、画面の先頭から探す。
    fn search_step(&mut self, dir: Direction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.search_state.as_ref() else {
            return;
        };
        let start = state.current.or_else(|| match dir {
            Direction::Forward => {
                let columns = self.loaded.as_ref().map_or(0, |l| l.table.column_count());
                self.top_of_view().previous_cell(columns)
            }
            Direction::Backward => Some(self.top_of_view()),
        });
        self.notice = None;
        self.run_search(start, dir, window, cx);
    }

    /// `start` の次（後ろ向きなら前）の一致を探す。見つからなければ、反対の端から探し直す（折り返し）。
    /// 全読み込みの表は、その場で探す。遅延読み込みの表は、背景のスレッドで探す。
    fn run_search(
        &mut self,
        start: Option<Match>,
        dir: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_cancel.store(true, Ordering::Relaxed);
        self.search_task = None;
        let flag = Arc::new(AtomicBool::new(false));
        self.search_cancel = flag.clone();

        let (Some(state), Some(loaded)) = (self.search_state.as_mut(), self.loaded.as_ref()) else {
            return;
        };
        let table = &loaded.table;
        let can_wrap = start.is_some();

        // まず、同じ行の中の、隣のセルを調べる（どちらの方式でも、すぐ終わる）。
        if let Some(found) = start.and_then(|m| search::same_row_step(table, &state.matcher, m, dir))
        {
            state.busy = false;
            self.apply_search(Outcome::Found(found), false, dir, cx);
            return;
        }

        match table.lazy_searcher() {
            None => {
                let outcome = search::find_match(table, &state.matcher, start, dir, &flag);
                let (outcome, wrapped) = match outcome {
                    Outcome::NotFound if can_wrap => {
                        (search::find_match(table, &state.matcher, None, dir, &flag), true)
                    }
                    other => (other, false),
                };
                state.busy = false;
                self.apply_search(outcome, wrapped, dir, cx);
            }
            Some(searcher) => {
                state.busy = true;
                let needle = state.needle.clone();
                let from_row = search::first_row_to_search(start, dir, table.row_count());
                let wrap_row = match dir {
                    Direction::Forward => 0,
                    Direction::Backward => table.row_count(),
                };
                let task = cx.background_spawn(async move {
                    let Some(matcher) = Matcher::new(&needle) else {
                        return (Outcome::NotFound, false);
                    };
                    match searcher.find_row(&matcher, from_row, dir, &flag) {
                        Outcome::NotFound if can_wrap => {
                            (searcher.find_row(&matcher, wrap_row, dir, &flag), true)
                        }
                        other => (other, false),
                    }
                });
                self.search_task = Some(cx.spawn_in(window, async move |this, cx| {
                    let (outcome, wrapped) = task.await;
                    this.update_in(cx, |this, _, cx| {
                        this.finish_row_search(outcome, wrapped, dir, cx)
                    })
                    .ok();
                }));
                cx.notify();
            }
        }
    }

    /// 背景で見つけた行の中から、一致するセルの列を決めて、結果を反映する。
    fn finish_row_search(
        &mut self,
        outcome: Outcome<usize>,
        wrapped: bool,
        dir: Direction,
        cx: &mut Context<Self>,
    ) {
        let (Some(state), Some(loaded)) = (self.search_state.as_mut(), self.loaded.as_ref()) else {
            return;
        };
        state.busy = false;
        let outcome = match outcome {
            Outcome::Found(row) => {
                match search::match_col_in_row(&loaded.table, &state.matcher, row, dir) {
                    Some(col) => Outcome::Found(Match { row, col }),
                    None => Outcome::NotFound,
                }
            }
            Outcome::NotFound => Outcome::NotFound,
            Outcome::Cancelled => Outcome::Cancelled,
        };
        self.apply_search(outcome, wrapped, dir, cx);
    }

    /// 検索の結果を、画面に反映する。
    fn apply_search(
        &mut self,
        outcome: Outcome<Match>,
        wrapped: bool,
        dir: Direction,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Outcome::Found(found) => {
                if let Some(state) = self.search_state.as_mut() {
                    state.current = Some(found);
                }
                self.notice = wrapped.then(|| match dir {
                    Direction::Forward => "末尾まで検索したので、先頭に戻りました".to_string(),
                    Direction::Backward => "先頭まで検索したので、末尾に戻りました".to_string(),
                });
                self.reveal(found);
            }
            Outcome::NotFound => {
                if let Some(state) = self.search_state.as_mut() {
                    state.current = None;
                }
            }
            Outcome::Cancelled => {}
        }
        cx.notify();
    }

    /// 一致したセルが、画面の中に見えるように、スクロールする（縦も横も）。
    fn reveal(&mut self, found: Match) {
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let viewport_height = match f32::from(self.viewport.get().size.height) {
            h if h > 0.0 => h,
            _ => DEFAULT_VIEWPORT_HEIGHT,
        };
        let visible = visible_rows(self.scroll, viewport_height, loaded.table.row_count());
        // 先頭の行が欠けているときと、最後の行は、全体が見えているとは限らない。
        let start = visible.first + usize::from(visible.offset > 0.0);
        let end = visible.last.saturating_sub(1).max(start);
        let scroll = reveal::reveal_row(self.scroll, start..end, found.row, 3);
        let widths = &loaded.col_widths;
        let left: f32 = widths.iter().take(found.col).sum();
        let right = left + widths.get(found.col).copied().unwrap_or(0.0);
        let current_x = -f32::from(self.h_scroll.offset().x);
        let target_x = reveal::reveal_column(
            current_x,
            f32::from(self.h_scroll.bounds().size.width),
            left,
            right,
            f32::from(self.h_scroll.max_offset().width),
        );
        self.scroll = scroll.clamped(self.max_scroll());
        self.h_scroll
            .set_offset(point(px(-target_x), self.h_scroll.offset().y));
    }

    /// 一致する行の数を、数える。全読み込みの表は、その場で。遅延読み込みの表は、背景で。
    fn start_count(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.count_cancel.store(true, Ordering::Relaxed);
        self.count_task = None;
        let flag = Arc::new(AtomicBool::new(false));
        self.count_cancel = flag.clone();

        let (Some(state), Some(loaded)) = (self.search_state.as_mut(), self.loaded.as_ref()) else {
            return;
        };
        match loaded.table.lazy_searcher() {
            None => {
                state.count = match search::count_rows(&loaded.table, &state.matcher, COUNT_CAP, &flag)
                {
                    Outcome::Found(counted) => Some(counted),
                    _ => None,
                };
            }
            Some(searcher) => {
                let needle = state.needle.clone();
                let task = cx.background_spawn(async move {
                    let matcher = Matcher::new(&needle)?;
                    let mut count = 0usize;
                    let mut next_row = 0usize;
                    loop {
                        match searcher.find_row(&matcher, next_row, Direction::Forward, &flag) {
                            Outcome::Found(row) => {
                                count += 1;
                                if count >= COUNT_CAP {
                                    return Some((count, true));
                                }
                                next_row = row + 1;
                            }
                            Outcome::NotFound => return Some((count, false)),
                            Outcome::Cancelled => return None,
                        }
                    }
                });
                let needle = state.needle.clone();
                self.count_task = Some(cx.spawn_in(window, async move |this, cx| {
                    let counted = task.await;
                    this.update_in(cx, move |this, _, cx| {
                        if let Some(state) = this.search_state.as_mut() {
                            // 数えているあいだに、検索語が変わっていたら、古い結果は捨てる。
                            if state.needle == needle {
                                state.count = counted;
                                cx.notify();
                            }
                        }
                    })
                    .ok();
                }));
            }
        }
        cx.notify();
    }

    /// 「行へ移動」の入力欄を開く。表が読み込まれていないときは何もしない。
    fn open_goto(&mut self, cx: &mut Context<Self>) {
        if self.loaded.is_some() {
            self.goto = Some(GotoInput::default());
            cx.notify();
        }
    }

    /// 入力欄が開いているときのキー入力を処理する。
    fn handle_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(input) = self.goto.as_mut() else {
            let modifiers = &event.keystroke.modifiers;
            let plain = !(modifiers.control || modifiers.alt || modifiers.platform || modifiers.shift);
            match event.keystroke.key.as_str() {
                // `Esc`：開いているものを閉じる。何も開いていなければ、選択を解く。
                "escape" => {
                    if self.popup.is_some() || self.encoding_menu.is_some() {
                        self.popup = None;
                        self.encoding_menu = None;
                        cx.stop_propagation();
                        cx.notify();
                    } else if self.selection.is_some() {
                        self.selection = None;
                        cx.stop_propagation();
                        cx.notify();
                    }
                }
                // `Enter`：選択しているセルの全文を、ポップアップで出す。
                "enter" if plain && self.popup.is_none() && self.selection.is_some() => {
                    self.open_popup_for_selection(cx);
                    cx.stop_propagation();
                }
                _ => {}
            }
            return;
        };
        let keystroke = &event.keystroke;
        let modifiers = &keystroke.modifiers;
        let has_command_modifier =
            modifiers.control || modifiers.alt || modifiers.platform || modifiers.function;
        match goto::classify(&keystroke.key, keystroke.key_char.as_deref(), has_command_modifier) {
            GotoKey::Digit(digit) => input.push_digit(digit),
            GotoKey::Backspace => input.backspace(),
            GotoKey::Escape => self.goto = None,
            GotoKey::Enter => {
                let value = input.value();
                self.goto = None;
                if let Some(value) = value {
                    self.jump_to_row(value);
                }
            }
            GotoKey::Ignore => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// データ行 `number`（1始まり）へ移動する。範囲外なら、行ける所まで行って理由を知らせる。
    fn jump_to_row(&mut self, number: u64) {
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let indexing = loaded.table.indexing_percent().is_some();
        let resolved = goto::resolve(number, loaded.table.row_count(), indexing);
        self.scroll = ScrollPos {
            row: resolved.row,
            offset: 0.0,
        }
        .clamped(self.max_scroll());
        self.notice = resolved.notice;
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
        // ポップアップを出しているセル。目立たせる。
        let popup_cell = self.popup.as_ref().map(|p| (p.row, p.col));
        // 検索の一致を、画面に見えているセルだけ、強調する。
        let search_matcher = self.search_state.as_ref().map(|s| &s.matcher);
        let search_current = self.search_state.as_ref().and_then(|s| s.current);
        let selection = self.selection;
        let column_count = loaded.table.column_count();
        let header = div()
            .flex()
            .flex_row()
            .h(px(ROW_HEIGHT))
            .bg(rgb(0xf0f0f0))
            .border_b_1()
            .border_color(rgb(0xcccccc))
            .font_weight(gpui::FontWeight::BOLD)
            .children(loaded.col_widths.iter().enumerate().map(|(i, w)| {
                cell(loaded.table.header(i).to_string(), *w)
                    .when(popup_cell == Some((None, i)), |d| d.bg(SELECTED_CELL_BG))
                    .on_mouse_down(MouseButton::Left, Self::cell_click(cx, None, i))
            }));
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
                    let text = loaded.table.cell(ix, i).to_string();
                    // 強調の優先順位：選択範囲、検索の現在の一致、検索の一致。
                    let background = if selection.is_some_and(|s| s.contains(ix, i, column_count)) {
                        Some(SELECTED_CELL_BG)
                    } else if search_current == Some(Match { row: ix, col: i }) {
                        Some(SEARCH_CURRENT_BG)
                    } else if search_matcher.is_some_and(|m| m.is_match(&text)) {
                        Some(SEARCH_HIT_BG)
                    } else {
                        None
                    };
                    cell(text, *w)
                        .when_some(background, |d, bg| d.bg(bg))
                        .on_mouse_down(MouseButton::Left, Self::cell_press(cx, ix, i))
                        .on_mouse_move(Self::cell_drag(cx, ix, i))
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
                .bg(if selection.is_some_and(|s| s.rows().contains(&ix)) {
                    rgb(0xd6e6fb)
                } else {
                    rgb(if ix % 2 == 1 { 0xf0f0f0 } else { 0xf6f6f6 })
                })
                .on_mouse_down(MouseButton::Left, Self::row_number_press(cx, ix))
                .on_mouse_move(Self::row_number_drag(cx, ix))
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
                    this.notice = None;
                    this.popup = None;
                    this.scroll = this.scroll.scrolled_by(-dy).clamped(this.max_scroll());
                    cx.notify();
                }
            }))
            .child(gutter)
            .child(div().flex_1().min_w_0().h_full().child(h_scroll_area))
            .children(vbar)
            .children(hbar)
    }

    /// 検索バー。表の右上に重ねて出す。
    fn render_search(&self) -> Option<impl IntoElement> {
        if !self.search_open {
            return None;
        }
        let input = self.search_input.clone()?;
        Some(
            div()
                .id("search-bar")
                .occlude()
                .absolute()
                .top(px(HEADER_HEIGHT + 8.0))
                .right(px(24.0))
                .w(px(340.0))
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .bg(rgb(0xf6f6f6))
                .border_1()
                .border_color(rgb(0xaaaaaa))
                .rounded_md()
                .shadow_md()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x888888))
                        .child("検索　Enter: 次 / Shift+Enter: 前 / Esc: 閉じる"),
                )
                .child(input)
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x555555))
                        .child(self.search_info()),
                ),
        )
    }

    /// 検索バーの下に出す、件数などの案内。
    fn search_info(&self) -> String {
        let Some(state) = self.search_state.as_ref() else {
            return String::new();
        };
        let indexing = self
            .loaded
            .as_ref()
            .is_some_and(|l| l.table.indexing_percent().is_some());
        search_info_text(
            state.busy,
            state.count,
            indexing,
            state.current.map(|m| m.row),
        )
    }

    /// クリックしたセルの全文。表の上に重ねて出す。
    fn render_popup(&self, window: &Window) -> Option<impl IntoElement> {
        let popup = self.popup.as_ref()?;
        let window_size = window.viewport_size();
        let (x, y) = cell_popup::place(
            (f32::from(popup.anchor.x), f32::from(popup.anchor.y)),
            (f32::from(window_size.width), f32::from(window_size.height)),
            (cell_popup::WIDTH, cell_popup::MAX_HEIGHT),
        );
        let title = match popup.row {
            Some(row) => format!("{} 行目 / {} 列目", row + 1, popup.col + 1),
            None => format!("ヘッダー / {} 列目", popup.col + 1),
        };
        let truncated = (popup.total_chars > cell_popup::MAX_CHARS).then(|| {
            format!(
                "先頭 {} 文字のみ表示（全体 {} 文字）",
                cell_popup::MAX_CHARS,
                popup.total_chars
            )
        });
        Some(
            div()
                .id("cell-popup")
                // 背後の表がマウスやホイールを受け取らないようにする。
                .occlude()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(cell_popup::WIDTH))
                .max_h(px(cell_popup::MAX_HEIGHT))
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .bg(rgb(0xffffff))
                .border_1()
                .border_color(rgb(0xaaaaaa))
                .rounded_md()
                .shadow_lg()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x888888))
                        .child(title),
                )
                .child(
                    div()
                        .id("cell-popup-body")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(popup.text.clone()),
                )
                .children(truncated.map(|note| {
                    div().text_xs().text_color(rgb(0xb06000)).child(note)
                })),
        )
    }

    /// 「行へ移動」の入力欄。表の上に重ねて出す。
    fn render_goto(&self) -> Option<impl IntoElement> {
        let input = self.goto.as_ref()?;
        let indexing = self
            .loaded
            .as_ref()
            .is_some_and(|l| l.table.indexing_percent().is_some());
        let range = format!(
            "1〜{}{}",
            self.row_count(),
            if indexing { "+" } else { "" }
        );
        Some(
            div()
                .absolute()
                .top(px(HEADER_HEIGHT + 8.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .w(px(300.0))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .p_3()
                        .rounded_md()
                        .bg(rgb(0xffffff))
                        .border_1()
                        .border_color(rgb(0xaaaaaa))
                        .shadow_md()
                        .child(format!("行へ移動（{range}）"))
                        .child(
                            div()
                                .h(px(28.0))
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded_sm()
                                .border_1()
                                .border_color(rgb(0x4a90d9))
                                .child(format!("{}▏", input.text())),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(0x888888))
                                .child("Enter で移動 / Esc でキャンセル"),
                        ),
                ),
        )
    }

    /// 文字コードの切り替えメニュー。ステータスバーの文字コード表示をクリックすると、真上に出る。
    fn render_encoding_menu(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let anchor = self.encoding_menu?;
        let choices = encodings::choices();
        let height = choices.len() as f32 * MENU_ITEM_HEIGHT + 10.0;
        let window_size = window.viewport_size();
        let (x, y) = cell_popup::place_above(
            (f32::from(anchor.x), f32::from(anchor.y)),
            (f32::from(window_size.width), f32::from(window_size.height)),
            (MENU_WIDTH, height),
        );
        let forced = self.forced_encoding;
        Some(
            div()
                .id("encoding-menu")
                // 背後の表がマウスやホイールを受け取らないようにする。
                .occlude()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(MENU_WIDTH))
                .flex()
                .flex_col()
                .py_1()
                .bg(rgb(0xffffff))
                .border_1()
                .border_color(rgb(0xaaaaaa))
                .rounded_md()
                .shadow_lg()
                .children(choices.into_iter().enumerate().map(|(i, choice)| {
                    let active = encodings::is_active(&choice, forced);
                    let target = choice.encoding;
                    div()
                        .id(("encoding-choice", i))
                        .h(px(MENU_ITEM_HEIGHT))
                        .px_3()
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .hover(|d| d.bg(rgb(0xe8f0fe)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                this.reopen_with_encoding(target, window, cx);
                            }),
                        )
                        .child(format!("{} {}", if active { "✓" } else { "  " }, choice.label))
                })),
        )
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let bar = div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(ROW_HEIGHT))
            .px_2()
            .bg(rgb(0xf0f0f0))
            .border_t_1()
            .border_color(rgb(0xcccccc))
            .text_sm();
        match (&self.error, &self.loading, &self.loaded) {
            (Some(error), _, _) => bar.text_color(rgb(0xb00020)).child(error.clone()),
            (None, Some(name), _) => bar.child(format!("読み込み中… {name}")),
            (None, None, Some(_)) if self.notice.is_some() => {
                bar.child(self.notice.clone().unwrap_or_default())
            }
            (None, None, Some(l)) => {
                let percent = l.table.indexing_percent();
                let head = format!(
                    "{}{} 行 / {} 列 / ",
                    l.table.row_count(),
                    if percent.is_some() { "+" } else { "" },
                    l.table.column_count(),
                );
                let label = encodings::status_label(l.table.encoding(), self.forced_encoding);
                let selected = self
                    .selection
                    .filter(|s| !s.is_single_cell(l.table.column_count()))
                    .map(|s| {
                        let (rows, cols) = s.dimensions(l.table.column_count());
                        format!(" / 選択 {rows} 行 × {cols} 列")
                    });
                bar.child(head)
                    .child(
                        div()
                            .id("encoding-label")
                            .px_1()
                            .rounded_sm()
                            .cursor_pointer()
                            .hover(|d| d.bg(rgb(0xdcdcdc)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                    // 全体のクリックで閉じる処理（ルート要素）には、渡さない。
                                    cx.stop_propagation();
                                    this.toggle_encoding_menu(event.position, cx);
                                }),
                            )
                            .child(label),
                    )
                    .children(percent.map(|p| format!(" / 索引作成中 {p}%")))
                    .children(selected)
            }
            (None, None, None) => bar,
        }
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

/// 検索バーの下に出す案内の文言。
///
/// - `busy`: 背景の検索の結果を待っている
/// - `count`: 一致する行の数と、上限で打ち切ったか。数えおわるまでは `None`
/// - `indexing`: 遅延読み込みの索引を作っている途中（この先に、まだ行が増える）
/// - `current_row`: 現在の一致の行（0始まり）
fn search_info_text(
    busy: bool,
    count: Option<(usize, bool)>,
    indexing: bool,
    current_row: Option<usize>,
) -> String {
    if busy {
        return "検索中…".to_string();
    }
    let summary = match count {
        Some((0, _)) if !indexing => return "見つかりません".to_string(),
        Some((n, capped)) => format!("{n}{} 行が一致", if capped || indexing { "+" } else { "" }),
        None => "数えています…".to_string(),
    };
    match current_row {
        Some(row) => format!("{summary} / 現在 {} 行目", row + 1),
        None => summary,
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

fn cell(text: String, width: f32) -> gpui::Div {
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
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0xffffff))
            .text_color(rgb(0x222222))
            .text_sm()
            .key_context(TABLE_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                this.handle_key_down(event, cx)
            }))
            .on_action(cx.listener(|this, _: &GotoRow, _, cx| this.open_goto(cx)))
            .on_action(cx.listener(|this, _: &FindInTable, window, cx| this.open_search(window, cx)))
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| this.copy_selection(cx)))
            .on_action(cx.listener(|this, _: &SelectAllCells, _, cx| this.select_all_cells(cx)))
            .on_action(cx.listener(|this, _: &FindNext, window, cx| {
                if this.search_state.is_some() {
                    this.search_step(Direction::Forward, window, cx);
                } else {
                    this.open_search(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FindPrevious, window, cx| {
                if this.search_state.is_some() {
                    this.search_step(Direction::Backward, window, cx);
                } else {
                    this.open_search(window, cx);
                }
            }))
            // セルとポップアップ以外のクリックで、ポップアップを閉じる。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    let closed_popup = this.popup.take().is_some();
                    let closed_menu = this.encoding_menu.take().is_some();
                    if closed_popup || closed_menu {
                        cx.notify();
                    }
                }),
            )
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
                    this.selecting = false;
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.drag = None;
                    this.selecting = false;
                    cx.notify();
                }),
            )
            .child(body)
            .child(self.render_status(cx))
            .children(self.render_goto())
            .children(self.render_search())
            .children(self.render_popup(window))
            .children(self.render_encoding_menu(window, cx))
    }
}

fn main() {
    let path = std::env::args_os().nth(1).map(PathBuf::from);

    // Finder のダブルクリックや「このアプリケーションで開く」は、コマンド引数ではなく、
    // OS からの「開く」イベント（URL）で届く。イベントは、ウィンドウができる前に届くこともあるので、
    // いったんチャネルに溜めておき、ウィンドウを作ってから処理する。
    let (open_tx, mut open_rx) = futures::channel::mpsc::unbounded::<Vec<String>>();
    let app = Application::new();
    app.on_open_urls(move |urls| {
        open_tx.unbounded_send(urls).ok();
    });

    app.run(move |cx: &mut App| {
        setup_app(cx);

        let bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
        let window = cx.open_window(
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

        cx.spawn(async move |cx| {
            use futures::StreamExt;
            while let Some(urls) = open_rx.next().await {
                let Some(path) = open_url::first_path(&urls) else {
                    eprintln!("csv_viewer: ignoring open request: {urls:?}");
                    continue;
                };
                eprintln!("csv_viewer: open {}", path.display());
                window
                    .update(cx, |viewer, window, cx| {
                        window.activate_window();
                        viewer.open_path(path, window, cx);
                    })
                    .ok();
            }
        })
        .detach();
    });
}

/// 表のキー割り当てに付けるコンテキスト。ルートの要素にも、同じ名前を付ける。
///
/// コンテキストのないキー割り当ては、「いちばん深い位置にある」ものとして扱われ、同じ深さなら、
/// あとで登録したものが勝つ。入力欄（`TextInput`）と同じキーを、コンテキストなしで割り当てると、
/// 入力欄にフォーカスがあっても、そちらが勝ってしまう。入力欄は、このコンテキストの内側にあるので、
/// 表のキー割り当てに `Table` を付けておけば、入力欄のほうが深く、登録の順序に関わらず、優先される。
const TABLE_CONTEXT: &str = "Table";

/// アプリのキー割り当て。
fn app_key_bindings() -> Vec<KeyBinding> {
    let table = Some(TABLE_CONTEXT);
    vec![
        // アプリ全体
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-o", OpenFile, None),
        KeyBinding::new("cmd-l", GotoRow, None),
        KeyBinding::new("cmd-f", FindInTable, None),
        KeyBinding::new("cmd-g", FindNext, None),
        KeyBinding::new("cmd-shift-g", FindPrevious, None),
        // 表。入力欄の同じキー（Cmd+C、Cmd+A、Home、End）に、負けるようにする。
        KeyBinding::new("cmd-c", CopySelection, table),
        KeyBinding::new("cmd-a", SelectAllCells, table),
        KeyBinding::new("up", ScrollLineUp, table),
        KeyBinding::new("down", ScrollLineDown, table),
        KeyBinding::new("pageup", ScrollPageUp, table),
        KeyBinding::new("pagedown", ScrollPageDown, table),
        KeyBinding::new("home", ScrollToTop, table),
        KeyBinding::new("end", ScrollToBottom, table),
        KeyBinding::new("cmd-up", ScrollToTop, table),
        KeyBinding::new("cmd-down", ScrollToBottom, table),
    ]
}

/// 終了まわりの設定。GPUIは最後のウィンドウが閉じてもアプリを終了しないため、自前で扱う。
fn setup_app(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    // CloseWindow / OpenFile はウィンドウ側（Viewer のルート要素）で受ける。
    // アクション処理中は当該ウィンドウが取り出されており、ここから window.update() はできない。
    cx.bind_keys(text_input::key_bindings());
    cx.bind_keys(app_key_bindings());
    cx.set_menus(vec![
        Menu {
            name: "csv_viewer".into(),
            items: vec![MenuItem::action("Quit", Quit)],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Open…", OpenFile),
                MenuItem::action("Go to Row…", GotoRow),
                MenuItem::action("Find…", FindInTable),
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

    #[test]
    fn the_search_info_text_covers_each_state() {
        // 検索中は、ほかの情報より、検索中であることを優先する。
        assert_eq!(search_info_text(true, Some((5, false)), false, Some(2)), "検索中…");
        // 一致が0行で、索引も作りおわっているなら、見つからない。
        assert_eq!(search_info_text(false, Some((0, false)), false, None), "見つかりません");
        // 索引の作成中は、0行でも、この先に見つかるかもしれない。
        assert_eq!(search_info_text(false, Some((0, false)), true, None), "0+ 行が一致");
        assert_eq!(search_info_text(false, Some((12, false)), false, None), "12 行が一致");
        // 上限で打ち切ったときと、索引の作成中は、「+」をつける。
        assert_eq!(search_info_text(false, Some((100_000, true)), false, None), "100000+ 行が一致");
        assert_eq!(search_info_text(false, Some((12, false)), true, None), "12+ 行が一致");
        // 数えている途中。
        assert_eq!(search_info_text(false, None, false, None), "数えています…");
        // 現在の一致の行は、1始まりで添える。
        assert_eq!(
            search_info_text(false, Some((12, false)), false, Some(0)),
            "12 行が一致 / 現在 1 行目"
        );
        assert_eq!(
            search_info_text(false, None, false, Some(41)),
            "数えています… / 現在 42 行目"
        );
    }

    // ---- キー割り当ての優先順位 ----

    use gpui::{KeyContext, Keymap, Keystroke};

    /// 本物のアプリと同じ一覧で、キーマップを作る。`text_first` は、入力欄の割り当てを、
    /// 先に登録するか（本物の登録順）、あとに登録するか。
    fn keymap(text_first: bool) -> Keymap {
        let mut keymap = Keymap::default();
        if text_first {
            keymap.add_bindings(text_input::key_bindings());
            keymap.add_bindings(app_key_bindings());
        } else {
            keymap.add_bindings(app_key_bindings());
            keymap.add_bindings(text_input::key_bindings());
        }
        keymap
    }

    /// そのキーを押したとき、最優先で動く割り当ての、動作の名前。
    fn top_action(keymap: &Keymap, key: &[Keystroke], contexts: &[&str]) -> Option<String> {
        let stack: Vec<KeyContext> = contexts
            .iter()
            .map(|c| KeyContext::parse(c).unwrap())
            .collect();
        let (bindings, _) = keymap.bindings_for_input(key, &stack);
        bindings.first().map(|b| b.action().name().to_string())
    }

    #[test]
    fn every_key_of_the_text_input_wins_while_it_has_focus() {
        for text_first in [true, false] {
            let keymap = keymap(text_first);
            for binding in text_input::key_bindings() {
                let keystrokes: Vec<Keystroke> = binding
                    .keystrokes()
                    .iter()
                    .map(|k| k.inner().clone())
                    .collect();
                let action = top_action(&keymap, &keystrokes, &[TABLE_CONTEXT, "TextInput"]);
                assert_eq!(
                    action.as_deref(),
                    Some(binding.action().name()),
                    "{keystrokes:?} (text input registered first: {text_first})"
                );
            }
        }
    }

    #[test]
    fn the_table_keys_work_when_the_table_has_focus() {
        for text_first in [true, false] {
            let keymap = keymap(text_first);
            let key = |s: &str| [Keystroke::parse(s).unwrap()];
            for (keystroke, expected) in [
                ("cmd-c", "csv_viewer::CopySelection"),
                ("cmd-a", "csv_viewer::SelectAllCells"),
                ("home", "csv_viewer::ScrollToTop"),
                ("end", "csv_viewer::ScrollToBottom"),
                ("up", "csv_viewer::ScrollLineUp"),
                ("pagedown", "csv_viewer::ScrollPageDown"),
            ] {
                assert_eq!(
                    top_action(&keymap, &key(keystroke), &[TABLE_CONTEXT]).as_deref(),
                    Some(expected),
                    "{keystroke}"
                );
            }
        }
    }

    #[test]
    fn keys_the_text_input_does_not_use_still_reach_the_table_and_the_app() {
        let keymap = keymap(true);
        let key = |s: &str| [Keystroke::parse(s).unwrap()];
        let stack = [TABLE_CONTEXT, "TextInput"];
        // 入力欄が使わない、表のキー（上下、ページ送り）は、入力中でも表に効く。
        assert_eq!(
            top_action(&keymap, &key("pagedown"), &stack).as_deref(),
            Some("csv_viewer::ScrollPageDown")
        );
        // アプリ全体のキーも、入力中に効く。
        assert_eq!(top_action(&keymap, &key("cmd-q"), &stack).as_deref(), Some("csv_viewer::Quit"));
        assert_eq!(top_action(&keymap, &key("cmd-g"), &stack).as_deref(), Some("csv_viewer::FindNext"));
    }

    #[test]
    fn the_old_global_bindings_would_have_beaten_the_text_input() {
        // 不具合の再現：コンテキストなしで、あとから登録すると、入力欄の `Cmd+C` に勝ってしまう。
        let mut keymap = Keymap::default();
        keymap.add_bindings(text_input::key_bindings());
        keymap.add_bindings([KeyBinding::new("cmd-c", CopySelection, None)]);
        assert_eq!(
            top_action(
                &keymap,
                &[Keystroke::parse("cmd-c").unwrap()],
                &[TABLE_CONTEXT, "TextInput"]
            )
            .as_deref(),
            Some("csv_viewer::CopySelection")
        );
    }
}

