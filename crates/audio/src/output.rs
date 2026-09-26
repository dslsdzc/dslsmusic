use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// 无锁 SPSC 环形缓冲，用 `AudioRing::split()` 拆成生产端与消费端。
///
/// **消费端在音频回调线程里使用：无锁、无分配、无 IO**（设计文档 §16.2）。
/// 原计划用 `Mutex<Vec<i16>>`，那会在实时线程里加锁 —— 已废弃。
pub struct AudioRing {
    capacity: usize,
}

impl AudioRing {
    /// `capacity` 是**样本个数**（不是字节数、不是帧数）。
    ///
    /// # Panics
    ///
    /// `capacity == 0` 时 panic（ringbuf 的既定行为，已核实源码
    /// `ringbuf-0.4.8/src/rb/macros.rs:20`）。
    pub fn new(capacity: usize) -> Self {
        Self { capacity }
    }

    pub fn split(self) -> (AudioRingProducer, AudioRingConsumer) {
        let (prod, cons) = HeapRb::<i16>::new(self.capacity).split();
        let overflows = Arc::new(AtomicU64::new(0));
        let underruns = Arc::new(AtomicU64::new(0));
        (
            AudioRingProducer {
                inner: prod,
                overflows: Arc::clone(&overflows),
                underruns: Arc::clone(&underruns),
            },
            AudioRingConsumer {
                inner: cons,
                underruns,
            },
        )
    }
}

/// 生产端 —— 解码线程使用。`!Sync` 但 `Send`。
pub struct AudioRingProducer {
    inner: HeapProd<i16>,
    overflows: Arc<AtomicU64>,
    underruns: Arc<AtomicU64>,
}

impl AudioRingProducer {
    /// 推入样本。**环满时新数据被丢弃并计一次溢出**，绝不阻塞、绝不分配。
    pub fn push_samples(&mut self, data: &[i16]) {
        let pushed = self.inner.push_slice(data);
        if pushed < data.len() {
            self.overflows.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 当前环内可读样本数（供背压判断）。
    pub fn available(&self) -> usize {
        self.inner.occupied_len()
    }

    pub fn overflows(&self) -> u64 {
        self.overflows.load(Ordering::Relaxed)
    }

    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
}

/// 消费端 —— **音频回调线程使用**。`!Sync` 但 `Send`。
pub struct AudioRingConsumer {
    inner: HeapCons<i16>,
    underruns: Arc<AtomicU64>,
}

impl AudioRingConsumer {
    /// 取样本填满 `out`。**取不满时尾部补静音并计一次 underrun**。
    /// 返回实际取到的样本数。永不阻塞、永不分配。
    pub fn pop_samples(&mut self, out: &mut [i16]) -> usize {
        let n = self.inner.pop_slice(out);
        if n < out.len() {
            out[n..].fill(0);
            self.underruns.fetch_add(1, Ordering::Relaxed);
        }
        n
    }

    /// **不补静音、不计数**的原始取值。永不阻塞、永不分配。
    ///
    /// 音频回调用它，而不用 `pop_samples`——因为**静音必须补在目标采样格式
    /// `T` 的域里**：`f32` 的静音是 `0.0`、`i16` 是 `0`、**`u8` 是 `128`**。
    /// 在 i16 域里补 0 再转换是对的，但直接往 `T` 上写 `0` 在 u8 设备上
    /// 是满幅直流噪声。回调侧的补静音见 `fill_from_ring` 的 `T::EQUILIBRIUM`。
    pub fn pop_raw(&mut self, out: &mut [i16]) -> usize {
        self.inner.pop_slice(out)
    }

    /// 当前环内可读样本数。**无锁**（两次原子读 + 本地缓存），回调里可用。
    ///
    /// 回调靠它把 pop 长度**向下取到偶数**（整帧立体声），见 `fill_from_ring`：
    /// 环里可能装着奇数个样本（push 端按任意边界切分同一个立体声流是合法的），
    /// 那半个样本必须留在环里，不能取出来丢掉。
    pub fn available(&self) -> usize {
        self.inner.occupied_len()
    }

    /// 记一次欠载。**按「事件」计，不按缺失样本数计**——与 `pop_samples`
    /// 的口径一致，这样「xrun 必须为 0」这条硬指标才有可比性。
    pub fn count_underruns(&self, n: u64) {
        self.underruns.fetch_add(n, Ordering::Relaxed);
    }

    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_order() {
        let ring = AudioRing::new(8);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[1, 2, 3]);
        let mut out = [0i16; 3];
        assert_eq!(cons.pop_samples(&mut out), 3);
        assert_eq!(out, [1, 2, 3]);
    }

    #[test]
    fn counts_underrun_when_empty() {
        let ring = AudioRing::new(8);
        let (_prod, mut cons) = ring.split();
        // ⚠️ 预填 99、**不是** 0：`out` 若初值就是 0，下面那条断言**恒真**
        //    （不管 `pop_samples` 有没有补静音都通过），等于没测。
        //    这是「空环」那一路；「取到一部分」那一路在
        //    `partial_underrun_still_fills_silence`。
        let mut out = [99i16; 4];
        assert_eq!(cons.pop_samples(&mut out), 0);
        assert_eq!(cons.underruns(), 1);
        // 回调必须填满整块缓冲 —— 没数据时填静音，绝不能留垃圾值（那是爆音）
        assert_eq!(out, [0i16; 4]);
    }

    #[test]
    fn counts_overflow_and_keeps_oldest() {
        // ringbuf 的语义：环满之后**新数据被丢弃**（不是挤掉最旧的）
        let ring = AudioRing::new(4);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[1, 2, 3, 4, 5, 6]);
        assert_eq!(prod.overflows(), 1, "6 个样本推进容量 4 的环，应记一次溢出");
        let mut out = [0i16; 4];
        assert_eq!(cons.pop_samples(&mut out), 4);
        assert_eq!(out, [1, 2, 3, 4], "ringbuf 保留先到的数据");
    }

    #[test]
    fn partial_underrun_still_fills_silence() {
        let ring = AudioRing::new(8);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[7, 8]);
        let mut out = [99i16; 4];
        assert_eq!(cons.pop_samples(&mut out), 2);
        assert_eq!(out, [7, 8, 0, 0], "尾部必须补静音，不能保留 out 原值");
        assert_eq!(cons.underruns(), 1, "取不满也算一次 underrun");
    }
}
