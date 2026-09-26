use dslsmusic::metrics::{FrameMetrics, percentile};

#[test]
fn percentile_of_single_value() {
    assert_eq!(percentile(&[5.0], 0.5), 5.0);
}

#[test]
fn p50_of_known_series() {
    let v: Vec<f32> = (1..=100).map(|x| x as f32).collect();
    let p50 = percentile(&v, 0.50);
    assert!((p50 - 50.0).abs() < 1.0, "got {p50}");
}

#[test]
fn p99_catches_the_tail() {
    // 99 帧 8ms + 1 帧 100ms —— 平均值看起来还行，p99 暴露问题
    let mut v: Vec<f32> = vec![8.0; 99];
    v.push(100.0);
    let avg: f32 = v.iter().sum::<f32>() / v.len() as f32;
    assert!(avg < 10.0, "平均值骗人: {avg}");
    let p99 = percentile(&v, 0.99);
    assert!(p99 > 50.0, "p99 应暴露长尾: {p99}");
}

#[test]
fn percentile_clamps_out_of_range() {
    let v = vec![1.0, 2.0, 3.0];
    assert_eq!(percentile(&v, 0.0), 1.0);
    assert_eq!(percentile(&v, 1.0), 3.0);
}

// ─────────────────────────────────────────────────────────────────────────
// 以下三笔**不在简报的四处测试里**，是补的。理由：简报把「`begin_frame()`
// 未配对」列为必读项，但没给任何测试；而那套逻辑有个安静的坑——`end_frame()`
// 若不 `take()` 走起点，每个**正常**帧的下一次 `begin_frame()` 都会误判成
// 「上一帧被放弃了」，于是 skip 计数恒等于帧数。只有测试能挡住它。
//
// 这三个函数只读 `summary()` 的输出（公开可观察面），因此不需要为测试
// 开任何新的公开 API。
// ─────────────────────────────────────────────────────────────────────────

/// 取 `summary()` 里 `skip` 后面那个数。不依赖 `{:>3}` 补了几个空格。
fn skip_of(s: &str) -> u64 {
    s.split("skip")
        .nth(1)
        .expect("summary 应含 skip 字段")
        .split_whitespace()
        .next()
        .expect("skip 后面应有数字")
        .parse()
        .expect("skip 后面应是整数")
}

/// 取 `summary()` 里 `(n=…)` 的 n。
fn n_of(s: &str) -> usize {
    s.rsplit("(n=")
        .next()
        .expect("summary 应含 (n=)")
        .trim_end_matches(')')
        .trim()
        .parse()
        .expect("(n=) 里应是整数")
}

#[test]
fn paired_frames_are_not_counted_as_skipped() {
    let mut m = FrameMetrics::new();
    for _ in 0..5 {
        m.begin_frame();
        m.end_frame();
    }
    let s = m.summary();
    assert_eq!(skip_of(&s), 0, "正常配对的帧不该被算成跳过: {s}");
    assert_eq!(n_of(&s), 5, "{s}");
}

#[test]
fn unpaired_begin_frame_is_counted_not_silently_dropped() {
    let mut m = FrameMetrics::new();
    m.begin_frame(); // 这一帧被 `continue` 放弃了：没有 end_frame
    m.begin_frame();
    m.end_frame();
    let s = m.summary();
    assert_eq!(skip_of(&s), 1, "未配对的 begin_frame 必须被计数: {s}");
    assert_eq!(n_of(&s), 1, "被放弃的帧不产出样本: {s}");
}

#[test]
fn ring_buffer_never_reports_more_than_capacity() {
    let mut m = FrameMetrics::new();
    for _ in 0..700 {
        m.begin_frame();
        m.end_frame();
    }
    let s = m.summary();
    assert_eq!(
        n_of(&s),
        600,
        "环形缓冲应停在 CAPACITY(=600)，不无限增长: {s}"
    );
}
