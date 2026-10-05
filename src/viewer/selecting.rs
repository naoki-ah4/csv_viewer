//! 選択とコピー：セル・行・列の選択、キーボードでの移動、ドラッグ中の自動スクロール。

use super::*;

impl Viewer {
    /// 範囲を広げる操作の、共通の準備。開いているものを閉じて、ドラッグ中の自動スクロールを始める。
    pub(super) fn begin_selecting(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // 全体のクリックで閉じる処理（ルート要素）には、渡さない。
        cx.stop_propagation();
        self.popup = None;
        self.encoding_menu = None;
        self.notice = None;
        self.selecting = true;
        self.last_pointer = Some(position);
        self.start_autoscroll(window, cx);
    }

    /// 範囲を広げる操作を終える。自動スクロールも止める。
    pub(super) fn stop_selecting(&mut self) {
        self.selecting = false;
        self.autoscroll_task = None;
    }

    /// 本体のセルを押した。クリックで選択、`Shift+クリック`で範囲を広げる、ダブルクリックで全文を出す。
    pub(super) fn press_cell(
        &mut self,
        row: usize,
        col: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pos = CellPos { row, col };
        if event.click_count >= 2 {
            cx.stop_propagation();
            self.popup = None;
            self.encoding_menu = None;
            self.notice = None;
            self.selection = Some(Selection::cell(pos));
            self.open_cell_popup(Some(row), col, event.position, cx);
            return;
        }
        self.begin_selecting(event.position, window, cx);
        self.selection = Some(match (event.modifiers.shift, self.selection) {
            (true, Some(current)) => current.extended_to(pos),
            _ => Selection::cell(pos),
        });
        cx.notify();
    }

    /// ボタンを押したまま、セルの上を通った。選択範囲を広げる。
    pub(super) fn drag_cell(&mut self, row: usize, col: usize, cx: &mut Context<Self>) {
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
    pub(super) fn press_row_number(
        &mut self,
        row: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_selecting(event.position, window, cx);
        self.selection = Some(match (event.modifiers.shift, self.selection) {
            (true, Some(current)) => current.extended_to(CellPos { row, col: 0 }),
            _ => Selection::whole_row(row),
        });
        cx.notify();
    }

    /// 列番号を押した。その列全体を選ぶ。`Shift+クリック`で、列の範囲を広げる。
    pub(super) fn press_col_number(
        &mut self,
        col: usize,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_selecting(event.position, window, cx);
        self.selection = Some(match (event.modifiers.shift, self.selection) {
            (true, Some(current)) => current.extended_to(CellPos { row: 0, col }),
            _ => Selection::whole_column(col),
        });
        cx.notify();
    }

    /// `Shift+矢印` など：現在位置を動かして、選択範囲を広げる。
    pub(super) fn extend_selection(&mut self, step: Step, cx: &mut Context<Self>) {
        self.change_selection(step, false, cx);
    }

    /// 矢印キーなど。選択があれば、スプレッドシートと同じく、選択を動かす（1つのセルになる）。
    /// 選択がなければ、これまでどおり、表をスクロールする。
    pub(super) fn move_or_scroll(&mut self, step: Step, nav: Nav, cx: &mut Context<Self>) {
        if self.selection.is_some() {
            self.change_selection(step, true, cx);
        } else {
            self.navigate(nav, cx);
        }
    }

    /// 現在位置を `step` だけ動かす。`collapse` なら、選択を、動かした先の1つのセルにする。
    /// そうでなければ、アンカーはそのままに、範囲を広げる。選択がなければ、画面の先頭の行から始める。
    pub(super) fn change_selection(&mut self, step: Step, collapse: bool, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let rows = loaded.table.row_count();
        let columns = loaded.table.column_count();
        if rows == 0 || columns == 0 {
            return;
        }
        let base = self.selection.unwrap_or_else(|| {
            Selection::cell(CellPos {
                row: self.scroll.row.min(rows - 1),
                col: 0,
            })
        });
        let extended = if collapse {
            base.moved_by(step, rows, columns)
        } else {
            base.extended_by(step, rows, columns)
        };
        self.selection = Some(extended);
        self.popup = None;
        self.notice = None;
        let head = extended.head();
        self.reveal(Match {
            row: head.row,
            col: head.col,
        });
        cx.notify();
    }

    /// ページ送りで広げる行数。
    pub(super) fn page_step_rows(&self) -> usize {
        scroll_pos::page_rows(f32::from(self.viewport.get().size.height))
    }

    /// ドラッグ中に、ポインタが表の端の外へ出たら、スクロールし続ける。ボタンを離すまで動く。
    pub(super) fn start_autoscroll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let timer = cx.background_executor().clone();
        self.autoscroll_task = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                timer.timer(AUTOSCROLL_INTERVAL).await;
                let keep_going = this
                    .update_in(cx, |this, _, cx| this.autoscroll_tick(cx))
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }

