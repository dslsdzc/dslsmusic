//! ring 的**帧对齐不变量** —— 生产端那一半。
//!
//! # 这个文件是替换过来的，先把来龙去脉写清楚
//!
//! **它原来钉的是什么。** 旧测试叫 `odd_drop_does_not_swap_channels`，前提是
//! 「环里可以装着**奇数个样本**」：先把环填成 7 个样本（容量 8），再推一个
//! 偶数长度的块（空闲 1 ⇒ 只能进 1 个 ⇒ **丢 7 个，奇数**）。此后每个「帧」
//! 都变成 `(R,L)`：**左右声道永久互换**，而采样率、`xrun`、波形统计全部照常。
//!
//! **为什么它现在不可表达。** 这个测试要**构造**的那个状态 —— 环里装着奇数个
//! 样本 —— 已经不存在了：`AudioRing` 的存储单元是 `StereoFrame`（整帧），
//! 容量按帧计。往环里推的时候，「推一半」这个动作在类型上就写不出来，
//! 所以「丢奇数个样本」不是被断言挡住，是**构造不出来**。
//! 一个先决条件不可达的测试，留着只会让人以为它还在保护什么。
//!
//! **它现在钉的是什么。** 旧测试真正想保护的东西一点没变，只是换了个更强的
//! 说法：**任意推送边界（含奇数长度）下，L/R 配对不能错位。** 下面用两条性质
//! 把它钉死，两条都**不依赖任何奇偶推理**：
//!
//! 1. **听到的每一帧都是源流里真实存在的某一帧，且源帧下标严格递增。**
//!    一次错位配对（哪怕只错一个样本）就会造出一个源流里根本不存在的帧
//!    （`(R,L)` 或跨帧的组合）。
//! 2. **丢的永远是整帧** —— 丢弃帧数 × 2 精确等于实际少掉的样本数。
//!
//! ⚠️ 第 1 条**不是**「听到的等于源流的前缀」：ringbuf 丢的是**新**数据，
//! 源流里会留出缺口，缺口之后的帧照样会被听到。第一版这里写的就是「前缀」，
//! 被容量 1 的穷尽检查当场证伪 —— 记在这里，免得下一个人再写一遍。
//! （只有「一次都没丢过」的序列才有前缀性质，下面 `is_prefix_of` 的文档里
//! 标了这条边界。）
//!
//! ⚠️ 这两条是**测试**不是证明：覆盖面到此为止（容量、序列长度、并发场景不在这
//! 条测试里）。但它们**不是恒真的** —— 本次改动做过变异测试。
//! 破坏 `push_samples` 的三种方式都让本文件的用例变红：**性质 2**
//! （`overflow_drops_whole_frames_only`）被 M1/M2/M3 三种全抓住，**性质 1**
//! （`arbitrary_split_points_never_mispair_frames`）被其中 M1/M2 抓住；
//! 第四种变异（M4）动的是**回调**的 L/R，不在 `push_samples` 上，由 `lib.rs`
//! 里的 `callback_*` 用例负责，**本文件不声称覆盖它**。
//!
//! ⚠️ 那份记录是**记录，不是可复现的产物**：变异在 `/tmp` 的隔离副本上做，
//! 副本没有留下来。逐条结果（哪个变异让哪些用例变红）见
//! `.superpowers/sdd/task-ring-frames-report.md` 的「变异测试证据」一节。

use dsls_audio::{AudioRing, AudioRingConsumer, StereoFrame};

/// 构造交错立体声流：**L 恒为非负、R 恒为负**，于是帧对齐可以逐帧断言。
///
/// 取值范围编码了下标本身（`i16` 放得下 64 以内），所以任意位置都能反推出
/// 它「本该」在哪一帧 —— 错位会立刻露馅。
fn stereo(n: usize) -> Vec<i16> {
    (0..n)
        .map(|i| if i % 2 == 0 { i as i16 } else { -(i as i16) })
        .collect()
}

/// 把环里能取到的帧全部抽干、追加到 `out`。
fn drain(c: &mut AudioRingConsumer, out: &mut Vec<StereoFrame>) {
    let mut buf = [StereoFrame::SILENT; 3]; // 故意用**奇数**大小的缓冲
    while c.available_frames() > 0 {
        let got = c.pop_raw_frames(&mut buf);
        out.extend_from_slice(&buf[..got]);
    }
}

