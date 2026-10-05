//! ファイルを開く：背景での読み込み、索引の作成、文字コードを変えての開き直し。

use super::*;

impl Viewer {
    /// CSVをバックグラウンドで読み込み、終わったら表示を差し替える。
    /// 読み込み中は直前の表示を残す。別のファイルを開くと、前の読み込みは破棄される。
    pub(crate) fn open_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        // 新しいファイルは、自動判定で開く。
        self.open_with(path, false, None, window, cx);
    }

    /// `force_eager` なら、サイズに関わらず全体を読み込んで開く。
    /// `forced` を指定すると、文字コードの自動判定をせずその文字コードで読む。
    pub(super) fn open_with(
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
    pub(super) fn finish_load(
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
                self.stop_selecting();
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
    pub(super) fn stop_indexing(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.index_task = None;
        self.poll_task = None;
    }

    /// 索引作成をバックグラウンドで走らせ、終わるまで画面を定期的に更新する。
    pub(super) fn start_indexing(
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
    pub(super) fn poll_index(
        &mut self,
        path: &Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
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
    pub(super) fn fall_back_to_eager(
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

    pub(super) fn prompt_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// 文字コードを `forced` に変えて、同じファイルを開き直す。`None` は自動判定に戻す。
    pub(super) fn reopen_with_encoding(
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
}
