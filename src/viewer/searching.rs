//! 検索：検索バーの操作、背景での検索、一致したセルの強調と、そこまでのスクロール。

use super::*;

impl Viewer {
    /// 検索バーを開いて、入力欄にフォーカスを移す。
    pub(super) fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loaded.is_none() {
            return;
        }
        self.popup = None;
        self.encoding_menu = None;
        self.goto = None;
        let input = match &self.search_input {
            Some(input) => input.clone(),
            None => {
                let input = cx.new(|cx| TextInput::new("検索", cx));
                self.search_subscription = Some(cx.subscribe_in(
                    &input,
                    window,
                    |this, _, event: &TextInputEvent, window, cx| {
                        this.on_search_event(event, window, cx)
                    },
                ));
                self.search_input = Some(input.clone());
                input
            }
        };
        self.search_open = true;
        input.read(cx).focus(window);
        cx.notify();
    }

    /// 検索の入力欄が、変換中か。変換中の `↑` `↓` は、変換候補の選択に使われるので、横取りしない。
    pub(super) fn search_input_is_composing(&self, cx: &App) -> bool {
        self.search_input
            .as_ref()
            .is_some_and(|input| input.read(cx).is_composing())
    }

    pub(super) fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = false;
        self.clear_search();
        self.notice = None;
        self.focus_handle.focus(window);
        cx.notify();
    }

    /// 検索の状態と、実行中の検索を、すべて捨てる。
    pub(super) fn clear_search(&mut self) {
        self.search_cancel.store(true, Ordering::Relaxed);
        self.count_cancel.store(true, Ordering::Relaxed);
        self.rank_cancel.store(true, Ordering::Relaxed);
        self.search_task = None;
        self.count_task = None;
        self.rank_task = None;
        self.search_state = None;
    }

    pub(super) fn on_search_event(
        &mut self,
        event: &TextInputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TextInputEvent::Cancel => self.close_search(window, cx),
            TextInputEvent::Changed => self.on_search_text_changed(window, cx),
            TextInputEvent::Confirm { backward } => {
                let dir = if *backward {
                    Direction::Backward
                } else {
                    Direction::Forward
                };
                self.search_step(dir, window, cx);
            }
        }
    }

    /// 検索語が変わった（文字が確定した）。今の一致が、まだ一致していれば、そこに留まる。
    pub(super) fn on_search_text_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let needle = self
            .search_input
            .as_ref()
            .map(|input| input.read(cx).text().to_string())
            .unwrap_or_default();
        let kept = self.search_state.as_ref().and_then(|s| s.current);
        self.clear_search();
        self.notice = None;
        let Some(matcher) = Matcher::new(&needle) else {
            cx.notify();
            return;
        };
        self.search_state = Some(SearchState {
            matcher,
            needle,
            current: kept,
            count: None,
            busy: false,
            rank: Rank::Unknown,
        });
        let columns = self.loaded.as_ref().map_or(0, |l| l.table.column_count());
        // 「今の一致の1つ前から」前向きに探すと、今の一致自身も、探す対象に入る。
        let start = match kept {
            Some(current) => current.previous_cell(columns),
            None => self.top_of_view().previous_cell(columns),
        };
        self.run_search(start, Direction::Forward, window, cx);
        self.start_count(window, cx);
    }

    /// 画面の先頭に見えている行の、先頭のセル。
    pub(super) fn top_of_view(&self) -> Match {
        Match {
            row: self.scroll.row,
            col: 0,
        }
    }

    /// 次（または前）の一致へ。一致がまだなければ、画面の先頭から探す。
    pub(super) fn search_step(
        &mut self,
        dir: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.search_state.as_ref() else {
            return;
        };
        let start = state.current.or_else(|| match dir {
            Direction::Forward => {
                let columns = self.loaded.as_ref().map_or(0, |l| l.table.column_count());
                self.top_of_view().previous_cell(columns)
            }
            Direction::Backward => Some(self.top_of_view()),
        });
        self.notice = None;
        self.run_search(start, dir, window, cx);
    }

    /// `start` の次（後ろ向きなら前）の一致を探す。見つからなければ、反対の端から探し直す（折り返し）。
    /// 全読み込みの表は、その場で探す。遅延読み込みの表は、背景のスレッドで探す。
    pub(super) fn run_search(
        &mut self,
        start: Option<Match>,
        dir: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.search_cancel.store(true, Ordering::Relaxed);
        self.search_task = None;
        let flag = Arc::new(AtomicBool::new(false));
        self.search_cancel = flag.clone();

        let (Some(state), Some(loaded)) = (self.search_state.as_mut(), self.loaded.as_ref()) else {
            return;
        };
        let table = &loaded.table;
        let can_wrap = start.is_some();

        // まず、同じ行の中の、隣のセルを調べる（どちらの方式でも、すぐ終わる）。
        if let Some(found) =
            start.and_then(|m| search::same_row_step(table, &state.matcher, m, dir))
        {
            state.busy = false;
            self.apply_search(Outcome::Found(found), false, dir, window, cx);
            return;
        }

        match table.lazy_searcher() {
            None => {
                let outcome = search::find_match(table, &state.matcher, start, dir, &flag);
                let (outcome, wrapped) = match outcome {
                    Outcome::NotFound if can_wrap => (
                        search::find_match(table, &state.matcher, None, dir, &flag),
                        true,
                    ),
                    other => (other, false),
                };
                state.busy = false;
                self.apply_search(outcome, wrapped, dir, window, cx);
            }
            Some(searcher) => {
                state.busy = true;
                let needle = state.needle.clone();
                let from_row = search::first_row_to_search(start, dir, table.row_count());
                let wrap_row = match dir {
                    Direction::Forward => 0,
                    Direction::Backward => table.row_count(),
                };
                let task = cx.background_spawn(async move {
                    let Some(matcher) = Matcher::new(&needle) else {
                        return (Outcome::NotFound, false);
                    };
                    match searcher.find_row(&matcher, from_row, dir, &flag) {
                        Outcome::NotFound if can_wrap => {
                            (searcher.find_row(&matcher, wrap_row, dir, &flag), true)
                        }
                        other => (other, false),
                    }
                });
                self.search_task = Some(cx.spawn_in(window, async move |this, cx| {
                    let (outcome, wrapped) = task.await;
                    this.update_in(cx, |this, window, cx| {
                        this.finish_row_search(outcome, wrapped, dir, window, cx)
                    })
                    .ok();
                }));
                cx.notify();
            }
        }
    }

    /// 背景で見つけた行の中から、一致するセルの列を決めて、結果を反映する。
    pub(super) fn finish_row_search(
        &mut self,
        outcome: Outcome<usize>,
        wrapped: bool,
        dir: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(state), Some(loaded)) = (self.search_state.as_mut(), self.loaded.as_ref()) else {
            return;
        };
        state.busy = false;
        let outcome = match outcome {
            Outcome::Found(row) => {
                match search::match_col_in_row(&loaded.table, &state.matcher, row, dir) {
                    Some(col) => Outcome::Found(Match { row, col }),
                    None => Outcome::NotFound,
                }
            }
            Outcome::NotFound => Outcome::NotFound,
            Outcome::Cancelled => Outcome::Cancelled,
        };
        self.apply_search(outcome, wrapped, dir, window, cx);
    }

    /// 検索の結果を、画面に反映する。
    pub(super) fn apply_search(
        &mut self,
        outcome: Outcome<Match>,
        wrapped: bool,
        dir: Direction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match outcome {
            Outcome::Found(found) => {
                let indexing = self
                    .loaded
                    .as_ref()
                    .is_some_and(|l| l.table.indexing_percent().is_some());
                let mut count_from = None;
                if let Some(state) = self.search_state.as_mut() {
                    // 隣の一致へ動いただけなら、数え直さずに、順位を1つ動かす。
                    let exact_total = state
                        .count
                        .filter(|&(_, capped)| !capped && !indexing)
                        .map(|(total, _)| total);
                    let previous = state.current.map(|m| (m.row, state.rank));
                    state.rank =
                        search::rank_after_move(previous, found.row, dir, wrapped, exact_total);
                    state.current = Some(found);
                    if state.rank == Rank::Unknown {
                        count_from = Some(found.row);
                    }
                }
                if let Some(row) = count_from {
                    self.start_rank(row, window, cx);
                }
                self.notice = wrapped.then(|| match dir {
                    Direction::Forward => "末尾まで検索したので、先頭に戻りました".to_string(),
                    Direction::Backward => "先頭まで検索したので、末尾に戻りました".to_string(),
                });
                self.reveal(found);
            }
            Outcome::NotFound => {
                if let Some(state) = self.search_state.as_mut() {
                    state.current = None;
                    state.rank = Rank::Unknown;
                }
            }
            Outcome::Cancelled => {}
        }
        cx.notify();
    }

    /// 一致したセルが、画面の中に見えるように、スクロールする（縦も横も）。
    pub(super) fn reveal(&mut self, found: Match) {
        let Some(loaded) = self.loaded.as_ref() else {
            return;
        };
        let viewport_height = match f32::from(self.viewport.get().size.height) {
            h if h > 0.0 => h,
            _ => DEFAULT_VIEWPORT_HEIGHT,
        };
        let visible = visible_rows(self.scroll, viewport_height, loaded.table.row_count());
        // 先頭の行が欠けているときと、最後の行は、全体が見えているとは限らない。
        let start = visible.first + usize::from(visible.offset > 0.0);
        let end = visible.last.saturating_sub(1).max(start);
        let scroll = reveal::reveal_row(self.scroll, start..end, found.row, 3);
        let widths = &loaded.col_widths;
        let left: f32 = widths.iter().take(found.col).sum();
        let right = left + widths.get(found.col).copied().unwrap_or(0.0);
        let current_x = -f32::from(self.h_scroll.offset().x);
        let target_x = reveal::reveal_column(
            current_x,
            f32::from(self.h_scroll.bounds().size.width),
            left,
            right,
            f32::from(self.h_scroll.max_offset().width),
        );
        self.scroll = scroll.clamped(self.max_scroll());
        self.h_scroll
            .set_offset(point(px(-target_x), self.h_scroll.offset().y));
    }

    /// 一致する行の数を、数える。全読み込みの表は、その場で。遅延読み込みの表は、背景で。
    /// 現在の一致（行 `row`）が、一致した行の何番目かを、先頭から数える。全読み込みの表は、その場で。
    /// 遅延読み込みの表は、背景で（数えおわるまでは、順位は「…」）。
    pub(super) fn start_rank(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.rank_cancel.store(true, Ordering::Relaxed);
        self.rank_task = None;
        let flag = Arc::new(AtomicBool::new(false));
        self.rank_cancel = flag.clone();

        let (Some(state), Some(loaded)) = (self.search_state.as_mut(), self.loaded.as_ref()) else {
            return;
        };
        match loaded.table.lazy_searcher() {
            None => {
                if let Outcome::Found(rank) =
                    search::rank_of_row(&loaded.table, &state.matcher, row, COUNT_CAP, &flag)
                {
                    state.rank = rank;
                }
            }
            Some(searcher) => {
                state.rank = Rank::Unknown;
                let needle = state.needle.clone();
                let task = cx.background_spawn({
                    let needle = needle.clone();
                    async move {
                        let matcher = Matcher::new(&needle)?;
                        match search::count_matching_rows(
                            |from| searcher.find_row(&matcher, from, Direction::Forward, &flag),
                            Some(row),
                            COUNT_CAP,
                        ) {
                            Outcome::Found(counted) => Some(search::rank_from_count(counted)),
                            _ => None,
                        }
                    }
                });
                self.rank_task = Some(cx.spawn_in(window, async move |this, cx| {
                    let rank = task.await;
                    this.update_in(cx, move |this, _, cx| {
                        if let (Some(state), Some(rank)) = (this.search_state.as_mut(), rank) {
                            // 数えているあいだに、検索語や、現在の一致が変わっていたら、古い結果は捨てる。
                            if state.needle == needle && state.current.map(|m| m.row) == Some(row) {
                                state.rank = rank;
                                cx.notify();
                            }
                        }
                    })
                    .ok();
                }));
            }
        }
        cx.notify();
    }

    pub(super) fn start_count(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.count_cancel.store(true, Ordering::Relaxed);
        self.count_task = None;
        let flag = Arc::new(AtomicBool::new(false));
        self.count_cancel = flag.clone();

        let (Some(state), Some(loaded)) = (self.search_state.as_mut(), self.loaded.as_ref()) else {
            return;
        };
        match loaded.table.lazy_searcher() {
            None => {
                state.count =
                    match search::count_rows(&loaded.table, &state.matcher, COUNT_CAP, &flag) {
                        Outcome::Found(counted) => Some(counted),
                        _ => None,
                    };
            }
            Some(searcher) => {
                let needle = state.needle.clone();
                let task = cx.background_spawn(async move {
                    let matcher = Matcher::new(&needle)?;
                    let counted = search::count_matching_rows(
                        |from| searcher.find_row(&matcher, from, Direction::Forward, &flag),
                        None,
                        COUNT_CAP,
                    );
                    match counted {
                        Outcome::Found(counted) => Some(counted),
                        _ => None,
                    }
                });
                let needle = state.needle.clone();
                self.count_task = Some(cx.spawn_in(window, async move |this, cx| {
                    let counted = task.await;
                    this.update_in(cx, move |this, _, cx| {
                        if let Some(state) = this.search_state.as_mut() {
                            // 数えているあいだに、検索語が変わっていたら、古い結果は捨てる。
                            if state.needle == needle {
                                state.count = counted;
                                cx.notify();
                            }
                        }
                    })
                    .ok();
                }));
            }
        }
        cx.notify();
    }

    /// 検索バー。表の右上に重ねて出す。
    pub(super) fn render_search(&self) -> Option<impl IntoElement> {
        if !self.search_open {
            return None;
        }
        let input = self.search_input.clone()?;
        Some(
            div()
                .id("search-bar")
                .occlude()
                .absolute()
                .top(px(self.header_height() + 8.0))
                .right(px(24.0))
                .w(px(340.0))
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .bg(rgb(0xf6f6f6))
                .border_1()
                .border_color(rgb(0xaaaaaa))
                .rounded_md()
                .shadow_md()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x888888))
                        .child("検索　Enter・↓: 次 / Shift+Enter・↑: 前 / Esc: 閉じる"),
                )
                .child(input)
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(0x555555))
                        .child(self.search_info()),
                ),
        )
    }

    /// 検索バーの下に出す、件数などの案内。
    pub(super) fn search_info(&self) -> String {
        let Some(state) = self.search_state.as_ref() else {
            return String::new();
        };
        let indexing = self
            .loaded
            .as_ref()
            .is_some_and(|l| l.table.indexing_percent().is_some());
        search_info_text(
            state.busy,
            state.count,
            indexing,
            state.current.map(|m| (m.row, state.rank)),
        )
    }
}

