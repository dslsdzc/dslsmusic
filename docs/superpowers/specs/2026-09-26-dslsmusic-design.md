# dslsmusic 设计文档

- 日期：2026-09-26
- 状态：**一级架构选型已落定；若干 backend contract 与实现假设待 spike 验证**（§15.2、§17）
- 未决项：见 §15。**本文档中所有「必须/Rgba16Float/零成本」一类的绝对表述均已按 §17 的能力分级改写**

> **注意：不要把这句读成「技术选型全部落定」。** 已落定的是**一级架构**（SDL3 平台层 + wgpu 渲染层 + 自研 retained UI + 音源独立 crate）。下游还有三类 **implementation contract** 未定型，见 §17。

**已确认的三项决策：**

| 决策 | 结论 |
|---|---|
| §12 里程碑顺序 | 确认「先出声」——M1（网易云登录 + 搜索 + 取流 + 出声，UI 用最丑方框）排在 M3（UI runtime）之前，先把最大不确定性排掉 |
| §8.1 M3E 取色源 | 确认从**专辑封面**提取，不读系统壁纸。跟着歌走更对味，且绕开壁纸权限 |
| §4.4 登录约束 | 确认**只做只读路径**（扫码 / UID），绝不做签到、打卡、云盘上传等写操作 |
- 目标平台：Linux（首发）→ Windows / macOS / iOS / Android

---

## 1. 项目定位与范围

### 1.1 是什么

一个 Rust 编写的跨平台音乐播放器。以网易云音乐为主要音源，同时支持本地文件，未来支持更多音源。视觉上走统一自研路线：Apple 液态玻璃风格（Apple 平台与桌面）与 Material 3 Expressive（Android），运行时热切换。

### 1.2 v1 范围：功能最小 + UI 完整

这是本项目最重要的一条范围决策。功能面收到极窄，但 UI 层要做到接近成品。

**功能面（只做这些）：**

- 登录（只读路径，见 §4.4）
- 搜索
- 播放
- 歌词（逐字）

**UI 层（做完整）：**

- 完整 UI runtime：树、布局、文本、滚动、手势、动画
- 完整 Glass 设计系统
- M3E 薄实现（见 §8.3）
- 玻璃渲染管线

**判定 v1 完成的出口标准：** 两个页面——**列表页**（搜索 + 结果）+ **播放页**（封面模糊背景 + 玻璃控制条 + 逐字歌词）——做到视觉与交互完整，两页之间的导航转场可用，设计系统可热切换。

### 1.3 明确的非目标（v1 不做）

| 不做 | 原因 |
|---|---|
| 歌单管理、收藏、每日推荐 | 功能面收窄；这些是 v2 的事 |
| 下载、离线缓存 | 同上 |
| EQ / DSP / 频谱可视化 | `dsls-audio` 预留 PCM graph 挂载点，但 v1 不实现 |
| 多音源插件 ABI | Rust 无稳定 ABI，动态库插件是大工程（见 §4.3） |
| 完整无障碍桥接 | v1 只建语义树（见 §13 风险 4） |
| 桌面歌词独立窗口 | v2 |
| 移动端实际落地 | 架构上不排斥，v1 不做 |

### 1.4 v1 的功能面为什么这么窄

窄功能面被刻意选成能压出每个 UI 子系统的最小子集：

- 播放页（封面模糊背景 + 玻璃控制条 + 逐字歌词）→ 逼出 backdrop 采样管线、玻璃材质、文本引擎的字形级定位
- 列表页（长列表 + 滚动）→ 逼出变高虚拟列表与滚动惯性
- 两页之间的跳转 → 逼出导航转场（M3E 的 container transform / Apple 的 zoom）

功能少，但每个 UI 子系统都被真实调用，不产生无人使用的代码。

---

## 2. 总体架构

```
┌──────────────────────────────────────────────────────┐
│  app/          组合根：装配 core + ui + platform       │
├───────────────────────────┬──────────────────────────┤
│  UI Runtime（自研）        │  Music Engine（自研）     │
│  tree / layout / text /   │  provider / audio /      │
│  motion / render / widgets│  library / lyrics        │
├───────────────────────────┴──────────────────────────┤
│  platform/     SDL3 封装 + 原生集成 trait              │
├──────────────────────────────────────────────────────┤
│  SDL3                    │  wgpu                      │
│  window / input / IME /  │  Vulkan / D3D12 /          │
│  popup / 移动端生命周期    │  Metal / GLES              │
└──────────────────────────────────────────────────────┘
```

### 2.1 边界铁律

**UI 不直接调用 provider。** 单向数据流：

```
UI  ──dispatch(action)──▶  core  ──处理──▶  状态快照  ──▶  UI 渲染
```

播放队列与播放状态机住在 `core`，不在 UI 里。这样将来的 CLI、桌面歌词独立窗口、移动端全部复用同一个 core。

**音源层不依赖任何 UI 类型。** 这一条由把音源做成独立 crate 来强制（见 §4.1），而不是靠约定。

### 2.2 一条架构约束：backdrop 是 pass 拓扑，不是样式

液态玻璃的定义性特征是它**采样并折射身后的内容**。这意味着渲染器必须在画玻璃层之前先做一次 backdrop 快照，玻璃面板本身在 scene texture 里**不画底**（Backdrop-Root rule，见 §9.3）。

因此设计系统的抽象必须能表达「渲染 pass 差异」，而不只是样式数据。见 §8。

### 2.3 仓库结构

```
dslsmusic/
├── crates/
│   ├── core/                 领域模型 + 播放队列 + 播放状态机
│   ├── audio/                解码 / 输出 / PCM graph
│   ├── library/              本地库扫描、元数据、封面
│   ├── database/             SQLite
│   ├── lyrics/               TTML / LRC / YRC 解析 + 时间轴
│   │
│   ├── provider-api/         MusicSource trait + 规范化模型（不依赖 UI）
│   ├── provider-netease/
│   ├── provider-local/
│   ├── source-resolver/      音质降级链、缓存、统一成 AudioSource
│   │
│   ├── ui-core/              树、事件、focus、状态
│   ├── ui-layout/            flex / grid / 约束求解 / 虚拟列表
│   ├── ui-text/              shaping / bidi / CJK / 字形级定位
│   ├── ui-motion/            弹簧、时间线、驱动
│   ├── ui-render/            wgpu、pass 图、图元、backdrop cache
│   ├── ui-widgets/           一套，不知道设计系统存在
│   │
│   ├── theme-api/            DesignSystem trait
│   ├── theme-glass/
│   ├── theme-m3e/
│   │
│   ├── platform/             SDL3 封装 + 原生集成 trait
│   └── platform-native/      MPRIS / SMTC / MediaSession / NowPlaying 实现
│
├── shaders/                  WGSL：blur / glass / gradient / shadow / artwork
├── platforms/                android/ ios/ 等壳工程
├── app/                      组合根
└── docs/
```

**边界由 crate 强制，不靠约定：** `provider-*` 不依赖任何 `ui-*`；`ui-*` 不依赖任何 `provider-*`；两边都只通过 `core` 通信。

---

## 3. 技术选型总表

| 层 | 选择 | 理由 / 被否决的选项 |
|---|---|---|
| 语言 | Rust（全栈） | — |
| 平台层 | **SDL3** | 提供 window / input / IME / popup / 移动端生命周期。IME 与 popup 都已验证可用（见 §11）。所有 SDL 调用包在 `platform/` trait 后面，应对 `sdl3` crate pre-1.0 风险 |
| 渲染层 | **wgpu** | 否决 `SDL_GPU`：SDL 官方对 Android 的 Vulkan feature 要求态度是「Android will almost certainly hate this」，有人建议直接放弃 Android。wgpu 有 GLES 3.0 后端可兜底 Android；naga 是 Rust 原生 shader 工具链（写 WGSL，无 DXC / SPIRV-Cross 的 C 依赖）；生态成熟（Bevy / egui / iced 都在用）。**版本 30.0.1——注意 wgpu 没有 1.x，版本号是 0.19 → 22 → … → 30 这条线**（本文档早期版本误写为「1.x」） |
| UI 模式 | **retained mode** | 否决 immediate mode（egui 式）：播放器需要动画状态、滚动状态、focus、文本选择、列表虚拟化、持久组件状态，immediate mode 全都做不了 |
| 文本引擎 | **parley**（布局）+ **swash**（纯光栅化） | 见 §7.2。**不用 glyphon** |
| 音频解码 | **symphonia** | 纯 Rust，mp3 / aac / flac / ogg / wav 全覆盖。格式不够用时换 ffmpeg 绑定，走 `dsls-audio` 抽象 |
| 音频输出 | **cpal** | 独占模式 / bit-perfect 是发烧友需求，v1 不做，抽象留入口 |
| 存储 | **SQLite + rusqlite** | 不引入 ORM |
| 网易云接口 | **ncm-api-rs** | 见 §4.2 |
| 歌词解析 | **ttml_processor** | MIT，Apple Music / AMLL TTML 专用 |
| 歌词数据源 | **amll-ttml-db** | CC0-1.0，见 §5.2 |

### 3.1 基座候选的评审结论

调研了 Rust 生态现有的 wgpu 2D 渲染器，**三个候选基座全部否决**：

| 项目 | 否决原因 |
|---|---|
| vello | 2026 年大重构，原 compute 渲染器移入 `research/` 自称 experimental，新推的 `vello_gpu` 自称 "roughly beta quality"。**完全没有 blur / filter / layer 概念**——没有 layer 就没有背景模糊的立足点 |
| femtovg | 功能表最对口（圆角裁剪、gaussian blur、image filter、layer + layer mask、渐变全家族），但 Features 明确写着**不支持自定义 shader**。玻璃折射插不进去 |
| slint | ⚠️ **许可理由已作废**（我们是 GPLv3，见 §14.1）。**但技术理由独立成立**：它的渲染模型是自己的（自有 DSL + Renderer trait），而本设计需要的是对 pass 拓扑的完全控制——own-loop 捕获、共享 backdrop texture、damage tracking、RenderTier 分级（§9 / §17）。这些不是「换个后端」能拿到的，是渲染器本身的所有权问题。**结论不变，理由改为技术性。** |

**结论：`ui-render` 层自己写。** 整个 Rust GUI 生态里没有一个成熟的 backdrop blur 实现（iced 的 Gaussian Blur PR #1952 找不到已发布迹象；`iced_blur` 明确标注 prototype）。这不是推测，是查证过的结论。

自己写时**可以抄的**：

- **iced 的 `wgpu/src/quad.rs`** —— 自研渲染器该有的图元层形状：一个单位正方形反复画，`Quad` 结构体作为 per-instance attribute 直接 mmap 上传，**纯色 / 线性渐变 / 圆角 / 边框 / 阴影全在 fragment shader 里算**，用 `order` 字段在保持 Z 序的前提下减少 pipeline 切换
- **epaint 的两个技巧** —— 抗锯齿用 "feathering"（图元微外扩 + 一圈 alpha 渐变顶点）而非 MSAA，省掉 MSAA 的带宽和 resolve 开销；颜色全程预乘 alpha
- **lyon 大概率用不上** —— 它只做「路径 → 三角形」，不渲染、不带抗锯齿。UI 只要圆角矩形 + 图片 + 文本的话，圆角矩形用 SDF fragment shader 更快，也就是 iced 那条路

---

## 4. 音源层

### 4.1 形态：进程内 crate + 可选 sidecar

**音源层做成独立发布的 crate，不依赖任何 UI 类型。** 这个约束由编译器强制，比「我们约定分层」可靠得多。参考 `app_rhyme` 的做法（音源单独发 `music_api` crate）。

同一个 core 再包一层**可选的 axum server**（`--features server`）：

- 进程内直接用：类型安全、可测试
- server 形态：给未来移动端、远程控制、以及 cookie 同源问题留口子

### 4.2 网易云 provider

用 **`ncm-api-rs`**（SPlayer-Dev，WTFPL 许可，最宽松）。

- 300+ 接口，**完整实现 weapi / eapi / linuxapi 三种加密**，无需 Node 运行时
- 内存约 5MB（对比 Node 版 50–100MB）
- 双形态：库模式（`create_client()` + `client.cloudsearch(&query).await`）/ 服务模式（`--features server`）
- cookie 只需要 `MUSIC_U` 一个 key

值得偷师的三点：

1. 每个 API 一个独立文件放 `src/api/`
2. **`build.rs` 编译期扫描 `src/api/mod.rs` 自动生成路由注册**，不手写路由表
3. `Query` 统一传参，支持 `proxy` / `real_ip` / `random_cn_ip`

**风险：它只有 11 commits。** eapi 加密的正确性必须对照 `Cloud370/ncm-rs` 或 `ddy314/netease-music`（crates.io `netease-music`，行为对齐 NeteaseCloudMusicApi@4.32.0）交叉验证。

### 4.3 本地文件 provider

用 `lofty` 处理 ID3 / FLAC / Vorbis 标签与内嵌封面。

**它的价值不只是「多一个音源」：**

1. **只有第二个实现能证伪抽象。** 本地文件会立刻暴露在线模型里藏着的假设——每首歌都有 ID？都有封面 URL？时长一定准？一首歌只属于一个专辑？（合辑、CUE 分轨、无标签文件名）
2. **它是 UI 开发的稳定数据源。** 网易云接口会挂、会改、登录会过期，调玻璃折射效果时不能被登录态卡住。

### 4.4 登录与账号使用的边界（产品约束）

> **SPlayer 官方文档明确警告：网易更改登录策略后，使用第三方客户端登录账号可能导致封号。**

> ⚠️ **本节经 2026-09-26 更正。** 早先的规则是「只做**只读**路径，绝不做写操作」，
> 但那条**与本项目 v2 的计划自相矛盾**——v2 的 B 组包含 **歌单管理** 与 **收藏**，
> 而它们本身就是写操作。规则过宽，宽到把 v2 自己的功能框了进去。
>
> 用户给出的判据更精确：**风险是营收导向的，不是活动导向的。**

#### 判据（可执行）

> **这件事会让网易少赚钱吗？**
>
> - **不会** → 可以做
> - **会**（绕过 VIP、破解付费内容、伪造权益） → **不做**

| 操作 | 影响营收？ | 结论 |
|---|---|---|
| 搜索 / 播放 / 读歌单 / 读歌词 | 否 | ✅ |
| **收藏 / 编辑歌单 / 上报播放历史** | 否 | ✅（v2 需要） |
| 匿名取流（用于本地分析，见网格设计 §3.5） | 否 | ✅ |
| **绕过 VIP / 破解付费内容** | **是** | ❌ **真红线** |

#### 仍然明确不做的两项（理由已更换）

| 不做 | 原理由 | **现在的理由** |
|---|---|---|
| 每日签到 / 打卡 | 封号风险 | **v1–v3 的功能面根本用不到；且收益（几天 VIP）与代价（你的账号）完全不对等** |
| 云盘上传 | 封号风险 | **音源设计里没有一处依赖它；而要额外承担上传带宽与存储管理** |

> **「不需要 + 代价不对称」比「有封号风险」是更好的理由**——它不依赖于一个
> 无法验证的概率估计。而读云盘里**已有**的音乐是只读的，不受此限。

#### 仍然成立的两条

- **登录只用扫码 / UID**，不存储密码
- **不把账号凭证分享给网格里的其他设备**——网格的媒体中继（§网格文档 3.3）
  与匿名取流（§网格文档 3.5）都是为避免这件事而设计的

### 4.5 数据契约

参考 MusicFree 的设计（其实现路线不适用——Rust 内嵌 JS 引擎代价太高——但契约值得翻译）：

- 统一 item：`id / platform / artist / title / duration / album / artwork / source / lyric`
- 音质统一四档：`low / standard / high / super`，各平台原始标识在 provider 内映射
- 搜索用并发，单个 provider 异常不影响整体

#### ⚠️ 错误模型：不能把一切压成「空结果」

MusicFree 的契约里有一条「**provider 未实现的方法返回空结果而非抛错**」。

**这一条我们不要照搬。** 它会把下面这些完全不同的情况压成同一个 `[]`：

```
Unsupported / NotFound / EmptyResult / NotAuthenticated
RateLimited / NetworkFailure / ProviderChanged / CopyrightUnavailable
```

**结果是 `source-resolver` 根本无从判断该不该 fallback** —— 「这首歌确实没有」和「这个音源根本不会这个接口」在它眼里一模一样。

**正确契约：**

```rust
pub enum SourceError {
    Unsupported,        // provider 根本没这个能力
    NotFound,           // 有能力，但这个对象不存在
    AuthRequired,       // 需要登录态
    Unavailable,        // 有能力，但当前拿不到（版权/地区）
    RateLimited,
    Network(/* ... */),
    Protocol(/* ... */),// 上游接口变了
}

pub struct CapabilitySet { /* 声明式能力位 */ }

pub type SourceResult<T> = Result<Option<T>, SourceError>;
```

语义划分：

| 返回 | 含义 |
|---|---|
| `Ok(Some(v))` | 拿到了 |
| `Ok(None)` | **请求合法，但确实没有** |
| `Err(Unsupported)` | **provider 没这个能力** |
| `Err(Unavailable)` | **有能力，但当前拿不到** |
| `Err(Protocol)` | **上游变了** ← 这一条对网易云尤其重要（§13.1） |

**这对未来多音源特别重要**：只有区分出 `Unsupported`，resolver 才知道「换一个音源试」有没有意义；只有区分出 `Unavailable`，才知道该不该降音质重试。

**音源获取优先链**（四级）：

```
本地文件 → 缓存 → provider → 回退
```

---

## 5. 歌词

### 5.1 数据模型

直接采用 AMLL 的模型：

```rust
pub struct LyricLine {
    pub words: Vec<LyricWord>,
    pub translated_lyric: String,
    pub roman_lyric: String,
    pub is_bg: bool,      // 背景歌词行
    pub is_duet: bool,    // 对唱行，右对齐
    pub start_time: u64,  // 毫秒
    pub end_time: u64,
}

pub struct LyricWord {
    pub word: String,
    pub start_time: u64,
    pub end_time: u64,
}
```

注意：整行格式（如 LRC）整行作为一个 word 存；`start_time` 不总等于第一个 word 的开始时间。

### 5.2 数据源优先级

**产品策略：逐字质量优先。**

> ⚠️ **更正一处自相矛盾：** 本文档早期版本一边把 `amll-ttml-db` 写成「主源」，一边把回退链写成 `… → provider → amll-ttml-db`——**那实际是把 AMLL 放在最低优先级**。现明确为质量优先。

**`amll-ttml-db`**（CC0-1.0，424 star，9429 commits）是社区维护的逐字 TTML 歌词库，按来源平台分目录（`ncm-lyrics/` / `qq-lyrics/` / `am-lyrics/` / `spotify-lyrics/` / `raw-lyrics/`）。每首歌同时提供 `.ttml` / `.lrc` / `.yrc` / `.qrc` / `.lys` / `.eslrc` 六种格式。

**回退链（六级，逐字质量优先）：**

```
手动关联覆盖
    ↓
本地 TTML / YRC / LRC
    ↓
本地持久缓存
    ↓
amll-ttml-db（逐字 TTML）
    ↓
provider（网易云 yrc / lrc，§5.3）
    ↓
lrclib / 纯文本兜底
```

#### ⚠️ 不要直连 `raw.githubusercontent.com`

**正式产品运行时不能每次直接请求 GitHub raw endpoint**——那是没有 SLA 的第三方服务，且受网络环境影响。

正确形态是**三层**：

```
amll 索引 / 元数据（可自建镜像）
        ↓
本地持久缓存          ← 运行时主要命中这一层
        ↓
上游 fetch            ← 未命中才走
```

**只需要镜像 metadata / index，不需要镜像整个数据库。**

**注意：** amll-ttml-db 的 README 明确警告「Apple Music 自己的 TTML 分词和时间轴也不完全准确合理」，且质量保证只覆盖有效的网易云 ID。**不要盲目信任任何单一来源。**

### 5.3 网易云 YRC 格式

时间单位毫秒。结构：

```
{"t":0,"c":[{"tx":"作词: "}]}          ← 行首/行尾 JSON 元数据，放制作人信息
[行起始,行时长](字起始,字时长,未知)文本(字起始,字时长,未知)文本...
```

**必须兼容三种编码：**

1. **标准 YRC**：`[1000,700](1000,300,0)Hello(1300,400,0)world`
2. **行内嵌时间戳的伪逐字**：`[00:04.260]遠[00:04.940]い[00:05.150]夏`——行首是锚点，之后每个 `[mm:ss.SSS]` 标记后续字的起始时间
3. **A2 尖括号式**：`[00:04.260]<00:04.260>遠<00:04.940>い<00:05.150>夏`

