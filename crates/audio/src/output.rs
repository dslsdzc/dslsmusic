use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

/// **一帧立体声**：左右各一个 i16 样本。这是 ring 的**存储单元**。
///
/// 它 4 字节，与「两个 i16」同尺寸 —— 帧化**不增加任何内存**，只是把
/// 「环里装着半个帧」这个状态从类型里删掉（`size_of` 由下面的
/// `stereo_frame_is_four_bytes` 钉住，不靠「按理说」）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StereoFrame {
    pub l: i16,
    pub r: i16,
}

impl StereoFrame {
    /// i16 域的静音帧（`pop_frames` 取不满时用它补尾）。
    ///
    /// ⚠️ 音频回调补的不是这个 —— 它必须补**目标格式 `T`** 的静音
    /// （`f32` 是 `0.0`、i16 是 `0`、**`u8` 是 `128`**），见 `fill_from_ring`。
    pub const SILENT: Self = Self { l: 0, r: 0 };
}

/// 无锁 SPSC 环形缓冲，用 `AudioRing::split()` 拆成生产端与消费端。
///
/// **消费端在音频回调线程里使用：无锁、无分配、无 IO**（设计文档 §16.2）。
/// 原计划用 `Mutex<Vec<i16>>`，那会在实时线程里加锁 —— 已废弃。
///
/// # 存储单元是**整帧**，所以「丢奇数个样本」不可表达
///
/// 环里承载的是**2 声道交错**的立体声 `L,R,L,R,…`，本类型把它按帧存
/// （[`StereoFrame`]），容量也**按帧计**。
///
/// 这样做的理由是一整类 bug：只要**丢弃的样本个数是奇数**，此后所有样本的
/// 奇偶位置就翻一位 —— 每个「帧」都变成 `(R,L)`，**左右声道永久互换**，
/// 而采样率、`xrun`、波形统计**全部照常**（本项目早先修过的 `got / 2` 截断
/// 是同一个坑，只是长在消费端）。
///
/// **前一版是靠一段奇偶证明维持的**：生产端按「实际丢弃数」配平（收得下就
/// 少收一个、一个都收不下就扣下最后一个或把手上那个一起丢），消费端把 pop
/// 长度**向下取到偶数**，两半合起来保证「丢掉的样本数恒为偶数」。
/// 那句话是**对的**，但它是一句**需要被维持**的不变量：新增一条推送路径、
/// 改一次取数边界，都可能把它悄悄破掉，而破了以后**没有任何读数会变**
/// （听感是声道互换，指标全正常）。
///
/// 现在这条不变量**不需要维持**了 —— 环的存储单元是帧，溢出在结构上只能
/// 丢整帧，「丢掉奇数个样本」这个状态**构造不出来**。消费端那句
/// 「向下取偶」也随之消失。
///
/// ⚠️ 帧化**没有**移除的一件事：调用方仍可按**任意边界**切分同一个立体声流
/// （`push_samples` 收的仍是交错 `&[i16]`，长度允许是奇数）。凑不成帧的那
/// **一个**样本扣在生产端的 `pending` 里等它的另一半 —— 它**不在环里**
/// （环里放不下半个帧），也**不与溢出交互**。失效档见
/// [`AudioRingProducer::push_samples`] 的文档。
pub struct AudioRing {
    capacity_frames: usize,
}

impl AudioRing {
    /// `capacity_frames` 是**帧数**（不是样本数、不是字节数）。
    /// 一帧 4 字节 ⇒ 字节数 = `capacity_frames * 4`、样本数 = `capacity_frames * 2`。
    ///
    /// ⚠️ **单位是帧。** 前一版这个方法收的是**样本数**，同一个时长得填一半的
    /// 数值（旧的 `率 × 2 声道 × 秒` ⇒ 新的 `率 × 秒`）。改名 + 改单位是**故意**
    /// 的：同名换单位会让调用方**静默**算错一半，带单位的参数名让编译器把每个
    /// 调用点都点出来。
    ///
    /// # Panics
    ///
    /// `capacity_frames == 0` 时 panic（ringbuf 的既定行为，已核实源码
    /// `ringbuf-0.4.8/src/rb/macros.rs:20`）。
    pub fn new(capacity_frames: usize) -> Self {
        Self { capacity_frames }
    }

    pub fn split(self) -> (AudioRingProducer, AudioRingConsumer) {
        let (prod, cons) = HeapRb::<StereoFrame>::new(self.capacity_frames).split();
        let dropped = Arc::new(AtomicU64::new(0));
        let underruns = Arc::new(AtomicU64::new(0));
        (
            AudioRingProducer {
                inner: prod,
                dropped_frames: Arc::clone(&dropped),
                underruns: Arc::clone(&underruns),
                pending: None,
            },
            AudioRingConsumer {
                inner: cons,
                underruns,
            },
        )
    }
}

