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
///
/// # 帧对齐不变量（**丢样本时左右声道会永久互换**）
///
/// 环里承载的是**2 声道交错**的立体声：`L,R,L,R,…`。样本被**丢弃**时，只要丢掉的
/// 个数是**奇数**，此后所有样本的奇偶位置就翻一位 —— 每个「帧」都变成 `(R,L)`，
/// **左右声道永久互换**，而采样率、`xrun`、波形统计**全部照常**。
/// （本项目早先修过的 `got / 2` 截断是同一个坑，只是长在消费端。）
///
/// 所以本类型的硬约束是：**`push_samples` 每次调用丢掉的样本数必须是偶数。**
///
/// ⚠️ 「丢掉的样本数」指**真正被扔掉的个数**，不是「没进环的个数」：扣在 `carry`
/// 里、下次才进环的那 1 个没进环，却**也没被丢**。把它算进去就差 1，奇偶正好
/// 翻面 —— 断言看着通过，声道却已经互换（A1 修复前就是如此，见 `push_samples`）。
///
/// 两侧各守一半：
/// - 消费端 `fill_from_ring`：把 pop 长度**向下取到偶数**，那半个样本留在环里
///   （见 `lib.rs` 里它的注释）。环里因此**允许**出现奇数个样本。
/// - 生产端 [`AudioRingProducer::push_samples`]：**按实际丢弃数配平** —— 能收
///   就少收一个（多丢一个），一个都收不下就扣下最后一个、或把手上那个一起丢掉。
///   注意不是对空闲空间取偶 —— `data` 的长度**允许是奇数**（见那个方法的文档），
///   只在偶数长度时两者才等价。
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
                carry: None,
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
    /// 上一次推送时**扣下**的那一个样本（只有环满才可能发生，见 `push_samples`）。
    /// 它下次排在最前面推出去，所以**流序不变**；最多积压 1 个，不会随调用增长。
    ///
    /// 它不在环里，因此 `available()` 数不到它 —— 这是「环内可读样本数」，不是
    /// 「还有多少样本没进环」，两者的差额最多就是这 1 个。
    ///
    /// **它不算「丢弃」** —— 只是晚一步进环，所以不计入 `push_samples` 的丢弃数、
    /// 也不计溢出事件；代价是它一旦再也没有机会进环（流结束了、环一直满着），
    /// 就**不会留下任何读数**（见 `available` 与 `finish_stream` 的注释）。
    carry: Option<i16>,
}

