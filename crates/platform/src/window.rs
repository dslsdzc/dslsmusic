use std::cell::RefCell;

use sdl3::event::{Event, WindowEvent};
use sdl3::keyboard::Keycode;

/// 对上层暴露的事件。SDL 的类型不允许逸出本 crate（设计文档 §11.3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformEvent {
    Resized(u32, u32),
    CloseRequested,
    /// 本帧没有需要处理的事件
    None,
}

pub struct PlatformWindow {
    window: sdl3::video::Window,
    /// `RefCell` 而非 `&mut self`：`GpuContext<'a>` 全程持有对窗口的**共享**借用，
    /// 只要 surface 还活着就不可能再拿到 `&mut PlatformWindow`，事件轮询必须走内部可变性。
    /// 本 crate 本就不跨线程（SDL 句柄），`RefCell` 的 !Sync 不是新增约束。
    event_pump: RefCell<sdl3::EventPump>,
}

impl PlatformWindow {
    pub fn new(title: &str, width: u32, height: u32) -> anyhow::Result<Self> {
        let sdl = sdl3::init()?;
        let video = sdl.video()?;
        let window = video
            .window(title, width, height)
            .position_centered()
            .resizable()
            .metal_view()
            .build()
            .map_err(|e| anyhow::anyhow!("SDL 窗口创建失败: {e}"))?;
        let event_pump = sdl.event_pump()?;
        Ok(Self {
            window,
            event_pump: RefCell::new(event_pump),
        })
    }

    pub fn size(&self) -> (u32, u32) {
        let (w, h) = self.window.size();
        (w, h)
    }

    /// 物理像素尺寸（`SDL_GetWindowSizeInPixels`）。
    /// **配置 wgpu surface 必须用这个**，不能用 `size()`——后者是逻辑点。
    pub fn size_in_pixels(&self) -> (u32, u32) {
        let (w, h) = self.window.size_in_pixels();
        (w, h)
    }

    /// 供 `gpu.rs` 创建 surface 使用。**不要在别处调用。**
    pub(crate) fn raw_window(&self) -> &sdl3::video::Window {
        &self.window
    }

    /// 取走本帧的全部事件（只保留最后一个 resize / 第一个 quit）。
    ///
    /// 接收者是 `&self`：`GpuContext` 借用窗口期间不可能取得 `&mut`。
    pub fn poll_events(&self) -> PlatformEvent {
        let mut result = PlatformEvent::None;
        // ⚠️ 真需要这个标志位：同一个 pump 批次里可能**两个事件都来**，
        //    而 or-pattern 只能决定单次匹配内的绑定顺序，**不构成两个
        //    事件之间的优先级**。没有它的话，若 PixelSizeChanged 之后
        //    又来了 Resized，物理值会被逻辑值覆盖。
        let mut got_pixel_size = false;
        let self_id = self.window.id();
        for event in self.event_pump.borrow_mut().poll_iter() {
            match event {
                Event::Quit { .. } => return PlatformEvent::CloseRequested,
                Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => {
                    return PlatformEvent::CloseRequested;
                }
                Event::Window {
                    window_id,
                    win_event: WindowEvent::PixelSizeChanged(w, h),
                    ..
                } if window_id == self_id => {
                    result = PlatformEvent::Resized(w as u32, h as u32);
                    got_pixel_size = true;
                }
                Event::Window {
                    window_id,
                    win_event: WindowEvent::Resized(w, h),
                    ..
                } if window_id == self_id && !got_pixel_size => {
                    // 物理像素优先：已拿到 PixelSizeChanged 就不被逻辑值覆盖。
                    // 条件写进 match guard（而不是块内 if）——否则触发 clippy
                    // 的 collapsible_match，而完成标准要求
                    // `cargo clippy --workspace -- -D warnings` 无告警。
                    result = PlatformEvent::Resized(w as u32, h as u32);
                }
                _ => {}
            }
        }
        result
    }
}
