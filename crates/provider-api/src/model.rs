/// **保真度**档位。有序，可沿 [`Quality::lower`] 逐级降。
/// 各平台原始标识在 provider 内部映射（设计文档 §4.5）。
///
/// ⚠️ **这里只有保真度。** 网易云的 `jyeffect` / `jymaster` / `sky` /
/// `dolby` / `vivid` **不属于本枚举**——它们是不同的混音／声道结构，
/// 不可排序、也不可互相降级（设计文档 §4.5）。见 [`AudioEffect`]。
///
/// `#[non_exhaustive]`：resolver 马上要 `match` 它，而写操作/新音源可能
/// 引入新档位。**加变体是破坏性变更，现在标比以后改便宜。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Quality {
    /// 网易云 `standard`，128k
    Low,
    /// 网易云 `higher`，192k
    Standard,
    /// 网易云 `exhigh`，320k
    High,
    /// 网易云 `lossless`，无损
    Lossless,
    /// 网易云 `hires`，高解析度
    HiRes,
}

impl Quality {
    /// 降一档。已是最后一档则返回 `None`——降级链到此为止。
    ///
    /// **没有它，resolver 的降级链写不出来**——`Quality` 只派生 `Ord`，
    /// 无法枚举档位。而 `SourceError::should_retry_lower_quality()` 返回
    /// true 时，调用方必须知道「更低是哪一档」才能重试。
    pub fn lower(self) -> Option<Quality> {
        match self {
            Quality::HiRes => Some(Quality::Lossless),
            Quality::Lossless => Some(Quality::High),
            Quality::High => Some(Quality::Standard),
            Quality::Standard => Some(Quality::Low),
            Quality::Low => None,
        }
    }
}

