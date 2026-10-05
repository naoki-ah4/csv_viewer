mod actions;
mod cell_popup;
mod drag_scroll;
mod encodings;
mod goto;
mod matcher;
mod open_url;
mod reveal;
mod scroll_pos;
mod scrollbar;
mod search;
mod selection;
mod settings;
mod table;
mod text_input;
mod text_select;
mod viewer;
mod windows;

use std::path::PathBuf;

use gpui::{App, Application, Menu, MenuItem};

use actions::{
    CloseWindow, FindInTable, GotoRow, OpenFile, Quit, ToggleColumnNumbers, ToggleRowNumbers,
    app_key_bindings,
};

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

        windows::open_paths(path.into_iter().collect(), cx);
        cx.activate(true);

        cx.spawn(async move |cx| {
            use futures::StreamExt;
            while let Some(urls) = open_rx.next().await {
                let paths = open_url::all_paths(&urls);
                if paths.is_empty() {
                    eprintln!("csv_viewer: ignoring open request: {urls:?}");
                    continue;
                }
                eprintln!("csv_viewer: open {paths:?}");
                cx.update(|cx| {
                    windows::open_paths(paths, cx);
                    cx.activate(true);
                })
                .ok();
            }
        })
        .detach();
    });
}

/// 終了まわりの設定。GPUIは最後のウィンドウが閉じてもアプリを終了しないため、自前で扱う。
fn setup_app(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.set_global(settings::ViewSettings::load());
    // CloseWindow / OpenFile はウィンドウ側（Viewer のルート要素）で受ける。
    // アクション処理中は当該ウィンドウが取り出されており、ここから window.update() はできない。
    cx.bind_keys(text_input::key_bindings());
    cx.bind_keys(app_key_bindings());
    cx.set_menus(vec![
        Menu {
            name: "CSV Viewer".into(),
            items: vec![MenuItem::action("Quit CSV Viewer", Quit)],
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
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Show/Hide Row Numbers", ToggleRowNumbers),
                MenuItem::action("Show/Hide Column Numbers", ToggleColumnNumbers),
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
