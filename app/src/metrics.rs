pub struct FrameMetrics;

impl FrameMetrics {
    pub fn new() -> Self {
        Self
    }
    pub fn begin_frame(&mut self) {}
    pub fn end_frame(&mut self) {}
    pub fn should_report(&self) -> bool {
        false
    }
    pub fn summary(&self) -> String {
        String::new()
    }
}