**解析用状态机，不要用单条正则。** 第三个未知字段含义未公开，通常为 0，要能容忍负数。

**接口行为与坑：**

- `music.163.com/api/song/lyric?os=pc&id={id}&lv=-1&kv=-1&tv=-1`，`lv`=原文、`tv`=翻译、`kv`=状态；返回 `yrc.yrc`（逐字）、`tlyric.lyric`（翻译）、`romalrc.lyric`（罗马音）
- **很多第三方封装根本不读 `yrc` 字段**
- **有些歌只有 yrc 没有 lrc**，只读 lrc 会返回空
- 公开的搜索 / 歌曲 / 专辑 / 歌词接口**无需登录**；cookie 只影响私人数据
- **脏数据普遍**：异常时间戳（如 `00:00:123`）、无效行、空行都要过滤

Linn 的 `utils/lyric_parse.rs`（MIT）有容错解析实现和测试夹具，**可直接拿来当我们的回归测试集**。

### 5.4 逐字渲染算法

照搬 AMLL 的参数化设计（下面的参数全部来自它），但渲染后端用 **swash alpha 图集 + per-instance 高亮**，而非 CSS mask。

> ⚠️ **更正一处内部矛盾：** 本节曾写「渲染后端用 **SDF GPU 文字**」，而 §7.2 定的是 `parley → swash → SwashImage → glyph atlas`——**那是 CPU 光栅化的 alpha 图集，不是 SDF**。两者不是一件东西。现统一为图集路线。

**为什么不用 SDF（虽然 Rustle 用的是它）：**

| 考量 | 结论 |
|---|---|
| SDF 的核心价值 | 任意连续缩放范围、任意 outline、持续 scale 动画 |
| 歌词的实际需求 | **字号只有寥寥几档**（正文 + 强调），缩放动画幅度约 ±5–10% |
| 逐字高亮 | **与字形表示无关**——它是 per-instance 的颜色/进度，在 fragment shader 里做 |
| 与已定决策的一致性 | `parley + swash` 天然产出 alpha bitmap，图集路线**零额外工作**；SDF 需要额外的距离场生成步骤 |
| GLES 友好度 | 图集更好（§17） |

**升级路径：MSDF。** 若 M5 实测发现缩放动画质量不够，再换。**但不是 v1。**

逐字歌词需要的全部东西：

```rust
GlyphInstance {
    rect,                 // 字形在屏幕上的位置与尺寸（parley 给）
    uv,                   // 图集采样坐标
    color,                // 逐字高亮的当前颜色
    highlight_progress,   // 0.0–1.0，由时间轴驱动
}
```

**shape 一次，字形位图也不每帧变；每帧只更新 instance 里的 `color` / `highlight_progress`。**

**驱动方式（最容易踩的坑）：**

- **必须用 RAF 每帧 `update(delta)` 驱动**（delta = 距上一帧毫秒数）
- **绝不能用 `<audio>` 的 `timeupdate` 事件**——频率太低且不稳定
- `setCurrentTime(time, is_seek)` 只更新时间轴，本身不触发动画
- 所有时间单位是毫秒
- `pause()` / `resume()` 要能控制内部表现，包括逐字动画、辉光、间奏点动画

**逐字高亮：**

- 遮罩自己随时间推进，**不是每帧重算**
- 正常播放时只对齐刚开始播放的行的遮罩；**seek 时一次性对齐所有正在高亮的行**
- `wordFadeWidth`：渐变宽度 = 该值 × 行主字号。默认 **0.5（模仿 iPad）/ 1（模仿 Android）**，**永远不要设为正好 0**
- **辉光只给时长 ≥1 秒且 ≤7 个字的词**——这个门槛很关键，滥用会糊

**弹簧物理：**

- 三参数：`weight` / `tension` / `friction`，三个轴各自可调（`linePosX` / `linePosY` / `lineScale`）
- 正常播放时按索引顺序错开动画（cascading displacement），**垂直弹簧参数根据相邻行的间隔时间动态调整——行间隔越近，弹簧越硬**
- **seek 时所有行同时移动，用固定的、更慢的弹簧参数**，降低开销并让动画更干净
- 性能开销评级「高」；关掉会退化成 CSS transition，低配机器友好但视觉朴素

**seek 检测：**

- **向后跳进度永远当 seek 处理**
- **推送间隔 > 约 1.2 秒时，每次推送都被当成 seek**（自动 seek 检测要求连续密集的时间推送）
- 不传 seek 标志会导致卡顿、歌词飞出屏幕等 glitch

**滚动：** seek 时清除用户滚动偏移并恢复自动对齐（除非滚动来自触摸）。

**布局状态机（值得照搬的结构）：**

- `PlayerLayoutState`：对齐锚点/位置、间奏点大小、overscan（虚拟化）
- `PlayerScrollState`：用户滚动偏移、滚动边界
- `PlayerTimelineState`：currentTime、hot/buffered groups、isSeeking
- 虚拟化：只渲染可视区域内的歌词行

**性能预算锚点（AMLL 实测）：**

| 目标 | 需求 |
|---|---|
| 30 FPS | 主流近五年 CPU |
| 60 FPS | 约 3.0 GHz+ CPU |
| 144 FPS | 约 4.2 GHz+ CPU |
| GPU 1080p | GTX 10 系以上 |
| GPU 2160p | RTX 2070+ |

逐字歌词 + 弹簧 + 模糊真的吃性能。**seek 时必须降级。**

---

## 6. 音频管线

```
UI ──▶ dsls-audio 抽象 ──▶ 解码（symphonia）
                      └─▶ 输出（cpal）──▶ PCM graph（未来 DSP 挂载点）
```

**音频后端不绑定 UI 框架。** 播放器将来可能需要 exclusive mode、bit-perfect、device switching、sample-rate negotiation、gapless、crossfade、ReplayGain、DSP、EQ、visualizer——这些不该被 UI 框架的选择反向绑定。

v1 用 symphonia 解码 + cpal 输出。**Rust 生态里这是事实标准**（Rustle、summer-player、RustMusicPlayer 都用这套，rodio + symphonia + cpal）。

被否决的选项：

| 选项 | 否决原因 |
|---|---|
| GStreamer | 功能最全，但要装全套系统依赖（base/good/bad/ugly），跨平台打包痛苦 |
| libmpv | 解码能力最强但是 C 库 + FFI |
| kira | 偏游戏音频（tween、混音），做播放器方向不对 |

### 6.1 v1 必须预留的钩子（不做实现，只留形状）

v1 的出口标准只是「能出声」，但**下面两件事的成本在 v1 几乎为零，事后补则要动管线**：

1. **预加载** —— 播放当前曲时就把下一曲解码到 PCM 缓冲。这是 gapless 与交叉淡入的共同前提，也是「切歌不卡」的关键。v1 只需把 `dsls-audio` 的队列设计成「当前 + 预备」两槽，不做交叉淡入。
2. **PCM graph 的挂载点** —— 解码输出到 ring 之间留出一个 `&mut [i16]` 的处理链位置。v1 是空的（零开销），v2 的 EQ / 响度 / 变调都挂在这里。

### 6.2 v2：DSP 路线

> **参考实现：`Adreno5/SouthsideMusic`**（17★，Python/PySide6，Windows-only）。
>
> **License 是 PolyForm Noncommercial 1.0.0 —— 代码不可用，只能读思路。**
> 注意：**非商业许可与 GPLv3 不兼容**（比 GPL 更严格的限制无法被 GPLv3 吸收），
> 所以即使本项目是 GPLv3，它仍然用不了。这与「无 license」是两类不同的障碍，
> 结论相同。
> 它的音频引擎是自研的（numpy/scipy + PortAudio，不用现成引擎库），有参考价值。

**它值得学的地方：**

| 点 | 内容 |
|---|---|
| **WSOLA 是流式的，不是离线的** | `WsolaStretcher` 用 `readSamples(start, end)` 回调**拉取**样本，而非一次性喂整首歌。播放器场景必须如此——不能为了变速先把整首歌拉伸完。流式实现的关键是 overlap tail 的跨块管理，以及「起点跳变超过容差就重置」的 seek 判定 |
| **调过的参数** | `hop = max(256, rate/43)`（44.1k → 1025）、`search = min(hop/2, max(32, rate/125))`（44.1k → 352）、搜索窗口**中心偏置 0.12**（防漂到极值）、seek 容差 **16 样本** |
| **响度走标准而非自创** | 完整的 ITU-R BS.1770 / EBU R128：K-weighting 双二阶 → 均方 → gating → integrated loudness + LRA |
| **复杂度在编排不在 DSP** | `playing_manager.py`(95KB) > `audio_player.py`(79KB)。预加载、切换、过渡的编排才是大头 |
| **交叉淡入需要节拍检测** | 单独的 `beat.py`(18KB)——因为要做 tempo 匹配 |

**对应的 Rust 轮子（已核实存在）：**

| 能力 | crate | 版本 | 备注 |
|---|---|---|---|
| EBU R128 响度 | `ebur128` | 0.1.10 | **直接用，不要自己写**——它是标准实现 |
| 时间拉伸 + 变调 | `signalsmith-stretch` | 0.1.3 | 纯 Rust |
| 同上（备选） | `rubberband` | 0.1.5 | 音质标杆，但是 C++ 依赖 |
| 双二阶滤波器 | `biquad` | 0.6.0 | EQ 用 |
| 节拍检测 | `aubio-rs` | 0.2.0 | C 依赖 |
| PCM DSP 基础 | `dasp` | 0.11.0 | 采样格式转换 |

**实施顺序建议（按性价比排）：**

```
1. 预加载 + gapless          ← v1 已留钩子，v2 先做，收益最直接
2. 响度归一化（ebur128）      ← 标准实现现成，接进 PCM graph 即可
3. 静音尾部检测              ← 成本极低，配合 gapless 消除假间隙
4. EQ（biquad）
5. 变速保音高（signalsmith-stretch）
6. 交叉淡入 + 节拍匹配        ← 最复杂，依赖节拍检测与编排层重构
7. 变调、立体声展宽、混响
```

**一个方法论上的借鉴**：SouthsideMusic 为交叉淡入单独建了实验台（`crossfade_lab.py` / `crossfade_sweep.py` / `crossfade_probe.py` / `crossfade_gallery.py`）去**扫参数空间**，而不是拍脑袋定参数。DSP 参数（淡入时长、过渡曲线、增益斜坡）本来就该实测决定——这和我们在 M3 用性能原型代替猜测是同一种做法。

---

## 7. UI Runtime

### 7.1 分层

```
ui-core      树、事件、focus、状态、单向数据流
ui-layout    flex / stack / grid / absolute + 约束求解 + 测量缓存 + 变高虚拟列表
ui-text      shaping / bidi / CJK 断行 / 字形级定位
ui-motion    弹簧、时间线、驱动、可中断
ui-render    wgpu、pass 图、图元（quad / image / text / shadow / blur / gradient / glass）
ui-widgets   一套，不知道设计系统存在
```

`ui-text` 必须为逐字歌词保留**字形级位置 + 时间轴**的输出——这是歌词组件的基础，不能等做歌词时才发现文本引擎给不了。

### 7.2 文本引擎：parley 做布局，swash 做光栅化

**结论：parley 做布局层，glyphon 不用，swash 只当纯光栅化器。**

**parley 可以脱离 vello 单独使用**——最硬的证据是 Slint 的 issue #9559：Slint 明确计划把 **FemtoVG、Skia、Qt 三个渲染器全部重构到 parley 上**，而这三个渲染器没有一个是 vello。parley 在其中的职责只有 line breaking、fallback font discovery、ellipsis、selection/cursor 边界计算；每个渲染器只保留「加载字体 / 渲染字形 / 渲染选区 / 渲染光标」四件事。

parley 的四大依赖**没有任何渲染相关的东西**：

| 依赖 | 职责 |
|---|---|
| `fontique` | 字体枚举 + 字体回退（跨 Unicode 区段的 run → font 匹配） |
| `HarfRust` | HarfBuzz 的 Rust 移植，shaping |
| `skrifa` | TrueType/OpenType 解析 → 缩放后的带 hint 矢量轮廓 |
| `ICU4X` | locale、bidi、segmentation、emoji、NFC/NFD |

注意 **swash 已被 skrifa 取代**，不在 parley 依赖里（我们仍然用它做光栅化，见下）。

**parley 输出什么 / 不输出什么：**

- **输出**：每字形的 x/y/advance（**保留小数**）、每字形的 font + font_size + synthesis 标志、字形轮廓（矢量路径）、**cluster → 文本字节范围的映射**、selection/cursor 几何
- **不输出**：**不做任何光栅化**（不出位图、不出纹理）、不碰 GPU、不出 coverage mask

**为什么这一层必须自己写：** 这个项目里唯一不该自研的是**文本整形和断行**，而 parley 正好把这两件事包完了。剩下的「字形 → 像素」是渲染器的事，我们本来就在写渲染器。

#### 为什么不用 glyphon

**glyphon 是阻碍而非帮助。** 它的抽象是「把这个 `Buffer` 画出来」，而逐字歌词要求的逐字形颜色动画必须写进 `Buffer` 的 attrs——**改 attrs 会触发重新 shaping**。要么每帧 re-shape（贵），要么绕过 glyphon 直接用它的 atlas（那 glyphon 就只剩 atlas 管理这点价值）。

**glyphon 为静态文本而生，逐字动画的抽象层级不对。**

#### 逐字歌词的用法（shape 一次，每帧只重建 instance）

逐字歌词每帧变化的只是「每个字形的颜色/透明度/裁剪进度」，**几何完全不变**。parley 天然支持这个模式：

```rust
let mut builder = layout_cx.ranged_builder(&mut font_cx, text, scale,
                                           /* quantize */ false);  // ← 必须 false
builder.build_into(&mut layout, text);
layout.break_all_lines(None);
layout.align(None, Alignment::Start);

// 每帧只迭代，不重新 shape
for line in layout.lines() {
    for item in line.items() {
        if let PositionedLayoutItem::GlyphRun(run) = item {
            let mut x = run.offset();
            let y = run.baseline();
            for g in run.glyphs() {
                let gx = x + g.x;
                let gy = y - g.y;   // 注意 g.y 符号约定相反
                // → 塞进 instance buffer
                x += g.advance;
            }
        }
    }
}
```

`quantize = false` **保留小数位置是必须的**——否则亚像素抖动会让字形在动画中跳动。

cluster 映射用 `Cluster::text_range()` / `Cluster::glyphs()`，「第 N 到第 M 个字符高亮」可精确映射成屏幕 x 区间，做渐变扫过或裁剪直接可用。

#### CJK 断行的真实差异

| | 断行实现 | 能力 |
|---|---|---|
| parley | **ICU4X `icu_segmenter`** | 完整 UAX #14 + **locale tailoring**，支持 CSS `line-break: strict/normal/loose` 对应的 BCP47 扩展，覆盖泰语/高棉语/老挝语的词典式断行 |
| cosmic-text | `unicode-linebreak` | UAX #14 默认规则，约等于 CSS `line-break: normal` |

**纯中/日文歌词两者都够**——CJK 禁则由 UAX #14 的 CL/OP/NS 类别覆盖。但音乐库大概率有日韩多语言混排，日文歌词要 `line-break: strict` 时只有 parley 能 tailoring。**这个差异是真实价值。**

#### ⚠️ 但这个能力依赖一个默认关闭的 feature

**已核实**（parley 0.11.1 的 `Cargo.toml`）：

```toml
[features]
complex-scripts = []
default = ["system"]        # ← complex-scripts 不在里面
```

**`complex-scripts` 才是启用 CJK / 泰语 / 高棉语 / 老挝语 / 缅甸语词典式断行的开关**，不开则退回轻量 fallback。

**必须钉死：**

```toml
parley = { version = "0.11", features = ["complex-scripts"] }
```

**不写这一行，上面整段「locale tailoring 优势」的论证就是空的**——文档承诺的能力和实际构建产物不是同一个东西。

#### GLES 约束的交叉验证（好消息）

**文本方案与「blur 走 fragment」零冲突。**

字形上传走 `Queue::write_texture`——这是 **copy 类操作，在所有后端都可用，包括 GLES 3.0 / WebGL2**。会依赖 compute 的是「GPU 端字形光栅化」那条路（vello 的 compute 渲染器、pathfinder 风格），**我们不走那条路**。

反过来说：如果当初选了 vello 那种 compute 光栅化，GLES 后端就直接出局了。这是一个正向的交叉验证。

#### 光栅化器选择

1. **swash 当纯光栅化器（推荐起步）** —— `swash::FontRef::from_index(font_data, 0)` 直接从原始字体数据构造，`Scaler` 出 `SwashImage`，完全不关心布局。代价是字体被解析两遍（parley 用 skrifa 一遍、swash 一遍），实测影响很小
2. **skrifa 轮廓 + `zeno`** —— swash 同一作者的纯 Rust 2D 路径光栅化库，架构更干净（skrifa 已出带 hint 的轮廓直接喂进去，无重复解析）。更小众
3. **skrifa 轮廓 + 自己写 coverage 光栅化** —— 控制力最强，工作量最大

#### 五条实施要求

1. **`FontContext` / `LayoutContext` 跨帧存活**，不要每帧重建
2. **建 `TextLayoutCache`**：文本 + 样式 → parley `Layout`。只在文本/样式/换行宽度变化时重建
3. **每帧从 cache 生成字形 instance**：`glyph_id` + 位置 + 颜色/进度 → instance buffer。逐字歌词的「当前时间 → 哪些字已高亮」在这一步转成每 instance 的 `color` / `clip_progress`
4. **加一层中立类型做风险隔离** —— 在 parley 与渲染器之间放我们自己的 `LaidOutText` / `Line` / `GlyphInstance`。这样万一 parley 出问题，换 cosmic-text 只改适配层。**成本极低，必须加**
5. **CI 里验证 peniko 版本兼容** —— linebender 全家桶（parley / fontique / skrifa / peniko / vello）版本联动紧。parley 唯一的外部耦合是 `StyleProperty::Brush` 用 `peniko::Brush`（纯类型 crate，无 GPU 依赖，可接受）

**Plan B（如果 parley 出问题）：** cosmic-text 做布局（`Buffer::layout_runs()` 取逐字形 x/y）+ **自己写 atlas 和 pipeline**（跳过 glyphon），只损失 ICU4X 的 locale tailoring。这是第 4 条中立类型层值得加的原因。

### 7.3 图元层

参考 iced 的 `wgpu/src/quad.rs` 形态：instance 化的 quad pipeline，纯色 / 线性渐变 / 圆角 / 边框 / 阴影全部在 fragment shader 里算。

**抗锯齿用 feathering 而非 MSAA**（参考 epaint）：图元微外扩 + 一圈 alpha 渐变顶点，省掉 MSAA 的带宽和 resolve 开销。

**颜色全程预乘 alpha。**

---

## 8. 设计系统抽象

### 8.1 Glass 与 M3E 的差异不在样式层

原方案把设计系统抽象成纯样式 trait，这是错误的层级。Glass 和 M3E 有三个差异**不在样式层**：

```rust
pub trait DesignSystem {
    /// 样式：解析成不可变快照，不在每帧分配
    fn theme(&self) -> &ThemeSnapshot;

    /// ① 渲染 pass 差异：Glass 要背景，M3E 不要
    fn backdrop_request(&self, surface: SurfaceKind) -> Option<BackdropRequest>;

    /// ② 导航转场差异：M3E 的 container transform / Apple 的 zoom
    fn screen_transition(&self, from: ScreenId, to: ScreenId, kind: NavKind) -> TransitionPlan;

    /// ③ 取色种子来源：M3E 从封面提取，Glass 用固定色板
    fn color_source(&self) -> ColorSource;

    fn motion(&self, kind: MotionKind) -> MotionSpec;
}
```

**为什么 ① 必须存在：** 液态玻璃的定义性特征是采样并折射身后的东西。这不是 `SurfaceStyle { color, radius, blur }` 能表达的，是渲染 pass 拓扑的差异。

**为什么 ② 必须存在：** M3E 的 container transform（列表项→详情页的共享元素形变）和 Apple 的 zoom transition 都是**跨屏转场**，不是 widget 内部动画。把 `motion(kind)` 挂在 widget 上是错误的层级。

