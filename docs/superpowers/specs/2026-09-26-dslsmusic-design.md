# dslsmusic 设计文档

- 日期：2026-09-26
- 状态：**已评审通过**。技术选型全部落定；三项产品/顺序决策经确认（见下方）
- 未决项：见 §15（均为非阻塞的补充调研与待验证假设）

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
| 渲染层 | **wgpu** | 否决 `SDL_GPU`：SDL 官方对 Android 的 Vulkan feature 要求态度是「Android will almost certainly hate this」，有人建议直接放弃 Android。wgpu 有 GLES 3.0 后端可兜底 Android；naga 是 Rust 原生 shader 工具链（写 WGSL，无 DXC / SPIRV-Cross 的 C 依赖）；且是 1.x 成熟生态 |
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
- **vibrancy / saturation 在降采样 pass 里逐级施加，不是最后统一施加**（Hyprland 的非显然技巧）。让饱和色逐 pass 扩散生长，避免 banding；每级除以 `1.0 / passes`。参考实现：Hyprland `src/render/shaders/glsl/blur1.glsl`

### 9.6 格式与生命周期

- 中间 target 用 **`Rgba16Float`（线性空间）**，**不要用 `-Srgb` 格式**。`backdrop-blur-wgpu` 源码注释明确写了 "sRGB targets are rejected for the gamma intermediate (they would double-decode)"。8-bit 会在大面积渐变上 banding
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
| **M0** | workspace 骨架 + SDL3 窗口 + wgpu 出三角形 | **验证 SDL3 → raw-window-handle → wgpu 这条路**（Linux 先行） |
| **M1** | 网易云登录（扫码）+ 搜索 + 取流 + 出声 | **能用最丑的方框听到歌** |
| **M2** | 本地文件 provider + 抽象被两个实现压过 | provider trait 定型 |
| **M3** | UI runtime：树 / 布局 / 文本 / 滚动 / 手势 / 动画 | 列表页能滚、能点、动画能跑，中文正常显示，IME 可用 |
| **M4** | 玻璃管线在播放页跑通（移植 Kyant0 的折射 + 高光 + 阴影三层） | 玻璃折射、高光、阴影达到预期效果 |
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

### 14.1 许可红线（硬约束——项目未来要闭源分发）

| 许可 | 能否使用 | 说明 |
|---|---|---|
| WTFPL / MIT / Apache-2.0 / CC0 | ✅ 可以 | 需保留署名（Apache-2.0 / CC0 要求） |
| **无 license** | ❌ 不可用 | 默认保留全部权利，不能抄任何代码 |
| **GPL / AGPL** | ❌ 不可抄代码 | **传染性**，会把整个项目拖下水。只能读思路，且要谨慎 |

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
| **Rustle** | AGPL ⚠️ | SDF GPU 文字做逐字歌词的可行性、弹簧参数建模、iced 里嵌自定义 shader、平台集成抽象 |
| **AMLL core** | AGPL ⚠️ | 逐字渲染算法参数（§5.4） |
| **Linn** `utils/lyric_parse.rs` | MIT ✅ | YRC 容错解析 + **测试夹具可直接复用** |
| **termusic** | MIT ✅ | 双二进制 + Protobuf + 可插拔播放后端（跑了 5 年的成熟范例） |
| **app_rhyme / music_api** | MIT/Apache ✅ | 音源层做成独立 crate 的范例 |
| **Kyant0/AndroidLiquidGlass** | Apache-2.0 ✅ | **折射算法源**（§9.10） |
| **argui** | MIT/Apache-2.0 ✅ | WGSL 移植参考、`argui-layout/src/paint/effects.rs` 看 effect 怎么接进 paint 流程 |
| **`iyinchao/liquid-glass-studio`** | MIT ✅ | **许可干净的可抄来源**：superellipse 圆角 SDF、Fresnel（§9.10） |
| ~~Reimagined-Glass~~ | **NONE-DECLARED ❌** | **不可用**（见 §14.5） |
| **Hyprland** `src/render/shaders/glsl/blur1.glsl` | GPL ⚠️ | dual Kawase 降采样参考实现 + **vibrancy/saturation 逐级施加**的技巧（§9.5）。**只读思路** |
| **`backdrop-blur-wgpu`** | 见 crate | **最贴近我们需求的架构参考**：两阶段 `prepare` → `record` 的 seam trait（因为 wgpu 走 Queue 上传而 glow 是 immediate mode）、按 target format 懒建 pipeline、sRGB 拒绝、Backdrop-Root rule、last-frame-used 淘汰。**注意它的 wgpu 后端目前只有可分离 Gaussian，dual Kawase 还是 TODO**。⚠️ **GitHub 主页 404——但源码可从 crates.io CDN 直接取**：`https://static.crates.io/crates/backdrop-blur-wgpu/backdrop-blur-wgpu-0.2.0.crate`（已验证可下载解包） |
| **Bevy render graph** | MIT/Apache ✅ | 阶段划分词汇表 + 异步 PipelineCache（§9.11）。不引入依赖 |
| **SIGGRAPH 2015** Marius Bjørge "Bandwidth-Efficient Rendering" + three.js PR #33920 | — | dual Kawase 原始出处与工程数据（§9.2） |
| **Slint** box shadow 缓存 | GPLv3 ⚠️ | `SkiaBoxShadowCache` / `FemtovgBoxShadowCache` 的模糊纹理复用思路。**只读思路不抄代码** |

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

### 14.5 不可用清单（无 license = 保留所有权利）

**代码和 shader 文本都不能进库。**

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
| SDL3 → `raw-window-handle` → wgpu 在 Linux 可行 | **M0** | 桌面端退到 winit（移动端仍留 SDL3） |
| 同上，在 **Android** 可行（走 SDL3 window properties 拿 `ANativeWindow`） | M0 之后尽早 | 需要实测，暂无干净退路——**这是最大的未验证风险** |
| parley 与渲染器的中立类型层能挡住 parley 的变动 | M3 | 换 cosmic-text，只改适配层（§7.2 第 4 条） |
| `ncm-api-rs` 的 eapi 加密正确 | M1 | 对照 `Cloud370/ncm-rs` / `ddy314/netease-music` 交叉验证；最坏情况自己 vendor 加密实现 |
| damage tracking 的收益值得复杂度 | M4 | backdrop cache 退化成「每帧全量重算 + last-frame-used 淘汰」 |

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
| 文本引擎 | parley + swash | cosmic-text + glyphon / 自研 | parley 原生给「保留小数的逐字形 x/y/advance + cluster 映射」，正是逐字歌词要的；glyphon 的 Buffer 抽象在逐字动画下属层级错误 |
| 文本光栅化 | swash（纯光栅化器） | skrifa + zeno / 自己写 | 能用，代价是字体解析两遍（实测影响很小） |
| pass 编排 | 手写 200–300 行 | engawa / rend3 / bevy_render | engawa v0.1 未实现 compute/blit；rend3 维护模式；bevy 太重。只借 bevy 的阶段词汇表与异步 PipelineCache |
