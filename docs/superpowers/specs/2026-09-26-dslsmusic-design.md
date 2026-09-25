# dslsmusic 设计文档

- 日期：2026-09-26
- 状态：草案，待定项见 §15
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
| 渲染层 | **wgpu** | 否决 `SDL_GPU`：SDL 官方对 Android 的 Vulkan feature 要求态度是「Android will almost certainly hate this」，有人建议直接放弃 Android。wgpu 有 GLES 3.0 后端可兜底 Android；naga 是 Rust 原生 shader 工具链（写 WGSL，无 DXC / SPIRV-Cross 的 C 依赖）；且是 1.x 成熟生态 |
| UI 模式 | **retained mode** | 否决 immediate mode（egui 式）：播放器需要动画状态、滚动状态、focus、文本选择、列表虚拟化、持久组件状态，immediate mode 全都做不了 |
| 文本引擎 | **待定**（见 §15.1） | 候选：parley vs cosmic-text + glyphon。原定 parley，但其配套渲染器 vello 已被否决 |
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
| slint | license 是 GPLv3 / 商业授权三选一，闭源分发要付费 |

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

### 4.4 登录：只读路径（产品硬约束）

> **SPlayer 官方文档明确警告：网易更改登录策略后，使用第三方客户端登录账号可能导致封号。**

因此：

- v1 登录只做**只读路径**（扫码 / UID）
- **绝不做每日签到、打卡、云盘上传这类写操作**

这不是技术选择，是产品约束，不可协商。

### 4.5 数据契约

参考 MusicFree 的设计（其实现路线不适用——Rust 内嵌 JS 引擎代价太高——但契约值得翻译）：

- 统一 item：`id / platform / artist / title / duration / album / artwork / source / lyric`
- 音质统一四档：`low / standard / high / super`，各平台原始标识在 provider 内映射
- **provider 未实现的方法返回空结果而非抛错**，单个 provider 异常不阻塞其他
- 搜索用并发，单平台失败不影响整体

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

**主源：`amll-ttml-db`**（CC0-1.0，424 star，9429 commits）

社区维护的逐字 TTML 歌词库，按来源平台分目录（`ncm-lyrics/` / `qq-lyrics/` / `am-lyrics/` / `spotify-lyrics/` / `raw-lyrics/`）。每首歌同时提供 `.ttml` / `.lrc` / `.yrc` / `.qrc` / `.lys` / `.eslrc` 六种格式。

消费方式——**一个 HTTP GET**：

```
https://raw.githubusercontent.com/amll-dev/amll-ttml-db/refs/heads/main/ncm-lyrics/[歌曲ID].ttml
```

**这条的价值：v1 的歌词完全不碰网易云歌词接口、不需要登录、不受接口变动影响，效果还优于网易云官方客户端。** 成本几乎为零。官方要求署名。

**回退链（五级）：**

```
手动关联 → 本地 .lrc → 缓存 → provider → amll-ttml-db
```

**辅助源：** `lrclib`（免费、无需鉴权的同步歌词 API，只到行级，无逐字），作为网易云之外的兜底。

**注意：** amll-ttml-db 的 README 明确警告「Apple Music 自己的 TTML 分词和时间轴也不完全准确合理」，同时质量保证只覆盖有效的网易云 ID。不要盲目信任任何单一来源。

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

照搬 AMLL 的参数化设计，但渲染后端用 **SDF GPU 文字**而非 CSS mask（Rustle 已证明这条路在 wgpu 里可行）。

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

### 7.2 文本引擎

**待定**，见 §15.1。

约束条件：Rust + wgpu 自研渲染 + 需要逐字歌词的字形级定位 + 需要 CJK 断行 + 要跨 GLES 后端。

**不要自己写文本引擎。** CJK 断行禁则、bidi、字形级定位是深坑，这是自研 UI 里唯一不该自研的部分。

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

### 9.1 own-loop：唯一干净的捕获方案

**整个 UI 场景渲染到一张 offscreen intermediate texture**（`usage = RENDER_ATTACHMENT | TEXTURE_BINDING | COPY_SRC`），然后模糊其中若干区域，最后合成到 display target。因为 scene texture 本来就在我们手里，**捕获这一步的成本是零**。

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

### 9.6 格式与生命周期

- 中间 target 用 **`Rgba16Float`（线性空间）**，**不要用 `-Srgb` 格式**。`backdrop-blur-wgpu` 源码注释明确写了 "sRGB targets are rejected for the gamma intermediate (they would double-decode)"。8-bit 会在大面积渐变上 banding
- 合成用的 sampler：**ClampToEdge + linear**

### 9.7 backdrop cache 与 damage tracking

**这是最大的性能杠杆。** 参考 Hyprland：壁纸静态 + 窗口平铺时模糊结果预计算掉，每帧零成本；只有动态壁纸 / 浮动窗口才逐帧算。

**per-region backdrop cache：**

- key = (屏幕区域矩形, blur 参数)
- value = 已模糊的 texture（存在 texture pool 里）
- **per-target-format composite cache**（每个输出格式一套 pipeline，懒创建）
- **last-frame-used 淘汰**
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