**为什么 ③ 从封面取色而不是壁纸：** M3E 的动态取色在 Android 上是系统壁纸服务（Monet），统一框架下要自己算。从**专辑封面**提取主色是更好的产品决策——音乐播放器天然每首歌都有封面，取色跟着歌走比跟着壁纸走更对味，还绕开了壁纸读取权限。

### 8.2 运行时热切换的前提

既然两套设计系统的 **pass 拓扑不同**，pipeline 从第一天就必须设计成**可重建的**，不能硬编码在初始化路径里。

### 8.3 v1 的范围：Glass 完整 + M3E 薄版

**Glass 做完整**（backdrop 采样、折射、高光、真实动效）。

**M3E 只做薄实现，但薄的地方要选对**：

- 不请求 backdrop
- 用 container transform 做导航转场
- 从专辑封面提取动态取色

这三个**恰好是上面 ①②③——抽象会出错的地方**。花很小代价就能把 `DesignSystem` trait 压弯一遍，避免抽象只被一个实现跑过。

其余 M3E 样式（tonal hierarchy、形状语言、完整 expressive 动效）先粗着写。

---

## 9. 渲染管线：玻璃

### 复刻基准：Apple 官方对 Liquid Glass 的定义

主源：WWDC25 session 219《Meet Liquid Glass》。

**官方定性：「digital meta-material」（数字超材料），明确不是任何真实材质的模拟。**

这句话是判断「像不像」的总纲——**不要往「物理正确的玻璃」方向过度投入**。它也直接支持了 §9.10 走启发式而非 Snell 物理折射的选择：目标是「看起来对」，不是「算得对」。

#### 四个视觉层（基准清单）

**1. Lensing（透镜）** —— 实时折射并**聚焦**背景光，区别于旧材质（`UIBlurEffect` 那套）的**散射**。

- 折射强度**沿边缘响应式变化**，这是 Apple 让控件「极度通透但仍能与背景区分」的手段
- **淡入淡出不是 alpha 渐变，而是折射强度的渐变**（Apple 叫 "materialize in/out"）
- **对我们的含义：`opacity` 这个参数在液态玻璃里语义是错的。** 正确的参数是「折射强度」和「模糊量」的插值

**2. Highlights（高光）** —— 材质处在一个类物理环境里，环境中的光照在材质上产生高光。

- **响应几何**：系统动效（如锁屏/解锁）时「光会绕过材质」勾出轮廓
- 部分情况下**响应设备运动**，让它感觉「知道自己在真实世界里的位置」

**3. Shadow / grounding（接地阴影）** —— 材质**感知背后内容**。

- 背后是文字 → 阴影加深；背后是浅色实底 → 阴影减弱
- 大元件（sidebar）受影响更明显：附近鲜艳内容会**微微溢到表面**，光反射、散射并渗进阴影

**4. Interaction illumination（交互内发光）** —— 指尖按下时材质**从内部发光**，并**蔓延到相邻的 Liquid Glass 元件**（不是单个控件的局部效果）。

#### 硬约束（API 设计约束，不只是视觉）

| 约束 | 内容 |
|---|---|
| **两变体不可混用** | **Regular**（全效果、自适应、任意尺寸任意内容）/ **Clear**（永久更透明、**无自适应行为**、**必须配 35% 黑色 dimming 层**才能保证可读性，仅限媒体丰富内容的小 footprint） |
| **glass cannot sample glass** | 相邻/重叠玻璃必须放进同一个 `GlassEffectContainer` 合并采样，否则折射不正确。**对我们的 backdrop 设计是直接约束，见 §9.12** |
| **明暗自适应有尺寸规则** | 玻璃根据背景亮度自动切明暗风格，但 **Apple 只翻转小元件，绝不翻转大元件** |
| **Scroll edge effect** | 内容滚到玻璃下方时**溶解**进背景；若深色内容触发玻璃切到 dark 风格，则改为轻微 dimming；固定配件视图用 "hard style" 统一效果 |
| **失焦后退** | 窗口失焦时（Mac/iPad）玻璃切换外观并**视觉上后退** |
| **无公开数值参数** | SwiftUI **不暴露任何数值参数**（模糊/饱和度由系统决定），只有 `.regular` / `.clear` / `.identity` 三档 |

**SwiftUI API 面**（供理解语义，我们不复刻 API）：`glassEffect(_:in:)`、`glassEffectID(_:in:)`、`GlassEffectContainer`、`.interactive()`、`.buttonStyle(.glass)`、`.containerConcentric`（配 `ConcentricRectangle`，**圆角同心而非硬编码半径**）。注意 `glassEffect()` 必须放在 modifier 链**最后**。

#### 开源实现的覆盖度：Kyant0 是唯一覆盖到第 3 层的

> **更正：** 早先版本的本文档写过「开源实现全都只做了第 1 层和第 2 层的一部分，第 3、4 层是空白」。**这句是错的**，经直接核查源码后更正如下。

| Apple 的四层 | `Kyant0/AndroidLiquidGlass` | 证据 |
|---|---|---|
| 1. Lensing | ✅ | `effects/Lens.kt` + `RoundedRectRefractionShaderString` / `...WithDispersionShaderString`（七通道色散） |
| 2. Highlights | ✅ | `highlight/` 完整模块：`Highlight(width, blurRadius, alpha, style)`，三种样式 `Default` / `Ambient` / `Plain`；shader 有 `angle` / `falloff` uniform |
| 3. Shadow / grounding | ✅ | `shadow/` 模块：`Shadow(radius, offset, color, alpha, blendMode)` + `InnerShadow` |
| 4. Interaction illumination | ❌ | 未见（这层需要触摸输入集成，库层面做不了） |

另外 **`CombinedBackdrop`（`rememberCombinedBackdrop` 系列）就是 Apple `GlassEffectContainer` 的对应物**——即「glass cannot sample glass」的现成解法。

**结论：Kyant0 是开源里唯一覆盖到第 3 层的实现，这就是它还原度最高的原因。** 一般性的「开源只做对第 1 层」结论对 Web/JS 那批成立，对 Kyant0 不成立。

**对我们的影响：移植范围要相应扩大**，见 §9.10。

#### 仍然是空白的地方

1. **背景明暗自适应** —— 且必须遵守「只翻转小元件」的规则
2. **glass-cannot-sample-glass 的完整合成语义** —— Kyant0 有 `CombinedBackdrop` 这个机制，但「什么情况下必须合并」的判定规则仍需自己定
3. **shadow / grounding 的「感知背后内容」** —— Kyant0 的 `Shadow` 是固定参数投影，不是随背后内容变化的那种
4. **触摸内发光及其向相邻元件的蔓延**

第 3、4 项是真正需要自己设计的部分。**v1 目标：做扎实第 1、2 层，移植第 3 层的基础形态，第 4 层列入 v2。**

### 9.1 own-loop：唯一干净的捕获方案

**整个 UI 场景渲染到一张 offscreen intermediate texture**（`usage = RENDER_ATTACHMENT | TEXTURE_BINDING | COPY_SRC`），然后模糊其中若干区域，最后合成到 display target。

**因为 scene texture 本来就在我们手里，capture copy cost ≈ 0。**

> ⚠️ **更正：** 本节曾写「捕获这一步的成本是零」。**那只对 copy 成立，对 compositing 不成立。**
>
> ```
> capture copy cost      ≈ 0     （不需要额外拷贝）
> offscreen compositing  ≠ 0     （绝对不是零）
> ```
>
> **本质是主动拿 bandwidth 换一个可采样的 backdrop。** 在 tile-based GPU（Arm 移动端、Apple TBDR）上尤其贵——post-processing 迫使 tile 内容落主存再读回。完整论证与 M0.5 的必测项见 **§17.8**。

另一条路线是从 swapchain 用 `COPY_SRC` 拷区域到 offscreen（`backdrop_capture` 式），**只在我们不得不直接渲染到 swapchain 时才用**。

**根本约束：绝对不能采样正在渲染的 surface texture。** surface 通常只配 `RENDER_ATTACHMENT`，而采样需要 `TEXTURE_BINDING`，backdrop blur 还需要 `COPY_SRC`。这条决定整个 pass 拓扑——所以 own-loop 是唯一干净解。

### 9.2 模糊：Dual Kawase

**算法出处：** Marius Bjørge（ARM Trondheim），SIGGRAPH 2015 "Bandwidth-Efficient Rendering"。是 Masaki Kawase 2003 GDC 单 filter 版本的改进：把「降采样链」和「模糊」合并——一边缩小一边模糊，再一边放大一边模糊。

**权重可直接抄：**

降采样（5 taps，目标像素覆盖 2×2 源区域）：

```
sum = 4 × center
    + 1 × (uv + (-hw, -hw))
    + 1 × (uv + ( hw, -hw))
    + 1 × (uv + (-hw,  hw))
    + 1 × (uv + ( hw,  hw));
sum /= 8;
```

升采样（8 taps，3×3 tent kernel）：

```
sum = 4 个对角点 × 1/12  +  4 个轴向点 × 1/6;
// 等价写法：(对角×1 + 轴向×2) / 12
```

**关键细节：**

- `hw` = half-pixel = `0.5 / textureSize`。**half-pixel 偏移让硬件 bilinear 免费混合 2–4 个纹素**——这是带宽效率的全部来源
- 每级分辨率减半 → 像素数减 4× → 往下走成本指数衰减
- 整条链是**带宽受限而非 ALU 受限**

### 9.3 Backdrop-Root rule

**被模糊的面板不能把自己的背景/填充画进 blur source**，否则模糊出来的是面板自己的填充色而不是背后的内容。

实现：scene texture 里那层玻璃面板**不画底**，只画它背后的东西；**玻璃的 tint 在合成阶段才叠上去**。

### 9.4 fragment 而不是 compute

**结论：blur 金字塔用 fullscreen fragment pass**（一个全屏三角形，viewport 设为目标 mip 尺寸，UV 自然落在 bilinear 采样点上）。

四条理由：

1. **Arm GPU（移动端 / iOS / Android）没有为 compute 实现专用片上 shared memory**，那块内存是系统 RAM，走 load-store cache。shared memory 优化的收益在移动端基本消失
2. **wgpu 的 WebGL2 / GLES 后端根本没有 compute shader** ← 对我们是决定性的。我们要 GLES 后端来兜底 Android
3. ThorVG 的 WebGPU 引擎明确把 Gaussian blur 和 drop shadow 从 compute pipeline 迁回了 fragment/render pipeline
4. wgpu 里 compute 走 storage texture 需要检查 `rgba8unorm` storage 支持并写 fallback 分支；fragment 没这个坑

compute 留给 fragment 做不了的事（前缀和、GPU culling）。

### 9.5 降采样链参数

- **层数**：`levels = floor(log2(min(w, h)))`，实践取 4–6 级。Hyprland 的经验是 **3–4 passes + 较大 radius 就够**，pass 越多越耗电（笔记本上是电池问题）
- **必须 clamp passes 到 1–8**：Hyprland 的 `decoration:blur:passes` 没做校验，≥63 会导致**有符号位移溢出和除零 UB**
- **半径 vs passes**：模糊量 ≈ `size × passes`。单次 offset 太大（`size > 5`）会出现块状 artifact，**加大 passes 比加大 size 更安全**。`1px` 以下的 sample 完全无意义
- **vibrancy / saturation 在降采样 pass 里逐级施加，不是最后统一施加**（Hyprland 的非显然技巧）。让饱和色逐 pass 扩散生长，避免 banding；每级除以 `1.0 / passes`。参考实现：Hyprland `src/render/shaders/glsl/blur1.glsl`

### 9.6 格式与生命周期

> ⚠️ **本节曾把 `Rgba16Float` 写成硬要求。已按 §17 的能力分级改写——硬要求会排除 GLES 低端设备，而那正是我们选 wgpu 的理由。**

- 中间 target 的**首选**是 **`Rgba16Float`（线性空间）**，**不要用 `-Srgb` 格式**（`backdrop-blur-wgpu` 源码注释："sRGB targets are rejected for the gamma intermediate (they would double-decode)"；8-bit 会在大面积渐变上 banding）
- **但不是硬要求**：adapter 不支持时按 §17.3 降到 Tier B（`Rgba8Unorm` + dithering）或 Tier C。**检查点见 §17.4——不只看格式可用性，还要看 `BLENDABLE`**
- **格式必须进 backdrop cache key** —— 不同 tier 的 scratch 不能混用（§17.5）
- 合成用的 sampler：**ClampToEdge + linear**

### 9.7 backdrop cache 与 damage tracking

**这是最大的性能杠杆。** 参考 Hyprland：壁纸静态 + 窗口平铺时模糊结果预计算掉，每帧零成本；只有动态壁纸 / 浮动窗口才逐帧算。

#### 捕获时机：per-surface 的类型化义务，不是全局开关

**这是最容易做错的一处。** 绝不能只给一个「每帧都捕获」的布尔值——那样会在静止内容上白耗电，在动画内容上出现陈旧模糊。

正确做法是让每个 surface 声明自己的刷新意图：

```rust
pub enum RepaintPolicy {
    Static,             // 默认。捕获一次，永不主动刷新
    Live,               // 每帧刷新
    Bounded(Duration),  // 按已知节奏定期刷新
}
```

**一个极易搞错的语义：**

> `Static` 意味着适配层**不为这个 surface 驱动重绘**——**不是**意味着停止合成。宿主因任何其他原因重绘时（输入、别处动画、另一个 surface 的 `Live`），这一帧仍会重新捕获并重新合成。
>
> **是「不请求帧」，不是「不绘制」。**

映射到我们的场景：

| 场景 | 策略 |
|---|---|
| 对话框、工具提示、静态背景上的玻璃面板 | `Static`（最省电） |
| **玻璃覆盖在滚动内容 / 专辑封面动画之上** | **必须 `Live`**，否则模糊陈旧 |
| 已知节奏的动画（如逐字歌词推进） | `Bounded` |

**为什么必须是类型化义务而不是全局开关：** 模糊背景在背后内容变化时是否还保持正确，**不能跨工具包泛化**——不同宿主的重绘语义完全不同（有的默认响应式，背后内容变了不重绘，也没有区域失效机制，于是「只要宿主重绘就是新鲜的」可能是**零帧**）。所以必须由领域类型声明意图，不能假设宿主会重绘。

#### 缓存 key：`levels` 的 log2 量化就是复用机制

```rust
pub struct PingPongKey {
    pub size: [u32; 2],   // chain 的 level-0 全尺寸，物理像素
    pub levels: u32,      // 1 = 可分离高斯；N = N 级 dual-Kawase 金字塔
}

pub const KAWASE_THRESHOLD_PX: f32 = 16.0;
pub const MAX_KAWASE_LEVELS: u32 = 6;      // 1/64 降采样
pub const MAX_GAUSSIAN_RADIUS: i32 = 64;

pub fn use_dual_kawase(r: f32) -> bool { r >= KAWASE_THRESHOLD_PX }

pub fn resolve_kawase_levels(r: f32) -> u32 {
    let levels = r.max(2.0).log2().round() as i32;
    levels.clamp(1, MAX_KAWASE_LEVELS as i32) as u32
}

pub fn resolve_gaussian(r: f32) -> GaussianKernel {
    let tap_radius = (r.round() as i32).clamp(0, MAX_GAUSSIAN_RADIUS);
    let sigma = (r / 3.0).max(0.5);        // 3σ = 视觉半径
    GaussianKernel { sigma, tap_radius }
}
```

**关键洞察：每 pass 的采样扩散固定为一个 half-texel，所以半径粒度是 log2 量化的。** 一个区间内所有半径（如 16–22 → `N=4`，23–45 → `N=5`）**模糊结果完全相同**。

这正是缓存能高效复用的原因——UI 里不同玻璃面板的 radius 只要落在同一区间，就命中同一个 cache entry。**这不是近似，是精确相等。**

两条阈值规则：`r >= 16px` 才启用 dual Kawase（以下是可分离高斯更划算），`levels` 上限 6 级（1/64 降采样）。

#### ⚠️ 缓存省的是「纹理分配」，不是「模糊计算」

**这一点极易误解，必须写清楚：**

```rust
fn ensure_scratch(&mut self, device: &wgpu::Device, key: PingPongKey) {
    if let Some(chain) = self.scratch.get_mut(&key) {
        chain.last_used_frame = self.frame;
        return;                                    // ← 命中：只更新时间戳，不分配
    }
    // ... 未命中才分配两条新 view
}

struct ScratchChain { views: [wgpu::TextureView; 2], last_used_frame: u64 }  // 高斯乒乓
struct PyramidChain { views: Vec<wgpu::TextureView>, last_used_frame: u64 }  // Kawase，N+1 级
```

结构体**只存 views**（`wgpu::TextureView` 通过引用计数保持父纹理存活）。

**所以：pass 每次都要重跑。缓存省的是纹理分配，不是卷积计算。** 背景内容变了就必须重新卷积——**没有任何「背景没变就跳过 blur」的优化**。

**要真正的「跳过重算」，那正是 §9.7 上半部分 damage tracking 要解决的事，而且是本项目要自己设计、`backdrop-blur-core` 没有提供的部分。**

#### 两个独立的 HashMap（照做）

```rust
scratch:  HashMap<PingPongKey, ScratchChain>,    // 高斯
pyramids: HashMap<PingPongKey, PyramidChain>,    // dual-Kawase
```

**两种算法分开存，键别名在结构上不可能。** glow 后端把两者塞进同一个缓存，才需要靠「选 Kawase 的必然 levels ≥ 2」这种单调性契约来保证不相交。**我们照 wgpu 这条做，就不需要操心那个契约。**

#### 失效策略：纯帧龄，无内存上限

**没有任何内存预算、没有 max_bytes、没有按大小的 LRU。** 唯一的失效机制是帧龄：

```rust
pub const RETENTION_FRAMES: u64 = 8;

pub fn evict_decision<K: Copy>(
    chains: impl IntoIterator<Item = (K, u64)>, now: u64, retention: u64,
) -> Vec<K> {
    chains.into_iter()
        .filter(|&(_, last_used)| now.wrapping_sub(last_used) >= retention)
        .map(|(key, _)| key).collect()
}
```

`RETENTION_FRAMES = 8` 是为 resize 定的：一个不再霜化的 surface 不应长期占内存，**但一条链必须挺过几帧抖动**（拖拽时每帧都在改尺寸）而不 thrashing。

**触发点的顺序很讲究：**

```rust
if request.target_rect.is_empty() { return Ok(None); }   // 先做 no-op 守卫
self.begin_frame();                                      // 再推进驱逐时钟
```

**这个设计很精妙：驱逐时钟只在「真的发生了霜化」的帧上走。**

守卫在两个地方，**都在 `begin_frame()` 之前**：`target_rect.is_empty()`（`size[0] == 0 || size[1] == 0`）以及源侧的 `clip_to` 返回 `None`（零面积/全出界）。

**推论：「不可见」不推进时钟，「未使用」才推进。** 一个滚出屏幕的玻璃面板不再产生「霜化帧」，所以它的链存活，重新出现时直接命中。这解决了「面板来回滚动的 thrashing」。

#### ⚠️ 但这个精妙有个重要的限定：计数器统计的是「霜化次数」，不是「渲染帧数」

源码注释写的是 "Advanced once per `prepare`"，而 **`prepare` 是每 surface 一次**。

**所以 `frame` 计数的是霜化操作次数。** 一帧内有 S 个玻璃表面时，计数器每个渲染帧推进 S，于是 `RETENTION_FRAMES = 8` 兑现成渲染帧是 **8 / S 帧**：

| 同屏玻璃数 | 8 次霜化 ≈ |
|---|---|
| 1 | 8 帧 |
| 3 | **≈ 2.7 帧** |
| 8 | **1 帧** |

**后果：「不可见 ≠ 未使用」这条保证，只在该 surface 是唯一霜化者时成立。** 面板 A 滚出屏幕、而面板 B 仍可见并霜化时，**B 的 `prepare` 照样推进共享计数器**，A 的链 8 次霜化后被驱逐。

**含义：同屏多个玻璃元件时（tab bar + 侧边栏 + 浮动按钮在播放页很常见），实际保留窗口按元件数成比例缩短，拖拽 churn 比字面严重得多。**

**我们的修正：** 二选一——按元件数放大 `RETENTION_FRAMES`，或者**给每个 surface 独立计数器**。后者更正确，且与 §9.7 上半部分要自己做的脏区追踪天然契合（每个 surface 已经需要登记自己的依赖集）。

#### 另一个计数器：`generation`

