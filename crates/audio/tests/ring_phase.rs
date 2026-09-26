//! ring 的**帧对齐不变量**。
//!
//! 立体声流进入 ring 时是交错样本 `L,R,L,R,…`。只要**丢弃的样本数是奇数**，
//! 后续所有样本的奇偶位置就翻一位 —— 从此每个「帧」都变成 `(R,L)`，
//! **左右声道永久互换**，而采样率、`xrun`、波形统计全部照常。
//!
//! 消费端（`fill_from_ring`）已经把 pop 长度向下取到偶数，保护了自己那一侧；
//! 生产端没有对应的约束。本测试钉住生产端。

use dsls_audio::AudioRing;

/// 构造交错立体声流：**L 恒为非负、R 恒为负**，于是帧对齐可以逐帧断言。
fn stereo(n: usize) -> Vec<i16> {
    (0..n)
        .map(|i| if i % 2 == 0 { i as i16 } else { -(i as i16) })
        .collect()
}

/// 把环里成对的样本抽干，追加到 `out`。
fn drain(c: &mut dsls_audio::AudioRingConsumer, out: &mut Vec<i16>) {
    let mut buf = [0i16; 2];
    while c.available() >= 2 {
        c.pop_samples(&mut buf);
        out.extend_from_slice(&buf);
    }
}

#[test]
fn odd_drop_does_not_swap_channels() {
    let src = stereo(64);
    let ring = AudioRing::new(8);
    let (mut prod, mut cons) = ring.split();
    let mut heard = Vec::new();

    // 先把环填成【奇数占用】7 —— push 端按任意边界切分同一个流是合法的。
    prod.push_samples(&src[0..7]);
    assert_eq!(prod.available(), 7, "前提：环里应有 7 个样本");

    // 再推一个【偶数长度】的块：空闲 1，只能进 1 个 ⇒ **丢掉 7 个（奇数）**。
    prod.push_samples(&src[7..15]);
    assert_eq!(prod.overflows(), 1, "前提：这一次应记一次溢出");

    drain(&mut cons, &mut heard);
    // 此后继续喂，看流有没有错位。
    for chunk in src[15..].chunks(8) {
        prod.push_samples(chunk);
        drain(&mut cons, &mut heard);
    }
    drain(&mut cons, &mut heard);

    assert!(!heard.is_empty(), "前提：总得听到点什么");
    for (k, pair) in heard.chunks_exact(2).enumerate() {
        assert!(
            pair[0] >= 0,
            "第 {k} 帧的左声道听到 {} —— 应为非负。\
             说明有一次奇数长度的丢弃把 L/R 翻了位（第 {} 个样本 = {}）",
            pair[0],
            k * 2,
            pair[0]
        );
        assert!(
            pair[1] < 0,
            "第 {k} 帧的右声道听到 {} —— 应为负。L/R 已互换。",
            pair[1]
        );
    }
}