/// 生产端 —— 解码线程使用（回环示例里**采集回调**也用它）。`!Sync` 但 `Send`。
pub struct AudioRingProducer {
    inner: HeapProd<StereoFrame>,
    /// 累计**丢掉的帧数**（不是事件次数，见 [`AudioRingProducer::dropped_frames`]）。
    dropped_frames: Arc<AtomicU64>,
    underruns: Arc<AtomicU64>,
    /// 上一次推送**凑不成一帧**的那一个样本（`data` 长度为奇数时才会有）。
    /// 它下次排在最前面，与新数据的第一个样本配成一帧。
    ///
    /// ⚠️ **它和本类型前一版那个 `carry` 不是一回事**，别按 `carry` 的直觉读：
    /// `carry` 是「环满时扣下的一个样本」，它的存在**与溢出耦合**，而且正是
    /// 它的奇偶参与过「丢多少个样本」的配平。这里的 `pending` **只由输入的
    /// 奇偶决定，从不与溢出交互** —— 环里只有帧，溢出只丢整帧，它进不进得去
    /// 环都不影响帧对齐。`push_samples` 里因此**一行奇偶运算都没有**。
    ///
    /// 它最多积压 1 个（新的一次推送就会把它消费掉，或连同新数据一起被丢），
    /// 不会随调用增长。
    ///
    /// # ⚠️ 前置条件：**一个生产端只喂一条流**
    ///
    /// 本字段**不认识流边界**：那个样本在下一次推送时会被配成帧，而「下一次推送」
    /// **可以已经是另一条流的数据**。于是，若流 N 的总样本数是**奇数**：
    ///
    /// 1. 流 N 结束时 `pending` 扣着它的**最后一个样本** —— 那是**左**声道；
    /// 2. 接着推**流 N+1** 时，它的第一个样本（本该是流 N+1 的**左**声道 L0）
    ///    被写进这一帧的 `r`：`StereoFrame { l: 流N的末样本, r: L0 }`；
    /// 3. 此后每帧都错开半个帧：`(R0, L1)`、`(R1, L2)`… ⇒ **流 N+1 整条 L/R 互换**。
    ///
    /// 这个失效档**比丢失更糟：它是静默的损坏，不是丢失。** `dropped_frames()` 保持 0、
    /// `underruns()` 保持 0、`available_frames()` 正常、帧的顺序也正常 ——
    /// 而且 **`has_pending_sample()` 读不出它**：配帧之后那个标志只反映**当前**
    /// 这条流的长度奇偶（为真也可能是流 N+1 自己的奇数尾巴），与这次错位无关。
    /// 唯一的痕迹是 [`crate::AudioEngine::finish_stream`] 在流边界记的那一条 `warn`。
    ///
    /// ⇒ **跨流复用同一个生产端的调用方必须自己保证边界上 `pending` 为空**
    /// （即上一条流的总样本数是偶数）。今天 `app` 是「一个 `play` 建一个
    /// `AudioEngine`」，走不到这里；而 v2 的预加载 / 无缝衔接正是「一个引擎连着
    /// 放两首」—— 那时候先补一个重置（或每条流一个生产端），再谈别的。
    pending: Option<i16>,
}