impl AudioRingProducer {
    /// 推入样本。**环满时新数据被丢弃并计一次溢出**，绝不阻塞、绝不分配。
    ///
    /// ⚠️ **本方法每次调用丢掉的样本数恒为偶数** —— 丢奇数个会**永久翻转
    /// 左右声道**，理由见 [`AudioRing`] 的文档。这是本方法的唯一硬约束。
    /// 它靠**按实际丢弃数配平**成立，不是靠「环满就是偶数个」这类巧合：
    /// 丢弃数 = `[carry] ++ data` 的个数 − 收进环里的 − 仍扣在手上的；
    /// 收得下时丢弃数为奇就**少收一个**，一个都收不下时**`total` 为奇就扣下 1 个、
    /// 为偶就一个都不留（连手上那个一起丢）**。扣在 `carry` 里的那个**不算丢弃**
    /// —— 它只是晚一步进环。代价是偶尔多丢 1 个样本（有界，且计入溢出事件）。
    ///
    /// # `data` 的长度允许是**奇数**
    ///
    /// 调用方按任意边界切分同一个立体声流是合法的：先推 `[L0,R0,L1]`、
    /// 再推 `[R1,L2,R2]`，那半个帧留在环里等它的另一半。这条不是本方法的宽容，
    /// 是**既有约定**：消费端 `fill_from_ring`（`lib.rs`）、
    /// [`crate::resample::StreamResampler::process`] 的文档都写明了它，
    /// `tests/ring_phase.rs` 也正是靠「先推 7 个」造出奇数占用来钉这条不变量的。
    ///
    /// 所以「把空闲空间向下取到偶数」**只在 `data` 为偶数长度时够用**
    /// （空闲 4、推 7 个，取偶后仍会丢掉 3 个 = 奇数）。这里对**丢弃数**取偶，
    /// 它对两种长度都成立。代价是**最多浪费 1 个空闲槽位** ——
    /// 环容量是「采样率 × 声道数 × 秒数」（`lib.rs` 的 `RING_SECONDS`），
    /// 上万个样本里少用 1 个可以忽略。
    pub fn push_samples(&mut self, data: &[i16]) {
        // 概念上要推的是 `[carry] ++ data`：`carry` 是上次扣下的样本，它在前。
        let lead = self.carry;
        let total = data.len() + usize::from(lead.is_some());
        let room = self.inner.vacant_len();
        let mut take = total.min(room);

        // 丢弃数 = `total - take - 扣下的个数`。`take > 0` 时 `lead` 会被推出去、
        // 一个都不扣，所以先只管 `take` 的奇偶：会是奇数就少收一个。
        if take > 0 && (total - take) % 2 == 1 {
            // 少收一个 ⇒ 多丢一个 ⇒ 丢弃数变偶。多丢的是末尾那一个，
            // 环里留下的仍是 `[lead] ++ data` 的一段前缀，帧对齐不变。
            take -= 1;
        }

        if take == 0 {
            // 一个都收不下 —— 环满（`room == 0`），或者上面那一步刚把 `take` 减到 0。
            // 此时要么整批丢（丢弃数 = `total`），要么**扣下 1 个**（= `total - 1`）；
            // 配成偶数的那个才是对的 ⇒ **`total` 为奇才扣**。
            // ⚠️ `is_multiple_of` 而不是 `% 2 == 0` —— 后者在 MSRV ≥ 1.87 下触发
            //    clippy 的 `manual_is_multiple_of`（与 `app/src/metrics.rs` 同一条）。
            self.carry = if total.is_multiple_of(2) {
                // 扣一个就成了奇数 ⇒ 一个都不留。手上若还扣着 `lead`，**连它一起丢**：
                // 它就是这批新数据的紧邻前驱，丢掉的仍是一段连续前缀。
                // 代价是这一档比「只丢新数据」多丢 1 个样本 —— 有界，且计一次溢出事件。
                None
            } else {
                // 扣下的必须是 `[lead] ++ data` 的**最后一个**样本：丢掉的才是一段
                // 连续前缀，而它下次排在最前面推出去，与后面的流依然连续 ——
                // 换个扣法就会在流中间挖掉一个样本。
                // `lead` 已在手上时它就是那最后一个（这一次一个样本都没进环）⇒ 不动它。
                //
                // （不写 let-chain：本仓 MSRV 是 1.87，let-chain 要 1.88。）
                lead.or_else(|| data.last().copied())
            };
        }

        // ⚠️ 这里必须是**实际**丢弃数 = 没收进环的个数 − 仍扣在手上的个数。
        // 把扣着的那个也算成「丢弃」（本方法曾经如此：`dropped = total - take`）会
        // 正好差 1 ⇒ 下面这条断言在「`take == 0` 且 `carry` 占着」那一档**永远不响**，
        // 而那一档恰恰是丢奇数个的地方。
        // `take == 0` 时手上扣着的那一个没被丢；`take > 0` 时它会被推出去、`carry`
        // 随之为空（见下面那段推送代码），所以这里只在 `take == 0` 时才减它。
        let dropped = total - take - usize::from(take == 0 && self.carry.is_some());
        debug_assert_eq!(
            dropped % 2,
            0,
            "丢弃数必须是偶数：丢奇数个会永久翻转左右声道（见 AudioRing 的文档）"
        );

        // `lead` 先走：它在流里排在 `data` 前面，且 `take >= 1` 时必定推得进去
        // （`take <= room`）。`take == 0` 时它必须**留在** `carry` 里，不能清掉。
        // （不写 let-chain：本仓 MSRV 是 1.87，let-chain 要 1.88。）
        let mut pushed = 0usize;
        if take >= 1 {
            if let Some(c) = lead {
                pushed = self.inner.push_slice(&[c]);
                self.carry = None;
            }
        }
        let from_data = take - pushed;
        if from_data > 0 {
            pushed += self.inner.push_slice(&data[..from_data]);
        }
        debug_assert_eq!(pushed, take, "定长推送：能收多少就收多少");

        // 扣下不算丢：只是把最后一个扣到手上、一个样本都没扔（`dropped == 0`）的那次
        // 不记事件。反过来 `dropped > 0` 就**一定**真的扔掉了样本。
        if dropped > 0 {
            self.overflows.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 当前环内可读样本数（供背压判断）。
    ///
    /// ⚠️ 它数的是**环里的**样本，不含 `carry` 里可能扣着的那 1 个 ——
    /// 「还有多少样本没进环」最多比它大 1（见那个字段的文档）。
    /// 背压按它算是对的：扣着的那个样本本来就没打算立刻进环。
    ///
    /// 后果（A1 补）：环一直不出空间，扣着的那 1 个就一直进不去。而它若在**最后
    /// 一次**推送时仍被扣着、那一次又没喂进任何数据（`data` 为空 ⇒ `dropped == 0`、
    /// **不计溢出事件**），此后没有任何推送能再把它送进环 —— 它就这么没了，
    /// `available()` 与 `overflows()` 都不会反映它（见 `finish_stream` 的前置条件）。
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

    /// 构造交错立体声流：**L 恒为非负、R 恒为负**，于是帧对齐可以逐帧断言。
    /// 与 `tests/ring_phase.rs` 里的同名函数是同一个构造。
    fn stereo(n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| if i % 2 == 0 { i as i16 } else { -(i as i16) })
            .collect()
    }

    /// 把环里成对的样本抽干、追加到 `out`。**丢奇数个样本就会在这里露馅**：
    /// 此后每对的第一个样本都会变成负的。
    fn drain(c: &mut AudioRingConsumer, out: &mut Vec<i16>) {
        let mut buf = [0i16; 2];
        while c.available() >= 2 {
            c.pop_samples(&mut buf);
            out.extend_from_slice(&buf);
        }
    }

    /// **奇数长度的推送被部分丢弃时，丢弃数也必须是偶数。**
    ///
    /// `tests/ring_phase.rs` 钉的是**偶数长度**推送那一路（那时「把空闲空间向下
    /// 取到偶数」就够）。这一路钉的是奇数长度：空闲 4（偶）、推 7 个，
    /// 只看空闲空间会丢 3 个（奇）⇒ 环里剩下的样本与下一批之间错开一个样本。
    #[test]
    fn odd_length_push_never_drops_an_odd_count() {
        let src = stereo(64);
        let ring = AudioRing::new(8);
        let (mut prod, mut cons) = ring.split();
        let mut heard = Vec::new();

        prod.push_samples(&src[0..4]); // 先占住 4 个（2 整帧）
        prod.push_samples(&src[4..11]); // 长度 7（奇）：空闲 4 ⇒ 只能收 3 个
        assert_eq!(prod.overflows(), 1, "前提：这一次应记一次溢出");
        assert_eq!(prod.available(), 7, "收 3 个、丢 4 个（偶）才对");

        drain(&mut cons, &mut heard);
        for chunk in src[11..].chunks(4) {
            prod.push_samples(chunk);
            drain(&mut cons, &mut heard);
        }
        drain(&mut cons, &mut heard);

        assert!(!heard.is_empty(), "前提：总得听到点什么");
        for (k, pair) in heard.chunks_exact(2).enumerate() {
            assert!(
                pair[0] >= 0 && pair[1] < 0,
                "第 {k} 帧听到 ({}, {}) —— 应为 (非负, 负)。\
                 说明有一次奇数长度的丢弃把 L/R 翻了位",
                pair[0],
                pair[1]
            );
        }
    }

    /// **环满时推奇数个样本**：整段丢掉就是奇数个 ⇒ 同样会翻转声道。
    /// 这里一个样本都收不下，所以只能**扣下**最后一个留到下次（丢弃数因此为偶）。
    ///
    /// ⚠️ 关键是**在环仍然满、`carry` 还占着时再推一次**：A1 修复前，那一档会把
    /// 实际丢弃数算少 1（`dropped` 把手上扣着的那个也算进去了），声道照样翻位。
    /// 只推一次、推完就排空的版本走不到那一档 —— 一排空 `carry` 立刻被推出去。
    #[test]
    fn full_ring_defers_one_sample_instead_of_flipping_channels() {
        let src = stereo(64);
        let ring = AudioRing::new(8);
        let (mut prod, mut cons) = ring.split();
        let mut heard = Vec::new();

        prod.push_samples(&src[0..8]); // 8 = 容量，环满
        assert_eq!(prod.available(), 8, "前提：环应被占满");
        prod.push_samples(&src[8..11]); // 长度 3（奇），一个都进不去
        assert_eq!(prod.overflows(), 1, "前提：这一次应记一次溢出");
        assert_eq!(prod.available(), 8, "环满时占用数不该变");
        // 环**仍然满**、`carry` **仍然占着**：上面那次只是扣下，这次才是
        // 「扣着的同时又丢」—— 丢弃数必须仍是偶数。
        prod.push_samples(&src[11..14]); // 长度 3（奇）

        drain(&mut cons, &mut heard); // 听掉这 4 帧，环空出来
        // 从 `src[14..]` 接着喂：推入的必须是**连续**的一条流。`src[11..14]` 上面推过
        // 且被丢了，再推一次会在流里插进重复样本 —— 那个错位是测试自己造的，与 ring
        // 的行为无关（`stereo` 的符号约定按 `src` 下标定，不是按推入序号）。
        for chunk in src[14..].chunks(4) {
            prod.push_samples(chunk);
            drain(&mut cons, &mut heard);
        }
        drain(&mut cons, &mut heard);

        assert!(!heard.is_empty(), "前提：总得听到点什么");
        for (k, pair) in heard.chunks_exact(2).enumerate() {
            assert!(
                pair[0] >= 0 && pair[1] < 0,
                "第 {k} 帧听到 ({}, {}) —— 应为 (非负, 负)。\
                 环满时丢奇数个样本把 L/R 翻了位",
                pair[0],
                pair[1]
            );
        }
    }

    /// **`carry` 占着 + 环仍然满 + 再推一次奇数长度** —— A1 复审给出的 4 步反例。
    ///
    /// 修复前这一档是**断言查不到**的：`dropped = total - take` 把仍扣在手上的
    /// `lead` 也算成了本次丢弃，于是「实际丢了 3 个（奇）」被记成 4（偶），
    /// `debug_assert_eq!(dropped % 2, 0)` 照常通过 —— 声道翻了位而没有任何东西报警。
    ///
    /// 这里走公开 API，并把断言直接架在「**实际丢弃数**」的奇偶上
    /// （= 推入数 − 听到数 − 环内剩下的）：断言自己就是覆盖证明，不靠人枚举分支。
    #[test]
    fn ring_full_with_carry_held_drops_an_even_count() {
        let src = stereo(64);
        let ring = AudioRing::new(8);
        let (mut prod, mut cons) = ring.split();
        let mut heard = Vec::new();
        let mut pushed = 0usize;

        // 1. 环满、帧对齐
        prod.push_samples(&src[0..8]);
        pushed += 8;
        assert_eq!(prod.available(), 8, "前提：环应被占满");

        // 2. 1 个（奇）：一个都收不下 ⇒ 扣下 src[8] 留到下次。**一个样本都没丢。**
        prod.push_samples(&src[8..9]);
        pushed += 1;
        assert_eq!(prod.available(), 8, "前提：环满时占用数不该变");
        let overflows_after_defer = prod.overflows();

        // 3. 3 个（奇）、**环仍然满、`carry` 还占着** —— 就是这一档丢过奇数个。
        prod.push_samples(&src[9..12]);
        pushed += 3;

        // 4. 排空后再推 `src[12..14]`。环已空 ⇒ 空间够 ⇒ 手上若还扣着那 1 个，
        //    这一次必定被推出去或丢掉 ⇒ 结束时 `carry` 必为空，于是
        //    「推入 − 听到 − 环内」算出来的正是**实际丢弃数**。
        drain(&mut cons, &mut heard);
        prod.push_samples(&src[12..14]);
        pushed += 2;
        drain(&mut cons, &mut heard);

        let dropped = pushed - heard.len() - prod.available();
        // ① 不变量本身，放在最前面断言：坏掉时最该被看到的就是这一条的信息。
        assert_eq!(
            dropped % 2,
            0,
            "实际丢弃了 {dropped} 个样本（奇数）—— 此后每一帧都会变成 (R,L)，\
             L/R 永久互换（见 AudioRing 的文档）。\
             推入 {pushed}、听到 {}、环内 {}。",
            heard.len(),
            prod.available()
        );
        // ② 口径：扣下不算丢 —— 第 2 步只扣下 1 个、没丢样本，那时不该记溢出事件。
        assert_eq!(
            overflows_after_defer, 0,
            "第 2 步只是把最后一个扣在手上（`dropped == 0`），不该计溢出事件"
        );

        assert!(!heard.is_empty(), "前提：总得听到点什么");
        for (k, pair) in heard.chunks_exact(2).enumerate() {
            assert!(
                pair[0] >= 0 && pair[1] < 0,
                "第 {k} 帧听到 ({}, {}) —— 应为 (非负, 负)。\
                 `carry` 占着时丢奇数个样本把 L/R 翻了位",
                pair[0],
                pair[1]
            );
        }
    }

    /// **小状态空间穷尽检查**：容量 1..=6（奇偶都有）× 长度 ≤ 5 的操作序列
    /// （推 0..=4 个、取 1..=4 个，长度奇偶都有）。
    ///
    /// 上面两条测试钉的是具体反例；这一条把分支空间扫一遍，**包括 A1 复审明说没排除
    /// 的那一档「容量为奇 + 消费端按奇数量取」**。每一步查两件事：
    /// - **实际丢弃数**（= 推入 − 听到 − 环内 − 手上扣着的）是偶数，且**每次推送的
    ///   增量**也是偶数；
    /// - **听到的流**没有错位：第 k 个听到的样本，其 `src` 下标应与 k 同奇偶。
    ///
    /// 它是**测试**不是证明：覆盖面到此为止 —— 容量更大、序列更长、并发场景不在
    /// 这条测试里（那几档靠同一条算术推，不靠它兜着）。
    #[test]
    fn small_state_machine_exhaustive_check() {
        // 每个操作：`op < 5` ⇒ 推 `op` 个（含 0 个）；否则 ⇒ 取 `op - 4` 个。
        const OPS: usize = 9;
        const DEPTH: u32 = 5;
        let src = stereo(32);

        for capacity in 1..=6usize {
            for seed in 0..OPS.pow(DEPTH) {
                // 操作序列 = 把 `seed` 反复对 `OPS` 取余（低位在前）；失败信息里带 `seed`。
                let mut code = seed;
                let (mut prod, mut cons) = AudioRing::new(capacity).split();
                let mut cursor = 0usize; // 下一个要推的 `src` 下标
                let mut pushed = 0usize;
                let mut delivered = 0usize;
                let mut dropped_before = 0usize;
                let mut overflows_before = 0u64;
                let mut buf = [0i16; 4];

                for step in 0..DEPTH {
                    let op = code % OPS;
                    code /= OPS;

                    if op < 5 {
                        let end = (cursor + op).min(src.len());
                        prod.push_samples(&src[cursor..end]);
                        pushed += end - cursor;
                        cursor = end;
                    } else {
                        let m = op - 4;
                        let n = cons.pop_raw(&mut buf[..m]);
                        for (i, &v) in buf[..n].iter().enumerate() {
                            // `stereo` 的取值唯一编码下标（i16 范围内）⇒ 反推回来查奇偶。
                            let idx = v.unsigned_abs() as usize;
                            assert_eq!(
                                idx % 2,
                                (delivered + i) % 2,
                                "容量 {capacity}、seed {seed}、第 {step} 步：\
                                 第 {} 个听到的样本是 src[{idx}] = {v}，\
                                 与它的位置奇偶不符 ⇒ 此前丢过奇数个样本，L/R 已互换",
                                delivered + i
                            );
                        }
                        delivered += n;
                    }

                    let dropped =
                        pushed - delivered - prod.available() - usize::from(prod.carry.is_some());
                    assert_eq!(
                        (dropped - dropped_before) % 2,
                        0,
                        "容量 {capacity}、seed {seed}、第 {step} 步：\
                         这一步实际丢了 {} 个样本（奇数）—— 丢奇数个会永久翻转左右声道\
                         （累计丢弃 {dropped}）",
                        dropped - dropped_before
                    );
                    assert_eq!(
                        prod.overflows() - overflows_before,
                        u64::from(dropped > dropped_before),
                        "容量 {capacity}、seed {seed}、第 {step} 步：\
                         溢出事件应当「真的丢了样本」才记一次（扣下不算丢）"
                    );
                    dropped_before = dropped;
                    overflows_before = prod.overflows();
                }
            }
        }
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