**实施路径：** 从上游 Kyant0 移植算法（Apache-2.0，需保留署名），以 `ExtraBinoss/argui` 的 WGSL 移植作为**翻译参考**——它已经趟过一遍（预乘 alpha 处理、法线退化防护、边界采样 clamp）。

**注意：** argui 的 shader 是**注入式 ABI**（`argui_effect(uv, source, backdrop)` + `argui_param_f32(N)` 编号取值），不是可直接使用的文件。但数学部分只有约 40 行，移植到我们自己的 shader ABI 上是几天的工作量。

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
| **M0** | workspace 骨架 + SDL3 窗口 + wgpu 出三角形 | **验证 SDL3 → raw-window-handle → wgpu 这条路**（Linux 先行） |
| **M1** | 网易云登录（扫码）+ 搜索 + 取流 + 出声 | **能用最丑的方框听到歌** |
| **M2** | 本地文件 provider + 抽象被两个实现压过 | provider trait 定型 |
| **M3** | UI runtime：树 / 布局 / 文本 / 滚动 / 手势 / 动画 | 列表页能滚、能点、动画能跑，中文正常显示，IME 可用 |
| **M4** | 玻璃管线在播放页跑通 | 玻璃折射和高光达到预期效果 |
| **M5** | 逐字歌词 + 导航转场 | 播放页完整 |
| **M6** | M3E 薄版 + 热切换 | `DesignSystem` 抽象定型 |

**M1 是关键节点。** 它把最大的不确定性（网易云接口）先排掉，而不是先投时间在你能控制的部分（渲染）上。

**M3 里必须做的一件事：** 建无障碍语义树骨架。语义树是 widget 组装时产生的，**事后补等于重写所有 widget**。v1 不需要三平台桥接，但可以用一个打印语义树的调试工具先占位。

**M3 之前必须先做的工具：** headless 渲染 PNG 的 golden test 工具。没有热重载的情况下，这是唯一能让人忍受的 UI 迭代方式（见 §13 风险 7）。

---

## 13. 风险清单

| # | 风险 | 应对 |
|---|---|---|
| 1 | **SDL3 + wgpu 在 Android 的 surface 创建未实测** | M0 在 Linux 验证这条路；Android 尽早补验。走 SDL3 window properties 拿 `ANativeWindow` 指针喂给 wgpu，理论通但需实测 |
| 2 | **`sdl3` crate 是 0.20.0，pre-1.0** | 所有 SDL 调用包在 `platform/` trait 后面；缺 API 自己补 FFI（§11.3） |
| 3 | **网易云接口非官方，且上游会被法务抹掉** | 见下方专条 |
| 4 | **无障碍成本** | v1 只建语义树（M3），不做三平台桥接。Windows UIA / Apple NSAccessibility / Linux AT-SPI2 是后置的专项工作 |
| 5 | **CJK 字体必须自带** | Noto Sans SC（OFL 协议），逐字歌词要多字重，打包体积要有预期 |
| 6 | **Rust 不等于轻** | Rustle 自称内存 250MB（对比 Electron 500MB+）。iced + wgpu + 大图缓存不便宜。backdrop texture 必须有明确生命周期和上限（§9.7） |
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

### 14.1 许可红线（硬约束——项目未来要闭源分发）

| 许可 | 能否使用 | 说明 |
|---|---|---|
| WTFPL / MIT / Apache-2.0 / CC0 | ✅ 可以 | 需保留署名（Apache-2.0 / CC0 要求） |
| **无 license** | ❌ 不可用 | 默认保留全部权利，不能抄任何代码 |
| **GPL / AGPL** | ❌ 不可抄代码 | **传染性**，会把整个项目拖下水。只能读思路，且要谨慎 |

**明确标记为「不能碰代码」的：**

- **`Fei-xiangShi/Rustle`（AGPL-3.0）** —— 和我们技术栈最像，最容易产生「抄两段」的冲动。**只读思路，不抄代码。**
- **AMLL（AGPL-3.0）** —— 同上。算法参数照搬没问题（那是事实性知识），代码不行。
- `naughtyduk/liquidGL`、`BarredEwe/LiquidGlass` —— **无 license**，不可用
- `slint` —— GPLv3 / 商业授权，已否决
- `Cider 1`、`SPlayer`、`listen1`、`MusicFree` —— AGPL / 其他，只读思路

### 14.2 可直接依赖

| 库 | 许可 | 用途 |
|---|---|---|
| `ncm-api-rs` | WTFPL | 网易云接口 + eapi 加密 |
| `ttml_processor` | MIT | TTML 逐字歌词解析（需 pin 版本，作者自述 unstable） |
| `symphonia` | MPL-2.0 | 音频解码 |
| `cpal` | Apache-2.0 | 音频输出 |
| `rusqlite` | MIT | 数据库 |
| `lofty` | MIT/Apache-2.0 | 本地音乐标签与封面 |