`frame` 驱动驱逐；`generation` 每次 `prepare` 递增并写进 `WgpuPrepared`，`record` 里断言它。它防的是「在 record 第一个 surface 前 prepare 了第二个，导致共享 scratch 被覆盖」——**如果我们照搬共享 scratch 池，这个防护必须一并照搬。**

#### resize：真实行为与一个高价值改进

`size` 是 level-0 全尺寸，**所以窗口 resize 时 key 必变**。真实行为：

- 每出现一个新的 (size, levels) 组合就插入一条新 chain
- **拖动窗口时尺寸每帧都变 → 每帧分配一条新链**
- 旧尺寸的链在 8 帧后老化驱逐

代价（`Rgba16Float`）：400×200 区域的单张图约 **640 KB**，拖拽期间峰值约 8 条链——**约 5–10 MB 的分配抖动**。

**上游 v1 没有任何缓解措施。我们的改进：把 texture 分配尺寸量化到 16px 或 32px 网格。** 这样拖动时只有跨越网格边界才新建链，其余帧全部命中。

**量化 `key.size`，不是量化请求区域。** 而且 UV 的问题**已经被现成机制解决了**——这正是它「实现极简」的原因：

```rust
pub fn backdrop_uv_remap(source_region: &Region, clipped: &Region) -> ([f32; 2], [f32; 2]) {
    let [sx, sy] = [source_region.origin[0] as f32, source_region.origin[1] as f32];
    let [sw, sh] = [source_region.size[0] as f32, source_region.size[1] as f32];
    let [cx, cy] = [clipped.origin[0] as f32, clipped.origin[1] as f32];
    let [cw, ch] = [clipped.size[0] as f32, clipped.size[1] as f32];
    ([(sx - cx) / cw, (sy - cy) / ch], [sw / cw, sh / ch])
}
```

这个函数**是纯几何的、接受两个任意 `Region`**，而且**今天已经在处理「scratch 尺寸 ≠ 请求区域」的情况**——用在**部分离屏**场景：`clip_to` 把部分出界的区域裁到界内子矩形，scratch 只装界内那部分，于是 `clipped ⊂ source_region`，`scale > 1`。

**量化后是同一种问题，只是方向相反**：scratch 比区域**大**，`scale < 1`。

> **同一个函数、同样的数学、零新增代码。** 只需把配对的第二个 `Region` 从「裁剪后的」换成「网格对齐的」。

完整实现：

```rust
// 今天
let clipped = request.source_region.clip_to(source_extent)?;   // 界内子矩形
let key = PingPongKey { size: clipped.size, levels };
let (off, scale) = backdrop_uv_remap(&request.source_region, &clipped);

// 量化后 —— 只多一个 Region，函数调用参数换一下
let clipped = request.source_region.clip_to(source_extent)?;
let quantized = clipped
    .quantize_up(GRID)                    // 同 origin，尺寸向上取整到网格
    .clip_to(source_extent)               // 关键：取整后可能越界，必须再裁一次
    .unwrap_or(clipped);
let key = PingPongKey { size: quantized.size, levels };       // ← 键稳定，可复用
let (off, scale) = backdrop_uv_remap(&clipped, &quantized);   // ← 同一个函数，参数换成 (实际区域, 分配区域)
```

**三个必须一并处理的点：**

1. **取整后要再 `clip_to(source_extent)` 一次** —— 右下角可能超出源纹理，否则采样越界。（上面代码已含。）
2. **pass 要 scissor 到实际使用区。** 纹理变大但真正处理的只有左上角子矩形；第一条 pass 本来就是「maps the scratch onto the source sub-rect」，scissor 是自然的。**不加则白付 fill rate。** Kawase 的 prefilter 同理。
3. **内存上界**（最坏比值 `⌈w/G⌉·G/w × ⌈h/G⌉·G/h`）：

| 区域 | G=16 | G=32 |
|---|---|---|
| 400×200 | 416×208 → **1.08×** | 416×224 → **1.17×** |
| 100×50 | 112×64 → **1.43×** | 128×64 → **1.64×** |

**小元件受网格影响大得多 —— 取 G=16 更稳；更激进可以只量化长边。**

**这个改进直接写进 `ui-render` 的设计。**

#### Static 策略不检测「背后内容没变」——它不检测

**最重要的一条结论：没有任何机制。`Static` 是契约，不是检测器。**

core 层**完全不做 backdrop 的脏检测**，把新鲜度责任整个推给适配层。`Static` 的字面含义只是「我们不主动请求帧，只在宿主因别的原因重绘时顺带重捕」。

它对跨工具包的诚实警告值得原样记住：

> 模糊背景在背后内容变化时是否还保持正确，**并不能跨工具包泛化**（egui 默认响应式：背后内容变了它不重绘，也没有区域失效机制——所以「只要宿主重绘就是新鲜的」可能是**零帧**）。

**对我们的含义：要真正的正确性，必须自己做脏区追踪。** 具体做法——**监听场景图里位于玻璃下方的节点是否发生变更或动画**，据此在 `Static` / `Live` / `Bounded` 之间切换。

> **这是 `backdrop-blur-core` 明确留下的空缺，不是我们没找到。** 好消息是它正好和上面 damage tracking 的方案合流：**damage rect 的来源就是「玻璃下方的节点变更集」，两者是同一套机制。**

#### 其余机制

- **per-target-format composite cache**（每个输出格式一套 pipeline，懒创建）
- **bind group 和 pool lease 必须持有到合成结束**，否则 pooled texture 会在被采样期间回收——这是很容易踩的 use-after-free 类 bug

**damage tracking（Rust 生态里没人做过，需要自己设计）：**

1. 每个模糊区域登记它依赖的「底层内容矩形集合」（这块玻璃背后到底压着什么）
2. 场景更新时，图元层产出 damage rect（脏矩形）
3. damage rect 与某个 backdrop 区域的依赖集求交——**交集非空才重算那块模糊**
4. 重算时只重新模糊「damage ∩ 区域」这个子矩形，然后 blit 回 cache

**子矩形必须向外扩**：先扩 `blur_radius` 像素，再按 mip 级别扩（最粗一级扩 `radius / 2^levels`），否则边缘会采到未更新的旧数据。

### 9.8 不能做的优化

**不要动画化 blur 半径。** Chromium 的实测行为是「动画中经过的每一个 blur 半径都编译一个新 shader」，这是 backdrop-filter 动画卡顿的主因（CSSWG issue #13997 就是为此提的提案）。

要做动画就动画 **tint / presence / 模糊结果的淡入**，不要动 radius。

### 9.9 相对 Web 阵营的结构性优势

CSS 做折射必须「在面板里放一份对齐的背景副本再对它做 filter」（因为 CSS filter 作用于元素而非背景），**成本翻倍且只对固定背景有效**。

我们有完整 scene texture，**折射是免费的**。这是自研这条路最值钱的地方。

### 9.10 折射算法

**算法源：`Kyant0/AndroidLiquidGlass`**（3922 star，Kotlin，Apache-2.0，有正式 release）。这是整个领域的公共祖先，后面多个实现都是它的变体。

三个关键点：

1. **SDF 梯度当做法线**，再混入径向法线（`depthEffect`）——解决圆角处法线不连续
2. **`circleMap` 做深度斜坡**，让位移在边缘处平滑归零
3. **色散是廉价近似**：不打三遍全屏，而是按 **R/O/Y/G/C/B/P 七通道**在 `±spread` 上采样求加权和，色散强度随 `centered.x * centered.y / (halfW * halfH)` 变化（四角最强）

关键参数：`refraction_height` / `refraction_amount` / `depth_effect` / `chromatic_aberration` / `highlight` / `edge_width` / `tint`。

#### 必须做的一步通用化

Kyant0 的原始偏移公式**只对圆形成立**：

```glsl
vec2 offset = mix(vec2(0), normalize(st) / sd, length(st));
```

对圆形，`normalize(st)` 恰好等于 `∇sd`，`length(st)` 恰好等于 `sd + R`。**但对圆角矩形，角的偏差可达 45°**；方形角上方向与法线分歧最大 45°，而且硬编码的 `sdCircle` 高光还是圆的。

必须做四处替换，把它通用化到任意形状（超椭圆、blob、morph blend、拉伸实例）：

```glsl
vec2  n  = normalize(∇f);                        // 原为 normalize(st)
float sd = f / |∇f|;                             // 任意场 → 度量距离
float w  = clamp(sd + inradius, 0, inradius);    // 原为 length(st)
vec2 offset = n * (w / sd);                      // 圆形下与原始公式恒等
```

- **法线**：4-tap 中心差分同时给出精确法线和梯度模长 `|∇f|`，**零额外成本**
- **深度/厚度**：除以 `|∇f|` 把任意场统一成度量距离，折射强度无需逐形状调参
- **厚度 ramp**：`clamp(sd + r, 0, r)`，`r` = 最大内切圆半径
- **形状用距离场而非二值 mask**——「距边缘多远」是连续量，折射和抗锯齿都要它

#### 三个真实踩过的坑

1. **`1/sd` 在边缘梯度无界 → 色差不能用比例分配。** 基础 offset 在边缘像 `1/sd` 爆炸，按百分比拆 RGB 会糊成彩虹。**必须解析推导屏幕空间 footprint（`amp·re·px/sd²`），再显式选 mip 级别（`textureLod`）。** 动态控制流下隐式导数是坏的，会强制 `textureLod`。对无界梯度 point sample，无论怎么超采样都有彩色噪点。

2. **中轴线接缝。** 最近边界点切换处（方形对角线、任意形状中轴线、多边形角）精确法线不连续翻转，一个像素宽的差分就渲染出硬线。**解法：stencil 宽度随形状缩放而非固定像素（默认取内切半径的 5%）**，把硬线变成等宽平滑旋转——轮廓在 `f == 0` 处，加宽 stencil 不损精度。这同时消除 SDF 贴图的双线性刻面。方形对角线上的接缝物理上真实，圆角会把中轴线内推从而消除。

3. **offset 在 aspect-scaled 空间算、却应用到裸 UV**，会被宽高比拉伸。mipmapped 边缘采样必须 `textureLod`。

#### 高光与 Fresnel

- **Fresnel / 边缘高光**：`dot(n, lightDir)` 调制贴边高光带 + 约 2px 镜面唇边。通用化后不能用硬编码的 `sdCircle` 高光，要用边界带 + 法线夹角
- **边缘反射带**：约 `0.35 · minDim`，平方 falloff，模拟玻璃胶囊上下端的倒影回声（iOS 26 的观感）
- **`axisFade`**：`smoothstep(0.15, 0.8, conf)`，作用是在 SDF 梯度塌缩的中轴线处消除接缝
- **lens profile**：`bend = rim * rim * axisFade`，**平方 falloff**——折射几乎只发生在边缘环带

#### Apple 的实现要点（作为「像不像」的基准）

- Liquid Glass **偏好凸形 profile**（Switch 组件除外）——凹面会把光线推到玻璃外部，需要采样物体之外的背景
- 折射遵循 **Snell–Descartes** `n1·sin(θ1) = n2·sin(θ2)`，玻璃 n2 = 1.5
- 因旋转对称，位移量只需在一条 2D 半切片 (x,z) 上按「距边界距离」预计算一次，再绕 z 轴旋转
- **一个半径需要 127 次光线模拟**（受 SVG displacement map 分辨率限制）
- 若预烘焙 displacement map：`RG = 128 + Normal × 127`，B 固定 0，A 固定 255（零向量 = RGBA(127,127,0,255)）；所有向量除以最大位移模长归一化，另存 `maximumDisplacement` 还原

#### 路线选择：启发式 SDF 赢过物理折射

有两条实现路线，调研结论是**走启发式**：

| | argui / Kyant（启发式） | liquid-glass-studio（物理） |
|---|---|---|
| 折射模型 | `circleMap` 深度斜坡 + SDF 梯度法线 | Snell–Descartes 真实折射 |
| 色散 | 7 通道加权采样，**只碰 1 张纹理** | RGB 采样，**6 次跨 2 张纹理**（3 次清晰 + 3 次模糊，再按 `blurMixRate` 混合） |
| 默认采样成本 | **1 次**（默认 `chromatic_aberration: 0.0`，要色散才 7 次） | **恒定 6 次 + 双纹理绑定** |
| 额外数学 | 几次 MAD | `asin`/`sin`/`tan`/`pow` + **LCH 色彩空间往返** |

**结论：argui 严格更便宜。** 移动端上这是决定性的——不想要色散就一次采样，而 studio 无论如何都要绑两张纹理跑 6 次采样。

**瓶颈不在 Snell 数学，在 LCH 色彩空间往返。** `asin`/`sin`/`tan` 在现代 GPU 上各约 1 个 SFU 周期，可以忽略；真正贵的是 LCH 转换。朴素往返的代价是：

> **9 次分数幂 `pow` + 1 次 `atan2` + 1 次 `sqrt`，约 40–60 ALU 周期/像素。**

（`SRGB_TO_LCH` 里 3 次 `pow(x, 2.4)` + 3 次 `pow(x, 0.3333)`；`LCH_TO_SRGB` 里 3 次 `pow(x, 0.4167)`。）

**但这里有一条重要的实施经验：** studio 的 Fresnel 代码里，`SRGB_TO_LCH` 的参数**只依赖 `u_tint` 这个 uniform，完全帧恒定** → 可以整个提到 CPU 或一个 1×1 预计算 pass，**逐像素成本为零**。而 `LCH_TO_SRGB` 里**只有 L 逐像素变化**，C 和 H 帧恒定 → `atan2` 可以提成帧常量，逐像素只剩 `x³` + 15 flops + 3 次 `pow`。

优化后 ≈ 3 次 `pow` + 25 flops（约 15 周期），比朴素版省 60–75%。**而且这 3 次 `pow` 还可以直接省掉**——见下方 Fresnel 部分。

**最终选择：以 argui/Kyant 的 SDF 路线为主，只从 studio 摘两样——superellipse SDF 和 Fresnel。**

#### 为什么走启发式：保真度论证（不只是性能）

上面是性能账。**但更重要的结论是：物理折射在保真度维度上也是偏离的。**

**总依据：Apple 官方定性是「digital meta-material」，不是真实材质的模拟。用物理正确的 Snell 去建模，是在建模一个 Apple 明确说没有建模的东西。**

**张力 1：物理折射引入尺寸依赖，而 Apple 明确要求尺寸无关。**

Apple 原文：Regular 变体 "**works at any size over any content**"。

物理链条：真实玻璃的折射由厚度 `t` 和曲率半径 `R` 决定，位移量 ≈ `t · tan(θT − θI)`。

- 位移量与厚度成正比 → 玻璃「多厚」直接决定折射多强
- 曲率决定法线分布 → 圆角半径越小，边缘法线旋转越快，折射带越窄越强
- 所以一块 **32px 高的胶囊按钮**和一块 **800px 高的侧边栏**，用同一组物理参数，**边缘折射带的视觉宽度完全不同**（前者几乎看不出，后者很夸张）

要让它尺寸无关，只能：(a) 按尺寸逐档调厚度/曲率——那就不是物理了，是伪装成物理的启发式；(b) 把厚度定义成「相对尺寸的比例」——这等于直接承认折射强度是**设计参数**，物理外衣被剥掉。

**反观启发式**：`circleMap(1 - (-sd)/height) * amount` 里 `height` 是**折射高度（一个设计令牌）**，位移量由 `amount` 直接给定，**与元素尺寸解耦**。这正是 Apple 要的性质。

**张力 2：Apple 的 lensing 是「响应式」的，折射强度是设计参数而非材料常数。**

Apple 原文：lensing 是 "**responsive lensing along its edges**"，且淡入淡出是「**折射强度的渐变**」（materialize in/out）——折射强度**必须能被动画驱动**。

**物理模型的问题：Snell 公式里 IOR 是材料常数。** 玻璃是 1.5 就是 1.5，没有「这帧折射强度 0.3、下帧 0.8」这种旋钮。所以必须引入一个非物理的缩放因子——studio 实际就是这么做的：

```wgsl
let thetaT = safeAsin(1.0 / u_refFactor * sin(thetaI));   // u_refFactor 是 uniform，不是常数
```

**一旦乘了这个因子，就已经回到启发式了**——只是额外付了 `asin`/`sin`/`tan`/`pow` 的钱，换来一个同样非物理的结果。

**张力 3：物理只覆盖四层里一层的真子集。**

| Apple 层 | 物理折射能提供吗 |
|---|---|
| Lensing（折射 + 聚焦） | 部分——**聚焦（focus）物理模型没有对应物** |
| Highlights（响应几何的环境光） | **完全不能** |
| Shadow / grounding（感知背后内容） | **完全不能** |
| Interaction illumination（触摸内发光 + 蔓延） | **完全不能** |

第 1 层做得再物理，也拿不到另外三层。**投入产出比很差。**

#### 公平地说：物理路线的真实优势是什么

**它更自洽、更少魔法数——但不是「更像 Apple」。**

| | 启发式 | 物理 |
|---|---|---|
| 边缘带 | 手工塑形，如 `pow(edge, 10.0) * bevelDepth` | 一套公式免费得到 |
| 色散 | 手工塑形，如 `centered.x * centered.y / (halfW * halfH)`（四角最强） | 自然得出 |
| 泛化性 | 为圆角矩形调的参数未必能推到 squircle / blob | 对任意形状成立 |

**一个有意思的佐证**：Apple 设计师**确实在工业设计工作室做了不同透明度/透镜特性的真实玻璃**来对照（Macworld 报道）。但目的是**用来告知一个被设计出来的材质**，不是用来模拟它。

> **真实玻璃是参考，不是目标函数。**

**唯一值得从物理路线拿的，是它的「自洽性」思路**——用一套公式覆盖所有形状，而不是给每种形状手调常数。落到实现就是：**用 SDF 梯度（对任意形状成立）而不是给圆角矩形特调的 bevel 项**。`argui` 的 `glass_gradient` 正是这么做的。

#### 从 studio 摘取的两样

**A. superellipse 圆角 SDF**（Apple 连续曲率的关键）

```wgsl
fn superellipseCornerSDF(p_in: vec2f, r: f32, n: f32) -> f32 {
  let p = abs(p_in);
  let v = pow(pow(p.x, n) + pow(p.y, n), 1.0 / n);
  return v - r;
}

fn roundedRectSDF(p_in: vec2f, center: vec2f, width: f32, height: f32,
                  cornerRadius: f32, n: f32) -> f32 {
  let p = p_in - center;
  let cr = cornerRadius * u.u_dpr;
  let d = abs(p) - vec2f(width, height) * u.u_dpr * 0.5;
  var dist: f32;
  if (d.x > -cr && d.y > -cr) {                       // 只在圆角区域走 superellipse
    let cornerCenter = sign(p) * (vec2f(width, height) * u.u_dpr * 0.5 - vec2f(cr));
    dist = superellipseCornerSDF(p - cornerCenter, cr, n);
  } else {                                            // 直边区域走标准 SD
    dist = min(max(d.x, d.y), 0.0) + length(max(d, vec2f(0.0)));
  }
  return dist;
}
```

`n` 由 `u_shapeRoundness` 控制：**n=2 是椭圆，n→∞ 趋近方角，Apple 的连续曲率约在 n≈4~5**。

**关键细节：只在圆角象限用 superellipse，直边仍用标准 SDF。** 否则会破坏梯度连续性，**折射方向会在直边处扭曲**。

**B. Fresnel**

原版：

```wgsl
let fresnelFactor = clamp(
  pow(1.0 + merged * u_resolution1x.y / 1500.0 * pow(500.0 / u_refFresnelRange, 2.0)
      + u_refFresnelHardness,
      5.0),
  0.0, 1.0);
```

`merged` 是 SDF 值（内部为负），所以离边缘越近 `fresnelFactor` 越大。结构上等价于 `pow(1 - NdotV, 5)` 的 Schlick 近似，**但用 SDF 距离替代了法线点积——这比真算法线便宜且更稳**（SDF 梯度在边缘处不会退化）。

**建议改写掉 LCH**：Fresnel 本来就是个边缘高光，不需要感知均匀性。

```wgsl
let fres = fresnelFactor * u_refFresnelFactor * 0.7 * length(normal);
outColor = mix(outColor, vec4f(linearTint + vec3f(fres * 0.2), 1.0), fres);
```

在线性 RGB 里给 tint 加量，而不是抬高感知亮度。视觉差异是 LCH 版在不同色相上提亮更均匀、RGB 版会把高饱和色更快推向白——但这里 `fresnelFactor` 被 clamp 到 [0,1] 又乘了 0.7，是边缘一圈的细微高光，**肉眼看不出差别**。

