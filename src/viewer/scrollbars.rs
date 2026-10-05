//! スクロールバー：つまみの位置と、ドラッグ。

use super::*;

impl Viewer {
    pub(super) fn axis_state(&self, axis: Axis) -> AxisState {
        match axis {
            Axis::Vertical => {
                let viewport = self.viewport.get();
                AxisState {
                    origin: viewport.origin.y,
                    viewport: viewport.size.height,
                    // つまみの長さの比率を求めるためだけに使うので、`f32` に丸めても問題ない。
                    content: px((self.row_count() as f64 * f64::from(ROW_HEIGHT)) as f32),
                    fraction: self.scroll.fraction(self.max_scroll()),
                }
            }
            Axis::Horizontal => AxisState::horizontal(&self.h_scroll),
        }
    }

    pub(super) fn start_drag(&mut self, axis: Axis, mouse: Point<Pixels>) {
        let state = self.axis_state(axis);
        let Some(thumb) = state.thumb() else { return };
        let mouse = axis_value(axis, mouse);
        self.drag = Some(Drag {
            axis,
            grab: mouse - state.origin - thumb.pos,
        });
    }

    pub(super) fn drag_to(&mut self, mouse: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(Drag { axis, grab }) = self.drag else {
            return;
        };
        let state = self.axis_state(axis);
        let Some(thumb) = state.thumb() else { return };
        let pos = axis_value(axis, mouse) - state.origin - grab;
        let fraction = state.fraction_for_thumb_pos(thumb.len, pos);
        match axis {
            Axis::Vertical => {
                self.scroll = ScrollPos::from_fraction(fraction, self.max_scroll());
            }
            Axis::Horizontal => {
                let travel = f64::from(f32::from(state.content - state.viewport));
                let offset = px(-(travel * fraction) as f32);
                self.h_scroll
                    .set_offset(point(offset, self.h_scroll.offset().y));
            }
        }
        cx.notify();
    }

    pub(super) fn thumb_element(
        &self,
        axis: Axis,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let thumb = self.axis_state(axis).thumb()?;
        let (id, base) = match axis {
            Axis::Vertical => (
                "vbar",
                div()
                    .absolute()
                    .top(px(self.header_height()) + thumb.pos)
                    .right(px(scrollbar::MARGIN))
                    .w(px(scrollbar::THICKNESS))
                    .h(thumb.len),
            ),
            Axis::Horizontal => (
                "hbar",
                div()
                    .absolute()
                    .left(px(self.gutter_width()) + thumb.pos)
                    .bottom(px(scrollbar::MARGIN))
                    .h(px(scrollbar::THICKNESS))
                    .w(thumb.len),
            ),
        };
        let dragging = self.drag.as_ref().is_some_and(|d| d.axis == axis);
        Some(
            base.id(id)
                .rounded_full()
                .bg(rgba(if dragging { 0x00000099 } else { 0x00000055 }))
                .hover(|d| d.bg(rgba(0x00000088)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                        this.start_drag(axis, ev.position);
                        cx.notify();
                    }),
                ),
        )
    }
}