/// 検索バーの下に出す案内の文言。「3 / 12 件（5 行目）」のように、一致した行のうち、何番目かを出す。
///
/// - `busy`: 背景の検索の結果を待っている
/// - `count`: 一致する行の数と、上限で打ち切ったか。数えおわるまでは `None`
/// - `indexing`: 遅延読み込みの索引を作っている途中（この先に、まだ行が増える）
/// - `current`: 現在の一致の（行（0始まり）、一致した行の何番目か）
fn search_info_text(
    busy: bool,
    count: Option<(usize, bool)>,
    indexing: bool,
    current: Option<(usize, Rank)>,
) -> String {
    if busy {
        return "検索中…".to_string();
    }
    let total = match count {
        Some((0, _)) if !indexing => return "見つかりません".to_string(),
        Some((n, capped)) => format!("{n}{}", if capped || indexing { "+" } else { "" }),
        None => "…".to_string(),
    };
    match current {
        Some((row, rank)) => {
            let position = match rank {
                Rank::Known(k) => k.to_string(),
                Rank::TooFar => format!("{COUNT_CAP}+"),
                Rank::Unknown => "…".to_string(),
            };
            format!("{position} / {total} 件（{} 行目）", row + 1)
        }
        None if count.is_none() => "数えています…".to_string(),
        None => format!("{total} 件"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_info_text_covers_each_state() {
        use Rank::{Known, TooFar, Unknown};
        // 検索中は、ほかの情報より、検索中であることを優先する。
        assert_eq!(
            search_info_text(true, Some((5, false)), false, Some((2, Known(1)))),
            "検索中…"
        );
        // 一致が0行で、索引も作りおわっているなら、見つからない。
        assert_eq!(
            search_info_text(false, Some((0, false)), false, None),
            "見つかりません"
        );
        // 索引の作成中は、0行でも、この先に見つかるかもしれない。
        assert_eq!(
            search_info_text(false, Some((0, false)), true, None),
            "0+ 件"
        );
        assert_eq!(
            search_info_text(false, Some((12, false)), false, None),
            "12 件"
        );
        // 上限で打ち切ったときと、索引の作成中は、「+」をつける。
        assert_eq!(
            search_info_text(false, Some((100_000, true)), false, None),
            "100000+ 件"
        );
        assert_eq!(
            search_info_text(false, Some((12, false)), true, None),
            "12+ 件"
        );
        // 数えている途中。
        assert_eq!(search_info_text(false, None, false, None), "数えています…");
        // 現在の一致は、「何番目 / 何件（何行目）」。
        assert_eq!(
            search_info_text(false, Some((12, false)), false, Some((4, Known(3)))),
            "3 / 12 件（5 行目）"
        );
        // 順位や総数が、まだ分からないときは、「…」。
        assert_eq!(
            search_info_text(false, None, false, Some((41, Unknown))),
            "… / … 件（42 行目）"
        );
        assert_eq!(
            search_info_text(false, Some((12, false)), false, Some((0, Unknown))),
            "… / 12 件（1 行目）"
        );
        // 上限を超えた位置は、「100000+」。
        assert_eq!(
            search_info_text(false, Some((100_000, true)), false, Some((7, TooFar))),
            "100000+ / 100000+ 件（8 行目）"
        );
        // 索引の作成中は、総数に「+」。
        assert_eq!(
            search_info_text(false, Some((12, false)), true, Some((0, Known(1)))),
            "1 / 12+ 件（1 行目）"
        );
    }
}