#### 实施路径：从 Kyant0 移植

**`Kyant0/AndroidLiquidGlass` 是本项目玻璃效果的主移植源。**

**它为什么可信（不是「年轻项目」）：**

| 项 | 值 |
|---|---|
| Star / Fork / Issues | 3922 / 316 / 35 open |
| License | **Apache-2.0** ✅ |
| 创建 / 最后推送 | 2025-06-18 / 2026-08-26（**超过一年，仍在维护**） |
| 版本纪律 | Maven Central 发布，release 从 1.0.2 到 **2.0.1**，十余个版本 |
| 平台 | **Compose Multiplatform**（默认分支 `kmp`）——有 `androidApp/` 与 `iosApp/` 两个壳 |
| 文档 | 独立 GitBook 文档站 |
| 定位 | 底层库，**明确不含高层组件**（与我们的定位一致） |

**三套 source set，两种 shader 方言：**

```
backdrop/src/commonMain/   ← 共享逻辑 + Shaders.kt（shader 源）
backdrop/src/androidMain/  ← AGSL（Android RuntimeShader）
backdrop/src/skikoMain/    ← SkSL（Compose Multiplatform 的 Skia 后端，覆盖 iOS/桌面）
```

**⚠️ 关键约束：AGSL 和 SkSL 都是 Android/Skia 专属，我们用不了。** 我们是 wgpu，shader 必须重写成 **WGSL**。**移植的是数学，不是文件。**

#### 移植清单

| Kyant0 模块 | 内容 | 我们的目标 |
|---|---|---|
| `internal/Shaders.kt` | `RoundedRectSDF`、`RoundedRectRefractionShaderString`、`...WithDispersionShaderString` | `shaders/glass.wgsl` |
| `internal/Shaders.kt` | `DefaultHighlightShaderString`、`AmbientHighlightShaderString`（`angle` / `falloff`） | `shaders/highlight.wgsl` |
| `effects/Lens.kt` | 折射参数封装 | `theme-glass` 参数层 |
| `highlight/` | `Highlight(width, blurRadius, alpha, style)`，三种样式 | `theme-glass` 高光层 |
| `shadow/` | `Shadow(radius, offset, color, alpha, blendMode)` + `InnerShadow` | `theme-glass` 阴影层 |
| `backdrops/CombinedBackdrop.kt` | `rememberCombinedBackdrop` 系列 | **对应 Apple `GlassEffectContainer`**，见 §9.12 |

**以 `ExtraBinoss/argui` 的 WGSL 移植作为翻译参考**——它已经趟过一遍（预乘 alpha 处理、法线退化防护、边界采样 clamp），且许可同样是 Apache-2.0。

**注意 `internal/Shaders.kt` 只有 6.4 KB** —— 整个移植源比想象的小得多。这是 M4 可控的原因。

**注意：** argui 的 shader 是**注入式 ABI**（`argui_effect(uv, source, backdrop)` + `argui_param_f32(N)` 编号取值），不是可直接使用的文件。但数学部分只有约 40 行，移植到我们自己的 shader ABI 上是几天的工作量。

#### ⚠️ 关于上面那份 SDF 通用化推导的来源

上面的四处替换、三个坑、以及中轴线接缝与 mip 选择的结论，来自对 `Jason-JP-Yang/Reimagined-Glass` 的阅读。

**但那个仓库是 NONE-DECLARED（无 license）——不可用，且它的上游 `Muggleee/liquid-glass` 同样无 license，属于双重 provenance 问题。**

#### ⚠️⚠️ provenance 污染链（单独标红）

**下面这一行代码在整个 Web 玻璃生态里被广泛复制，但它的源头是未授权的：**

```glsl
vec2 offset = mix(vec2(0), normalize(st) / sd, length(st));
```

```
Muggleee/liquid-glass（无 license）原创
        ↓ 推广到任意形状
Jason-JP-Yang/Reimagined-Glass（无 license）
        ↓ 被多个下游复制
   大量教程 / 博客 / 仓库
```

**双重无授权。** 你在网上搜「liquid glass shader」时，撞到这条链的概率很高。

**许可干净的等价实现：用 `argui` 的 `glass_gradient` + `glass_normalize`（Apache-2.0 ✅）。** 它做的是同一件事，但授权链清楚。

因此：

- **不得从这两个仓库复制任何代码文本**（包括 shader 注释里的公式写法）
- 上面记录的是**算法推导与踩坑结论**（事实性知识），可以据此自行实现
- 这几条推导在数学上可以从第一性原理重新得出（SDF 梯度、度量距离、中心差分都是标准工具），**实现时应当自行推导一遍，不要对照那些仓库的代码**
- 许可干净的来源：**`Kyant0/AndroidLiquidGlass`（Apache-2.0 ✅）** 主体算法、**`ExtraBinoss/argui`（Apache-2.0 ✅）** 等价 SDF 写法、**`iyinchao/liquid-glass-studio`（MIT ✅）** 的 superellipse SDF 与 Fresnel

#### 另一个「只能读思路」的例子：liquidGL 的高光

`naughtyduk/liquidGL`（**无 license**）是生态里**唯一实现了 Apple Highlights 层**的，功能最全（折射 + bevel + 色散 + frost + 双移动光源高光 + tilt + magnify + reveal），**诱惑最大**。

它的高光做法是**两个按时间漂移的光源 + `smoothstep` 距离场相加**：

```wgsl
let lp1 = vec2(sin(u.time*0.2), cos(u.time*0.3)) * 0.6 + vec2(0.5);
let lp2 = vec2(sin(u.time*-0.4+1.5), cos(u.time*0.25-0.5)) * 0.6 + vec2(0.5);
var h = smoothstep(0.4, 0.0, distance(in.uv, lp1)) * 0.1;
h += smoothstep(0.5, 0.0, distance(in.uv, lp2)) * 0.08;
```

**思路本身不受版权保护**：双光源而非单光源是为了让高光**不规则移动**（单光源是纯周期性运动，一眼假）；两个光源用不同的频率和相位（0.2/0.3 vs -0.4/0.25），合成出来的轨迹近似随机。这对应 Apple 说的 Highlights「响应几何」和「响应设备运动」。

**实现时自己重写**，不要复制上面这段。

### 9.11 pass 编排：手写，不引入框架

**结论：手写 200–300 行的 pass graph**（对资源依赖做拓扑排序 + cycle 检测）就够了。

被否决的选项：

| 项目 | 否决原因 |
|---|---|
| `engawa` / `engawa-wgpu` | v0.1.1，**compute / blit pass 和 MRT 都还没实现**，零生态验证。设计干净值得读，但不能依赖 |
| `rend3` | 标注 "MAINTENANCE MODE"，0.3.0 是 3–4 年前的版本。有反馈说它 "owns the event loop"、与 winit 冲突、内部单队列导致复杂场景掉到 ~10 FPS |
| `bevy_render` | 生产可用，但引入 bevy 太重。**只借它的词汇表和机制** |

#### 借 bevy 的阶段划分

```
Extract（主世界 → 渲染世界的唯一接触点）
  → Prepare（创建 GPU 资源、触发 pipeline 编译）
  → Queue（按 camera 构建 PhaseItems）
  → PhaseSort
  → Render（执行 pass graph）
  → Cleanup
```

#### 必须照抄的一个机制：异步 PipelineCache

一个 `wgpu::RenderPipeline` 对应 material × mesh layout × shader 特化。**未就绪的 pipeline 当帧跳过、下帧重试。**

这直接解决「首次出现新材质时的掉帧」——对我们是首次出现新玻璃效果、新字形样式、新封面格式时的卡顿。**必须照抄。**

#### 一个必须知道的前提（对 backdrop cache 尤其关键）

> **wgpu 官方没有文档化 pass 的时序 / 并发 / 同步保证**（issue #6273）。

同一个 command buffer 里的两个 render pass，如果 pipeline 之间没有依赖，**可能并发执行**。

因此**任何跨 pass 的隐式假设都必须显式建模成资源依赖**。对我们的 backdrop cache 尤其重要：**读 scene texture 的 blur pass 和写 scene texture 的图元 pass 之间必须有显式依赖边**，不能靠提交顺序隐式保证。

### 9.12 "glass cannot sample glass" 在架构上的落点

Apple 的硬约束是：**相邻/重叠的玻璃必须合并到同一次采样**，否则会互相采样到对方（错误的递归），折射结果不正确。Apple 的公开解法是 `GlassEffectContainer`——把相邻玻璃装进同一个容器统一处理。

**我们的 own-loop + Backdrop-Root 架构（§9.1 / §9.3）在结构上天然满足这条约束**，推导如下：

1. Backdrop-Root rule 说的是「玻璃面板不把自己的填充画进 blur source」
2. 把它推广一步：**scene texture 里不含任何玻璃**——玻璃是完全在合成阶段产生的
3. 因此所有玻璃面板采样的是**同一张不含玻璃的 scene texture**
4. 玻璃之间**不可能互相采样到**——它们要采样的东西里根本没有玻璃

也就是说 Apple 需要显式容器来保证的事，在我们的管线拓扑下是**免费**的。代价是：嵌套玻璃（玻璃上的玻璃）不会呈现「透过上层玻璃看下层玻璃」的效果，而是都直接看穿到最底层内容。**这正是 Apple 的行为**。

**但这条推导有一个前提必须守住：scene texture 里绝不能有任何玻璃元素。**（包括 HUD、悬浮按钮、歌词遮罩这类可能被顺手画进去的东西。）这需要一条明确的绘制层规则，并且应该在 golden test 里覆盖。

> **状态：这条推导是基于架构的推论，逻辑自洽但尚未经第二方核验。M4 实现时需要实测确认，尤其是嵌套玻璃与滚动内容重叠的场景。**

---

## 10. 数据与缓存

SQLite（rusqlite）存：

- 播放历史
- 收藏
- 搜索历史
- 本地库索引
- 缓存条目元数据

音频与封面缓存走文件系统 + 索引表。

**不引入 ORM。**

**内存约束：** 参见 §13 风险 6——Rust 不等于轻，backdrop texture 必须有明确的生命周期和上限。

---

## 11. 平台层

### 11.1 SDL3 提供什么

window / input / mouse / touch / keyboard / **IME** / DPI / display / clipboard / GPU surface / 平台生命周期 / **popup window**。

**IME 已验证可用**（对中文搜索框是刚需）：

- `SDL_EVENT_TEXT_EDITING` / `SDL_EVENT_TEXT_EDITING_CANDIDATES` / `SDL_EVENT_TEXT_INPUT`
- `SDL_StartTextInputWithProperties` + `SDL_TextInputType`
- `SDL_SetTextInputArea` 让系统把候选框放到对应输入区域；移动端可调起系统软键盘

**popup 已验证可用**（对菜单/下拉/提示是刚需）：

- `SDL_CreatePopupWindow`（SDL 3.2.0 起），`SDL_WINDOW_POPUP_MENU` / `SDL_WINDOW_TOOLTIP` 标志
- popup 隐式无边框、不出现在任务栏/alt-tab；popup menu 自动夺取键盘焦点

### 11.2 SDL3 不提供什么

**消除的是平台 UI 代码，不是平台集成代码。**

必须保留 `platform-native/`，处理：

| 平台 | 需要原生实现 |
|---|---|
| Windows | SMTC（系统媒体控件）、托盘、原生文件选择器 |
| macOS | NowPlayingInfoCenter、菜单栏、Control Center |
| Linux | MPRIS2 over D-Bus、StatusNotifierItem 托盘 |
| Android | MediaSession、通知、后台播放、权限 |
| iOS | MPNowPlayingInfoCenter、后台音频、锁屏控制 |

还有：系统音量、分享、系统主题、系统代理、keychain/credential、全局快捷键、文件选择器。

统一抽象成一套 trait 接口（Rustle 的做法可参考：Linux 用 StatusNotifierItem + MPRIS2，Windows 用原生托盘 + 系统媒体控件，macOS 用菜单栏 + Control Center）。

### 11.3 工程约束

**所有 SDL 调用包在 `platform/` trait 后面**（`PlatformWindow` / `GpuDevice` / `InputBackend` / `Clipboard` / `TextInput`），不要让 `use sdl3::...` 散落各处。

理由：`sdl3` crate 是 **0.20.0（2026-09-07）**，官方自述「仍在迁移中、部分 API 未实现」，docs.rs 文档覆盖率 **32.14%**。绑定缺什么就自己补 FFI 层，不让 SDL3 版本变化污染整个项目。

---

## 12. 里程碑

| | 内容 | 出口标准 |
|---|---|---|
**排序原则：先做「便宜且高不确定性」的，再做「昂贵且能一票否决路线」的。**

> ⚠️ **本节经过一次重排。** 早期版本把 M1 称为「最关键节点、要最先排掉最大不确定性」，却又把昂贵的 GPU 性能原型插在它前面——**逻辑自相矛盾**。现按「便宜优先」重排。

| | 内容 | 出口标准 |
|---|---|---|
| **M0** | workspace 骨架 + SDL3 窗口 + wgpu 出三角形 | **验证 SDL3（Wayland）→ raw-window-handle → wgpu 这条路**；**断言拿到硬件 adapter 而非 llvmpipe**（§16.0 坑 2）。参考 `sdl3` crate 自带的 `examples/raw-window-handle-with-wgpu/` |
| **M1** | **音乐链路（纯 CLI，无 UI runtime）**：扫码登录 → 搜索 → 解析取流 → 出声 | **能用命令行听到歌。** 二维码直接输出成 PNG/终端字符画，搜索走 CLI 参数 |
| **M2** | 本地文件 provider + 抽象被两个实现压过 | provider trait 与 `SourceError` 定型（§4.5） |
| **M3** | **玻璃性能原型**：全屏模糊 + 十几个玻璃面板 + 一屏文字，不需要好看 | **在基准硬件上达到 60 fps**（§16）。顺带提前验证 Dual Kawase 移植、backdrop 捕获链路、text instance 上传；**必须专测 bandwidth / tile store-load（§17.8）**，不只看 shader ALU |
| **M4** | UI runtime（拆 M4a–e，见下） | 列表页能滚、能点、动画能跑，中文正常显示，IME 可用 |
| **M5** | 玻璃管线接入播放页（移植 Kyant0 折射 + 高光 + 阴影三层） | 玻璃折射、高光、阴影达到预期效果 |
| **M6** | 逐字歌词 + 导航转场 | 播放页完整 |
| **M7** | M3E 薄版 + 热切换 | `DesignSystem` 抽象定型 |

**为什么 M1 不需要 UI runtime：** 它的唯一目的是验证**音乐链路**（登录态、eapi 加密、取流、解码、出声）。二维码输出成 PNG、搜索走 CLI，同样能验证整条链——**而且省掉整个 UI 依赖**。如果这条链走不通，UI 做得再好也没用。

**为什么 M3（性能原型）排在 M1 之后而不是之前：** M1 便宜（几天）且是产品存在性的硬门槛；M3 昂贵。**先做便宜的。**

**但 M3 不能晚过 M4** —— 它是一票否决整个「玻璃 + 自绘 UI」路线的验证点。若 M3 不达标，M4 之后的所有投入都要重估。

#### M4 内部拆分（否则会长期停在「80%」）

「UI Runtime」作为单个里程碑会两个月不动，必须拆：

| | 内容 |
|---|---|
| **M4a** | tree + event + quad + text |
| **M4b** | layout + input + focus + IME |
| **M4c** | scroll + 虚拟列表 |
| **M4d** | motion + navigation |
| **M4e** | 无障碍语义树骨架 |

出口标准仍是同一个 M4。

**M4e 必须做（不能推迟）：** 语义树是 widget 组装时产生的，**事后补等于重写所有 widget**。v1 不需要三平台桥接，用一个打印语义树的调试工具先占位即可。

**M4 之前必须先做的工具：** headless 渲染 PNG 的 golden test 工具。没有热重载的情况下，这是唯一能让人忍受的 UI 迭代方式（§13 风险 7）。

---

## 13. 风险清单

| # | 风险 | 应对 |
|---|---|---|
| 1 | **SDL3 + wgpu 在 Android 的 surface 创建未实测** | M0 在 Linux 验证这条路；Android 尽早补验。走 SDL3 window properties 拿 `ANativeWindow` 指针喂给 wgpu，理论通但需实测 |
| 2 | **`sdl3` crate 是 0.20.0，pre-1.0** | 所有 SDL 调用包在 `platform/` trait 后面；缺 API 自己补 FFI（§11.3） |
| 3 | **网易云接口非官方，且上游会被法务抹掉** | 见下方专条 |
| 4 | **无障碍成本** | v1 只建语义树（M3），不做三平台桥接。Windows UIA / Apple NSAccessibility / Linux AT-SPI2 是后置的专项工作 |
| 5 | **CJK 字体必须自带** | Noto Sans SC（OFL 协议），逐字歌词要多字重，打包体积要有预期 |
| 6 | **Rust 不等于轻；backdrop scratch 是实打实的显存开销** | Rustle 自称内存 250MB（对比 Electron 500MB+）。**backdrop scratch 的具体量级（400×200 区域）**：高斯一条链 1.28 MB（`Rgba16Float` 640 KB × 2 张乒乓），拖拽峰值 8 条约 **10.2 MB**；Kawase 反而更省（1+1/4+1/16+… ≈ 1.333，N=6 时约 0.85 MB/链，峰值 ≈ 6.8 MB）。做尺寸量化再乘 1.08–1.17×。**这仅是 backdrop scratch**，不含背景纹理、目标 framebuffer 及我们自己的其他 GPU 资源。必须有明确生命周期和上限（§9.7） |
| 7 | **没有热重载** | 自研 UI 框架最痛的开发体验问题。M3 之前必须先做 headless 渲染 PNG 的 golden test 工具 |
| 8 | **逐字歌词吃性能** | AMLL 实测 60FPS 需 3.0GHz+，1080p 要 GTX 10 系以上。seek 时必须降级（§5.4） |
| 9 | **ncm-api-rs 只有 11 commits** | eapi 加密正确性对照 `Cloud370/ncm-rs` 或 `ddy314/netease-music` 交叉验证 |
| 10 | **版权与合规** | 非官方接口 + VIP 内容。这是产品层面的判断，需自行确认边界 |

### 13.1 风险 3 专项：上游已死，且死法重要

**`NeteaseCloudMusicApi` 已于 2024-02-28 被网易法务通知后清库归档**——30,288 star 一夜清零。理由是破解、绕开限制提供版权内容，涉嫌侵犯著作权罪和破坏计算机信息系统罪。

**结论：eapi / weapi 加密必须 vendor 进我们自己的代码库，不依赖任何外部 API 服务。** 这也是选择 `ncm-api-rs`（Rust 原生实现加密，而非调用外部服务）而非「起一个 Node API 服务」的原因。

**这是一个产品级的持续风险，不是一次性风险。** 要预留快速跟进接口变动、以及必要时换 provider 的能力。

---

## 14. 参考实现与许可约束

### 14.1 许可红线

> **⚠️ 重大更正（2026-09-26）：本项目是 GPLv3。**
>
> 本文档早期版本在此处写了「硬约束——项目未来要闭源分发」，并据此否决了 GPL / AGPL 代码。
> **那个前提是错的**——用户从未说过闭源，是本文档自行假定的（源头是外部审查里的一句
> 「按你的闭源目标」，未经核实即被采纳）。整个小节据此重写。

**本项目采用 GPLv3。** 因此：

| 类别 | 许可 | 结论 |
|---|---|---|
| **宽松许可** | MIT / Apache-2.0 / BSD / WTFPL | ✅ 可用，且**与 GPLv3 兼容**。Apache-2.0 需保留 NOTICE |
| **公共领域奉献** | **CC0** | ✅ 可用。**CC0 没有法律上的署名要求**（它是奉献，不是许可）。*amll-ttml-db 在社区规范上请求署名，但那是礼节不是义务* |
| **弱文件级 copyleft** | **MPL-2.0** | ✅ 可用，且**与 GPLv3 兼容**（MPL-2.0 §3.3 把 GPLv3 列为 Secondary License） |
| **强 copyleft** | **GPL-3.0 / AGPL-3.0** | ✅ **可用**。见下方 AGPL 说明 |
| **非商业许可** | **PolyForm Noncommercial 等** | ❌ **不可用，且与 GPLv3 不兼容**——比 GPL 更严格的限制无法被 GPLv3 吸收 |
| **无 license** | NONE-DECLARED | ❌ **仍不可用**。默认保留全部权利，**GPLv3 也救不了**——没有授权就是没有授权 |