    /// 自動スクロールの1回ぶん。続けて呼ぶ必要があれば `true`。
    pub(super) fn autoscroll_tick(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.selecting {
            return false;
        }
        let (Some(pointer), Some(loaded)) = (self.last_pointer, self.loaded.as_ref()) else {
            return self.selecting;
        };
        let rows_area = self.viewport.get();
        let x_area = self.h_scroll.bounds();
        if rows_area.size.height <= px(0.) || x_area.size.width <= px(0.) {
            return true;
        }
        let top = f32::from(rows_area.origin.y);
        let bottom = f32::from(rows_area.origin.y + rows_area.size.height);
        let left = f32::from(x_area.origin.x);
        let right = f32::from(x_area.origin.x + x_area.size.width);
        let (pointer_x, pointer_y) = (f32::from(pointer.x), f32::from(pointer.y));
        let dy = drag_scroll::edge_scroll(pointer_y, top, bottom);
        let dx = drag_scroll::edge_scroll(pointer_x, left, right);
        if dy == 0.0 && dx == 0.0 {
            return true;
        }
        if dy != 0.0 {
            self.scroll = self
                .scroll
                .scrolled_by(f64::from(dy))
                .clamped(self.max_scroll());
        }
        if dx != 0.0 {
            let max = f32::from(self.h_scroll.max_offset().width).max(0.0);
            let next = (-f32::from(self.h_scroll.offset().x) + dx).clamp(0.0, max);
            self.h_scroll
                .set_offset(point(px(-next), self.h_scroll.offset().y));
        }
        // 範囲を、ポインタがある側の端のセルまで広げる。
        let rows = loaded.table.row_count();
        let visible = visible_rows(self.scroll, f32::from(rows_area.size.height), rows);
        let content_left = left + f32::from(self.h_scroll.offset().x);
        let row = drag_scroll::row_at(
            drag_scroll::clamp_into(pointer_y, top, bottom),
            top,
            visible.first,
            visible.offset,
            ROW_HEIGHT,
            rows,
        );
        let col = drag_scroll::col_at(
            drag_scroll::clamp_into(pointer_x, left, right),
            content_left,
            &loaded.col_widths,
        );
        if let (Some(row), Some(col), Some(current)) = (row, col, self.selection) {
            self.selection = Some(current.extended_to(CellPos { row, col }));
        }
        cx.notify();
        true
    }

    /// `Cmd+A`：ポップアップが開いていれば、その文章を全部選ぶ。開いていなければ、表の全行を選ぶ。
    pub(super) fn select_all_cells(&mut self, cx: &mut Context<Self>) {
        if let Some(popup) = self.popup.as_mut() {
            popup.selection = Some(TextSelection::all(popup.text.len()));
            cx.notify();
            return;
        }
        let rows = self.row_count();
        self.selection = Selection::all_rows(rows);
        self.notice = None;
        cx.notify();
    }

    /// `Cmd+C`。ポップアップが開いていて、文章を選んでいれば、その部分を。選んでいなければ、そのセルの
    /// 全文を（ポップアップに出ている、切り詰めた文字列ではなく）。ポップアップがなければ、選択範囲を、
    /// タブ区切りで。
    pub(super) fn copy_selection(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        if let Some(popup) = self.popup.as_ref() {
            let selected = popup
                .selection
                .filter(|s| !s.is_empty())
                .map(|s| text_select::slice(&popup.text, &s.range()).to_string());
            let (text, message) = match selected {
                Some(text) => {
                    let chars = text.chars().count();
                    (text, format!("選んだ文字をコピーしました（{chars} 文字）"))
                }
                None => {
                    let text = match popup.row {
                        Some(row) => loaded.table.cell(row, popup.col),
                        None => loaded.table.header(popup.col),
                    }
                    .into_owned();
                    let chars = text.chars().count();
                    (text, format!("セルの全文をコピーしました（{chars} 文字）"))
                }
            };
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.notice = Some(message);
            cx.notify();
            return;
        }
        let Some(selection) = self.selection else {
            return;
        };
        let rows_total = loaded.table.row_count();
        let columns = loaded.table.column_count();
        let cells = selection.cell_count(rows_total, columns);
        if cells > selection::COPY_CAP_CELLS {
            self.notice = Some(format!(
                "選択が大きすぎて、コピーできません（{cells} セル。上限は {} セル）",
                selection::COPY_CAP_CELLS
            ));
            cx.notify();
            return;
        }
        let text = selection::to_tsv(
            selection.rows(rows_total),
            selection.cols(columns),
            |row, col| loaded.table.cell(row, col),
        );
        let (rows, cols) = selection.dimensions(rows_total, columns);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.notice = Some(format!("{rows} 行 × {cols} 列をコピーしました"));
        cx.notify();
    }

    /// `Enter`：選択している（現在位置の）セルの全文を、ポップアップで出す。
    pub(super) fn open_popup_for_selection(&mut self, cx: &mut Context<Self>) {
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
    pub(super) fn cell_anchor(&self, row: usize, col: usize) -> Option<Point<Pixels>> {
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

    pub(super) fn cell_press(
        cx: &Context<Self>,
        row: usize,
        col: usize,
    ) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
            this.press_cell(row, col, event, window, cx)
        })
    }

    pub(super) fn cell_drag(
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

    pub(super) fn row_number_press(
        cx: &Context<Self>,
        row: usize,
    ) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
            this.press_row_number(row, event, window, cx)
        })
    }

    pub(super) fn row_number_drag(
        cx: &Context<Self>,
        row: usize,
    ) -> impl Fn(&MouseMoveEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
            if event.dragging() {
                this.drag_cell(row, 0, cx);
            }
        })
    }

    pub(super) fn col_number_press(
        cx: &Context<Self>,
        col: usize,
    ) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
            this.press_col_number(col, event, window, cx)
        })
    }

    pub(super) fn col_number_drag(
        cx: &Context<Self>,
        col: usize,
    ) -> impl Fn(&MouseMoveEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
            if event.dragging() {
                this.drag_cell(0, col, cx);
            }
        })
    }
}
