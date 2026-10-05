//! 索引を作る背景の処理（`IndexJob`）。チャンクごとに進め、途中で中断でき、文字コードを遅れて判定する。

use super::*;

/// 索引を1回の `feed` で進める最大バイト数。
pub(super) const CHUNK: usize = 4 * 1024 * 1024;

/// 最初の1回だけは小さくして、最初の行をすぐ読めるようにする。HDD や遅い CPU では、
/// 4MiB の読み出しと走査だけで数十ms かかる（M4 Pro＋NVMe の実測で約8ms → 約2.5ms）。
pub(super) const FIRST_CHUNK: usize = 256 * 1024;

/// 索引を作る側。別スレッドへ渡して `run` を呼ぶ。1チャンクずつ進める `step` も使える。
pub struct IndexJob {
    shared: Arc<Shared>,
    /// 次に走査するバイト位置。
    pos: usize,
    scanner: Scanner,
    /// 見つけたが、まだ索引に追加していないレコード。
    batch: Vec<Record>,
    /// まだ最初のステップを踏んでいないか。
    first_step: bool,
}

impl IndexJob {
    pub(super) fn new(shared: Arc<Shared>, body_start: usize) -> Self {
        Self {
            shared,
            pos: body_start,
            scanner: Scanner::new(body_start),
            batch: Vec::new(),
            first_step: true,
        }
    }

    /// 索引を最後まで作る。`cancel` が立ったら、次のチャンクの前で止まる。
    pub fn run(&mut self, cancel: &AtomicBool) {
        self.run_chunked(cancel, CHUNK);
    }

    /// チャンクの大きさを指定して実行する（テストで境界をまたがせるため）。
    pub fn run_chunked(&mut self, cancel: &AtomicBool, chunk: usize) {
        loop {
            if cancel.load(Ordering::Relaxed) {
                if *self.shared.status.lock().unwrap() == IndexStatus::Indexing {
                    self.set_status(IndexStatus::Cancelled);
                }
                return;
            }
            if !self.step(chunk) {
                return;
            }
        }
    }

    /// 最大 `chunk` バイトぶん進める。まだ続きがあれば `true`。
    pub fn step(&mut self, chunk: usize) -> bool {
        if *self.shared.status.lock().unwrap() != IndexStatus::Indexing {
            return false;
        }
        match self.shared.kind {
            UnitKind::Byte => self.step_with::<Byte>(chunk),
            UnitKind::Utf16Le => self.step_with::<Utf16Le>(chunk),
            UnitKind::Utf16Be => self.step_with::<Utf16Be>(chunk),
        }
    }

    fn set_status(&self, status: IndexStatus) {
        *self.shared.status.lock().unwrap() = status;
    }

    fn step_with<U: Unit>(&mut self, chunk: usize) -> bool {
        let shared = self.shared.clone();
        let bytes = shared.bytes();
        let end = aligned_end::<U>(bytes);
        let chunk = if std::mem::take(&mut self.first_step) {
            chunk.min(FIRST_CHUNK)
        } else {
            chunk
        };
        let chunk = chunk.max(U::WIDTH) / U::WIDTH * U::WIDTH;

        if self.pos >= end {
            if let Some(record) = self.scanner.finish(end) {
                self.batch.push(record);
            }
            self.publish(end);
            self.set_status(IndexStatus::Done);
            return false;
        }
        let to = (self.pos + chunk).min(end);

        if !shared.encoding.lock().unwrap().locked {
            if let Some(offset) = find_first_non_ascii(&bytes[self.pos..to]) {
                let encoding = classify_from(bytes, self.pos + offset);
                *shared.encoding.lock().unwrap() = EncodingState {
                    encoding,
                    locked: true,
                };
                if encoding == encoding_rs::ISO_2022_JP {
                    self.set_status(IndexStatus::Unsupported(encoding));
                    return false;
                }
            }
        }

        let batch = &mut self.batch;
        self.pos = self.scanner.feed::<U>(bytes, self.pos, to, &mut |record| {
            batch.push(record);
            std::ops::ControlFlow::Continue(())
        });
        self.publish(self.pos);
        true
    }

    /// 溜めたレコードを索引へ追加する。ロックはチャンクごとに1回だけ取る。
    fn publish(&mut self, scanned: usize) {
        {
            let mut index = self.shared.index.lock().unwrap();
            for record in self.batch.drain(..) {
                index.push(record);
            }
        }
        self.shared.scanned.store(scanned, Ordering::Relaxed);
    }
}
