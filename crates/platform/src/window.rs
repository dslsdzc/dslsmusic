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

    /// 供 `gpu.rs` 创建 surface 使用。**不要在别处调用。**
    pub(crate) fn raw_window(&self) -> &sdl3::video::Window {
        &self.window
    }

    /// 取走本帧的全部事件（只保留最后一个 resize / 第一个 quit）。
    ///
    /// 接收者是 `&self`：`GpuContext` 借用窗口期间不可能取得 `&mut`。
    pub fn poll_events(&self) -> PlatformEvent {
        let mut result = PlatformEvent::None;
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
                    win_event: WindowEvent::PixelSizeChanged(w, h) | WindowEvent::Resized(w, h),
                    ..
                } if window_id == self_id => {
                    result = PlatformEvent::Resized(w as u32, h as u32);
                }
                _ => {}
            }
        }
        result
    }
}