/// `heard` 是不是 `src` 的**前缀**（按样本算）。不分配。
///
/// ⚠️ **只对「一次都没丢过」的序列成立。** 有丢弃时源流里会留出缺口，
/// 听到的不再是前缀 —— 那种情况下用 `assert_source_frame_subsequence`。
fn is_prefix_of(heard: &[StereoFrame], src: &[i16]) -> bool {
    heard.len() * 2 <= src.len()
        && heard
            .iter()
            .enumerate()
            .all(|(i, f)| f.l == src[i * 2] && f.r == src[i * 2 + 1])
}

/// **有丢弃时「L/R 永不互换」的正确说法**：`heard` 里每一帧都必须是源流里
/// **真实存在的某一帧**，且对应的源帧下标**严格递增**。
///
/// 一次错位配对（哪怕只错一个样本）就会造出一个源流里根本不存在的帧。
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

/// 每条听到的帧都必须「左非负、右为负」—— 换成 `(R,L)` 会当场违反。
fn assert_frames_not_swapped(heard: &[StereoFrame], ctx: &str) {
    for (k, f) in heard.iter().enumerate() {
        assert!(
            f.l >= 0 && f.r < 0,
            "{ctx}：第 {k} 帧听到 ({}, {}) —— 应为 (非负, 负)。L/R 已互换。",
            f.l,
            f.r
        );
    }
}

/// **性质 1：任意推送边界（奇数、偶数、跨帧、空推）都不许错位配对。**
///
/// 推送长度由一串**确定性伪随机**数决定（LCG，无依赖），覆盖新旧结构下最容易
/// 出错的那种切法：一次次把帧切成两半再接着推。每一步都查前缀性质 ——
/// 不等就说明有帧被错配，或流中间被挖掉了一个样本。
#[test]
fn arbitrary_split_points_never_mispair_frames() {
    let src = stereo(600);
    let ring = AudioRing::new(16); // 16 帧 = 32 个样本，故意小，好让它溢出
    let (mut prod, mut cons) = ring.split();
    let mut heard: Vec<StereoFrame> = Vec::new();

    let mut cursor = 0usize;
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut step = 0usize;

    while cursor < src.len() {
        // 1..=7 个样本，**奇偶都有** —— 这一步是旧测试那个「7 个」的推广。
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let want = (state >> 33) as usize % 7 + 1;
        let end = (cursor + want).min(src.len());
        prod.push_samples(&src[cursor..end]);
        cursor = end;

        drain(&mut cons, &mut heard);

        assert!(
            is_prefix_of(&heard, &src),
            "第 {step} 步（推到 src[{end}]）后：听到的样本不是源流的前缀 —— \
             有帧被错配（L/R 已互换）或有样本在中间被挖掉。\
             已推 {cursor} 个、听到 {} 个、环内 {} 帧、手上扣着 {}。",
            heard.len() * 2,
            cons.available_frames(),
            usize::from(prod.has_pending_sample())
        );
        step += 1;
    }

    // 收尾：把最后一个可能扣着的半个样本推完（流的长度是偶数，所以这里应当
    // 本来就没有 pending —— 先断言这一点，再断言最后的流也全对）。
    assert!(
        !prod.has_pending_sample(),
        "前提：源流总样本数是偶数 ⇒ 不该留下半个帧。\
         若这里为真，说明 `push_samples` 把整帧的尾巴也算成了半个 —— \
         那会让后面所有帧错位"
    );
    assert!(step > 50, "前提：步数够多，才谈得上「任意边界」");
    assert!(!heard.is_empty(), "前提：总得听到点什么");
    assert_eq!(
        prod.dropped_frames(),
        0,
        "前提：环比一次推送大得多、且每步都排空 ⇒ 一次都不该丢。\
         丢掉的话上面那些「前缀」断言就成了对一个被截断的流的断言，说明不了配对对不对"
    );
    assert_eq!(
        heard.len() * 2,
        src.len() - prod.available_frames() * 2,
        "前提：没有丢弃、也没有 pending 时，听到的 + 还在环里的 = 推过的"
    );
    assert_frames_not_swapped(&heard, "任意切分");
}

