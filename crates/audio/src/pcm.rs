//! PCM 处理链。**v2 的 DSP（EQ / 响度 / 变速 / 交叉淡入）都挂在这里。**
//!
//! v1 是空实现：`process` 是恒等变换，无 stage 时纯属循环开销（可被优化掉）。
//! 留这个调用点的理由——它位于**解码与 ring 之间**，事后补要改解码循环。

pub trait PcmStage: Send {
    fn process(&mut self, samples: &mut [i16]);
}

#[derive(Default)]
pub struct PcmGraph {
    stages: Vec<Box<dyn PcmStage>>,
}

impl PcmGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// v2 用，例如 `graph.push(Box::new(EqStage::new(&config)))`
    pub fn push(&mut self, stage: Box<dyn PcmStage>) {
        self.stages.push(stage);
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    /// 原地处理。stage 按 push 顺序依次作用。
    pub fn process(&mut self, samples: &mut [i16]) {
        for stage in &mut self.stages {
            stage.process(samples);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Gain(f32);
    impl PcmStage for Gain {
        fn process(&mut self, samples: &mut [i16]) {
            for s in samples.iter_mut() {
                *s = ((*s as f32) * self.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            }
        }
    }

    /// 测试用 stage：加一个常数（饱和）。**故意与 `Gain` 不可交换** —— 见
    /// `stages_apply_in_order`。
    struct Offset(i16);
    impl PcmStage for Offset {
        fn process(&mut self, samples: &mut [i16]) {
            for s in samples.iter_mut() {
                *s = s.saturating_add(self.0);
            }
        }
    }

    #[test]
    fn empty_graph_is_identity() {
        let mut g = PcmGraph::new();
        let mut s = [1i16, -2, 3, i16::MAX, i16::MIN];
        let before = s;
        g.process(&mut s);
        assert_eq!(s, before);
    }

    #[test]
    fn stages_apply_in_order() {
        // ⚠️ 两个 stage **必须不可交换**，否则顺序对调照样通过，测不出
        //    「按 push 顺序」。原来用的 ×2 与 ×0.5 都是正增益、彼此可交换
        //    （`(x*2)*0.5 == (x*0.5)*2`），那条断言对顺序不敏感 —— 已换掉。
        //    这里用 ×2 与 +10：先乘后加 ≠ 先加后乘。
        let mut mul_then_add = PcmGraph::new();
        mul_then_add.push(Box::new(Gain(2.0)));
        mul_then_add.push(Box::new(Offset(10)));
        let mut s = [10i16, -20, 30];
        mul_then_add.process(&mut s);
        assert_eq!(s, [30, -30, 70], "先 ×2 再 +10");

        let mut add_then_mul = PcmGraph::new();
        add_then_mul.push(Box::new(Offset(10)));
        add_then_mul.push(Box::new(Gain(2.0)));
        let mut s = [10i16, -20, 30];
        add_then_mul.process(&mut s);
        assert_eq!(s, [40, -20, 80], "先 +10 再 ×2：与上面不同 ⇒ 顺序真的生效");
    }

    #[test]
    fn gain_must_saturate_not_wrap() {
        // 溢出回绕是音频里最刺耳的 bug 之一，必须饱和
        let mut g = PcmGraph::new();
        g.push(Box::new(Gain(100.0)));
        let mut s = [1000i16, -1000];
        g.process(&mut s);
        assert_eq!(s, [i16::MAX, i16::MIN]);
    }
}
