//! アプリの操作（アクション）の定義と、キー割り当て。

use gpui::{KeyBinding, actions};

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
        ExtendUp,
        ExtendDown,
        ExtendLeft,
        ExtendRight,
        ExtendPageUp,
        ExtendPageDown,
        ExtendToTop,
        ExtendToBottom,
        ExtendToLineStart,
        ExtendToLineEnd,
        ExtendToFirst,
        ExtendToLast,
        MoveLeft,
        MoveRight,
        GoHome,
        GoEnd,
        GoFirst,
        GoLast,
        ScrollLineUp,
        ScrollLineDown,
        ScrollPageUp,
        ScrollPageDown,
        ScrollToTop,
        ScrollToBottom,
    ]
);

/// 表のキー割り当てに付けるコンテキスト。ルートの要素にも、同じ名前を付ける。
///
/// コンテキストのないキー割り当ては、「いちばん深い位置にある」ものとして扱われ、同じ深さなら、
/// あとで登録したものが勝つ。入力欄（`TextInput`）と同じキーを、コンテキストなしで割り当てると、
/// 入力欄にフォーカスがあっても、そちらが勝ってしまう。入力欄は、このコンテキストの内側にあるので、
/// 表のキー割り当てに `Table` を付けておけば、入力欄のほうが深く、登録の順序に関わらず、優先される。
pub(crate) const TABLE_CONTEXT: &str = "Table";

/// アプリのキー割り当て。
pub(crate) fn app_key_bindings() -> Vec<KeyBinding> {
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
        KeyBinding::new("left", MoveLeft, table),
        KeyBinding::new("right", MoveRight, table),
        KeyBinding::new("home", GoHome, table),
        KeyBinding::new("end", GoEnd, table),
        KeyBinding::new("cmd-home", GoFirst, table),
        KeyBinding::new("cmd-end", GoLast, table),
        KeyBinding::new("cmd-up", ScrollToTop, table),
        KeyBinding::new("cmd-down", ScrollToBottom, table),
        // 選択範囲を、キーボードで広げる。
        KeyBinding::new("shift-up", ExtendUp, table),
        KeyBinding::new("shift-down", ExtendDown, table),
        KeyBinding::new("shift-left", ExtendLeft, table),
        KeyBinding::new("shift-right", ExtendRight, table),
        KeyBinding::new("shift-pageup", ExtendPageUp, table),
        KeyBinding::new("shift-pagedown", ExtendPageDown, table),
        KeyBinding::new("shift-home", ExtendToLineStart, table),
        KeyBinding::new("shift-end", ExtendToLineEnd, table),
        KeyBinding::new("cmd-shift-home", ExtendToFirst, table),
        KeyBinding::new("cmd-shift-end", ExtendToLast, table),
        KeyBinding::new("cmd-shift-up", ExtendToTop, table),
        KeyBinding::new("cmd-shift-down", ExtendToBottom, table),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_input;

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
                ("home", "csv_viewer::GoHome"),
                ("end", "csv_viewer::GoEnd"),
                ("left", "csv_viewer::MoveLeft"),
                ("cmd-home", "csv_viewer::GoFirst"),
                ("cmd-up", "csv_viewer::ScrollToTop"),
                ("shift-home", "csv_viewer::ExtendToLineStart"),
                ("up", "csv_viewer::ScrollLineUp"),
                ("pagedown", "csv_viewer::ScrollPageDown"),
                ("shift-down", "csv_viewer::ExtendDown"),
                ("shift-left", "csv_viewer::ExtendLeft"),
                ("shift-pagedown", "csv_viewer::ExtendPageDown"),
                ("cmd-shift-up", "csv_viewer::ExtendToTop"),
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