impl AudioRingProducer {
    /// 推入**交错 i16 立体声样本**（`L,R,L,R,…`）。**环满时新数据被丢弃并
    /// 计入 [`Self::dropped_frames`]**，绝不阻塞、绝不分配。
    ///
    /// # 溢出在结构上只能丢**整帧**
    ///
    /// 环的存储单元是 [`StereoFrame`] ⇒「丢奇数个样本」**不可表达**。
    /// 丢弃数就是 `想推的帧数 − 真进环的帧数`，**没有配平、没有 `% 2`**。
    ///
    /// ringbuf 的语义是**新数据被丢弃**（保留先到的，见
    /// `counts_dropped_frames_and_keeps_oldest`），所以丢掉的是本次推送的**末尾**：
    /// 有一帧被丢时，它的 `l` 与 `r` **一起**被丢 —— 不存在「留下半个」。
    ///
    /// # `data` 的长度允许是**奇数**
    ///
    /// 调用方按任意边界切分同一个立体声流是合法的：先推 `[L0,R0,L1]`、
    /// 再推 `[R1,L2,R2]`。凑不成帧的那一个样本扣在 `pending` 里等它的另一半
    /// —— 注意它**不在环里**，所以 `available_frames()` 数不到它，
    /// 它**也不算丢弃**（没被扔掉，只是晚一步）。
    ///
    /// ⚠️ **它的失效档**：一个流的**总**样本数是奇数时，最后那一个样本
    /// **在这条流里**等不到它的右声道。它既不会进环、也不会被计进 `dropped_frames`
    /// ——它没被「丢弃」，是这条流自己不成帧。**它能被看见**：
    /// [`Self::has_pending_sample`] 在那一刻为真，
    /// [`crate::AudioEngine::finish_stream`] 就是靠它把这条**本来静默的**损失
    /// 变成一条日志。
    ///
    /// ⚠️ 但「等不到」有一个前提：**这条流之后不再由同一个生产端推数据**。
    /// 若接着推的是**下一条流**，那一个样本不是消失，而是被配进下一条流的帧里
    /// —— 后果是**下一条流整条 L/R 互换**，而且**没有任何读数**会变。
    /// 完整的失效档与前置条件见 `pending` 字段的「一个生产端只喂一条流」。
    pub fn push_samples(&mut self, data: &[i16]) {
        // 概念上要推的是 `[pending] ++ data`，按 (L,R) 两两成帧。
        // `first` 是 `pending` 与新数据第一个样本配成的那一帧；`rest` 是剩下的
        // 样本 —— 它的配对起点**永远是 0**（`pending` 已经被 `first` 消耗掉了），
        // 所以新的 `pending` 只由 `rest` 的奇偶决定。
        let (first, rest) = match (self.pending, data.split_first()) {
            (Some(l), Some((&r, rest))) => (Some(StereoFrame { l, r }), rest),
            // 空推送、手上又扣着半个帧：**什么都不做**，`pending` 原样留着。
            //
            // ⚠️ 这里与前一版的 `carry` **行为不同**，而且是**故意**的：旧代码在
            //    「空推送 + 环里有空间」时会把扣着的那一个样本**推进环里**
            //    （`total = 1`、`take = 1`）。那在旧结构下是「救回一个样本」，
            //    在新结构下就是**把一个左声道单独塞进环** —— 正是本次要消灭的
            //    那个状态。所以这里必须什么都不做。
            //
            //    可听见的后果**没有变**：那个样本是某一帧缺少右声道的左声道，
            //    两种存法都永远凑不成帧（旧结构里它进环后 `available() == 1`，
            //    消费端 `& !1` 恒取 0 个 ⇒ 同样永远播不出来）。变的只是它**待在哪**
            //    —— 而且新结构里它**有读数**（`has_pending_sample`）。
            (Some(_), None) => return,
            (None, _) => (None, data),
        };

        // 想推的帧数 = `first`（0 或 1）+ `rest` 里能成对的那些。
        // `chunks_exact(2)` 丢掉的那个余数正是要扣下的样本，两处**同一件事**。
        let total = usize::from(first.is_some()) + rest.len() / 2;
        // ⚠️ 这里**没有**任何为奇偶而做的调整：能塞多少帧就塞多少帧，
        //    `first` 塞不进去时 `push_iter` 自然一帧都塞不进去（它按空闲槽位
        //    顺序写、写满即停），少塞的帧数就是丢的帧数。
        // ⚠️ `chunks_exact(2)` 保证每个 `p` 恰好两个元素，但编译器不知道
        //    （它看到的类型是 `&[i16]`），所以下面两处下标带边界检查。
        //    这是**生产者**这一侧唯一的逐元素代价（`push_iter` 不是 memcpy，
        //    而旧代码走的是 `push_slice`）—— 实测的量级见报告，可忽略。
        let frames = first.into_iter().chain(
            rest.chunks_exact(2)
                .map(|p| StereoFrame { l: p[0], r: p[1] }),
        );
        let pushed = self.inner.push_iter(frames);
        // ⚠️ 这里**故意没有**「推满了没有」的断言。三条理由，按重要性排：
        //
        // ① 前一版那句 `debug_assert!(pushed <= total)` 是**同义反复** ——
        //    `total` 就是上面那个迭代器的长度（`first` 的 0/1 + `rest.len() / 2`），
        //    `push_iter` 又永远不会返回比迭代器更长的东西 ⇒ **它一个输入都抓不到**。
        // ② 它替换掉的那条（`debug_assert_eq!(pushed, take, "定长推送：能收多少就收多少")`）
        //    在旧结构里**有承重**：那时 `dropped` 是拿**算出来的** `take` 相减
        //    （`total - take - …`），推送数与它不一致会让计数**静默**算错。
        //    现在 `dropped` 直接用**实际的** `pushed`（下一行）—— 推送数就是计数的
        //    输入，「算出来的」与「实际的」不再是两个东西，承重搬进了代码本身。
        // ③ 想补一条等价检查的话，**不能**写成 `pushed == total || is_full()`：
        //    消费端在这两行之间把环抽走几帧是合法的，`is_full()` 读到的是**另一个
        //    时刻**的环 —— ringbuf 自己的文档就说这个结果「可能随时失效」
        //    （已核实·源码 `ringbuf-0.4.8/src/traits/observer.rs:69-75`）⇒ 那条断言会
        //    **误报**（decode 线程上 panic）。竞态下安全的形式要在 `push_iter`
        //    **之前**读一次 `vacant_len()`（`pushed >= total.min(room)`），那要加一行
        //    读；本轮只改注释，所以先不加。
        let dropped = (total - pushed) as u64;
        if dropped > 0 {
            self.dropped_frames.fetch_add(dropped, Ordering::Relaxed);
        }

        // `pending` 只由**源流**的奇偶决定，与上面丢没丢帧无关：丢掉的是
        // `[pending] ++ data` 的**末尾**，所以下一批数据接在丢掉的那一段
        // **之后**，奇偶关系不变。（换个算法——例如「丢了末尾就清空
        // `pending`」——会把下一批的第一个样本当成左声道，而它其实是右声道，
        // 于是此后每帧都是 `(R,L)`：我们要删掉的那类 bug 原样回来。）
        self.pending = if rest.len().is_multiple_of(2) {
            None
        } else {
            rest.last().copied()
        };
    }

    /// 当前环内可读**帧数**（供背压判断）。
    ///
    /// ⚠️ 它数的是**环里的**帧，不含 `pending` 里可能扣着的那 1 个样本
    /// ——「还有多少样本没进环」最多比它大 1。背压按它算是对的：扣着的那一个
    /// 本来就没打算立刻进环。
    ///
    /// ⚠️ **前一版这个方法叫 `available()` 且数的是样本。** 改名 + 改单位是
    /// 故意的：`available() >= 2` 这类写法在换单位后会**静默**改变含义
    /// （从「两个样本」变成「两帧」），带单位的名字让编译器点出每个调用方。
    pub fn available_frames(&self) -> usize {
        self.inner.occupied_len()
    }

