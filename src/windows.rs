//! ファイルをどのウィンドウで開くか。
//!
//! 空のウィンドウ（何も開いていない）は、残しても価値がないので、あれば使い回す。
//! なければ、新しいウィンドウで開く。複数のファイルは、1つずつウィンドウに入れる。

use std::path::PathBuf;

use gpui::{App, Bounds, WindowBounds, WindowHandle, WindowOptions, point, prelude::*, px, size};

use crate::viewer::Viewer;

/// 新しいウィンドウを、前のウィンドウから少しずつずらす最大の段数。
const CASCADE_STEPS: usize = 8;
const CASCADE_OFFSET: f32 = 24.0;

/// 空のウィンドウのうち、最初のものの位置。`empty` は、ウィンドウごとの「空か」。
fn first_empty(empty: &[bool]) -> Option<usize> {
    empty.iter().position(|&e| e)
}

/// `paths` を、それぞれ開く。`paths` が空なら、何も開いていないウィンドウを1つ用意する
/// （空のウィンドウがすでにあれば、新しくは作らない）。
pub(crate) fn open_paths(paths: Vec<PathBuf>, cx: &mut App) {
    if paths.is_empty() {
        if empty_window(cx).is_none() {
            open_new_window(None, cx);
        }
        return;
    }
    for path in paths {
        match empty_window(cx) {
            Some(handle) => {
                handle
                    .update(cx, |viewer, window, cx| {
                        window.activate_window();
                        viewer.open_path(path, window, cx);
                    })
                    .ok();
            }
            None => open_new_window(Some(path), cx),
        }
    }
}

/// 使い回せる空のウィンドウ。読み込み中のウィンドウは、空ではない。
fn empty_window(cx: &App) -> Option<WindowHandle<Viewer>> {
    let handles: Vec<WindowHandle<Viewer>> = cx
        .windows()
        .into_iter()
        .filter_map(|w| w.downcast::<Viewer>())
        .collect();
    // 読めない（いま更新中の）ウィンドウは、空とは見なさない。
    let empty: Vec<bool> = handles
        .iter()
        .map(|h| h.read(cx).is_ok_and(|v| v.is_empty()))
        .collect();
    first_empty(&empty).map(|i| handles[i])
}

fn open_new_window(path: Option<PathBuf>, cx: &mut App) {
    let step = cx.windows().len() % CASCADE_STEPS;
    let mut bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
    bounds.origin += point(px(CASCADE_OFFSET), px(CASCADE_OFFSET)) * step as f32;
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        },
        |window, cx| {
            cx.new(|cx| {
                let mut viewer = Viewer::new(cx);
                if let Some(path) = path {
                    viewer.open_path(path, window, cx);
                }
                viewer.focus(window);
                viewer
            })
        },
    );
    if let Err(e) = opened {
        eprintln!("csv_viewer: could not open a window: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_empty_window_is_reused() {
        assert_eq!(first_empty(&[false, true, true]), Some(1));
        assert_eq!(first_empty(&[true]), Some(0));
    }

    #[test]
    fn without_an_empty_window_a_new_one_is_needed() {
        assert_eq!(first_empty(&[false, false]), None);
        assert_eq!(first_empty(&[]), None);
    }
}
