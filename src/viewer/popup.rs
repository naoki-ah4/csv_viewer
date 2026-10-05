//! セルのポップアップ（全文の表示と、文字の選択）と、文字コードの切り替えメニュー。

use super::*;

impl Viewer {
    /// 文字コードの切り替えメニューを開く。開いているときは閉じる。
    pub(super) fn toggle_encoding_menu(&mut self, anchor: Point<Pixels>, cx: &mut Context<Self>) {
        self.popup = None;
        self.encoding_menu = match self.encoding_menu {
            Some(_) => None,
            None => Some(anchor),
        };
        cx.notify();
    }

    /// セルのクリックを受ける。同じセルをもう一度クリックすると閉じる。
    pub(super) fn open_cell_popup(
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
            selection: None,
            layout: Rc::new(RefCell::new(None)),
            selecting: false,
        });
        cx.notify();
    }

    /// ポップアップの文章の中で、マウスの位置にある文字の位置。
    pub(super) fn popup_text_index(&self, position: Point<Pixels>) -> Option<usize> {
        let layout = self.popup.as_ref()?.layout.borrow().clone()?;
        Some(match layout.index_for_position(position) {
            Ok(index) | Err(index) => index,
        })
    }

    /// ポップアップの文章を押した。そこから、範囲を広げられるようにする。
    pub(super) fn press_popup_text(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(index) = self.popup_text_index(position) else {
            return;
        };
        if let Some(popup) = self.popup.as_mut() {
            popup.selection = Some(TextSelection::at(index));
            popup.selecting = true;
        }
        cx.notify();
    }

    pub(super) fn drag_popup_text(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(index) = self.popup_text_index(position) else {
            return;
        };
        if let Some(popup) = self.popup.as_mut() {
            if let (true, Some(current)) = (popup.selecting, popup.selection) {
                let extended = current.extended_to(index);
                if extended != current {
                    popup.selection = Some(extended);
                    cx.notify();
                }
            }
        }
    }

    pub(super) fn release_popup_text(&mut self) {
        if let Some(popup) = self.popup.as_mut() {
            popup.selecting = false;
        }
    }

    /// セルのクリックを `open_cell_popup` へつなぐリスナー。
    pub(super) fn cell_click(
        cx: &Context<Self>,
        row: Option<usize>,
        col: usize,
    ) -> impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
            this.open_cell_popup(row, col, event.position, cx)
        })
    }

    /// クリックしたセルの全文。表の上に重ねて出す。
    pub(super) fn render_popup(&self, window: &Window, cx: &mut Context<Self>) -> Option<impl IntoElement> {
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
        // 選んだ範囲の背景を塗る。文章を組んだ結果は、マウスの位置を文字の位置に直すために、取っておく。
        let mut text = StyledText::new(popup.text.clone());
        if let Some(range) = popup.selection.map(|s| s.range()).filter(|r| !r.is_empty()) {
            text = text.with_highlights([(
                range,
                HighlightStyle {
                    background_color: Some(Hsla::from(TEXT_SELECTION_BG)),
                    ..Default::default()
                },
            )]);
        }
        *popup.layout.borrow_mut() = Some(text.layout().clone());
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
                        .cursor(CursorStyle::IBeam)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, event: &MouseDownEvent, _, cx| {
                                this.press_popup_text(event.position, cx)
                            }),
                        )
                        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                            if event.dragging() {
                                this.drag_popup_text(event.position, cx);
                            }
                        }))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, _| this.release_popup_text()),
                        )
                        .on_mouse_up_out(
                            MouseButton::Left,
                            cx.listener(|this, _, _, _| this.release_popup_text()),
                        )
                        .child(text),
                )
                .children(truncated.map(|note| {
                    div().text_xs().text_color(rgb(0xb06000)).child(note)
                })),
        )
    }

    /// 文字コードの切り替えメニュー。ステータスバーの文字コード表示をクリックすると、真上に出る。
    pub(super) fn render_encoding_menu(
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
}
