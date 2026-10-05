use gpui::{Pixels, ScrollHandle, UniformListScrollHandle, px};

pub const THICKNESS: f32 = 10.0;
pub const MARGIN: f32 = 2.0;
const MIN_THUMB_LEN: f32 = 24.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

/// ある軸方向の表示領域・全体サイズ・スクロール量。座標はウィンドウ基準。
#[derive(Clone, Copy)]
pub struct AxisState {
    pub origin: Pixels,
    pub viewport: Pixels,
    pub content: Pixels,
    pub offset: Pixels,
}

/// つまみの位置と長さ（表示領域の先頭からの相対値）。
#[derive(Clone, Copy)]
pub struct Thumb {
    pub pos: Pixels,
    pub len: Pixels,
}

impl AxisState {
    pub fn vertical(list: &UniformListScrollHandle) -> Self {
        let state = list.0.borrow();
        let bounds = state.base_handle.bounds();
        Self {
            origin: bounds.origin.y,
            viewport: bounds.size.height,
            content: state
                .last_item_size
                .map(|s| s.contents.height)
                .unwrap_or_default(),
            offset: state.base_handle.offset().y,
        }
    }

    pub fn horizontal(handle: &ScrollHandle) -> Self {
        let bounds = handle.bounds();
        Self {
            origin: bounds.origin.x,
            viewport: bounds.size.width,
            content: bounds.size.width + handle.max_offset().width,
            offset: handle.offset().x,
        }
    }

    /// 表示領域が未確定、または全体が収まっているときは None（バーを出さない）。
    pub fn thumb(&self) -> Option<Thumb> {
        if self.viewport <= px(0.) || self.content <= self.viewport {
            return None;
        }
        let len = (self.viewport * (self.viewport / self.content)).max(px(MIN_THUMB_LEN));
        let fraction = (-self.offset / (self.content - self.viewport)).clamp(0., 1.);
        Some(Thumb {
            pos: (self.viewport - len) * fraction,
            len,
        })
    }

    /// つまみの先頭が表示領域の先頭から `pos` の位置にあるときのスクロール量。
    pub fn offset_for_thumb_pos(&self, thumb_len: Pixels, pos: Pixels) -> Pixels {
        let travel = self.viewport - thumb_len;
        if travel <= px(0.) {
            return px(0.);
        }
        let fraction = (pos / travel).clamp(0., 1.);
        -(self.content - self.viewport) * fraction
    }
}
