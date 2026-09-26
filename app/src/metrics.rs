use std::time::{Duration, Instant};

/// 环形缓冲容量：约 10 秒 @60fps。
const CAPACITY: usize = 600;
/// 每这么多帧汇报一次。
const REPORT_EVERY: u64 = 300;

/// 帧时间统计（设计文档 §16.5）。
///
/// # 配对契约：未配对的 `begin_frame()` 是**常态**，不是异常
///
/// `main.rs` 的主循环里有三条 `continue` 路径（`Timeout|Occluded`、
/// `Outdated|Lost`）在 `begin_frame()` 之后、`end_frame()` 之前跳出循环，
/// 所以「begin 了却没有 end」每天都会发生。本类型的处理方式是：
///
/// - **`begin_frame()` 只记起点。** 若上一个起点还没被 `end_frame()` 消费，
///   就说明那一帧被放弃了：`skipped` 计数 +1，起点被覆盖。
/// - **`end_frame()` 是唯一的样本产出点。** 没走到它的帧不进直方图——
///   那种帧根本没有「结束时刻」，硬造一个样本等于伪造数据。
/// - **被放弃的帧数在 `summary()` 里恒以 `skip` 字段出现**，不是可选信息。
///
/// 为什么非要单独计数，而不是默默丢掉：被放弃的那一帧**恰恰可能是最贵的
/// 一帧**（例如 `get_current_texture()` 卡住 500ms 才返回 `Timeout`），
/// 而它**不会**出现在 p99 里。只报 p99 不报 skip，就等于把这类卡顿藏了
/// 起来——而「藏起来」正是 §16.5 要防的事（「平均 8ms 但每 3 秒卡一次
/// 50ms」）。两者必须同时可见，读的人才判断得出「这份 p99 可不可信」。
pub struct FrameMetrics {
    last: Option<Instant>,
    /// 毫秒，环形缓冲
    samples: Vec<f32>,
    write_at: usize,
    frame_count: u64,
    /// `begin_frame()` 未配对（即该帧被 `continue` 放弃）的次数。
    skipped: u64,
}

impl Default for FrameMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameMetrics {
    pub fn new() -> Self {
        Self {
            last: None,
            samples: Vec::with_capacity(CAPACITY),
            write_at: 0,
            frame_count: 0,
            skipped: 0,
        }
    }

    /// 记下本帧起点。若上一帧的起点未被 `end_frame()` 消费，计入 `skipped`。
    ///
    /// 只在**循环开头**调用一次。重入（同一帧调两次）会被误记为一次跳过。
    pub fn begin_frame(&mut self) {
        if self.last.is_some() {
            self.skipped += 1;
        }
        self.last = Some(Instant::now());
    }

    /// 产出唯一的一笔样本。未配对时直接返回，不产出样本（见类型级文档）。
    pub fn end_frame(&mut self) {
        let Some(start) = self.last.take() else {
            return;
        };
        let ms = start.elapsed().as_secs_f32() * 1000.0;
        if self.samples.len() < CAPACITY {
            self.samples.push(ms);
        } else {
            self.samples[self.write_at] = ms;
            self.write_at = (self.write_at + 1) % CAPACITY;
        }
        self.frame_count += 1;
    }

    /// 按**已产出样本**计数，不是按循环迭代次数——被放弃的帧不算一帧。
    pub fn should_report(&self) -> bool {
        self.frame_count > 0 && self.frame_count % REPORT_EVERY == 0
    }

    pub fn summary(&self) -> String {
        let mut sorted = self.samples.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let avg: f32 = sorted.iter().sum::<f32>() / sorted.len().max(1) as f32;
        format!(
            "帧时间 ms  avg {:>6.2} | p50 {:>6.2} | p95 {:>6.2} | p99 {:>6.2} | max {:>6.2} | skip {:>3}  (n={})",
            avg,
            percentile(&sorted, 0.50),
            percentile(&sorted, 0.95),
            percentile(&sorted, 0.99),
            sorted.last().copied().unwrap_or(0.0),
            self.skipped,
            sorted.len(),
        )
    }
}

/// 分位数，相邻次序统计量之间**线性插值**（Hyndman & Fan type 5 / Hazen 作图位置）。
///
/// `sorted` **必须已升序排序**：本函数刻意不排序。排序是 O(n log n)，调用方
/// 排一次就能算全部四个分位；放在这里则每个分位各排一次，白付三倍代价。
/// 传未排序输入不会 panic，只会给出无意义的结果。
///
/// 位置取 `pos = p·n − 0.5`，再在 `floor(pos)` 与 `floor(pos)+1` 之间插值；
/// `pos` 夹到 `[0, n−1]`，所以 `p=0` / `p=1` 精确命中首/末元素（无浮点漂移）。
///
/// # 为什么不用「四舍五入取整下标」
///
/// 因为取整下标会在**最要紧的地方**瞎掉。对「99 帧 8ms + 1 帧 100ms」
/// （n=100，正是 §16.5 那句「平均 8ms 但每 3 秒卡一次 50ms」的字面场景）：
///
/// | 方法 | avg | p50 | p95 | p99 | max |
/// |---|---|---|---|---|---|
/// | `round((n−1)·p)` 取整下标 | 8.92 | 8.00 | 8.00 | **8.00** ← 长尾整个消失 | 100.00 |
/// | 本函数（线性插值） | 8.92 | 8.00 | 8.00 | **54.00** | 100.00 |
///
/// 两边的 avg/p50/p95 **完全一样**，只有 p99 分道扬镳——也就是说，用取整下标
/// 时这个系列的 p99 报 8.00ms，而真相是每 100 帧里有 1 帧 100ms。测量工具最
/// 不能接受的失败模式不是不准，而是**静默地看不见问题**；取整下标恰好把
/// §16.5 要抓的那类卡顿藏了起来。（拿它算 1..=100 的 p50 还会得到 51.0，
/// 比真值偏一个下标——小毛病，但同一个病根。）
///
/// 取整下标还有个副作用：结果随 `p` 跳变，p50/p95/p99 会分别吸附到某个
/// 恰好落在整数下标上的样本，掩盖分位之间的真实差距。
pub fn percentile(sorted: &[f32], p: f32) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let p = p.clamp(0.0, 1.0);
    let n = sorted.len();
    let pos = (p * n as f32 - 0.5).clamp(0.0, (n - 1) as f32);
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(n - 1);
    let frac = pos - lo as f32;
    sorted[lo] + (sorted[hi] - sorted[lo]) * frac
}

/// 供后续 GPU timestamp 查询使用；GLES 后端不支持时回退到 CPU 计时（设计文档 §16.5）。
pub fn frame_budget_ms(target_fps: u32) -> f32 {
    1000.0 / target_fps as f32
}

#[allow(dead_code)]
fn _assert_duration_used(_: Duration) {}