/// **AI / 音效**档位。**与 [`Quality`] 之间没有 `Ord` 关系。**
///
/// **已核实的是「它们不属于保真度梯度」这件事本身**：SPlayer 的
/// `src/utils/meta.ts:55` 写着 `AI_AUDIO_LEVELS = ["jymaster","sky","jyeffect","vivid"]`、
/// 注释「AI 增强音质」，并在 `helper.ts:437` 的**保真度判断里显式排除**它们。
/// ⇒ 因此 **`sky → jymaster` 不是降级**，把它们塞进 `Quality` 的 `Ord` 梯度
/// 会产出荒唐的降级路径。
///
/// ⚠️ **但它们的「机理」我一条都没有证据，别把下面当事实读：**
///
/// - 「`jymaster` 是神经网络升频」「`sky` 是 AI 分离声部后模拟 5.1」
///   「`dolby`/`vivid` 是空间音频格式」—— 这三条**都属于解释，不是核实**。
///   它们最早以「**机理吻合**」的措辞出现在调研里，**我把它当事实写进了这里**。
/// - 2026-09-26 的复核查了：**没有任何可采信的实测/抓包证据**说明这几个档
///   返回什么格式。流传最广的「杜比 = EAC3 in MP4」**出自一条代码注释**，
///   追它的两个引用项目都是同一血脉 ⇒ **独立证据数 = 1**。
/// - **反证**：严肃逆向客户端 `chaunsin/netease-cloud-music` 把常量**注释掉了**：
///   `// LevelDolby 杜比 暂时未知`。
/// - **一条反向线索**：`vivid` 需要 `encodeType=mp3` —— 那**指向它有可能是 mp3**，
///   而不是什么特殊声道格式。
///
/// **⇒ 想用这个枚举做播放决策之前，先抓一次真实响应。** 见 `docs/` 的调研结论。
///
/// **v1 不实现。** 这个类型存在的意义是把「这里有一条轴」写进类型里，
/// 防止以后误把音效档当成保真度档塞进 `Quality`（用户 2026-09-26 裁决）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AudioEffect {
    /// 高清臻音。⚠️ 官方中文名**待定**——来源互相矛盾，别照抄任何一处
    Jyeffect,
    /// 超清母带。**神经网络升频，不是原生更高的保真度**
    Jymaster,
    /// 沉浸环绕声（官方名已核实）。⚠️ 请求时要带 `immerseType`（默认 `c51`）。
    /// ⚠️ 「AI 分离声部后模拟 5.1」这个机理**没有证据**，见枚举级文档
    Sky,
    /// 杜比全景声。⚠️ 需要 `os=pc`，**与 `Vivid` 互斥**
    Dolby,
    /// 臻音全景声。⚠️ 需要 `os=android` + `appver=9.5.61`，**与 `Dolby` 互斥**
    ///
    /// ⚠️ **中文名的证据弱，落地前需外部核实**：`vivid` 被 api-enhanced 自己的
    /// 枚举、ncm-api-rs 文档、SPlayer 的 `song.ts` 联合类型**全部排除在外**，
    /// 只有 api-enhanced 文档与 SPlayer 的 UI 配置表提到它。
    Vivid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub id: String,
    /// **这首歌来自哪个音源**（provider 的 `name()`）。
    ///
    /// 必须有：不同音源的 id 命名空间是私有的（网易云是数字串、本地是
    /// 路径/哈希、云盘是另一套）。没有它，resolver 拿到一个 `Track`
    /// **无法路由回正确的 provider 去调 `stream_url`**，只能外挂一张
    /// (provider, track) 边表；跨音源合并去重也没有键。
    ///
    /// 设计文档 §4.5 的「统一 item」本来就含 `platform`——这里是补齐。
    pub platform: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// 毫秒
    pub duration_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lower_walks_the_five_fidelity_tiers_to_none() {
        assert_eq!(Quality::HiRes.lower(), Some(Quality::Lossless));
        assert_eq!(Quality::Lossless.lower(), Some(Quality::High));
        assert_eq!(Quality::High.lower(), Some(Quality::Standard));
        assert_eq!(Quality::Standard.lower(), Some(Quality::Low));
        assert_eq!(Quality::Low.lower(), None);
    }

    /// 钉住**保真度梯度就是这 5 档、且是一条直线**。
    ///
    /// ⚠️ **不要从 `Low` 起步往下数。** `lower()` 是**向下降级**的，
    /// 从最低档起步循环体一次都不进，`steps` 恒为 1 ——
    /// **那条测试无论实现怎么写都不可能通过。**（本计划的初稿正是这么写的，
    /// 实现者实测 `left: 1, right: 5` 才发现。）
    ///
    /// ⚠️ **也不要只从 `HiRes` 起步数个数。** 那样「在 `HiRes` **之上**
    /// 再加一档」会**静默通过**，与这条测试自称的用途不符。
    ///
    /// 所以用两件事合起来钉：
    /// **(a)** 一个**穷尽 `match`** —— 加变体而没列进这里，`match` 会因为
    /// 不穷尽而**编译失败**（`Quality` 虽标了 `#[non_exhaustive]`，
    /// 但那只约束**下游** crate，crate 内部的 `match` 仍是穷尽的）；
    /// **(b)** 从**最高档**起步逐级降，必须正好 4 步落到 `Low`。
    #[test]
    fn ladder_is_exactly_the_five_fidelity_tiers() {
        let mut q = Quality::HiRes;
        let mut steps = 0usize;
        while let Some(next) = q.lower() {
            steps += 1;
            // 防死循环（不是对梯子形状的断言）：`lower()` 是总函数，
            // 理论上可以配出环，跑测试时挂死比断言失败更难查。
            assert!(steps < 16, "降级链出现环");
            q = next;
        }
        assert_eq!(steps, 4, "HiRes→Low 应正好 4 步（5 档）");

        // ⭐ **终点断言故意写成穷尽 `match`，不加辅助函数、不加 `#[allow]`。**
        //    5 个变体**逐个列出**：给 `Quality` 加第 6 个变体而没列进这里 →
        //    这个 `match` 不穷尽 → **E0004 编译失败**。这是唯一能堵住
        //    「在 `HiRes` 之上再加一档会静默通过」的办法，且不需要 strum
        //    之类的枚举反射依赖。
        //
        //    ⚠️ **绝不能加通配臂** —— 无论是 `_ => {}` 还是绑定臂
        //    `other => ...`，都会让这个检查**静默失效**（实测：6 档下照样 PASS）。
        match q {
            Quality::Low => {}
            Quality::Standard | Quality::High | Quality::Lossless | Quality::HiRes => {
                panic!("降级链的终点必须是 Low，实际是 {q:?}")
            }
        }
    }

    /// 单调性：每一档严格低于上一档。
    ///
    /// **它不是上面那条的复述。** 上面只查「几步到 Low」；若把 `lower()` 的
    /// **声明顺序**写反（例如把 `Quality::Low => None` 挪到最前），
    /// 只有这一条会失败。（复审用变异测试确认过：6 组变异里，
    /// 「声明顺序反转」那一组**只有它 FAIL**。）
    #[test]
    fn lower_is_strictly_decreasing() {
        let mut q = Quality::HiRes;
        let mut steps = 0usize;
        while let Some(next) = q.lower() {
            assert!(next < q, "lower() 必须严格下降：{q:?} -> {next:?}");
            q = next;
            steps += 1;
            // 防死循环。**这不是对梯子形状的断言** —— `lower()` 是总函数，
            // 理论上能配出环，而测试挂死比断言失败难查得多。
            assert!(steps < 16, "降级链出现环");
        }
        assert_eq!(q, Quality::Low, "链的终点必须是最低档");
    }
}