    /// **累计丢掉的帧数**（不是「溢出事件次数」）。
    ///
    /// 一次推送里丢了 3 帧就加 3。口径与 [`crate::AudioEngine::dropped_frames`]
    /// 一致（那个方法转发到这里）。
    ///
    /// ⚠️ **前一版这个方法叫 `overflows()`，口径是「事件次数」。** 改成帧数
    /// 的理由：帧化之后「丢了多少帧」是**免费**得到的准确量
    /// （`想推的帧数 − 真进环的帧数`），而它正是用户拿到非零读数后要问的下一
    /// 个问题——「丢了一次」和「丢了一千帧」是两个量级的事故。
    /// 旧口径存在的理由（样本计数被奇偶配平污染过 ±1）随帧化一起消失了。
    ///
    /// ⚠️ 因此它与 [`crate::AudioEngine::xrun_count`] **不同单位**：那个按事件计，
    /// 这个按帧计。两者本来也不是一个量（一个数环被抽干、一个数环被填满），
    /// 数字从来没有可比性。**唯一要做的事是判是否为 0。**
    pub fn dropped_frames(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }

    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// 手上是否还扣着**半个帧**（见 `pending` 字段）。
    ///
    /// ⚠️ **只可能在流的末尾为真**：只要还有下一次推送，它就会被配成帧
    /// （或连同新数据一起被丢）。为真即「这条流**总**样本数是奇数，最后那一个
    /// 样本播不出来」——[`crate::AudioEngine::finish_stream`] 靠它报警。
    ///
    /// ⚠️ **它读不出跨流那一次错位**（见 `pending` 字段的前置条件）：配帧之后
    /// 这个标志只反映**当前**这条流的长度奇偶 —— 为真也可能是这条流自己的奇数
    /// 尾巴，为假则说明这一次推送收尾收得干净。**两种取值都与上一次错位无关。**
    pub fn has_pending_sample(&self) -> bool {
        self.pending.is_some()
    }
}

/// 消费端 —— **音频回调线程使用**。`!Sync` 但 `Send`。
pub struct AudioRingConsumer {
    inner: HeapCons<StereoFrame>,
    underruns: Arc<AtomicU64>,
}

impl AudioRingConsumer {
    /// 取帧填满 `out`。**取不满时尾部补静音并计一次 underrun**。
    /// 返回实际取到的帧数。永不阻塞、永不分配。
    pub fn pop_frames(&mut self, out: &mut [StereoFrame]) -> usize {
        let n = self.inner.pop_slice(out);
        if n < out.len() {
            out[n..].fill(StereoFrame::SILENT);
            self.underruns.fetch_add(1, Ordering::Relaxed);
        }
        n
    }

    /// **不补静音、不计数**的原始取值。永不阻塞、永不分配。
    ///
    /// 音频回调用它，而不用 `pop_frames`——因为**静音必须补在目标采样格式
    /// `T` 的域里**：`f32` 的静音是 `0.0`、`i16` 是 `0`、**`u8` 是 `128`**。
    /// 在 i16 域里补 0 再转换是对的，但直接往 `T` 上写 `0` 在 u8 设备上
    /// 是满幅直流噪声。回调侧的补静音见 `fill_from_ring` 的 `T::EQUILIBRIUM`。
    pub fn pop_raw_frames(&mut self, out: &mut [StereoFrame]) -> usize {
        self.inner.pop_slice(out)
    }

    /// 当前环内可读**帧数**。**无锁**（两次原子读 + 本地缓存），回调里可用。
    ///
    /// 回调拿它决定这次能取几帧。**取数边界天然是整帧**，不需要任何取偶 ——
    /// 前一版这里要 `available() & !1`，那正是为「环里可能装着奇数个样本」
    /// 打的补丁，而那个状态现在已经不可表达（见 [`AudioRing`]）。
    pub fn available_frames(&self) -> usize {
        self.inner.occupied_len()
    }

    /// 记一次欠载。**按「事件」计，不按缺失样本数计**——与 `pop_frames`
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

    /// 帧化**不增加内存**：一帧恰好与两个 i16 同宽。这条不是「按理说」——
    /// 它是「容量改按帧计、字节数与改前相同」这个说法的**依据**，
    /// 一旦有人给 `StereoFrame` 加字段（或改成 `#[repr(...)]` 之外带填充的
    /// 布局），它必须先红。
    #[test]
    fn stereo_frame_is_four_bytes() {
        assert_eq!(
            std::mem::size_of::<StereoFrame>(),
            2 * std::mem::size_of::<i16>(),
            "StereoFrame 必须与两个 i16 同宽，否则帧化会改变 ring 的内存占用"
        );
    }

