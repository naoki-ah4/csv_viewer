//! 表を見せる画面（`Viewer`）。状態はここに集め、操作と描画は、役割ごとにファイルを分けている。

mod keyboard;
mod layout;
mod loading;
mod popup;
mod render;
mod scrollbars;
mod searching;
mod selecting;

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, Entity, ExternalPaths, FocusHandle,
    HighlightStyle, Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    PathPromptOptions, Pixels, Point, Rgba, ScrollHandle, ScrollWheelEvent, StyledText,
    Subscription, Task, TextLayout, Window, canvas, div, point, prelude::*, px, rgb, rgba,
};

use crate::goto::{GotoInput, GotoKey};
use crate::matcher::{Direction, Matcher, Outcome};
use crate::scroll_pos::{Nav, ROW_HEIGHT, ScrollPos};
use crate::scrollbar::{Axis, AxisState};
use crate::search::{Match, Rank};
use crate::selection::{CellPos, Selection, Step};
use crate::table::Table;
use crate::table::lazy::{IndexJob, IndexStatus};
use crate::text_input::{TextInput, TextInputEvent};
use crate::text_select::TextSelection;
use crate::{
    cell_popup, drag_scroll, encodings, goto, reveal, scroll_pos, scrollbar, search, selection,
    text_select,
};
use layout::*;

const MENU_WIDTH: f32 = 200.0;
/// ドラッグ中に、表の端の外へ出たとき、スクロールを続ける間隔。
const AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(16);
/// ポップアップの文章で、選んだ範囲の背景色。
const TEXT_SELECTION_BG: Rgba = Rgba {
    r: 0.23,
    g: 0.51,
    b: 0.96,
    a: 0.30,
};
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
    /// 文章の中で選んだ範囲。
    selection: Option<TextSelection>,
    /// 文章を組んだ結果。マウスの位置を、文字の位置に直すのに使う。描画のたびに、差し替わる。
    layout: Rc<RefCell<Option<TextLayout>>>,
    /// 文章の上で、ボタンを押して、範囲を広げているところ。
    selecting: bool,
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
    /// 現在の一致が、一致した行の何番目か。
    rank: Rank,
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

/// 表示の設定。すべてのウィンドウで共通。
pub(crate) struct ViewSettings {
    /// 行番号の欄と、列番号の行を出すか。
    pub(crate) show_numbers: bool,
}

impl gpui::Global for ViewSettings {}

impl Default for ViewSettings {
    fn default() -> Self {
        Self { show_numbers: true }
    }
}

pub(crate) struct Viewer {
    /// 行番号・列番号を出すか。描画のたびに、`ViewSettings` から写す。
    show_numbers: bool,
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
    /// 範囲を広げているときの、ポインタの位置。端の外へ出たとき、スクロールを続けるのに使う。
    last_pointer: Option<Point<Pixels>>,
    /// ドラッグ中に、表の端の外へ出たとき、スクロールし続けるタスク。ボタンを離すと止まる。
    autoscroll_task: Option<Task<()>>,
    /// 検索バーの入力欄。最初に開いたときに作る。
    search_input: Option<Entity<TextInput>>,
    search_subscription: Option<Subscription>,
    search_open: bool,
    search_state: Option<SearchState>,
    /// 実行中の検索を中断するための旗。新しい検索を始めるたびに、前の旗を立てて、作り直す。
    search_cancel: Arc<AtomicBool>,
    search_task: Option<Task<()>>,
    count_cancel: Arc<AtomicBool>,
    /// 現在の一致の順位を、先頭から数えている処理を、中断するための旗と、そのタスク。
    rank_cancel: Arc<AtomicBool>,
    rank_task: Option<Task<()>>,
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
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        Self {
            show_numbers: true,
            loaded: None,
            error: None,
            loading: None,
            goto: None,
            popup: None,
            selection: None,
            selecting: false,
            last_pointer: None,
            autoscroll_task: None,
            search_input: None,
            search_subscription: None,
            search_open: false,
            search_state: None,
            search_cancel: Arc::new(AtomicBool::new(false)),
            search_task: None,
            count_cancel: Arc::new(AtomicBool::new(false)),
            rank_cancel: Arc::new(AtomicBool::new(false)),
            rank_task: None,
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

    fn row_count(&self) -> usize {
        self.loaded.as_ref().map_or(0, |l| l.table.row_count())
    }

    /// 行番号の欄の幅。行数の桁数で決まる。
    fn gutter_width(&self) -> f32 {
        if self.show_numbers {
            gutter_width(self.row_count())
        } else {
            0.0
        }
    }

    /// 縦スクロールしない部分（列番号の行とヘッダー行）の高さ。
    fn header_height(&self) -> f32 {
        if self.show_numbers {
            COLNO_HEIGHT + ROW_HEIGHT
        } else {
            ROW_HEIGHT
        }
    }

    /// 行番号・列番号の表示を切り替える。設定は全ウィンドウで共通なので、全部を描き直す。
    fn toggle_numbers(&mut self, cx: &mut Context<Self>) {
        let settings = cx
            .try_global::<ViewSettings>()
            .map_or(true, |s| s.show_numbers);
        cx.set_global(ViewSettings {
            show_numbers: !settings,
        });
        cx.refresh_windows();
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

    /// 縦に動かせる上限（最後の行が領域の下端に揃う位置）。
    fn max_scroll(&self) -> ScrollPos {
        ScrollPos::max(self.row_count(), self.viewport.get().size.height.into())
    }

    /// まだ何も開いていない（読み込み中でも、失敗の表示中でもない）か。
    /// 空のウィンドウは、次に開くファイルに使い回す。
    pub(crate) fn is_empty(&self) -> bool {
        self.loaded.is_none() && self.loading.is_none() && self.error.is_none()
    }

    /// 表のフォーカスを、この画面に移す。
    pub(crate) fn focus(&self, window: &mut Window) {
        self.focus_handle.focus(window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table::eager::CsvTable;
    use crate::table::lazy::LazyTable;

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
        assert!(
            !loaded.refresh_widths(),
            "nothing changes after indexing is done"
        );
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
        let table = Table::Eager(CsvTable::from_bytes(b"a,b\n1,2\n").unwrap());
        let mut loaded = Loaded::new(table);
        assert!(!loaded.refresh_widths());
    }
}
