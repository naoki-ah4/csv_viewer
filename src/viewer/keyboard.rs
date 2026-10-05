//! 行への移動（`Cmd+L`）と、表にフォーカスがあるときのキー入力。

use super::*;

impl Viewer {
    /// 「行へ移動」の入力欄を開く。表が読み込まれていないときは何もしない。
    pub(super) fn open_goto(&mut self, cx: &mut Context<Self>) {
        if self.loaded.is_some() {
            self.goto = Some(GotoInput::default());
            cx.notify();
        }
    }

    /// 入力欄が開いているときのキー入力を処理する。
    pub(super) fn handle_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
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
    pub(super) fn jump_to_row(&mut self, number: u64) {
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

    /// 「行へ移動」の入力欄。表の上に重ねて出す。
    pub(super) fn render_goto(&self) -> Option<impl IntoElement> {
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
}
