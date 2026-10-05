mod actions;
mod cell_popup;
mod csv_data;
mod drag_scroll;
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
mod text_select;
mod viewer;

use std::path::PathBuf;

use gpui::{
    App, Application, Bounds, Menu, MenuItem, WindowBounds, WindowOptions, prelude::*, px, size,
};

use actions::{CloseWindow, FindInTable, GotoRow, OpenFile, Quit, app_key_bindings};
use viewer::Viewer;

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
                    viewer.focus(window);
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