#### 我们是 GPLv3，所以负有义务

- 分发二进制时**必须同时提供完整对应源码**
- 修改过的文件必须保留声明、标注改动
- 不能与 GPLv3 不兼容的许可代码链接（**PolyForm Noncommercial 正是这一类**）

#### AGPL-3.0 可以吗？可以，但有一条要注意

**GPLv3 §13 明确允许**把 GPLv3 作品与 AGPL-3.0 作品链接/组合成单一作品，整体按 GPLv3 分发。

**但 AGPL §13 的网络条款会适用于整个组合**——即：如果用户通过网络与该程序交互，须向其提供源码。对**桌面音乐播放器而言这条基本不构成实际负担**（不提供网络服务），但若将来做 Web 版或服务端组件，就必须认真对待。

#### 这解除了什么（原本被错误否决的）

| 项目 | 原判 | 现判 |
|---|---|---|
| `Fei-xiangShi/Rustle`（AGPL-3.0） | ❌ 只读思路 | ✅ **可用**（技术栈与我们最像） |
| AMLL（AGPL-3.0） | ❌ 只读思路 | ✅ **可用** |
| Cider 1、SPlayer、MusicFree（AGPL-3.0） | ❌ 只读思路 | ✅ **可用** |
| Hyprland `blur1.glsl`（GPL） | ❌ 只读思路 | ✅ **可用** |
| Slint（GPLv3 / 商业双授权） | ❌ 许可否决 | ⚠️ **许可理由作废**，见 §3.1 |

**但「可用」不等于「该抄」**——Rustle 是 27★ 个人项目、AMLL 只有一名活跃维护者，**代码质量与维护持续性仍是独立于许可的判断**。

**明确标记为「不能碰代码」的：**

- **`Fei-xiangShi/Rustle`（AGPL-3.0）** —— 和我们技术栈最像，最容易产生「抄两段」的冲动。**只读思路，不抄代码。**
- **AMLL（AGPL-3.0）** —— 同上。算法参数照搬没问题（那是事实性知识），代码不行。
- **`Jason-JP-Yang/Reimagined-Glass`（NONE-DECLARED）** —— 上游 `Muggleee/liquid-glass` 同样无 license，**双重 provenance 问题**。见 §14.5
- `naughtyduk/liquidGL`、`BarredEwe/LiquidGlass` —— **无 license**，不可用
- `slint` —— GPLv3 / 商业授权，已否决
- `Cider 1`、`SPlayer`、`listen1`、`MusicFree` —— AGPL / 其他，只读思路

**已坐实的可用许可（玻璃相关）：**

| 仓库 | 许可 | 用途 |
|---|---|---|
| `Kyant0/AndroidLiquidGlass` | **Apache-2.0** ✅ | 主体折射算法（§9.10） |
| `iyinchao/liquid-glass-studio` | **MIT** ✅ | superellipse SDF、Fresnel（§9.10） |
| `ExtraBinoss/argui` | **Apache-2.0** ✅ | WGSL 移植参考（须保留 `LICENSE-android-liquid-glass`） |
| `backdrop-blur-core` / `-wgpu` / `-glow` / `-egui` | **MIT OR Apache-2.0** ✅ | backdrop 架构参考 |
| `QmDeve/AndroidLiquidGlassView` | MIT ✅ | — |
| `skydoves/Cloudy` | Apache-2.0 ✅ | — |
| `OverShifted/LiquidGlass` | MIT ✅ | 13-tap 可分离高斯 |
| `andydbc/unity-frosted-glass` | MIT ✅ | 可分离模糊 shader |

### 14.2 可直接依赖

| 库 | 许可 | 用途 |
|---|---|---|
| `wgpu` | MIT/Apache-2.0 | 渲染层（无争议） |
| `parley` | MIT/Apache-2.0 | 文本 shaping / layout（`parley_emoji` 是 MIT only） |
| `swash` | MIT | 纯字形光栅化（见 §7.2） |
| `etagere` | MIT/Apache-2.0 | atlas packing |
| `lyon` | MIT/Apache-2.0 | **按需**——只在真需要任意贝塞尔路径填充/描边时引入。它不做抗锯齿 |
| `ncm-api-rs` | WTFPL | 网易云接口 + eapi 加密 |
| `ttml_processor` | MIT | TTML 逐字歌词解析（**需 pin 版本**，作者自述 unstable） |
| `symphonia` | MPL-2.0 | 音频解码 |
| `cpal` | Apache-2.0 | 音频输出 |
| `rusqlite` | MIT | 数据库 |
| `lofty` | MIT/Apache-2.0 | 本地音乐标签与封面 |

**明确不依赖的：** `glyphon`（见 §7.2）、`vello`、`femtovg`、`slint`、`engawa`、`rend3`。

**版本联动风险：** linebender 全家桶（parley / fontique / skrifa / peniko / vello）版本联动较紧，CI 里要提前验证兼容。

### 14.3 读源码抄思路（不看代码，看设计）

| 项目 | 许可 | 看什么 |
|---|---|---|
| **iced** `wgpu/src/quad.rs` | MIT | 图元层的正确形状（见 §3.1） |
| **epaint** | MIT/Apache-2.0 | feathering 抗锯齿、预乘 alpha |
| **Rustle** | **AGPL-3.0 ✅ 可用** | SDF GPU 文字做逐字歌词的可行性、弹簧参数建模、iced 里嵌自定义 shader、平台集成抽象。**但注意它只有 27★、是个人项目**——许可放开了，质量判断不受影响 |
| **AMLL core** | **AGPL-3.0 ✅ 可用** | 逐字渲染算法参数（§5.4）。仅有 1 名活跃维护者 |
| **Linn** `utils/lyric_parse.rs` | MIT ✅ | YRC 容错解析 + **测试夹具可直接复用** |
| **termusic** | MIT ✅ | 双二进制 + Protobuf + 可插拔播放后端（跑了 5 年的成熟范例） |
| **app_rhyme / music_api** | MIT/Apache ✅ | 音源层做成独立 crate 的范例 |
| **Kyant0/AndroidLiquidGlass** | Apache-2.0 ✅ | **折射算法源**（§9.10） |
| **argui** | MIT/Apache-2.0 ✅ | WGSL 移植参考、`argui-layout/src/paint/effects.rs` 看 effect 怎么接进 paint 流程 |
| **`iyinchao/liquid-glass-studio`** | MIT ✅ | **许可干净的可抄来源**：superellipse 圆角 SDF、Fresnel（§9.10） |
| ~~Reimagined-Glass~~ | **NONE-DECLARED ❌** | **不可用**（见 §14.5） |
| **Hyprland** `src/render/shaders/glsl/blur1.glsl` | **GPL ✅ 可用** | dual Kawase 降采样参考实现 + **vibrancy/saturation 逐级施加**的技巧（§9.5） |
| **`backdrop-blur-wgpu`** | 见 crate | **最贴近我们需求的架构参考**：两阶段 `prepare` → `record` 的 seam trait（因为 wgpu 走 Queue 上传而 glow 是 immediate mode）、按 target format 懒建 pipeline、sRGB 拒绝、Backdrop-Root rule、last-frame-used 淘汰。**注意它的 wgpu 后端目前只有可分离 Gaussian，dual Kawase 还是 TODO**。⚠️ **GitHub 主页 404——但源码可从 crates.io CDN 直接取**：`https://static.crates.io/crates/backdrop-blur-wgpu/backdrop-blur-wgpu-0.2.0.crate`（已验证可下载解包） |
| **Bevy render graph** | MIT/Apache ✅ | 阶段划分词汇表 + 异步 PipelineCache（§9.11）。不引入依赖 |
| **SIGGRAPH 2015** Marius Bjørge "Bandwidth-Efficient Rendering" + three.js PR #33920 | — | dual Kawase 原始出处与工程数据（§9.2） |
| **Slint** box shadow 缓存 | **GPLv3 ✅ 可用** | `SkiaBoxShadowCache` / `FemtovgBoxShadowCache` 的模糊纹理复用思路 |

### 14.4 关于 argui：抄 WGSL，不取依赖

`ExtraBinoss/argui` 是一个 **29 个 crate** 的 Rust UI workspace（11.6 MB），架构和我们设计的几乎完全一致：`argui-layout`（taffy）、`argui-text`（cosmic-text）、`argui-widgets`、`argui-animation`、`argui-accessibility`（AccessKit）、`argui-theme`、`argui-devtools`、`argui-render`、`argui-paint`、`argui-android` / `argui-ios`。

**这是和我们直接竞争的完整运行时，不是可插拔的效果库。**

#### 三条否决理由（比「太年轻」更硬）

1. **`argui-effects` 不是独立 crate。** 它的依赖是 `argui-paint` + `argui-render`（`scroll` feature 再加 `argui-ui`）。**用它就等于接入 argui 的整条 paint/render 管线**，而不是单独拿一个效果。
2. **`wgpu = "=30.0.1"` 是精确锁。** 下游消费者会被强行钉死在某个 wgpu **补丁版本**上，和我们自己的 wgpu 版本必然打架。这是对下游最不友好的依赖形式。
3. **bus factor = 1。** 创建 2026-09-01，最后提交 2026-09-25，版本 **0.3.2**（pre-1.0），32 star，1 fork，无生态、无第三方使用记录、无安全审计历史。迭代速度是「每天重构」。

#### 正确用法：抄 shader（Apache-2.0 允许，代价只是保留署名）

| 文件 | 内容 |
|---|---|
| `crates/argui-effects/src/shaders/effects/liquid_glass.wgsl` | 主 shader，85 行 |
| `crates/argui-effects/src/shaders/effects/liquid_glass_blur.wgsl` | H/V 两轴共用，靠 `glass_blur_axis(uv, source, vec2(1,0))` 参数化 |
| `crates/argui-effects/src/liquid_glass.rs` | 参数结构体 + 默认值 |
| `crates/argui-effects/LICENSE-android-liquid-glass` | **必须一并保留** |

参数结构（可直接照搬）：

```rust
pub struct LiquidGlass {
    pub refraction: f32,             // 逻辑像素
    pub chromatic_aberration: f32,
    pub blur: f32,                   // 高斯 sigma，0–16，0 保持锐利
    pub highlight: f32,
    pub edge_width: f32,
    pub saturation: f32,
    pub brightness: f32,
    pub contrast: f32,
    pub depth_effect: bool,          // 径向深度是否混入折射法线
    pub tint: [f32; 4],              // 线性 RGB + tint 量；alpha 不改变源覆盖
}
// 默认：refraction: 38.0, chromatic_aberration: 0.0, blur: 8.0, tint: [0,0,0,0.272]
```

注意它的 shader 是**注入式 ABI**（`argui_effect(uv, source, backdrop)` + `argui_param_f32(N)` 编号取值），**不是可直接使用的文件**——移植的是数学，不是文件。

### 14.5 不可用清单

**这里有两类，原因不同：**

- **无 license（NONE-DECLARED）** —— 作者保留全部权利，**没有任何授权**。GPLv3 也救不了。
- **非商业许可（PolyForm Noncommercial）** —— 有授权但仅限非商业，**与 GPLv3 不兼容**（更严格的限制无法被 GPLv3 吸收）。

两类结论相同：**代码和 shader 文本都不能进库。**