    #[test]
    fn preserves_order() {
        let ring = AudioRing::new(8);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[1, 2, 3, 4]);
        let mut out = [StereoFrame::SILENT; 2];
        assert_eq!(cons.pop_frames(&mut out), 2);
        assert_eq!(
            out,
            [StereoFrame { l: 1, r: 2 }, StereoFrame { l: 3, r: 4 }]
        );
    }

    #[test]
    fn counts_underrun_when_empty() {
        let ring = AudioRing::new(8);
        let (_prod, mut cons) = ring.split();
        // ⚠️ 预填一个**不是静音**的帧、**不是** `SILENT`：`out` 若初值就是静音，
        //    下面那条断言**恒真**（不管 `pop_frames` 有没有补静音都通过），
        //    等于没测。这是「空环」那一路；「取到一部分」那一路在
        //    `partial_underrun_still_fills_silence`。
        let garbage = StereoFrame { l: 99, r: -99 };
        let mut out = [garbage; 4];
        assert_eq!(cons.pop_frames(&mut out), 0);
        assert_eq!(cons.underruns(), 1);
        // 回调必须填满整块缓冲 —— 没数据时填静音，绝不能留垃圾值（那是爆音）
        assert_eq!(out, [StereoFrame::SILENT; 4]);
    }

    /// 环满时**新数据被丢弃**、先到的留下；计的是**帧数**。
    ///
    /// ⚠️ 本测试前一版叫 `counts_overflow_and_keeps_oldest` —— 名字里的
    /// 「overflow」是旧口径（按事件计），帧化时口径与名字一起改了。
    /// 改名时留这一行是为了让按旧名字搜索的人找到这里（本仓把测试名当规格读，
    /// 旧名字出现在历史 brief / 报告 / 计划里，那些是记录，不改）。
    #[test]
    fn counts_dropped_frames_and_keeps_oldest() {
        // ringbuf 的语义：环满之后**新数据被丢弃**（不是挤掉最旧的）
        let ring = AudioRing::new(4); // 4 帧 = 8 个样本
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]); // 6 帧
        assert_eq!(
            prod.dropped_frames(),
            2,
            "6 帧推进容量 4 帧的环，应丢 2 帧（**帧数**，不是「一次事件」）"
        );
        let mut out = [StereoFrame::SILENT; 4];
        assert_eq!(cons.pop_frames(&mut out), 4);
        assert_eq!(
            out,
            [
                StereoFrame { l: 1, r: 2 },
                StereoFrame { l: 3, r: 4 },
                StereoFrame { l: 5, r: 6 },
                StereoFrame { l: 7, r: 8 },
            ],
            "ringbuf 保留先到的数据"
        );
    }

    /// 构造交错立体声流：**L 恒为非负、R 恒为负**，于是帧对齐可以逐帧断言。
    fn stereo(n: usize) -> Vec<i16> {
        (0..n)
            .map(|i| if i % 2 == 0 { i as i16 } else { -(i as i16) })
            .collect()
    }

    /// 把环里能取到的帧全部抽干、追加到 `out`。
    fn drain(c: &mut AudioRingConsumer, out: &mut Vec<StereoFrame>) {
        let mut buf = [StereoFrame::SILENT; 1];
        while c.available_frames() > 0 {
            let got = c.pop_raw_frames(&mut buf);
            out.extend_from_slice(&buf[..got]);
        }
    }

    /// 把听到的帧摊回交错样本（与 `stereo()` 的产物可直接比对）。
    #[cfg(test)]
    fn flatten(frames: &[StereoFrame]) -> Vec<i16> {
        frames.iter().flat_map(|f| [f.l, f.r]).collect()
    }

    /// `heard` 是不是 `src` 的**前缀**（按样本算）。不分配 —— 穷尽检查里
    /// 要跑上百万次，每次 `collect` 一个 `Vec` 会把测试本身变成瓶颈。
    ///
    /// ⚠️ **只对「一次都没丢过」的序列成立。** 有丢弃时丢弃会在源流里留出缺口，
    /// 听到的就不再是前缀（见 `assert_source_frame_subsequence`）。
    fn is_prefix_of(heard: &[StereoFrame], src: &[i16]) -> bool {
        heard.len() * 2 <= src.len()
            && heard
                .iter()
                .enumerate()
                .all(|(i, f)| f.l == src[i * 2] && f.r == src[i * 2 + 1])
    }

    /// **有丢弃时「L/R 永不互换」的正确说法**：`heard` 里每一帧都必须是源流里
    /// **真实存在的某一帧**，且对应的源帧下标**严格递增**（顺序不许乱）。
    ///
    /// 为什么不是前缀：ringbuf 丢的是**新**数据 ⇒ 源流里会留出缺口，缺口之后
    /// 的帧仍然会被推、被听到。所以正确的不变量是「子序列」而不是「前缀」。
    ///
    /// 它抓得住的东西一点没少：**一次错位配对（哪怕只错一个样本）就会造出一个
    /// 源流里根本不存在的帧**（`(R,L)` 在 `stereo()` 的编码下不等于任何源帧），
    /// 顺序错乱、中间挖洞同样会露馅。
    ///
    /// （这条断言本身就是本次实现里的一次纠错：一开始写的是「前缀」，
    /// 容量 1 的穷尽检查立刻把它证伪了 —— 缺口是会出现的。）
    fn assert_source_frame_subsequence(heard: &[StereoFrame], src: &[i16], ctx: &str) {
        let total = src.len() / 2;
        let mut next = 0usize;
        for (k, f) in heard.iter().enumerate() {
            match (next..total).find(|&j| src[j * 2] == f.l && src[j * 2 + 1] == f.r) {
                Some(j) => next = j + 1,
                None => panic!(
                    "{ctx}：第 {k} 帧听到 ({}, {}) 在源流第 {next} 帧之后就找不到了 —— \
                     它是一个**源流里根本不存在的帧**，说明此前累积了一次奇数个样本的\
                     错位（L/R 已互换）。",
                    f.l, f.r
                ),
            }
        }
    }

    /// **奇数长度的推送：凑不成帧的那一个样本被扣下，不是被丢掉。**
    ///
    /// 这一条对应前一版的 `odd_length_push_never_drops_an_odd_count`。旧测试钉的是
    /// 「奇数长度的推送被部分丢弃时，丢弃数也必须是偶数」—— 那条不变量今天
    /// **不可表达**（丢弃数按帧算，没有奇偶可言）。这里钉的是新结构下同一件事的
    /// 落点：**一个样本都不会因此消失**，`pending` 只是让它晚一步成帧。
    #[test]
    fn odd_length_push_defers_the_unpaired_sample() {
        let src = stereo(64);
        let ring = AudioRing::new(8); // 8 帧 = 16 个样本
        let (mut prod, mut cons) = ring.split();
        let mut heard = Vec::new();

        prod.push_samples(&src[0..7]); // 3 整帧 + 1 个样本（奇）
        assert_eq!(prod.available_frames(), 3, "只进得去 3 整帧");
        assert!(
            prod.has_pending_sample(),
            "第 7 个样本应扣在 pending 里等它的右声道"
        );
        assert_eq!(
            prod.dropped_frames(),
            0,
            "扣下**不是丢弃** —— 环没满，一个帧都没丢"
        );

        drain(&mut cons, &mut heard);
        for chunk in src[7..].chunks(4) {
            prod.push_samples(chunk);
            drain(&mut cons, &mut heard);
        }
        drain(&mut cons, &mut heard);

        assert!(!heard.is_empty(), "前提：总得听到点什么");
        // 这条序列**一次都没丢过**（环 8 帧、每次最多推 2 帧、每步都排空），
        // 所以不变量是最强的那一档：**听到的就是源流的前缀**。
        assert!(
            is_prefix_of(&heard, &src),
            "听到的必须是源流的前缀；不等说明有帧被错配或样本被吞。\
             听到 {:?}，源流前 {} 个 {:?}",
            flatten(&heard),
            heard.len() * 2,
            &src[..heard.len() * 2]
        );
        for (k, f) in heard.iter().enumerate() {
            assert!(
                f.l >= 0 && f.r < 0,
                "第 {k} 帧听到 ({}, {}) —— 应为 (非负, 负)。L/R 翻了位",
                f.l,
                f.r
            );
        }
    }

    /// **环满时推奇数个样本**：进不去的那一帧被整帧丢掉，`pending` 留着。
    ///
    /// 对应前一版的 `full_ring_defers_one_sample_instead_of_flipping_channels`
    /// （旧的前提是「一个都收不下时只能扣下最后一个来配平奇偶」）。
    /// 这里钉的是：**整帧丢**（`l` 与 `r` 一起走），而扣下的那个样本
    /// 仍然等着它的另一半。
    ///
    /// ⚠️ 关键是**在环仍然满、`pending` 还占着时再推一次**：前一版正是在这一档
    /// 把实际丢弃数算少 1、断言查不到，声道照样翻位。
    #[test]
    fn full_ring_drops_whole_frames_and_keeps_the_half_one_waiting() {
        let src = stereo(64);
        let ring = AudioRing::new(4); // 4 帧 = 8 个样本
        let (mut prod, mut cons) = ring.split();
        let mut heard = Vec::new();

        prod.push_samples(&src[0..8]); // 4 帧，环满
        assert_eq!(prod.available_frames(), 4, "前提：环应被占满");
        prod.push_samples(&src[8..11]); // 1.5 帧，一个都进不去
        assert_eq!(
            prod.dropped_frames(),
            1,
            "想推 1 整帧（(8,9)），塞不进去 ⇒ 丢 **1 帧**（帧数口径）"
        );
        assert!(
            prod.has_pending_sample(),
            "src[10] 是半个帧，应扣在 pending 里 —— 它**没**被丢掉"
        );
        assert_eq!(prod.available_frames(), 4, "环满时占用数不该变");

        // 环**仍然满**、`pending` **仍然占着**：再推一次 —— 前一版丢奇数个
        // 样本就发生在这一档。这次推的是 src[11..14]：第一个样本与扣着的
        // src[10] 配成一帧 (10, 11)，后面 src[12..14] 又是一帧。
        prod.push_samples(&src[11..14]);
        assert_eq!(
            prod.dropped_frames(),
            3,
            "这次想推 2 帧（(10,11)、(12,13)），一个都进不去 ⇒ 累计丢 3 帧；\
             注意 src[11] 是与 src[10] 配对后被**整帧**丢的，不存在「留下半个」"
        );

        drain(&mut cons, &mut heard); // 听掉这 4 帧，环空出来
        // 从 `src[14..]` 接着喂：环里那 4 帧是 src[0..8]，此后流断在 src[8..14]
        // 被丢的那一段上 —— 这是测试自己造的错位（`stereo` 的符号约定按
        // `src` 下标定，不是按推入序号），与 ring 的行为无关。
        for chunk in src[14..].chunks(4) {
            prod.push_samples(chunk);
            drain(&mut cons, &mut heard);
        }
        drain(&mut cons, &mut heard);

        assert!(!heard.is_empty(), "前提：总得听到点什么");
        for (k, f) in heard.iter().enumerate() {
            assert!(
                f.l >= 0 && f.r < 0,
                "第 {k} 帧听到 ({}, {}) —— 应为 (非负, 负)。\
                 环满时丢样本把 L/R 翻了位",
                f.l,
                f.r
            );
        }
    }

    /// **`pending` 占着 + 环仍然满 + 再推一次奇数长度** —— A1 复审给出的 4 步反例。
    ///
    /// 对应前一版的 `ring_full_with_carry_held_drops_an_even_count`。那个版本
    /// 走公开 API 把断言架在「**实际丢弃数**的奇偶」上（推入 − 听到 − 环内 − 手上）。
    /// 帧化之后「实际丢弃数」**只在帧口径上有意义**，所以这里断言的是同一件事的
    /// 新形式：**若手上的样本数守恒，则推入的样本与「听到 + 环内 + 手上」之间
    /// 的差额必须是偶数** —— 因为差额只能来自整帧（2 个样本）的丢弃。
    /// 这正是「丢奇数个样本」在旧结构里干的坏事，也是新结构不允许的。
    #[test]
    fn ring_full_with_pending_held_loses_whole_frames_only() {
        let src = stereo(64);
        let ring = AudioRing::new(8);
        let (mut prod, mut cons) = ring.split();
        let mut heard = Vec::new();
        let mut pushed = 0usize;

        // 1. 环满、帧对齐
        prod.push_samples(&src[0..16]); // 8 帧 = 容量
        pushed += 16;
        assert_eq!(prod.available_frames(), 8, "前提：环应被占满");

        // 2. 1 个（奇）：一个都进不去 ⇒ src[16] 扣进 pending。**一个样本都没丢。**
        prod.push_samples(&src[16..17]);
        pushed += 1;
        assert_eq!(prod.available_frames(), 8, "前提：环满时占用数不该变");
        let dropped_after_defer = prod.dropped_frames();

        // 3. 3 个（奇）、**环仍然满、`pending` 还占着** —— 就是这一档丢过奇数个。
        prod.push_samples(&src[17..20]);
        pushed += 3;

        // 4. 排空后再推 `src[20..22]`。环已空 ⇒ 空间够 ⇒ 手上若还扣着那 1 个，
        //    这一次必定被推出去或丢掉 ⇒ 结束时 `pending` 必为空，于是
        //    「推入 − 听到 − 环内」算出来的正是**实际丢弃的样本数**。
        drain(&mut cons, &mut heard);
        prod.push_samples(&src[20..22]);
        pushed += 2;
        drain(&mut cons, &mut heard);

        let heard_samples = heard.len() * 2;
        let in_ring = prod.available_frames() * 2;
        assert!(
            !prod.has_pending_sample(),
            "前提：排空后再推过 ⇒ 手上不该还扣着样本（否则下面的差额算不准）"
        );
        let lost = pushed - heard_samples - in_ring;
        // ① 不变量本身，放在最前面断言：坏掉时最该被看到的就是这一条的信息。
        assert_eq!(
            lost % 2,
            0,
            "实际丢了 {lost} 个样本（奇数）—— 此后每一帧都会变成 (R,L)，\
             L/R 永久互换（见 AudioRing 的文档）。丢的只可能是整帧，\
             所以差额必然是偶数。推入 {pushed}、听到 {heard_samples}、环内 {in_ring}。"
        );
        // ② 口径：扣下不算丢 —— 第 2 步只扣下 1 个、没丢样本，那时不该记丢弃。
        assert_eq!(
            dropped_after_defer, 0,
            "第 2 步只是把最后一个扣在手上（没进环也没被扔），不该计入丢弃帧数"
        );
        // ③ 口径：丢的是**帧数**，与第①条的样本差额必须对得上（差额 / 2）。
        assert_eq!(
            prod.dropped_frames() * 2,
            lost as u64,
            "丢弃口径是帧数 ⇒ 累计丢弃帧数 × 2 必须等于实际丢掉的样本数"
        );

        assert!(!heard.is_empty(), "前提：总得听到点什么");
        for (k, f) in heard.iter().enumerate() {
            assert!(
                f.l >= 0 && f.r < 0,
                "第 {k} 帧听到 ({}, {}) —— 应为 (非负, 负)。\
                 `pending` 占着时丢了奇数个样本把 L/R 翻了位",
                f.l,
                f.r
            );
        }
    }

    /// **小状态空间穷尽检查**：容量 1..=6 帧 × 长度 ≤ 5 的操作序列
    /// （推 0..=4 个、取 1..=4 个，长度奇偶都有）。
    ///
    /// 这是前一版 `small_state_machine_exhaustive_check` 的替代。旧测试每步查
    /// 「实际丢弃数（含手上扣着的）是偶数」+「第 k 个听到的样本与 k 同奇偶」——
    /// 两条都建立在「环里可以装着奇数个样本」之上，那个状态今天不可表达。
    ///
    /// 新测试查的是**更强**的一条，而且不需要任何奇偶推理：
    ///
    /// > **听到的每一帧都是源流里真实存在的某一帧，且对应的源帧下标严格递增。**
    ///
    /// 它一条盖住三件事：
    /// - **没有错位配对**：任何一次「半个样本」的错处理都会造出源流里不存在的帧
    ///   （`(R,L)` 或跨帧的组合）；
    /// - **丢的只有整帧**：丢半个样本会让后续所有帧错位 ⇒ 立刻违反；
    /// - **顺序不乱**：源帧下标严格递增。
    ///
    /// ⚠️ **不是「前缀」**：ringbuf 丢的是新数据 ⇒ 源流里会留出缺口，缺口之后
    /// 的帧照样会被听到。（这条是本次实现里被这条测试自己纠正的：一开始写成
    /// 「前缀」，容量 1 的第一个反例就把它证伪了。）
    ///
    /// 它是**测试**不是证明：覆盖面到此为止 —— 容量更大、序列更长、并发场景不在
    /// 这条测试里。覆盖面本身有前置断言兜着（见末尾那两个计数），
    /// 免得它悄悄退化成空断言。
    #[test]
    fn small_state_machine_exhaustive_check() {
        // 每个操作：`op < 5` ⇒ 推 `op` 个（含 0 个）；否则 ⇒ 取 `op - 4` 帧。
        const OPS: usize = 9;
        const DEPTH: u32 = 5;
        let src = stereo(32);
        // 覆盖面计数器（见下面「不是空断言」那段）
        let mut seqs_with_heard = 0usize;
        let mut seqs_with_drops = 0usize;

        for capacity in 1..=6usize {
            for seed in 0..OPS.pow(DEPTH) {
                // 操作序列 = 把 `seed` 反复对 `OPS` 取余（低位在前）；失败信息里带 `seed`。
                let mut code = seed;
                let (mut prod, mut cons) = AudioRing::new(capacity).split();
                let mut cursor = 0usize; // 下一个要推的 `src` 下标
                let mut heard = Vec::<StereoFrame>::new();
                let mut buf = [StereoFrame::SILENT; 4];

                for step in 0..DEPTH {
                    let op = code % OPS;
                    code /= OPS;

                    if op < 5 {
                        let end = (cursor + op).min(src.len());
                        prod.push_samples(&src[cursor..end]);
                        cursor = end;
                    } else {
                        let m = op - 4;
                        let n = cons.pop_raw_frames(&mut buf[..m]);
                        heard.extend_from_slice(&buf[..n]);
                    }

                    assert_source_frame_subsequence(
                        &heard,
                        &src,
                        &format!("容量 {capacity}、seed {seed}、第 {step} 步"),
                    );
                    // 丢弃帧数的口径：**精确等于**「已推的样本 − 听到的 −
                    // 环内的 − 手上扣着的」的一半 —— 差额只能来自整帧的丢弃。
                    // 这一条把「丢了多少帧」从「记了几次事件」变成可核对的确数。
                    let accounted = cursor
                        - heard.len() * 2
                        - cons.available_frames() * 2
                        - usize::from(prod.has_pending_sample());
                    assert_eq!(
                        prod.dropped_frames() as usize * 2,
                        accounted,
                        "容量 {capacity}、seed {seed}、第 {step} 步：\
                         丢弃帧数对不上实际少掉的样本数（已推 {cursor}、\
                         听到 {}、环内 {} 帧、手上扣着 {}）",
                        heard.len() * 2,
                        cons.available_frames(),
                        usize::from(prod.has_pending_sample())
                    );
                    assert!(
                        cons.available_frames() <= capacity,
                        "容量 {capacity}、seed {seed}、第 {step} 步：占用超过容量"
                    );
                }

                if !heard.is_empty() {
                    seqs_with_heard += 1;
                }
                if prod.dropped_frames() > 0 {
                    seqs_with_drops += 1;
                }
            }
        }

        // ⚠️ **证明这条测试不是空断言**：上面那些 `assert` 只有在 `heard` 非空时
        //    才可能变红，而「丢弃」那一路还要求这个容量下真的丢过。两个计数把它
        //    钉住 —— 布局一改（例如 `OPS` 或指针偏移写错，导致永远取不到数据），
        //    这两个数会掉下去，测试**在这里**变红，而不是安静地变成恒真。
        assert!(
            seqs_with_heard > 10_000,
            "只有 {seqs_with_heard} 条序列听到过东西 —— 断言覆盖不到什么，\
             这条测试正在退化成空断言"
        );
        assert!(
            seqs_with_drops > 1_000,
            "只有 {seqs_with_drops} 条序列丢过帧 —— 「丢弃只丢整帧」那一路没被覆盖到"
        );
    }

    #[test]
    fn partial_underrun_still_fills_silence() {
        let ring = AudioRing::new(8);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[7, 8]);
        // 预填垃圾帧（见 `counts_underrun_when_empty` 里对「恒真断言」的说明）
        let mut out = [StereoFrame { l: 99, r: -99 }; 4];
        assert_eq!(cons.pop_frames(&mut out), 1);
        assert_eq!(
            out,
            [
                StereoFrame { l: 7, r: 8 },
                StereoFrame::SILENT,
                StereoFrame::SILENT,
                StereoFrame::SILENT,
            ],
            "尾部必须补静音，不能保留 out 原值"
        );
        assert_eq!(cons.underruns(), 1, "取不满也算一次 underrun");
    }

    /// **`pending` 不会被静默吞掉**：总样本数是奇数时，
    /// `has_pending_sample()` 必须为真 —— 这是那条「永远等不到右声道」的
    /// 样本**唯一**的读数（它既不在环里、也不计入丢弃帧数）。
    #[test]
    fn a_trailing_half_frame_is_visible_not_silent() {
        let ring = AudioRing::new(8);
        let (mut prod, _cons) = ring.split();
        prod.push_samples(&[1, 2, 3, 4, 5]); // 2 整帧 + 1 个样本
        assert_eq!(prod.available_frames(), 2);
        assert_eq!(prod.dropped_frames(), 0, "扣下不是丢弃");
        assert!(
            prod.has_pending_sample(),
            "第 5 个样本凑不成帧 ⇒ 必须能被看见，不许静默消失"
        );

        // 反面：偶数长度的流不该留下 pending（否则这条断言就是恒真的）
        let ring = AudioRing::new(8);
        let (mut prod, _cons) = ring.split();
        prod.push_samples(&[1, 2, 3, 4]);
        assert!(
            !prod.has_pending_sample(),
            "整帧输入不该留下半个帧 —— 否则上面那条断言是恒真的"
        );
    }
}