/// **性质 2：溢出只丢整帧，而且 `dropped_frames()` 是确数。**
///
/// 把环推爆（**整段推完再排空** —— 边推边排空的话环永远不满，这条测试就空了），
/// 然后核对三件事：
/// - 丢掉的样本数是**偶数**（只能由整帧构成）；
/// - `dropped_frames()` × 2 **精确等于**实际少掉的样本数（不是「记了几次事件」）；
/// - 每一帧仍然是**源流里真实存在的某一帧**、且源帧下标严格递增。
///
/// ⚠️ 最后一条**不是**「前缀」：ringbuf 丢的是**新**数据 ⇒ 源流里会留出缺口，
/// 缺口之后的帧照样会被听到。（第一版这里写的是「前缀」，被容量 1 的穷尽检查
/// 证伪了 —— 记在这里，免得下一个人再写一遍。）
#[test]
fn overflow_drops_whole_frames_only() {
    let src = stereo(64);
    let ring = AudioRing::new(6); // 6 帧 = 12 个样本
    let (mut prod, mut cons) = ring.split();
    let mut heard: Vec<StereoFrame> = Vec::new();

    let mut pushed = 0usize;
    // 长度 7（奇）的推送：每次有 1 个样本扣在 pending 里等下一次。**不排空**，
    // 环才会被填满 ⇒ 才谈得上溢出。
    for chunk in src.chunks(7) {
        prod.push_samples(chunk);
        pushed += chunk.len();
    }
    assert!(
        prod.dropped_frames() > 0,
        "前提：这个容量下必须真丢过帧，否则这条测试是空断言"
    );
    drain(&mut cons, &mut heard);

    let in_ring = prod.available_frames() * 2;
    let pending = usize::from(prod.has_pending_sample());
    let lost = pushed - heard.len() * 2 - in_ring - pending;

    assert!(lost > 0, "前提：必须真丢过东西，否则这条测试是空的");
    assert_eq!(
        lost % 2,
        0,
        "实际丢了 {lost} 个样本（奇数）—— 此后每一帧都会变成 (R,L)，L/R 永久互换。\
         丢的只可能是整帧，所以差额必然是偶数。已推 {pushed}、听到 {}、环内 {in_ring}、\
         手上扣着 {pending}。",
        heard.len() * 2
    );
    assert_eq!(
        prod.dropped_frames() as usize * 2,
        lost,
        "`dropped_frames()` 的口径是**帧数** ⇒ × 2 必须精确等于实际少掉的样本数"
    );
    assert_source_frame_subsequence(&heard, &src, "溢出之后");
    assert_frames_not_swapped(&heard, "溢出之后");
}

/// **`pending` 里的那半个帧不会被静默吞掉。**
///
/// 一个流的总样本数是奇数时，最后一个样本永远等不到它的右声道：它不进环、
/// 也不计入丢弃帧数（它不是被丢弃，是这条流自己不成帧）。它**唯一**的读数是
/// `has_pending_sample()`。这条测试同时是「可见性」这条规格的钉子。
#[test]
fn a_trailing_half_frame_is_reported_not_swallowed() {
    let ring = AudioRing::new(8);
    let (mut prod, mut cons) = ring.split();
    let mut heard = Vec::new();

    prod.push_samples(&stereo(9)); // 4 整帧 + 1 个样本
    assert_eq!(prod.available_frames(), 4, "只进得去 4 整帧");
    assert_eq!(prod.dropped_frames(), 0, "扣下不是丢弃 —— 环没满");
    assert!(
        prod.has_pending_sample(),
        "第 9 个样本凑不成帧 ⇒ 必须能被看见，不许静默消失"
    );

    drain(&mut cons, &mut heard);
    assert_eq!(heard.len(), 4, "听到的是 4 整帧");
    assert!(
        is_prefix_of(&heard, &stereo(9)),
        "听到的仍是源流的前缀（那半个样本只是还没成帧，没有被跳过）"
    );
}