| 仓库 | Star | 风险 | 代价 |
|---|---|---|---|
| [`naughtyduk/liquidGL`](https://github.com/naughtyduk/liquidGL) | **897** | **高** | **诱惑最大**。功能最全：折射 + bevel + 色散 + frost + **双移动光源高光** + tilt + magnify + reveal。**生态里唯一实现了 Apple Highlights 层的** |
| [`Muggleee/liquid-glass`](https://github.com/Muggleee/liquid-glass) | 161 | **高** | **那行被广泛抄袭的代码的源头**（见 §9.10 provenance 污染链） |
| [`Jason-JP-Yang/Reimagined-Glass`](https://github.com/Jason-JP-Yang/Reimagined-Glass) | 0 | **高** | 推广了上面那行。0 star、非 fork、最后 push 2026-08-01 |
| [`archisvaze/liquid-glass`](https://github.com/archisvaze/liquid-glass) | 781 | 中 | three.js `ShaderMaterial` + 全屏 quad |
| [`ybouane/liquidglass`](https://github.com/ybouane/liquidglass) | 507 | 中 | Web 折射/模糊/色散库 |
| [`whynotmake-it/flutter_liquid_glass`](https://github.com/whynotmake-it/flutter_liquid_glass) | 444 | 中 | Flutter 包工作区 |
| [`DnV1eX/LiquidGlassKit`](https://github.com/DnV1eX/LiquidGlassKit) | 443 | 中 | Apple Liquid Glass 向 iOS 13–18 的 backport |
| [`conorluddy/LiquidGlassReference`](https://github.com/conorluddy/LiquidGlassReference) | 340 | 低（纯文档） | 只能读，**不能引用为规范来源** |
| [`GetStream/awesome-liquid-glass`](https://github.com/GetStream/awesome-liquid-glass) | 251 | 低（示例代码） | SwiftUI 示例集 |
| [`BarredEwe/LiquidGlass`](https://github.com/BarredEwe/LiquidGlass) | 202 | 中 | Metal 折射 + 快照背景捕获 |

**这张表是 §14.1 红线的具体落点。** 前三条尤其危险——它们功能完整、star 数不低、看起来「就是我们要的东西」，但**没有 license 文件就意味着作者保留全部权利**。

**GPL / AGPL 零命中** —— 已核验 25 个仓库无 copyleft。**但仅覆盖已核验的这些，新增仓库要重核。**

**遇到「仓库没 license 但代码很诱人」时的正确做法：**

1. 先看它的算法是否有**许可干净的同类上游**（本例：Kyant0 是 Reimagined-Glass 那类实现的公共祖先，且是 Apache-2.0）
2. 记下**算法思路**（事实性知识），自行实现
3. **不要复制任何代码文本**，包括 shader 注释里的公式写法
4. 若该仓库本身是从别处 fork/移植的，**追它的上游 license**——上游干净的话，走上游（本例：`Muggleee/liquid-glass` 也不干净，所以只能走 Kyant0）

---

## 15. 未定项

**技术选型已全部落定。** 仅剩以下非阻塞项。

### 15.1 待补充的调研

调研已基本收口。以下为仍在等待的补充项，**均不阻塞动工**：

- **纯模糊的「捕获时机」与「失效策略」** —— `render-research` 已覆盖 own-loop 架构、Backdrop-Root rule、per-region cache、damage tracking 的接法，但**捕获时机**与**失效策略**两点讲得较粗。我们自己的方案见 §9.7，若补充材料有更优解再修订
- **「走启发式而非物理折射」在保真度（不只是性能）上的依据** —— §9.10 目前只有性能论证；Apple 的「digital meta-material」定性（§9 开头）在方向上支持这个选择，但缺少直接的保真度对比材料
- **§9.12 的架构推论待核验** —— 「own-loop + Backdrop-Root 天然满足 glass-cannot-sample-glass」是基于架构的推论，逻辑自洽但未经第二方确认

### 15.2 需要在实施中验证的假设

| 假设 | 验证时机 | 万一不成立的退路 |
|---|---|---|
| SDL3 → `raw-window-handle` → wgpu 在 Linux 可行 | **M0** | 换 winit 的 **Wayland** 后端（**换库不换协议**——本机 niri 是 Wayland-only，没有 X11 退路） |
| 同上，在 **Android** 可行 | M3 之后 | ✅ **接口链已核实存在**（见下），风险已下调 |
| parley 与渲染器的中立类型层能挡住 parley 的变动 | M3 | 换 cosmic-text，只改适配层（§7.2 第 4 条） |
| `ncm-api-rs` 的 eapi 加密正确 | M1 | 对照 `Cloud370/ncm-rs` / `ddy314/netease-music` 交叉验证；最坏情况自己 vendor 加密实现 |
| damage tracking 的收益值得复杂度 | M5 | backdrop cache 退化成「每帧全量重算 + last-frame-used 淘汰」 |

#### SDL3 → wgpu 在 Android 的支持：已核实（风险下调）

**本文档早期版本写「Android 暂无干净退路，这是最大的未验证风险」——写重了。** 已直接核查 `sdl3` 0.20.0 源码：

```rust
// src/sdl3/raw_window_handle.rs
#[cfg(target_os = "android")]
unsafe {
    use self::raw_window_handle::AndroidNdkWindowHandle;
    let window_properties = sys::video::SDL_GetWindowProperties(self.raw());
    let handle = AndroidNdkWindowHandle::new(NonNull::new_unchecked(native_window));
    Ok(WindowHandle::borrow_raw(RawWindowHandle::AndroidNdk(handle)))
}
```

而且该 crate **自带 `examples/raw-window-handle-with-wgpu/`**，做的正是 `create_surface(&instance, &window)`。

**所以接口链不是「理论上应该可以」，是现成的。**

**仍然未验证的是生命周期那一层**（这些桌面模拟不了，必须真机）：

```
surface lost / recreate
background → foreground 切换
rotation / resize
Vulkan 初始化失败 → GLES 重试（§17.6）
IME + 软键盘
ANativeWindow 生命周期
```

**Android bring-up checklist 必须包含一条**：SDL3 曾在 Android 上自行先创建 GL context，导致 Vulkan/GLES 初始化报 `VK_ERROR_NATIVE_WINDOW_IN_USE_KHR`；处理方式是在窗口创建**前**设置 `SDL_VIDEO_EXTERNAL_CONTEXT=1`。

---

## 16. 性能预算与约束

性能不是收尾时才考虑的事。本项目的多处架构决策**本身就是性能决策**（fragment 而非 compute、blur 半径 log2 量化、backdrop 共享、damage tracking、不动画化 blur 半径）。本节把它们汇总成可执行的预算与红线。

### 16.0 目标硬件基线（实测）

v1 的基准是开发机本身（决策见附录 A）。**实测结果**：

| | 实测值 | 判断 |
|---|---|---|
| CPU | **AMD A8-7650K**（4C/4T，1.9–3.3 GHz，2014 Kaveri APU） | ⚠️ **真瓶颈** |
| GPU | **NVIDIA GTX 1650**（Turing，2019） | ✅ 1080p 充裕 |
| 内存 | 7.7 GB 总量，**可用约 3.3 GB** | ⚠️ 偏紧 |
| 显示 | 1920×1080 @ **60 Hz**，Scale 1.0，Adaptive Sync 关 | ✅ 完整 16.67 ms 预算 |
| 会话 | **Wayland（niri）**，NVIDIA 专有驱动 615.71.09 | ⚠️ 见下 |
| Vulkan | 1.4.351（GTX 1650）+ **llvmpipe 软件回退** | ⚠️ 见下 |

#### ⚠️ 结论与直觉相反：瓶颈在 CPU，不在 GPU

**原先把 backdrop blur 列为「最大头」，在这台机器上不成立。** GTX 1650 跑 1080p 的 Dual Kawase 会非常轻松，3–6 ms 的 GPU 预算大概率用不到一半。

**真正的瓶颈是 A8-7650K。** AMLL 的实测标准是「60 FPS 需 3.0 GHz+ CPU」，而这颗 2014 年的 APU 虽然标称 3.3 GHz，**单核 IPC 约为现代芯片的一半**——它恰好卡在那条线上下。

**直接后果：所有 CPU 侧每帧工作都要重新审视。** 尤其是**逐字歌词的弹簧物理**（per-frame per-word 运算，AMLL 自己标注「性能开销：高」）——这是在本机最容易感知到卡顿的地方。

**因此 §16.6 的降级阶梯顺序要相应调整：本机应先砍 CPU 侧开销，而不是先砍 GPU 侧的 blur pass。**

#### 三个本机特有的坑

**1. niri 是 Wayland-only 合成器，没有 X11 回退。**

SDL3 的 **Wayland 后端是唯一路径**。这抬高了「SDL3 + Wayland + NVIDIA」链路的验证优先级——M0 必须覆盖。（好消息：niri 默认不做窗口装饰，与我们自绘 UI 的路线天然一致。）

**2. llvmpipe 出现在 Vulkan 设备列表里——必须主动防。**

若 wgpu 因任何原因选中 **llvmpipe**（软件光栅化）而非 GTX 1650，**性能会慢两个数量级，且不会报错**。

> **M0 必须加断言：验证拿到的是硬件 adapter，不是 llvmpipe / lavapipe。**

**3. 可用内存仅约 3.3 GB。**

封面缓存上限要定得比直觉保守。见 §16.4 第 5 条。

### 16.1 三个维度，优先级不同

| 维度 | 失败的后果 | 优先级 |
|---|---|---|
| **音频连续性** | 爆音、卡顿、掉采样 | **最高——播放器的根本功能** |
| **帧时间**（UI 流畅度） | 掉帧、动画卡 | 高 |
| **资源占用**（内存 / 显存 / 启动 / 空载 CPU） | 用户觉得「重」 | 中 |

> **铁律：音频优先于 UI。** UI 掉一帧用户能忍，音频爆一次音不能忍。

### 16.2 音频线程的实时性约束（最高优先级）

**cpal 的回调线程里绝对不能：**

- 分配 / 释放内存
- 加锁（任何可能被 UI 线程持有的锁）
- 做 IO（文件、网络）
- 调用任何可能阻塞的系统调用

**架构要求：**

```
网络/文件 ──▶ 解码线程（symphonia）──▶ ring buffer ──▶ cpal 回调线程 ──▶ 设备
```

解码在独立线程运行，通过**无锁 ring buffer** 喂给回调。**任何 UI 侧的卡顿都不允许影响音频线程的调度。**

`dsls-audio` 的 PCM graph 挂载点（未来 EQ / DSP）也必须跑在音频线程或专用线程上，不经过 UI 线程。

**硬指标：xrun / underrun 计数必须为 0。**

### 16.3 帧时间预算（60 fps = 16.67 ms）

| 阶段 | 预算 | 备注 |
|---|---|---|
| 事件处理 + 状态更新 | 0.5 ms | |
| 布局（增量） | 1.0 ms | **全量布局只应在窗口 resize 时发生** |
| 文本 shaping | ≈0（缓存命中） | 只有文本/样式/换行宽度变化才重建（§7.2） |
| instance 构建 | 1.0 ms | **增量**，不是每帧全量重建 |
| GPU 提交（CPU 侧） | 1.0 ms | |
| **GPU 执行** | **≈12 ms** | |
| —— 背景 / 封面 | 1.0 ms | |
| —— **backdrop blur** | **3–6 ms** | **最大头，最大风险** |
| —— 玻璃合成 | 1.5 ms | |
| —— 文本 | 1.0 ms | |
| —— UI 图元 | 1.0 ms | |

**⚠️ 高刷新率显示器的警告：144 Hz ≈ 6.9 ms/帧，GPU 只剩约 2 ms，backdrop blur 一项就超预算。** 不能按 60 fps 设计然后指望自适应——降级阶梯（§16.6）必须从第一天就在，而不是事后补。

### 16.4 六个具体风险点

1. **backdrop blur 是最大头。** Dual Kawase 是**带宽受限**而非 ALU 受限。集显上全屏模糊容易吃掉 3–5 ms。**§9.7 的缓存与 damage tracking 不是优化，是必需。**
2. **逐字歌词很贵。** AMLL 实测：60 FPS 需 3.0 GHz+ CPU，1080p 要 GTX 10 系以上，2160p 要 RTX 2070+。SDF 文字 + 每帧重建 instance + 弹簧物理。**seek 时必须降级**（§5.4）。
3. **封面解码与缩放（最容易被忽略）。** 大图解码在主线程会直接卡帧。**必须在后台线程解码并预先缩放到实际显示尺寸再上传。** 网易云封面动辄 1000×1000，而播放页可能只显示 300 px。
4. **音频线程的实时性。** 见 §16.2。
5. **内存 / 显存。** backdrop scratch 峰值约 10 MB（§9.7）；封面缓存必须有明确上限。
6. **空载 CPU。** 静态播放页不动时应 < 1%。这正好对应 §9.7 的 `RepaintPolicy`——静止内容用 `Static`，不请求帧。

### 16.5 M0 就必须建立的测量能力

**不能等最后才发现性能不行。**

- **帧时间直方图**（不只看平均值，**要看 p99**）——平均 8 ms 但每 3 秒卡一次 50 ms，比稳定 12 ms 更糟
- **各 pass 耗时分解** —— `wgpu` 的 `TIMESTAMP_QUERY`，但**注意 GLES 后端可能不支持，必须有 fallback**（回退到 CPU 侧计时）
- **音频 xrun / underrun 计数** —— 硬指标，必须为 0
- **内存与显存占用采样**

### 16.6 降级阶梯（保证低端机「可用」而非「卡死」）

性能不足时主动降级，不要硬扛。**阶梯分两组，且在本机（CPU 瓶颈）上应先动 CPU 侧。**

#### 第一组：CPU 侧（本机优先）

```
1. 逐字歌词弹簧动画退化到简单缓动
2. 歌词虚拟化窗口收窄（overscan 减半）
3. 弹簧参数固定化（不做「行间隔越近弹簧越硬」的动态调整）
4. 降低 damage tracking 的粒度要求，接受更多重绘
```

第 1 档是**本机收益最大的一档**——弹簧物理是 per-frame per-word 运算，AMLL 自己标注开销为「高」，而它纯属锦上添花（关掉退化成简单过渡，视觉朴素但完全可用）。

#### 第二组：GPU 侧（其他机器 / 高分辨率时）

```
5. blur pass 数减半（4 → 2）
6. backdrop 用半分辨率模糊再上采样
7. 关闭色散（7 次纹理采样 → 1 次）   ← 单档收益最大
8. 关闭折射，只保留模糊
```

**第 7 档特别值：** 色散是 7 次纹理采样，关掉直接降到 1 次。而且 Kyant0 的默认值本来就是 `chromatic_aberration: 0.0`——**低配机上保持默认即可，零额外工作**。

> **顺序不是固定的。** 正确的做法是 M0.5 原型先测出本机真正的瓶颈在哪一侧，再据此排序。上面两组只是预判，不是结论。

### 16.7 性能红线（明确不能做的）

| 不能做 | 原因 |
|---|---|
| 音频回调里分配 / 加锁 / IO | 见 §16.2 |
| 主线程做图片解码 | 直接卡帧（§16.4 第 3 条） |
| 每帧重新 shape 文本 | §7.2 的设计前提就是 shape 一次 |
| 每帧全量重建 instance buffer | 应增量更新 |
| 动画化 blur 半径 | Chromium 实测每个经过的半径都编译一个新 shader（§9.8） |
| 每个玻璃面板各做一次 blur | 必须共享 backdrop（§9.7） |
| 全量布局每帧跑 | 只应在 resize 时发生 |

---

## 17. RenderTier：能力分级（本项目最大的剩余结构性缺口）

### 17.1 这个缺口是怎么来的

我们**为了 Android GLES 兜底才选了 wgpu**（§3）。但选定之后，那条理由**没有贯彻到下游设计**——§9.6 一度写下「中间 target 用 `Rgba16Float`，不要用 sRGB」这种绝对要求，而这条要求**恰好会把 GLES 低端设备排除掉**。

**自己否掉了自己选 wgpu 的理由。** 这是当前文档最大的结构性漏洞，本节专门补上。

### 17.2 核心原则

```
没有 RGBA16F
  → Glass quality 降一级        ✅ 正确

没有 RGBA16F
  → 设备不能运行 dslsmusic      ❌ 错误
```

**渲染管线必须服从 adapter 的实际能力，而不是反过来。** 所有「必须用 X 格式」的要求一律改写成分级。

### 17.3 三级能力档

| | Tier A（完整） | Tier B（降级） | Tier C（最低） |
|---|---|---|---|
| 中间格式 | `Rgba16Float`（线性） | `Rgba8Unorm` + dithering | 无中间格式，预模糊静态 backdrop |
| 模糊精度 | 全精度，4–6 级金字塔 | 降精度，dithering 抑制 banding | 半分辨率或预处理 |
| 色散 | 7 通道 | 3 通道（RGB） | 关闭 |
| 折射 | 完整 | 简化 profile | 关闭，仅模糊 + tint |
| 目标设备 | 桌面独显、现代 iOS/Android | 低端 Android、老集显 | 极低端 / 软件渲染 |

**Tier B 的关键是 dithering** —— `Rgba8Unorm` 在大面积渐变上会 banding，这是它相对 16F 的主要劣势，而 dithering 能补掉大部分。

### 17.4 能力查询（不是猜测，是查询）

```rust
// 格式支持
let features = adapter.get_texture_format_features(format);

// 关键：不只看「能不能渲染/采样」，要看 BLENDABLE
// —— 能渲染不代表能混合，后者是更严的要求
let can_blend = features.flags.contains(wgpu::TextureFormatFeatureFlags::BLENDABLE);
```

**检查点不只是「格式是否可用」，还有混合支持。** 这是比格式可用性更严的约束，容易漏。

### 17.5 必须贯穿的下游位置

`RenderTier` 一旦确定，必须同时约束以下全部——**任何一处漏掉都会在低端设备上炸**：

| 位置 | 受影响内容 |
|---|---|
| 中间纹理格式 | §9.6 |
| blur 金字塔级数与格式 | §9.2 / §9.5 |
| shader 变体 | 折射/色散的通道数、`textureLod` 可用性 |
| backdrop cache key | **格式进 key**（不同 tier 的 scratch 不能混用） |
| backend 选择与重试 | Android：Vulkan 初始化失败 → 回退 GLES（§17.6） |
| 降级阶梯 | §16.6 第二组的触发条件 |

### 17.6 Android backend 重试

Android 上 Vulkan 可能因驱动问题初始化失败。正确行为是**回退 GLES 而不是崩溃**：

```
尝试 Vulkan → 失败 → 尝试 GLES → 失败 → Tier C / 明确报错
```

**并且必须在启动时断言拿到的是硬件 adapter：**

> `llvmpipe` / `lavapipe` 出现在 Vulkan 设备列表里（本机实测就有）。若 wgpu 选中软件光栅化，**性能慢两个数量级且不报错**。见 §16.0 坑 2。

### 17.7 本项目的执行策略：在桌面上模拟低能力

**不单独做早期 Android spike。** 用 wgpu 的 downlevel limits 在桌面上模拟：

```rust
// M0 起就在 GLES 能力集下开发
let device = adapter.request_device(&DeviceDescriptor {
    required_limits: wgpu::Limits::downlevel_webgl2_defaults(),
    ..default()
});
```

**收益：从写第一行渲染代码起就撞上 `Rgba16Float` 这类问题，而不是等到 M4。** 成本几乎为零——不需要 Android 工具链、不需要 NDK 交叉编译、不需要真机。

**真机验证仍然必须做**（surface lost/recreate、前后台、旋转、IME、ANativeWindow 生命周期——桌面模拟不了），但排在 M2 之后。

### 17.8 ⚠️ 但 own-loop 不是零成本（更正 §9.1）

§9.1 曾写「捕获这一步的成本是零」。**这句必须更正：**

```
capture copy cost        ≈ 0     （不需要额外拷贝）
offscreen compositing    ≠ 0     （绝对不是零）
```

**尤其在 tile-based GPU 上（Arm 移动端、Apple TBDR）。** 移动 GPU 上全屏 image 的写回/重新读取非常昂贵：post-processing 会迫使 tile 内容落到主存、再从主存读回来。跨 render pass 保存并重新读取纹理会发生 tile memory → system memory → texture cache 的往返。

**所以本质是：主动拿 bandwidth 换一个可采样的 backdrop。**

路线依然正确（这是实现折射的前提），但 M0.5 的性能原型**必须专门测这一项**：

```
external-memory bandwidth
tile store/load
1080p / 1440p
Rgba16Float vs Rgba8
full scene intermediate vs partial backdrop
```

**而不是只看 shader ALU 耗时。** 在桌面独显上这项通常不是瓶颈，但它是 Android 上最可能翻车的地方——而我们的 Tier 设计就是为了它。

---

## 18. v2 范围（已定）

**v2 = 音频引擎 + 功能补全 + 视觉完整度。仍然只跑 Linux。**

v2 会有**自己的设计文档**——本文档是 v1 的，范围、里程碑、风险都围绕「Linux 上跑通两个页面 + 玻璃」；v2 目标不同，混在一起会让两份都变糊。

### 18.1 圈进来的（A + B + B+ + C）

**A. 音频引擎**（详见 §6.2）

预加载 + gapless · 响度归一化（LUFS 可调）· 静音尾部检测 · EQ · 变速保音高 0.1x–3.0x · 交叉淡入 + 节拍匹配 · 变调 / 立体声展宽 / 混响 · 输出设备选择

**B. 功能补全**

歌单管理 · 收藏 · 每日推荐 · 下载与离线缓存 · 桌面歌词独立窗口 · 播放历史

**B+ · 播放基本盘**（2026-09-26 补入 —— 展开清单时发现这三项不属于任何里程碑）

| 项 | 为什么必须补 |
|---|---|
| **播放模式** | 顺序 / 单曲循环 / 列表循环 / **随机**。**没有随机播放的播放器不可用** |
| **播放队列操作** | 查看 / 拖拽排序 / 插入下一首 / 从队列移除。这是「播放器」的核心交互，不是加分项 |
| **桌面集成** | MPRIS（Linux）/ 全局媒体键 / 托盘 / 系统媒体控件。**没有 MPRIS，键盘媒体键和桌面面板的播放按钮全都不工作**。虽然它和 v3 的 SMTC / MediaSession / NowPlaying 同属一层，但 Linux 自用场景下它是刚需，且成本低（几个 D-Bus 接口 + 一个托盘图标） |

**B++ · 内容域扩展**（2026-09-26 补入 —— 参考 `NEORUAA/MeiloX`）

> 来源：<https://github.com/NEORUAA/MeiloX>（119★，Kotlin/Compose，**GPL-3.0** ✅ 与项目兼容）。
> 它独立选用了 **`Kyant0/AndroidLiquidGlass / Backdrop`** 与 **`amll-ttml-db`** ——
> 和我们的两个关键选型相同，是一次有力的独立验证。

| 项 | 成本 | 说明 |
|---|---|---|
| **各类详情页**（歌手 / 专辑 / 歌单 / 歌曲） | **低** | 都是「头部 + 列表」，复用现有组件。**但没有它们体验是断的**——搜索到一首歌想点进歌手，路径就断了 |
| **评论（阅读）** | 中 | 列表 + 分页 + 楼中楼。**注意：评论「发表」是写操作，与 §4.4 的只读约束冲突——v2 只读不写** |
| **播客** | 中 | 是**另一类内容形态**（节目/单集/订阅），不是音乐模型的复用。且需要「长音频记忆播放位置」 |
（原先列在此处的 **动态/社交**、**听歌识曲**、**歌词对外发布** 三项已按用户决定移入 v3，见 §18.2。）

**关于「设置/关于/日志页」**：用户未选中，但需说明——**设置页不是可选项**，EQ / 输出设备 / 响度目标 / 主题切换都要落点。关于页与日志页可选（日志页在自研开发阶段其实很实用）。

**C. 视觉完整度**

玻璃第 3 层（**感知背后内容的 shadow**）· 第 4 层（**触摸内发光及其向相邻元件蔓延**）· 背景明暗自适应（含 Apple 的「只翻转小元件」规则）· scroll edge effect · 实时 FFT 频谱 · 歌词视频导出

> **C 的战略价值**：第 3、4 层是**开源实现的空白区**（§9 复刻基准）。这也是「统一框架 + 自绘 UI」这条路线真正拉开差距的地方。

### 18.2 圈出去的（留给 v3）

**D. 平台铺开**：Windows（SMTC / 托盘）· macOS（NowPlaying / 菜单栏）· Android（MediaSession / 后台播放 / 通知）· iOS · 各平台打包签名公证

**排除理由：** 平台集成需要一次做对「平台层要暴露什么」（SMTC / MediaSession / NowPlaying 三套语义不同），现在做等于赌。而且它是纯扩张，不改变产品本身。**应该等 v2 把产品做实、知道平台层真正需要什么之后再做。**

> #### iOS：位置保留，但分发方式有硬约束（用户 2026-09-26 明确）
>
> **iOS 必须以「允许第三方安装」的方式分发，不上 App Store。**
>
> 原因：**GPLv3 与 App Store 存在已知冲突**——GPLv3 §7 禁止附加限制、§10 要求能够安装修改后的版本，而 Apple 的 DRM 与开发者协议正属于这类附加限制。VLC 当年即因此被下架，且至今未回归。
>
> 因此 iOS 版只能走**允许第三方安装的渠道**（侧载 / 自签名 / 欧盟 DMA 下的替代应用市场等）。
>
> **v3 开始前需要确认：** 具体可行的分发渠道、是否仍需 Apple 开发者账号、签名有效期对用户体验的影响（自签名通常 7 天过期，这是真实的产品问题）。

**E. 基础能力**：完整无障碍三平台桥接（Windows UIA / Apple NSAccessibility / Linux AT-SPI2）· 多音源插件 ABI · M3E 完整版（v1 是薄版）

**F. 从 v2 移出的内容域**（2026-09-26 用户决定）

| 项 | 为什么适合 v3 |
|---|---|
| **听歌识曲** | 需要**音频指纹算法 + 检索服务**——是独立子系统，不是页面，且与播放/音乐库主流程关系不大 |
| **歌词对外发布**（系统 / 车机 / 其他 App） | 需要**各平台原生桥接**（Android 的 Lyricon/SuperLyric、iOS 锁屏、macOS NowPlaying），**桌面 Linux 没有对应物得自己发明**。与 §18.2 D 的平台铺开是同层的事，一起做更划算 |
| **动态 / 社交信息流** | 关注关系 + 信息流，接口调用量大，与音乐主流程关系较弱 |

**排除理由：** 无障碍是专项工程（数月），且 v1 已建语义树骨架（M4e），桥接可以后补；插件 ABI 在只有一个外部音源需求时不值得。

### 18.3 自然过渡：目标是 Spotify「Mix」级别

**用户设定的质量基准：达到 Spotify 最新版自然过渡（「Mix」，2025-08-19 上线）的程度。**

它的机制（据官方与媒体报道）：

- **自动分析每首歌的 key 与 BPM**，并显示波形与节拍数据
- **节拍 / 速度匹配**——官方建议选速度相近的歌
- **调性匹配**——推荐 Camelot 调性相近的歌（DJ 用来避免调性冲突的系统）
- **过渡点可调**——用户按波形与节拍数据选「最佳切换时刻」
- **可调的音量 / EQ / 效果曲线**，预设如 Fade、Rise
- Auto 模式（AI 驱动）与 Custom 模式

对照：Apple Music 的 AutoMix 明确使用 **timestretching + beatmatching**。

#### 分层拆解：每层要什么、Rust 有没有

| 层 | 需要什么 | Rust 可行性 |
|---|---|---|
| 1. 响度匹配的交叉淡入 | `ebur128` + 增益斜坡 | ✅ 简单 |
| 2. 节拍对齐 | BPM + 节拍相位 | ✅ `stratum-dsp` / `beat-track-rs` |
| 3. **乐句对齐**（4/8/16 小节边界） | **downbeat 检测** | ⚠️ 需 `beat-this`（ML + ONNX + 10.5MB 模型） |
| 4. **调性匹配**（Camelot） | key 检测 | ✅ `stratum-dsp`。**但 key 检测是这几项里最不可靠的** |
| 5. **速度对齐** | 时间拉伸 | ✅ `timestretch`（纯 Rust）或 `signalsmith-stretch` |
| 6. EQ 曲线 / bass swap | 双二阶 + 斜坡 | ✅ `biquad` |
| 7. **编排**（何时切、切多久、曲线怎么走） | 自研 | ⚠️ **有参考实现，见下** |

#### 编排层有现成实现（更正）

**本文档早先写过「编排没有现成的」。这是错的。**

`NEORUAA/MeiloX`（GPL-3.0 ✅）里有完整的实现：

| 文件 | 内容 |
|---|---|
| `playback/StableDeckPlayer.kt` | **双 deck 播放器**——交叉淡入的物理前提 |
| `playback/AutoMixController.kt`（30KB） | 编排控制：`prepareNext` / `monitorPosition` / `ensureSmartPlan` / `standbyReadyForTransition` / `beginTransition` / `finishTransition` / `cancelTransition`，含失败重试与状态清理 |
| `playback/BeatNetAutoMixAnalyzer.kt` | 节拍分析（AutoMix 的输入） |
| `playback/*Test.kt`（20+ 个） | 决策逻辑的测试覆盖 |

**含义：我们不必从零设计编排层。** 用户决定：**以它为参考、用 Rust 自行实现**（Kotlin → Rust 是重写，不是移植）。

**最重要的一点**：它正好印证了 §6.1 的判断——「v2 要改音频回调，从两个 ring 取数并混合」。`StableDeckPlayer` 就是那个结构的成品。

**但要注意它的局限**：它是 Android + ExoPlayer 专用的，`StableDeckPlayer` 直接持有 `ExoPlayer` 实例。**架构思路可学，代码不可直接搬。**

**已核实的组件（许可全部干净）：**

| crate | 版本 | 许可 | 给什么 | 代价 |
|---|---|---|---|---|
| `stratum-dsp` | 1.0.0 | **MIT / Apache-2.0** ✅ | BPM + 调性 + 节拍（tempogram / novelty / chroma，**纯经典 DSP**） | 无模型文件 |
| `beat-this` | 1.0.0 | **MIT** ✅ | 节拍 + **downbeat**（ML 推理） | **内嵌 10.5MB ONNX 模型**，需 ONNX 运行时 |
| `timestretch` | 0.15.0 | **MIT** ✅ | 纯 Rust 时间拉伸 | 作者自述「optimized for EDM」 |
| `beat-track-rs` | 0.1.2 | Apache-2.0 ✅ | Ellis 2007 动态规划节拍跟踪 | 0 star，53 下载 |

#### ⚠️ 最容易被低估的一点：这是一条分析管线，不是一个 DSP 函数

**做乐句对齐，必须在切换发生前就知道下一首歌的 downbeat 网格**——而那首歌可能还没播到那里。

```
歌进入队列
    ↓
后台下载 → 分析（BPM / 调性 / 节拍 / downbeat）→ 存进 SQLite
                                    ↓
                    播放时读分析结果 → 编排过渡
```

**Spotify 能做到是因为它后端已把全部曲目分析过。我们必须在本地做等效的事。**

分析一首 4 分钟的歌（tempogram + chroma + ML 推理）约需**数秒到数十秒**。因此：

- 必须**后台**跑，不能阻塞播放
- 必须**持久化缓存**（分析结果进 SQLite，按 track id 索引）
- **必须能在尚未分析完时优雅降级**
- 用户决定：**后台常驻、逐步铺开**——常用的（听过的、收藏的、歌单里的）先分析，
  空闲时慢慢扫全库。低优先级、可中断、可续传
- **播放绝对优先**：本机是 4 核 A8-7650K，实测负载已 5.74、quickshell 占 48% CPU。
  分析任务必须让路给音频线程，否则会直接违反 §16.2 的「xrun 必须为 0」

#### 默认范围（用户决定 2026-09-26，经 Apple AutoMix 调研后细化）

| 场景 | 默认 | 用户可否开启 | 模式 |
|---|---|---|---|
| **专辑顺序播放** | **关** | ✅ 可以 | Apple AutoMix 式 |
| **通用情况**（电台 / 私人FM / 每日推荐 / 搜索后播放 / 全部歌曲随机） | **开** | —（已开） | **Apple AutoMix 式**：全自动，无调节 UI |
| **我喜欢的音乐**（网易云自带那个歌单） | **开** | —（已开） | **Spotify Mix 式**：可调（见下） |
| **其他歌单** | **关** | ✅ 可以 | Apple AutoMix 式 |

**「默认关」不等于「不能开」**——它们只差默认值，用户都能开。四个场景**全部允许开启**，
区别只在默认状态与所用的模式。

**规则完备性**：凡是未被例外命中的场景，走「通用」。所以没有悬空的场景。

> **待确认的一处推断**：上表把「专辑」与「其他歌单」在被用户开启时归为
> **Apple AutoMix 式（全自动）**——因为 Spotify Mix 那套可调模式是目前只为
> 「我喜欢的音乐」定的。若用户希望**任何歌单**开启后都进入可调模式，
> 上表第二列的「模式」需要改；那会把编辑器 UI 的适用范围从 1 个场景扩到 N 个，
> 是工程量上的实质变化。**标记为待用户确认。**

> **这个切法解决了一个看似二选一的矛盾。** 早先的讨论把「Apple AutoMix」与
> 「Spotify Mix」当成两种候选方案在比——但它们**不是同一个东西的两套实现，而是
> 两种产品形态**：前者全自动无 UI，后者带一整套编辑器（波形、节拍网格、曲线）。
> **工程量差一个数量级。** 分配到不同场景后，两者可以并存。

##### 为什么「我喜欢的音乐」单独给 Spotify Mix

那是用户自己攒的、最常用的集合，「我想按我的意思排顺序」的需求在这里最强。
而电台/日推那种用户本就不控制顺序的场景，全自动就够。

#### ⚠️ Apple AutoMix 调研的关键输入（2026-09-26）

调研了 Apple Music 的 AutoMix，以下几条直接影响本节的设计：

**1. Apple 官方有一份逐字的「何时不混」清单**（支持文档 105067）：

> "AutoMix may not dynamically transition between songs if:"
> - "**Your album is played in sequential order.**"
> - "The genres or tempos are incompatible."
> - "Your content is uploaded from your music library."
> - "Your songs are purchased and downloaded from the iTunes Store."

**2. 那句「专辑顺序播放」的落点是「播放状态」，不是「专辑边界」。**
官方主语是 `Your album`、谓语是 `is played in sequential order`。媒体转述成
"wouldn't fade **between different albums**" 是**放宽了语义**。
→ 我们的判据应当是「**队列来源是一张专辑 且 顺序未被打乱**」，而非
「相邻两首属于不同专辑」（后者会在「两张专辑的歌混在一个歌单里」时误伤）。

**3. Apple 从不定义任何阈值**，官方只有定性的 "when appropriate"。

第三方观察到一些边界数值：放行到 **0.75x–1.5x 的速度变化**、吃掉
**20 秒到整分钟**的内容、**渐弱尾奏不受保护**（ABBA《Keep An Eye On Dan》
尾部和弦被盖掉，具体可复现，来源 6–7 个独立）。

> ⚠️ **但这些数字的语气必须标对**（调研者的置信度标定）：
>
> | 论断 | 强度 | 语气 |
> |---|---|---|
> | Apple 的作用域门槛窄（仅 catalog 流媒体） | **高** | 陈述事实，带引用 |
> | Apple 在某些曲目对上**确实**产出难听结果 | **高** | 陈述事实，带引用 |
> | Apple 的配对门槛**偏松** / 是二元分类器 | **中**（推断） | 标为推断 |
> | 「效果**可量化地**不好」 | **无法支撑** | **不要写** |
>
> **决定性缺陷：没有基数测量。** 没有任何来源统计过 Apple 实际混了多少比例、
> 拒了多少比例。**「混 95% 其中 20% 难听」与「混 30% 其中 5% 难听」会产生
> 完全相同的文章**——公开材料无法区分。所以「配对松」是推断，不是测量结果。
> 另有一处混淆：**±25% 变速在 DJ 语境是常规操作**，部分「太松」的抱怨
> 可能只是**审美分歧**，不是功能失败。

**修正后的表述（推断，中等置信度）**：

> **Apple 的配对门槛看起来是一个「二元分类器」（同类 / 异类），而不是连续评分。**
> 跨 genre / tempo 类别时它收得很紧（**大量拒混**——官方明说
> "genres or tempos are incompatible" 就拒；非电子类用户反馈 "doesn't seem
> to be working much"），**但一旦判为「同类」，它就不再评估这一对具体有多匹配，
> 而是全力执行到底**——于是出现 0.75x/1.5x 与吃掉一分钟尾奏。

这个表述能同时解释两个方向的证据，而「配对松」不能。

→ 对我们的启发不是「抄一个更紧的数字」，而是：**别做二元分类器，要做连续评分**
（见下第 7 条）。

**4. 最强的间接证据：Apple 在 iOS 27 重做了算法。**
「improved the underlying algorithms to generate new transition types」
**等于官方承认 iOS 26 的输出不达标**。一个拥有完整目录分析数据、专属 entitlement、
还做过真人 demo 的团队，一年后仍要重做——**说明这个问题比它看起来难，我们宁可偏保守。**

**4.5 iOS 27 的「new transition types」——没有任何具体描述。**
查过 MacRumors / 9to5Mac / MacObserver / MBW / it-boltwise 等，
**均未命名、未描述任何一种 transition type**（没有 filter sweep、
没有 echo out、没有 loop）。所以无法判断它是在「做得更花」还是
「判断得更准」——**而对我们来说只有后者可借鉴**。这条查不到。

**5. ⚠️ 但我们不能照抄它的判定逻辑，只能照抄它的判定姿态。**

Apple 的门槛是「**在不在订阅目录里**」——**二值的、外部的、100% 可靠的**。
我们的门槛必须是「**分析置信度**」——**连续的、内部的、会出错的**。

> **这是两者最重要的结构性差异。** 而且 Apple 根本没有尝试过本地分析（它直接
> 放弃了本地文件与已购内容），所以我们**无法从它的做法里推断本地分析是否可行**。
> **我们必须自己在本地做全部分析，且判据要比 Apple 更保守**——因为它不会判断错，
> 而我们会。

**6. 反过来，我们有一点能做得比 Apple 好。**
Apple 用「是不是专辑」作为「**用户的顺序意图**」的代理变量，因为它拿不到更细的信号。
**我们的播放器拿得到**——我们知道队列是怎么来的、用户有没有手动排序或 shuffle。
所以该用更准的判据，而不是照抄「是不是专辑」。

**7. ❗❗ 最重要的发现：Apple 的失败是「不对称」的，我们要做它的镜像。**

Apple 的**作用域门槛很窄**（只对订阅目录流媒体），**配对门槛却很松**（放行
0.75x–1.5x 的速度变化）。**这是最差的组合**：

- **正向失败**（混得难听）：见到了功能的用户常常不满意
- **反向失败**（该混却没混）：**大量用户根本见不到这个功能**

反向失败的证据很丰富：

| 类别 | 证据 |
|---|---|
| **内容门槛导致功能缺席**（最大宗） | MacRumors 用户原话 *"Requires Apple Music subscription. **Not available for purchased music. Lame!**"*；Apple 官方讨论区有主题帖名为 *"AutoMix not working on iTunes purchases"*；有评测者买了 CD、被识别并转成 Dolby Atmos，**"no matter what I tried"** 仍不工作 |
| **降级过头 → 感知不到功能存在** | 「降级为普通 fade 后**与 Crossfade 无从区分**」；*"for more traditional music it doesn't seem to be working much for me"* |
| **状态被静默破坏** | 开 Dolby Atmos 下载后 AutoMix 整个消失；iOS 更新静默重置开关；手动快进打断准备阶段 |
| **正面反证** | 一位从业 40 年的职业 DJ 称它 *"the best thing to happen to Apple Music since... ever"* |

> **我们要做的不是「更保守」，而是它失败画像的镜像：**
> **作用域门槛放宽**（本地全部分析，让绝大多数曲目对进入候选），
> **配对门槛收紧**（不确定就不混）。
>
> **收紧只施加在「这一对要不要混」上，不施加在「这首歌要不要进候选」上。**
> 无差别地全面取紧，会同时得到 Apple 的两个毛病。

**v2 设计必须为「该混却混了」与「该混却没混」两侧都定验收标准**——
这正是 §18.3 早先那句「反向风险」的精确化。

**8. 几条来自第三方实测的实现约束（同一篇 iPhone Mania 观察）**

- **暂停会取消过渡且不可恢复**：*「AutoMix 动作中暂停，继续播放后效果就没了」*
  → **实现约束**：中途 seek / pause 必须能**干净地取消并回退**，
  不能留下半混合的音频状态。
- **用户自建歌单若照专辑顺序排列，同样不混**
  → **对「专辑顺序」语义辨析的强独立佐证**：一个用户自建歌单都能触发禁用，
  说明门槛是**顺序检测**，**不是「这是不是专辑」**。
- **tempo 差越大，重叠时间反而越长**（Apple 未解释原因，调研者不推测）。
- **三个真实过渡实例**跨越三档：约 40 秒内速度渐变下降 / 结尾速度上升 / **几乎不重叠**
  → **三档降级在真实使用中确实都在发生**。
- **「不足 15 秒不激活」这条不可用作阈值**：该测试跑在 iOS 26 **公测版**、
  作者自认实验「刁钻」（人为截断）、**试验次数未披露**、且**早于 iOS 27 的算法重做**。
  它只说明「**需要一段预留时间做对齐**」这个定性事实。

> ⚠️ **默认全局开 = 系统必须自己判断「这两首能不能混」。** 因为默认开意味着它会在
> 用户没预谋的时候改变音乐，而**失败形态是「难听」而不是「没效果」**。所以那个
> 判断函数是必需品，不是优化。

**判断函数的输入信号：**

| 信号 | 来源 | 判据 |
|---|---|---|
| 双方 BPM + 置信度 | 分析管线 | 置信度低 → 不混 |
| 双方调性（Camelot 距离） | 分析管线 | 距离过大 → 不混 |
| downbeat 置信度 | `beat-this` 的 ML 输出 | 低 → 不做乐句对齐，退普通交叉淡入 |
| **内容类型** | **API 元数据** | **播客 / 长音频 → 永不启用** |

最后一行最好办：**播客在 v2 范围内（§18.1 B++），而它是可以从播客 API 直接知道的**，
不需要靠启发式猜。**AI 混音混进播客是灾难，不是功能。**

#### 歌单的 Spotify Mix 式模式（用户决定 2026-09-26）

歌单场景要做成 Spotify Mix 那种可调模式，而不只是自动播放。对照 Spotify 的 Mix，它包含：

- **逐曲显示 BPM 与调性**，以及波形与节拍数据
- **过渡点可调**——用户按波形与节拍数据选择切入点
- **Auto / Custom 两种模式**
- **预设曲线**（Fade、Rise）+ 可调的音量 / EQ / 效果曲线
- **允许重排**以让相邻曲目相配

> ⚠️ **这已经不只是音频功能，是一套编辑器 UI**：波形绘制、节拍网格叠加、曲线编辑、
> 每段过渡的持久化覆盖。**它需要自己的设计 pass，不能靠本节几行带过。**
> 归入 v2 设计文档单独展开。

#### 降级链（必需，不是可选）

Spotify 自己说明这类功能**只对「为无缝切换而制作的音乐」效果最好**（house / techno / trance / disco），对 acoustic / rock / ballad 会退化。加上分析可能永远完不成（网络慢、格式怪、用户直接跳歌）：

```
1. 两首都已分析 + 速度接近 + 调性相容
      → 完整过渡（节拍对齐 + 乐句对齐 + 调性匹配 + 时间拉伸）
2. 都分析了但速度差太多
      → 不做时间拉伸，只做乐句对齐的淡入淡出
3. 只分析了一边
      → 响度匹配的交叉淡入
4. 都没分析
      → 普通短交叉淡入
```

#### 成本评估（诚实说）

**这是 v2 音频引擎里最大的一块，可能占其三分之一以上。** 它横跨三个子系统：

```
分析管线（新）→ 持久化缓存（数据库）→ 播放编排（改音频回调）
```

对照 §6.2 的实施顺序，它排在最后是对的——**它依赖前面全部**（时间拉伸、响度、以及 v1 预留的两槽队列）。

### 18.4 音频取向：处理模式 / 直通模式（2026-09-26 定）

**DSP 丰富与 bit-perfect 天然互斥**——任何 DSP（EQ / 响度归一化 / 变速 / 交叉淡入）都会破坏 bit-perfect。**用户决定两者以显式开关共存。**

| 模式 | 内容 | 默认 |
|---|---|---|
| **处理模式** | PCM graph 全开：EQ / 响度归一化 / 变速 / 交叉淡入 / 变调 | ✅ **默认** |
| **直通模式** | 追求 bit-perfect，全部处理旁路 | |

**默认取处理模式的理由**：直通模式对普通内容没有可感知收益，而处理模式的收益是即时的。

#### ⚠️ 直通模式要真正做到，需要四件事同时成立（缺一不可）

| 条件 | 说明 |
|---|---|
| PCM graph 全旁路 | 不挂任何 `PcmStage` |
| **不重采样** | 源采样率直通设备。设备不支持该采样率时**明确报错，而不是静默重采样** |
| **独占设备访问** | Linux：ALSA `hw:` 而非 `default`/PipeWire；Windows：WASAPI Exclusive；macOS：Hog Mode |
| **不做格式转换** | 源位深直通。**注意：我们 M1 的 `AudioRing<i16>` 与 cpal 回调里的 `i16 → f32` 转换，两处都破坏 bit-perfect** |

#### 对现有设计的影响（这是 v2 要认真做的部分，不是加个开关）

- `AudioRing<i16>` 需泛化，或增加一条原生格式（`i32` / 源位深）的路径
- cpal 回调里的 `i16 as f32 / i16::MAX as f32` 必须按模式分流
- **独占模式需要平台原生后端**——cpal 不保证 WASAPI Exclusive / ALSA `hw:` 的可用性。**这正是 §6 里「未来加 PipeWire / WASAPI 原生后端」的真正触发点**

#### 与 M1 的关系

**M1 不做这件事**（出口标准只是「能出声」），`AudioRing<i16>` 与格式转换保持不变——**不为它预留结构**。v2 接入时改造 `dsls-audio` 的内部格式通路即可，调用方不受影响。

### 18.5 v1 为此预留的东西

| 预留项 | 位置 |
|---|---|
| PCM graph 挂载点（**实做**，空的 passthrough） | §6.1 / M1 Task 8 |
| 当前曲 + 下一曲两槽队列（**只记扩展点**） | §6.1。v2 要改音频回调，v1 无法免费预留 |
| 无障碍语义树骨架（**实做**） | M4e。事后补等于重写所有 widget |
| `RenderTier` 能力分级（**实做**） | §17。v2 的多平台要靠它 |

**明确「不预留」的一项**：`AudioRing<i16>` 与 cpal 回调的格式转换 —— 直通模式需要它们改造，但 M1 不为它留结构（§18.4）。理由是改造局限在 `dsls-audio` 内部，调用方无感，**预留反而是写没人用的代码**。

---

## 附录 A：决策记录

| 决策 | 选择 | 被否决 | 关键理由 |
|---|---|---|---|
| 首发平台 | Linux | 其他 | 开发机即 Linux，反馈循环最短；内核占七成工作量且与平台无关 |
| UI 技术栈 | 全 Rust，自建 UI runtime | Flutter / Tauri / Iced / Slint | 需要完全控制 pass 拓扑做液态玻璃（own-loop / 共享 backdrop / damage tracking / RenderTier）；不接受浏览器架构。（**原记录的「不接受 GPL」已作废**——项目即 GPLv3，见 §14.1；Slint 的出局理由改为纯技术性） |
| 平台层 | SDL3 | winit | SDL3 的 IME 与 popup 支持更完整，移动端生命周期更成熟 |
| 渲染层 | wgpu | SDL_GPU | Android GLES 兜底 + Rust 原生 shader 工具链 + 1.x 成熟度 |
| UI 模式 | retained mode | immediate mode | 播放器需要动画状态、滚动状态、文本选择、列表虚拟化 |
| blur 后端 | fragment | compute | wgpu GLES 后端没有 compute shader |
| 玻璃算法 | 从 Kyant0 上游移植 | vendor argui / 从零写 | 上游成熟有 release；argui 只有十五天大 |
| v1 功能 | 最小闭环 | 完整客户端 | 先排掉最大不确定性（网易云接口） |
| v1 歌词 | amll-ttml-db 打底 | 只走网易云 yrc | 一个 GET 拿到逐字 TTML，不碰接口、不需登录、效果更好 |
| 音源抽象 | 独立 crate + 本地文件第二实现 | 只留 trait 边界 / 插件 ABI | 单一实现的抽象大概率是错的；插件 ABI 是大工程 |
| 主题 | Glass 完整 + M3E 薄版 | 只做一套 / 两套都完整 | 薄版只需实现 ①②③ 三个差异点即可压弯抽象 |
| **项目许可证** | **GPLv3** | 闭源专有 | **由用户确定。**本文档早期误按「闭源分发」推导 §14.1，据此错误否决了全部 GPL/AGPL 参考实现——该前提从未经用户确认。已更正 |
| 文本引擎 | parley + swash | cosmic-text + glyphon / 自研 | parley 原生给「保留小数的逐字形 x/y/advance + cluster 映射」，正是逐字歌词要的；glyphon 的 Buffer 抽象在逐字动画下属层级错误 |
| 文本光栅化 | swash（纯光栅化器） | skrifa + zeno / 自己写 | 能用，代价是字体解析两遍（实测影响很小） |
| pass 编排 | 手写 200–300 行 | engawa / rend3 / bevy_render | engawa v0.1 未实现 compute/blit；rend3 维护模式；bevy 太重。只借 bevy 的阶段词汇表与异步 PipelineCache |
