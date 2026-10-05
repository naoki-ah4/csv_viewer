//! 遅延読み込みの表の検索（`LazySearcher`）。生のバイト列を先に走査して候補を絞り、セルの中身で確かめる。

use super::*;

/// 生のバイト列を、1回の正規表現で走査する最大の長さ。中断の要求に、すぐ応えられる長さにする。
const SEARCH_SLICE: usize = 64 * 1024 * 1024;

/// 後ろ向きの検索で、候補を集める窓の大きさ。
const SEARCH_WINDOW: usize = 4 * 1024 * 1024;

/// 遅延読み込みの表を検索する。生のバイト列を先に走査して候補を絞り、
/// その行をデコードして、セルの中身で確かめる。索引ができている範囲だけを調べる。
#[derive(Clone)]
pub struct LazySearcher {
    shared: Arc<Shared>,
    /// 1回の走査の最大の長さ。
    slice: usize,
    /// 後ろ向きの検索で、候補を集める窓の大きさ。
    window: usize,
}

impl LazySearcher {
    pub(super) fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            slice: SEARCH_SLICE,
            window: SEARCH_WINDOW,
        }
    }

    /// 走査の長さを小さくして、境界をまたぐ動きを、テストで確かめられるようにする。
    #[cfg(test)]
    pub(crate) fn with_limits(mut self, slice: usize, window: usize) -> Self {
        self.slice = slice.max(1);
        self.window = window.max(1);
        self
    }

    /// `from_row` 以降（前向き）、または `from_row` より前（後ろ向き）で、最初に一致する行。
    /// 行は、ヘッダーを除くデータ行の0始まり。`cancel` が立つと、`Cancelled` を返す。
    pub fn find_row(
        &self,
        matcher: &Matcher,
        from_row: usize,
        dir: Direction,
        cancel: &AtomicBool,
    ) -> Outcome<usize> {
        match self.shared.kind {
            UnitKind::Byte => self.find_row_with::<Byte>(matcher, from_row, dir, cancel),
            UnitKind::Utf16Le => self.find_row_with::<Utf16Le>(matcher, from_row, dir, cancel),
            UnitKind::Utf16Be => self.find_row_with::<Utf16Be>(matcher, from_row, dir, cancel),
        }
    }

    fn find_row_with<U: Unit>(
        &self,
        matcher: &Matcher,
        from_row: usize,
        dir: Direction,
        cancel: &AtomicBool,
    ) -> Outcome<usize> {
        let encoding = self.shared.encoding.lock().unwrap().encoding;
        // 検索語が、この文字コードで表せなければ、どのセルにもない。
        let (Some(regex), Some(raw_len)) = (matcher.raw_regex(encoding), matcher.raw_len(encoding))
        else {
            return Outcome::NotFound;
        };
        let bytes = self.shared.bytes();
        // 索引ができている範囲だけを調べる。
        let limit = self
            .shared
            .scanned
            .load(Ordering::Relaxed)
            .min(aligned_end::<U>(bytes));
        let scan = Scan {
            bytes,
            limit,
            regex: &regex,
            overlap: raw_len.saturating_sub(1),
            matcher,
            encoding,
            cancel,
        };
        match dir {
            Direction::Forward => self.forward::<U>(&scan, from_row),
            Direction::Backward => self.backward::<U>(&scan, from_row),
        }
    }

    fn forward<U: Unit>(&self, scan: &Scan, from_row: usize) -> Outcome<usize> {
        // データ行 r は、レコード r + 1（レコード0はヘッダー）。
        let Some(mut pos) = self.record_start::<U>(scan.bytes, from_row + 1) else {
            return Outcome::NotFound;
        };
        loop {
            let offset = match self.next_candidate(scan, scan.limit, pos) {
                Ok(Some(offset)) => offset,
                Ok(None) => return Outcome::NotFound,
                Err(Cancelled) => return Outcome::Cancelled,
            };
            if U::WIDTH == 2 && offset % 2 != 0 {
                pos = offset + 1;
                continue;
            }
            let Some((record, range)) = self.record_at::<U>(scan.bytes, offset) else {
                // その位置のレコードは、まだ索引にない。
                return Outcome::NotFound;
            };
            if record >= 1 && scan.record_matches(range.clone()) {
                return Outcome::Found(record - 1);
            }
            // この行の中の、ほかの候補は、確認済みなので飛ばす。
            pos = range.end.max(offset + 1);
        }
    }

    fn backward<U: Unit>(&self, scan: &Scan, from_row: usize) -> Outcome<usize> {
        if from_row == 0 {
            return Outcome::NotFound;
        }
        // 調べるのは、レコード1（最初のデータ行）から、レコード from_row まで。
        let end_record = from_row + 1;
        let Some(data_start) = self.record_start::<U>(scan.bytes, 1) else {
            return Outcome::NotFound;
        };
        let end_byte = self
            .record_start::<U>(scan.bytes, end_record)
            .unwrap_or(scan.limit)
            .min(scan.limit);

        let mut window_end = end_byte;
        while window_end > data_start {
            let window_start = window_end.saturating_sub(self.window).max(data_start);
            // 窓の終わりをまたぐ一致も拾えるよう、検索語の長さぶん先まで読む。
            let hay_end = window_end.saturating_add(scan.overlap).min(scan.limit);
            let mut candidates = Vec::new();
            let mut pos = window_start;
            loop {
                match self.next_candidate(scan, hay_end, pos) {
                    Ok(Some(offset)) if offset < window_end => {
                        candidates.push(offset);
                        pos = offset + 1;
                    }
                    Ok(_) => break,
                    Err(Cancelled) => return Outcome::Cancelled,
                }
            }
            // 後ろの候補から確かめる。確認済みの行の中の候補は、飛ばす。
            let mut verified_from = usize::MAX;
            for &offset in candidates.iter().rev() {
                if offset >= verified_from || (U::WIDTH == 2 && offset % 2 != 0) {
                    continue;
                }
                let Some((record, range)) = self.record_at::<U>(scan.bytes, offset) else {
                    continue;
                };
                if record == 0 || record >= end_record {
                    continue;
                }
                if scan.record_matches(range.clone()) {
                    return Outcome::Found(record - 1);
                }
                verified_from = range.start;
            }
            window_end = window_start;
        }
        Outcome::NotFound
    }

    /// `pos` 以降で、最初に正規表現が一致する位置。`limit` までしか読まない。
    /// 長い範囲を、区切って走査して、そのたびに中断の要求を確かめる。
    fn next_candidate(
        &self,
        scan: &Scan,
        limit: usize,
        mut pos: usize,
    ) -> Result<Option<usize>, Cancelled> {
        // 1回の走査には、検索語が丸ごと収まる長さが要る。それより短いと、一致を見つけられない。
        let slice = self.slice.max(scan.overlap + 1);
        while pos < limit {
            if scan.cancel.load(Ordering::Relaxed) {
                return Err(Cancelled);
            }
            let slice_end = pos.saturating_add(slice).min(limit);
            if let Some(found) = scan.regex.find_at(&scan.bytes[..slice_end], pos) {
                return Ok(Some(found.start()));
            }
            if slice_end == limit {
                return Ok(None);
            }
            // 区切りをまたぐ一致を逃さないよう、検索語の長さぶん戻って、続きを走査する。
            pos = slice_end.saturating_sub(scan.overlap).max(pos + 1);
        }
        Ok(None)
    }

    /// レコード `record` の先頭のバイト位置。
    fn record_start<U: Unit>(&self, bytes: &[u8], record: usize) -> Option<usize> {
        let block_start = self.shared.index.lock().unwrap().block_start(record)?;
        locate_in_block::<U>(bytes, block_start, record % STRIDE).map(|range| range.start)
    }

    /// バイト位置 `offset` を含む（その後ろで始まる最初の）レコードの番号と範囲。
    /// まだ索引にないレコードなら `None`。
    fn record_at<U: Unit>(&self, bytes: &[u8], offset: usize) -> Option<(usize, Range<usize>)> {
        let (first_record, block_start, record_count) = {
            let index = self.shared.index.lock().unwrap();
            let (first_record, block_start) = index.block_containing(offset)?;
            (first_record, block_start, index.record_count())
        };
        let (record, range) = record_containing::<U>(bytes, first_record, block_start, offset)?;
        (record < record_count).then_some((record, range))
    }
}

/// 検索の中断。
struct Cancelled;

/// 1回の検索で、使い回すもの。
struct Scan<'a> {
    bytes: &'a [u8],
    /// 調べてよい範囲の終わり（索引ができている範囲）。
    limit: usize,
    regex: &'a regex::bytes::Regex,
    /// 検索語のバイト数 - 1。走査を区切るときの、重なりの長さ。
    overlap: usize,
    matcher: &'a Matcher,
    encoding: &'static Encoding,
    cancel: &'a AtomicBool,
}

impl Scan<'_> {
    /// バイト範囲のレコードをデコードして、どこかのセルが一致するか。
    fn record_matches(&self, range: Range<usize>) -> bool {
        let (text, _) = self
            .encoding
            .decode_without_bom_handling(&self.bytes[range]);
        parse_record(&text)
            .iter()
            .any(|cell| self.matcher.is_match(cell))
    }
}
