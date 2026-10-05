use gpui::{Pixels, ScrollHandle, px};

pub const THICKNESS: f32 = 10.0;
pub const MARGIN: f32 = 2.0;
const MIN_THUMB_LEN: f32 = 24.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// ある軸方向の表示領域・全体サイズ・スクロール量。座標はウィンドウ基準。
///
/// スクロール量は、全体の長さに対する割合（0〜1）で持つ。縦は行数が数千万を超え得るため、
/// ピクセル数にすると精度が足りなくなる。`content` は、つまみの長さを求める比率にだけ使う。
#[derive(Clone, Copy)]
pub struct AxisState {
    pub origin: Pixels,
    pub viewport: Pixels,
    pub content: Pixels,
    /// どこまでスクロールしたかの割合。0 が先頭、1 が末尾。
    pub fraction: f64,
}

/// つまみの位置と長さ（表示領域の先頭からの相対値）。
#[derive(Clone, Copy)]
pub struct Thumb {
    pub pos: Pixels,
    pub len: Pixels,
}

impl AxisState {
    pub fn horizontal(handle: &ScrollHandle) -> Self {
        let bounds = handle.bounds();
        let max_offset = f64::from(f32::from(handle.max_offset().width));
        let fraction = if max_offset > 0.0 {
            (-f64::from(f32::from(handle.offset().x)) / max_offset).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Self {
            origin: bounds.origin.x,
            viewport: bounds.size.width,
            content: bounds.size.width + handle.max_offset().width,
            fraction,
        }
    }

    /// 表示領域が未確定、または全体が収まっているときは None（バーを出さない）。
    pub fn thumb(&self) -> Option<Thumb> {
        if self.viewport <= px(0.) || self.content <= self.viewport {
            return None;
        }
        let len = (self.viewport * (self.viewport / self.content)).max(px(MIN_THUMB_LEN));
        Some(Thumb {
            pos: (self.viewport - len) * self.fraction.clamp(0.0, 1.0) as f32,
            len,
        })
    }

    /// つまみの先頭が表示領域の先頭から `pos` の位置にあるときの、スクロール量の割合（0〜1）。
    pub fn fraction_for_thumb_pos(&self, thumb_len: Pixels, pos: Pixels) -> f64 {
        let travel = f64::from(f32::from(self.viewport - thumb_len));
        if travel <= 0.0 {
            return 0.0;
        }
        (f64::from(f32::from(pos)) / travel).clamp(0.0, 1.0)
    }
}
