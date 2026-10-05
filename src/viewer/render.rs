//! 描画：表、ステータスバー、空の画面。

use super::*;
use crate::actions::*;

impl Viewer {
    pub(super) fn render_empty(&self) -> impl IntoElement {
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

    pub(super) fn render_table(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let loaded = self
            .loaded
            .as_ref()
            .expect("render_table requires a loaded CSV");
        // ポップアップを出しているセル。目立たせる。
        let popup_cell = self.popup.as_ref().map(|p| (p.row, p.col));
        // 検索の一致を、画面に見えているセルだけ、強調する。
        let search_matcher = self.search_state.as_ref().map(|s| &s.matcher);
        let search_current = self.search_state.as_ref().and_then(|s| s.current);
        let selection = self.selection;
        let column_count = loaded.table.column_count();
        let row_total = loaded.table.row_count();
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
            .children(loaded.col_widths.iter().enumerate().map(|(i, w)| {
                cell((i + 1).to_string(), *w)
                    .when(
                        selection.is_some_and(|s| s.cols(column_count).contains(&i)),
                        |d| d.bg(SELECTED_CELL_BG),
                    )
                    .on_mouse_down(MouseButton::Left, Self::col_number_press(cx, i))
                    .on_mouse_move(Self::col_number_drag(cx, i))
            }));
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
                    let background =
                        if selection.is_some_and(|s| s.contains(ix, i, row_total, column_count)) {
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
                .bg(
                    if selection.is_some_and(|s| s.rows(row_total).contains(&ix)) {
                        rgb(0xd6e6fb)
                    } else {
                        rgb(if ix % 2 == 1 { 0xf0f0f0 } else { 0xf6f6f6 })
                    },
                )
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

    pub(super) fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                let (rows_total, cols_total) = (l.table.row_count(), l.table.column_count());
                let selected = self
                    .selection
                    .filter(|s| !s.is_single_cell(rows_total, cols_total))
                    .map(|s| {
                        let (rows, cols) = s.dimensions(rows_total, cols_total);
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
            .on_key_down(
                cx.listener(|this, event: &KeyDownEvent, _, cx| this.handle_key_down(event, cx)),
            )
            .on_action(cx.listener(|this, _: &GotoRow, _, cx| this.open_goto(cx)))
            .on_action(
                cx.listener(|this, _: &FindInTable, window, cx| this.open_search(window, cx)),
            )
            .on_action(cx.listener(|this, _: &CopySelection, _, cx| this.copy_selection(cx)))
            .on_action(cx.listener(|this, _: &SelectAllCells, _, cx| this.select_all_cells(cx)))
            .on_action(cx.listener(|this, _: &ExtendUp, _, cx| this.extend_selection(Step::Up, cx)))
            .on_action(
                cx.listener(|this, _: &ExtendDown, _, cx| this.extend_selection(Step::Down, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ExtendLeft, _, cx| this.extend_selection(Step::Left, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ExtendRight, _, cx| this.extend_selection(Step::Right, cx)),
            )
            .on_action(cx.listener(|this, _: &ExtendPageUp, _, cx| {
                let rows = this.page_step_rows();
                this.extend_selection(Step::PageUp(rows), cx)
            }))
            .on_action(cx.listener(|this, _: &ExtendPageDown, _, cx| {
                let rows = this.page_step_rows();
                this.extend_selection(Step::PageDown(rows), cx)
            }))
            .on_action(
                cx.listener(|this, _: &ExtendToTop, _, cx| this.extend_selection(Step::Top, cx)),
            )
            .on_action(cx.listener(|this, _: &ExtendToBottom, _, cx| {
                this.extend_selection(Step::Bottom, cx)
            }))
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
            .on_action(cx.listener(|this, _: &ScrollLineUp, _, cx| {
                this.move_or_scroll(Step::Up, Nav::LineUp, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollLineDown, _, cx| {
                this.move_or_scroll(Step::Down, Nav::LineDown, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollPageUp, _, cx| {
                let rows = this.page_step_rows();
                this.move_or_scroll(Step::PageUp(rows), Nav::PageUp, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollPageDown, _, cx| {
                let rows = this.page_step_rows();
                this.move_or_scroll(Step::PageDown(rows), Nav::PageDown, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollToTop, _, cx| {
                this.move_or_scroll(Step::Top, Nav::Top, cx)
            }))
            .on_action(cx.listener(|this, _: &ScrollToBottom, _, cx| {
                this.move_or_scroll(Step::Bottom, Nav::Bottom, cx)
            }))
            // 左右は、選択を動かすだけ（選択がなければ、何もしない）。
            .on_action(cx.listener(|this, _: &MoveLeft, _, cx| {
                if this.selection.is_some() {
                    this.change_selection(Step::Left, true, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &MoveRight, _, cx| {
                if this.selection.is_some() {
                    this.change_selection(Step::Right, true, cx);
                }
            }))
            // `Home` `End`：選択があれば、その行の先頭/末尾の列へ。なければ、表の先頭/末尾へ。
            .on_action(cx.listener(|this, _: &GoHome, _, cx| {
                this.move_or_scroll(Step::LineStart, Nav::Top, cx)
            }))
            .on_action(cx.listener(|this, _: &GoEnd, _, cx| {
                this.move_or_scroll(Step::LineEnd, Nav::Bottom, cx)
            }))
            .on_action(cx.listener(|this, _: &GoFirst, _, cx| {
                this.move_or_scroll(Step::First, Nav::Top, cx)
            }))
            .on_action(cx.listener(|this, _: &GoLast, _, cx| {
                this.move_or_scroll(Step::Last, Nav::Bottom, cx)
            }))
            .on_action(cx.listener(|this, _: &ExtendToLineStart, _, cx| {
                this.extend_selection(Step::LineStart, cx)
            }))
            .on_action(cx.listener(|this, _: &ExtendToLineEnd, _, cx| {
                this.extend_selection(Step::LineEnd, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ExtendToFirst, _, cx| {
                    this.extend_selection(Step::First, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &ExtendToLast, _, cx| this.extend_selection(Step::Last, cx)),
            )
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                if let Some(path) = paths.paths().first() {
                    this.open_path(path.clone(), window, cx);
                }
            }))
            .drag_over::<ExternalPaths>(|style, _, _, _| style.bg(rgb(0xe8f0fe)))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if this.selecting {
                    this.last_pointer = Some(ev.position);
                }
                if ev.dragging() {
                    this.drag_to(ev.position, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.drag = None;
                    this.stop_selecting();
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.drag = None;
                    this.stop_selecting();
                    cx.notify();
                }),
            )
            .child(body)
            .child(self.render_status(cx))
            .children(self.render_goto())
            .children(self.render_search())
            .children(self.render_popup(window, cx))
            .children(self.render_encoding_menu(window, cx))
    }
}