### 14.3 读源码抄思路（不看代码，看设计）

| 项目 | 许可 | 看什么 |
|---|---|---|
| **iced** `wgpu/src/quad.rs` | MIT | 图元层的正确形状（见 §3.1） |
| **epaint** | MIT/Apache-2.0 | feathering 抗锯齿、预乘 alpha |
| **Rustle** | AGPL ⚠️ | SDF GPU 文字做逐字歌词的可行性、弹簧参数建模、iced 里嵌自定义 shader、平台集成抽象 |
| **AMLL core** | AGPL ⚠️ | 逐字渲染算法参数（§5.4） |
| **Linn** `utils/lyric_parse.rs` | MIT ✅ | YRC 容错解析 + **测试夹具可直接复用** |
| **termusic** | MIT ✅ | 双二进制 + Protobuf + 可插拔播放后端（跑了 5 年的成熟范例） |
| **app_rhyme / music_api** | MIT/Apache ✅ | 音源层做成独立 crate 的范例 |
| **Kyant0/AndroidLiquidGlass** | Apache-2.0 ✅ | **折射算法源**（§9.10） |
| **argui** | MIT/Apache-2.0 ✅ | WGSL 移植参考、`argui-layout/src/paint/effects.rs` 看 effect 怎么接进 paint 流程 |

### 14.4 关于 argui 的重要说明

`ExtraBinoss/argui` 是一个 Rust UI 框架，**架构和我们设计的几乎完全一致**（retained tree / flex-grid 布局 / 文本整形 + IME / 无障碍语义 / 多窗口 / 热重载 / 移动端入口），且已经有 `liquid_glass.wgsl`。

**但它不能作为基座依赖：** 仓库创建于 2026-09-01，提交集中在 09-05 到 09-20，**十五天大的单人项目**，32 star，1 fork。风险等级比已被否决的 vello 更高。

**正确用法：** 作为**参考实现**读（许可干净，MIT/Apache-2.0），shader 算法从上游 Kyant0 移植。不要依赖。

---

## 15. 未定项

### 15.1 文本引擎：parley vs cosmic-text + glyphon

**状态：待调研结论。**

约束：Rust + wgpu 自研渲染 + 需要逐字歌词的字形级定位 + 需要 CJK 断行 + 要跨 GLES 后端。

已知的取舍线索：

- **parley** 是 Linebender 的文本引擎，但其配套渲染器 **vello 已被否决**
- **cosmic-text + glyphon** 提供了完整的 wgpu 路径：cosmic-text 的 shaping 后端已从 rustybuzz 换成 **HarfRust**，光栅化交给 swash；**glyphon 渲染到「已存在的 render pass」**（遵循 wgpu wiki 的 middleware 模式），不额外开 pass，这对我们很友好。MSRV 1.92
- 附带约束：**glyph atlas 上传不能用 compute**（因为 GLES 后端没有 compute shader，见 §9.4）

待确认：parley 脱离 vello 单独用是否可行、它的字形级输出能否支撑逐字高亮动画。

### 15.2 其他待定

- 玻璃 shader 的完整实现细节（调研进行中）
- render graph / pass 编排的具体形态（调研进行中）
- `glass-research` 的完整结论（Apple 官方对 Liquid Glass 的视觉成分定义、更多实现的取舍）

---

## 附录 A：决策记录

| 决策 | 选择 | 被否决 | 关键理由 |
|---|---|---|---|
| 首发平台 | Linux | 其他 | 开发机即 Linux，反馈循环最短；内核占七成工作量且与平台无关 |
| UI 技术栈 | 全 Rust，自建 UI runtime | Flutter / Tauri / Iced / Slint | 需要完全控制渲染管线做液态玻璃；不接受浏览器架构；不接受 GPL |
| 平台层 | SDL3 | winit | SDL3 的 IME 与 popup 支持更完整，移动端生命周期更成熟 |
| 渲染层 | wgpu | SDL_GPU | Android GLES 兜底 + Rust 原生 shader 工具链 + 1.x 成熟度 |
| UI 模式 | retained mode | immediate mode | 播放器需要动画状态、滚动状态、文本选择、列表虚拟化 |
| blur 后端 | fragment | compute | wgpu GLES 后端没有 compute shader |
| 玻璃算法 | 从 Kyant0 上游移植 | vendor argui / 从零写 | 上游成熟有 release；argui 只有十五天大 |
| v1 功能 | 最小闭环 | 完整客户端 | 先排掉最大不确定性（网易云接口） |
| v1 歌词 | amll-ttml-db 打底 | 只走网易云 yrc | 一个 GET 拿到逐字 TTML，不碰接口、不需登录、效果更好 |
| 音源抽象 | 独立 crate + 本地文件第二实现 | 只留 trait 边界 / 插件 ABI | 单一实现的抽象大概率是错的；插件 ABI 是大工程 |
| 主题 | Glass 完整 + M3E 薄版 | 只做一套 / 两套都完整 | 薄版只需实现 ①②③ 三个差异点即可压弯抽象 |
