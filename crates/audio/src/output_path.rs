//! 输出路径的**能力模型** —— 以及 Linux 上「我们能不能拿到设备独占」的判定。
//!
//! 这个模块存在的理由，是 Task 10 做完之后量到的那条：
//! **`AudioEngine::is_passthrough() == true` 只说明「我们没重采样」** ——
//! 那 184 秒里 44.1kHz 的素材在 48kHz 的硬件上，是 **PipeWire 在替我们重采样**。
//! 也就是说「我们动了没有」与「系统动了没有」是**两个**问题，本模块回答第二个：
//! 我们实际拿到的那条路上，系统混音器在不在中间。
//!
//! # 能力（是能力表，不是愿望表）
//!
//! | 平台 | 现状 |
//! |---|---|
//! | Linux | ✅ 能直开硬件（ALSA 的 `hw:`）。判定见 [`probe_default_target`] |
//! | Windows | ❌ cpal 只走共享模式。独占要 WASAPI exclusive，本版没做 ⇒ 一律 `Shared` |
//! | macOS | ❌ cpal 不暴露 hog mode ⇒ 一律 `Shared` |
//! | Android / iOS | ❓ 未核实 ⇒ 一律按 `Shared` 报告，**不假装** |
//!
//! # ⚠️ 一条硬边界（必须跟着路径一起显示，见 [`OutputPath::caveat`]）
//!
//! **`Exclusive` 不等于「逐位精确」。** 见 [`OutputPath`] 的文档。
//!
//! # 为什么这里**不猜**「哪张卡是默认卡」
//!
//! brief 里写的做法是「把 `default_output_device()` 对应的物理卡映射到它的 `hw:` 名字」。
//! 这个映射**在进程内没有可靠的信号**（已实测，见 [`probe_default_target`] 的文档）：
//! cpal 的 `Device` 只给 `description()` / `id()`，不给「`default` 解析到了哪张卡」；
//! ALSA 自己那条 `Default ALSA Output (currently …)` 提示文本，在把 `default`
//! 定义成真实设备（`hw` / `plug`）的配置下**根本不出现**（本机实测：条目直接消失）。
//!
//! 于是只剩两条路：**猜**，或者**拿证据**。本机有三张卡（USB 摄像头 / 板载模拟 /
//! HDMI），猜错的后果是把声音送到一个没接音箱的口 —— 而采样率、xrun、时长**全都正常**。
//! 那正是本项目反复禁止的失败模式。所以这里只拿证据（[`probe_default_target`]），
//! 拿不到就**如实降级**到 `Shared`，并把「怎么指定」告诉用户。

use std::fmt;

use cpal::traits::{DeviceTrait, HostTrait};

/// ALSA 里**唯一**「系统插件不介入」的 PCM 前缀。
const HW_PREFIX: &str = "hw:";

/// 首选独占设备的环境变量名。见 [`preferred_exclusive_from_env`]。
pub const EXCLUSIVE_DEVICE_ENV: &str = "DSLS_AUDIO_DEVICE";

/// 我们**实际**拿到的那条输出路径。
///
/// ⚠️ **`Exclusive` 不等于「逐位精确」。** 它只表示「**我们直接对硬件写，
/// 系统混音器不在我们和设备之间**」。真正的逐位精确还需要**同时**满足三条：
///
/// 1. **格式与源一致**（我们解码出来的是 i16；设备跑 f32/i32/I24 时虽然是无损转换，
///    但它仍然是一次转换 —— 设备侧的格式见 `AudioEngine::output_format`）；
/// 2. **采样率与源一致**（不一致时是**我们**在重采样，
///    `AudioEngine::is_passthrough()` 会是 `false`）；
/// 3. **设备本身不做处理** —— 某些声卡有内部 DSP / 硬件混音器。这一条**从进程内
///    无法验证**，只能靠硬件级采集去证。
///
/// ⇒ 本类型能给的上限是：「我们没有重采样」+「系统混音器不在路径上」。
/// 超出这个范围的任何说法都是夸大。
///
/// 还有一条容易误解的：`hw:` 设备若是**多子流**的（例如 `snd-aloop` 有 8 个子流），
/// 别的进程仍可能同时打开同一张卡的另一个子流。那不算「混音器在中间」，
/// 但也不等于「这台设备只有我们在用」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputPath {
    /// 直接打开硬件设备（Linux 上是 `hw:`）。**系统不介入。**
    Exclusive { device: String, rate: u32 },
    /// 经系统混音器（`default`）。**系统可能替我们重采样。**
    Shared { device: String, rate: u32 },
}

const EXCLUSIVE_CAVEAT: &str = "独占（系统混音器不在我们与设备之间）。⚠️ **这不等于逐位精确**：\
     还要格式与源一致、采样率与源一致，且**设备自己不做处理**\
     （某些声卡有内部 DSP/硬件混音，从进程内看不出来）⇒ 只能说「到我们这一层为止没动过样本」。";

/// ⚠️ 这句话会跟着 `Shared` **每条路径**打印，而「列出本机可选设备」的日志只在
/// **自动选路**那一支（[`crate::preferred_exclusive`]）里打印 —— 用 `--device`
/// 点名设备时走的是 `new_on`，那条日志根本不出现。所以这里写成**条件从句**：
/// 承诺必须在所有会打印它的场合都成立，否则就是下一句「日志里会列出…」而日志里没有。
///
/// ⚠️ 还有**第二道**门：那份列表只在**有** `hw:` 设备时打印
/// （`crate::preferred_exclusive` 里按 `hardware_output_devices` 是不是空表分了
/// 两句，空表那句说的是「本机也没有可选的 `hw:` 输出设备」）⇒ 一台物理输出设备
/// 都没有的机器上，日志里不会出现任何列表，承诺得跟着补上这个条件。
const SHARED_CAVEAT: &str = "共享（经系统混音器）。**系统可能替我们重采样**，我们看不见也管不着 —— \
     这一档只能保证「我们交出去的是源采样率的样本」。要绕开系统混音器，\
     把 DSLS_AUDIO_DEVICE 设成一台物理设备（`hw:`）；走**自动选路**且没拿到独占时，\
     上面的日志里会列出本机可选的 hw: 设备（**本机有物理输出设备时**；\
     用 --device 点名设备时不会打印那份列表）。";

impl OutputPath {
    /// 系统混音器在不在我们和设备之间。
    pub fn is_exclusive(&self) -> bool {
        matches!(self, Self::Exclusive { .. })
    }

    /// 我们实际打开的设备（`hw:CARD=1,DEV=0` / `default` / …）。
    pub fn device(&self) -> &str {
        match self {
            Self::Exclusive { device, .. } | Self::Shared { device, .. } => device,
        }
    }

    /// 我们实际建流的采样率。
    pub fn rate(&self) -> u32 {
        match self {
            Self::Exclusive { rate, .. } | Self::Shared { rate, .. } => *rate,
        }
    }

    /// **必须跟着路径一起显示**的那句话：它保证什么、不保证什么。
    pub fn caveat(&self) -> &'static str {
        match self {
            Self::Exclusive { .. } => EXCLUSIVE_CAVEAT,
            Self::Shared { .. } => SHARED_CAVEAT,
        }
    }
}

impl fmt::Display for OutputPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exclusive { device, rate } => write!(f, "Exclusive（{device} @ {rate}Hz）"),
            Self::Shared { device, rate } => write!(f, "Shared（{device} @ {rate}Hz）"),
        }
    }
}

/// 这个名字是不是「直接开硬件、系统插件不介入」的 ALSA PCM。
///
/// ⚠️ **只有 `hw:` 算。** `plughw:` 名字里也含 "hw:"，但它前面是 `plug` ——
/// 那个插件**会替我们做格式/采样率转换**（它存在的唯一意义就是这个），
/// 所以它和 `default` 一样属于「系统在中间」。`dmix:` / `front` / `surround*` /
/// `iec958` / `hdmi` / `sysdefault` 同理，全都不是独占。
///
/// 判据是**前缀**，所以 `plughw:…` 天然被排除（它以 `p` 开头）—— 这条有测试钉住。
pub fn is_hardware_pcm_id(pcm_id: &str) -> bool {
    pcm_id.starts_with(HW_PREFIX)
}

/// 本平台**为什么**拿不到独占。`None` = 本平台会去尝试（Linux）。
///
/// 给 UI 用：降级可以，**假装不行** —— 用户有权知道是「这台机器上没有」
/// 还是「这个平台没做」。
pub fn platform_note() -> Option<&'static str> {
    #[cfg(target_os = "linux")]
    {
        None
    }
    #[cfg(target_os = "windows")]
    {
        Some(
            "Windows：cpal 只提供共享模式（WASAPI shared）。要独占得换用 WASAPI exclusive \
             后端，本版没做 ⇒ 输出路径一律是 Shared。",
        )
    }
    #[cfg(target_os = "macos")]
    {
        Some("macOS：cpal 不暴露 hog mode（CoreAudio 的独占需要它）⇒ 输出路径一律是 Shared。")
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Some(
            "这个平台（Android/iOS/其它）**没有核实过** cpal 能不能拿到设备独占 ⇒ \
             按 Shared 报告，不假装。",
        )
    }
}

/// 独占候选：`default` 背后的物理播放设备。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Candidate {
    /// 找到了 —— 这个名字可以直接喂给 `AudioEngine::new_on`。
    Found { pcm_id: String },
    /// 没找到。`reason` 是**给用户看的原因**，不是「不可用」三个字。
    Unavailable { reason: String },
}

/// 环境变量 [`EXCLUSIVE_DEVICE_ENV`] 的解析结果。
///
/// 三种状态要分开，因为**「没设」和「设错了」的处置完全不同**：
/// 前者静默走探针，后者必须告警（用户以为自己在用独占，其实没有）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvPreference {
    NotSet,
    /// 设了，且是 `hw:` 设备 ⇒ 用它。
    Hardware(String),
    /// 设了，但**不是** `hw:` 设备 ⇒ 不构成独占。附带给用户的说明。
    Invalid(String),
}

/// 读 [`EXCLUSIVE_DEVICE_ENV`]。
///
/// 只接受 `hw:` 前缀：这个变量的语义是「**首选哪台物理设备做独占**」，
/// 不是「换一台输出设备」。像 `pulse` / `default` 这种名字交给它不是「独占」，
/// 静默当共享设备用还会**背地里改掉系统默认路由** —— 所以这里判为 `Invalid` 并告警。
pub fn preferred_exclusive_from_env() -> EnvPreference {
    classify_env_value(std::env::var(EXCLUSIVE_DEVICE_ENV).ok().as_deref())
}

/// [`preferred_exclusive_from_env`] 的纯函数部分（测试不需要动环境变量 ——
/// 动环境变量在 edition 2024 里是 `unsafe`，而且会串到别的测试上去）。
fn classify_env_value(raw: Option<&str>) -> EnvPreference {
    let Some(v) = raw.map(str::trim).filter(|v| !v.is_empty()) else {
        return EnvPreference::NotSet;
    };
    if is_hardware_pcm_id(v) {
        EnvPreference::Hardware(v.to_owned())
    } else {
        EnvPreference::Invalid(format!(
            "{EXCLUSIVE_DEVICE_ENV}={v:?} 不是 `hw:` 设备，忽略它 —— \
             这个变量的意思是「首选哪台**物理**设备做独占」，不是「换一台输出设备」。\
             要用它请写成 hw:CARD=<卡号>,DEV=<设备号>（可选值见下面的日志）。"
        ))
    }
}

/// 判定 `default` 背后是哪张物理卡，返回它的 `hw:` 名字。
///
/// # 怎么判的（以及为什么只能这么判）
///
/// cpal **不暴露** ALSA 的 PCM 归属：`Device` 只有 `description()`/`id()`，
/// 没有「这个 `default` 解析到了哪张卡」。所以这里用的是**证据**，不是猜：
///
/// 1. 以本进程打开一次 `default`（一条**探针流**，不出声、不播放）；
/// 2. 趁它活着扫 `/proc/asound/card*/pcm*p/sub*/status`，找 `owner_pid == 本进程` 的
///    那条 —— ALSA 把「谁持有这个 PCM」写在那里；
/// 3. 找到了 ⇒ `default` 真的落在这张卡的**硬件 PCM** 上 ⇒ 它的
///    `hw:CARD=<卡号>,DEV=<设备号>` 就是独占目标；找不到 ⇒ `Unavailable`。
///
/// 找不到是**常态**：`default` 指向 PipeWire / PulseAudio 这类服务型后端时，
/// 硬件是**服务进程**持有的，不是我们 —— 从那一步就注定映射不出来。
///
/// # 这条判据**否定**了 brief 里的另一个做法
///
/// 本机实测：把 `pcm.!default` 定义成 `type hw; card 1` 之后，
/// ALSA 的 `Default ALSA Output (currently …)` 提示条目**从设备列表里消失了** ——
/// 也就是说「解析提示文本里的卡名」这条路在真实设备配置下根本没有字符串可解析
/// （它只在 `default` 指向 pipewire/pulse 这种**服务**时才出现，而那正是映射不出卡的情况）。
/// 相关实测记录见 `.superpowers/sdd/task-11-report.md`。
///
/// # 前置条件
///
/// 调用时本进程**不持有别的播放 PCM**（否则第 2 步可能认到别的设备上）。
/// `AudioEngine::new` 在构造的最开始调它，满足这条。
///
/// 这条前置条件**不靠调用方自觉**：第 2 步拿到的是本进程持有的**全部**播放 PCM
/// （[`held_playback_pcms`]），**多于一台就判 `Unavailable`**，不取第一个 ——
/// **两台及以上**那种破坏方式下，结果是「如实判不出来」，而不是「认到别的卡上」。
///
/// ⚠️ **但只做到这一步，别读成「前置条件一被破坏就判得出来」。** 去重后**恰好一台**
/// 时走的是 [`HeldTarget::One`] 那一支（[`resolve_held`] 里 `[(card, dev)]` 那个模式），
/// 它**照样把那一台当成 `default` 的目标**（再拿去 cpal 的枚举里查一次，
/// 查得到就报 `Found`）；而「本进程持有一台**别的** engine 的 PCM、探针自己那条
/// 又落在服务型后端上（不记在本进程名下）」给出的正是恰好一台 —— 那正是这条
/// 前置条件被破坏的一种方式。**「恰好一台 ⇒ 认下」这半步有单测钉住
/// （`held_pcm_resolution`）；「这个场景给出的确实就是恰好一台」没有单测** ——
/// 那一步靠的是上面那条「服务型后端下探针不记在本进程名下」的推理。
/// 今天**同一时刻**只有一个 `AudioEngine`：app 的 `play()` 一次调用只建一个
/// （`app/src/main.rs:248-251`）、返回时即释放，而它一次只播一首（`:200` 取的
/// `tracks[0]`；`:190` 那个 `for` 只是列举搜索结果、`:198` 就闭合了，建引擎不在
/// 它里面）；`loopback_verify.rs` 里三个建流点（`:340` 的 `--mapping`、`:432` 判据 1、
/// `:596` 判据 2）各在自己的作用域里建、用完即弃，重测时是**再建一个**（`measure`
/// 重新调 `run`）而不是两个并存；`--hold`（`:165`）那条是另一条模式，也是建一个
/// 持到进程结束；`smoke.rs:84` 那个同理。
/// 而引擎建流都在这次探针**之后**（探针在 `AudioEngine::new` 的 `preferred_exclusive`
/// 里、建流在它返回之后的 `new_on` / `build_on_device` 里）⇒ 探针跑的时候本进程
/// 名下不会有别人的 PCM，所以到不了这里；将来真加第二个引擎的人要先把这一条补上。
///
/// # 一条**没有消除**的窗口（如实记着）
///
/// 探针必须先释放、独占流才能重开 —— 这中间 PCM 是空着的，别的进程可以抢。
/// 这里把它缩到最小（发现工作全部在持有期间做完，见 [`probe_via_proc`] 里那段），
/// 但**消不掉**：剩下的窗口是 `new_on` 自己的一次枚举 + open。抢输了的后果是
/// **如实退回 `Shared`**，不是静默出错 —— 前提是那条错误信息别把它说成
/// 「设备被占着」（见 `AudioEngine::new` 里按 `ErrorKind::DeviceBusy` 分档的告警）。
pub fn probe_default_target(host: &cpal::Host) -> Candidate {
    match platform_note() {
        // 本平台做不到 ⇒ 从这一步就如实降级，不去跑下面的探针。
        Some(note) => Candidate::Unavailable {
            reason: note.to_owned(),
        },
        None => probe_via_proc(host),
    }
}

/// 只有 Linux 会走到这里（`platform_note()` 在别的平台上先返回了）。
#[cfg(target_os = "linux")]
fn probe_via_proc(host: &cpal::Host) -> Candidate {
    let Some(device) = host.default_output_device() else {
        return Candidate::Unavailable {
            reason: "没有默认输出设备".to_owned(),
        };
    };
    let Ok(config) = device.default_output_config() else {
        return Candidate::Unavailable {
            reason: "默认设备不报告任何输出配置，无法判定它背后是哪张卡".to_owned(),
        };
    };
    // 探针流：**不 play()**，所以不会有任何声音；它唯一的作用是「持有」那个 PCM。
    // 缓冲区由 cpal 预填静音（`build_output_stream_raw` 的文档明载），
    // 所以这个空回调不会留下垃圾值。
    let probe = device.build_output_stream_raw(
        config.config(),
        config.sample_format(),
        |_data: &mut cpal::Data, _info: &cpal::OutputCallbackInfo| {},
        |err| log::debug!("探针流错误（不影响判定）: {err}"),
        None,
    );
    let probe = match probe {
        Ok(s) => s,
        Err(e) => {
            return Candidate::Unavailable {
                reason: format!(
                    "打不开默认设备（{e}）—— 判定不了它背后是哪张卡；\
                     这也可能是设备正被别的进程独占着"
                ),
            };
        }
    };

    let me = std::process::id();
    let found = held_playback_pcms(me);
    // 诊断用：探针还活着的时候把「本进程持有哪个 PCM」记下来。
    log::debug!("探针期间本进程（pid {me}）持有的播放 PCM: {found:?}");

    // ⚠️ **发现工作全部在探针还活着时做完，`drop(probe)` 留到最后。**
    //    原因是「先释放再重开」这个窗口：探针一放，PCM 就空出来了，别的进程可以抢；
    //    而 `hardware_output_device` 是一次**完整的设备枚举**（枚举本身要读 ALSA 的
    //    hint 列表与 `/proc`，不是几个指令），把它放在 `drop` 后面等于把这个窗口
    //    从「一次 open」拉长成「一次枚举 + 一次 open」。
    //    挪到最后只剩 `AudioEngine::new` 那边 `new_on` 自己的一次枚举 + open，
    //    这一段**消不掉**（除非把已经解析好的 `cpal::Device` 交给引擎，那是接口改动）——
    //    所以这里是「把窗口缩到不可再缩」而不是「消除竞态」，如实记着。
    let candidate = match resolve_held(&found) {
        HeldTarget::Unknown(reason) => Candidate::Unavailable { reason },
        HeldTarget::One { card, dev } => {
            let pcm_id = format!("{HW_PREFIX}CARD={card},DEV={dev}");
            // 只在 cpal 自己枚举得出的设备里选 —— 我们不自己拼 ALSA 名字去打开：
            // 拼错了是一条静默的失败路径，而枚举列表是 cpal/ALSA 报出来的事实。
            match hardware_output_device(host, &pcm_id) {
                Some(_) => Candidate::Found { pcm_id },
                None => Candidate::Unavailable {
                    reason: format!(
                        "从 /proc/asound 判定目标是 {pcm_id}，但它不在 cpal 的输出设备列表里 \
                         （可能是只支持录音的设备）"
                    ),
                },
            }
        }
    };
    drop(probe);
    candidate
}

/// 「本进程持有的播放 PCM（去重后）」⇒ 该拿哪一台当 `default` 的目标。
///
/// 从 [`probe_via_proc`] 里抽出来**只为了可测**：这段判定是**纯逻辑**，原来却嵌在
/// 「真扫 `/proc` + 真枚举 cpal 设备」的流程里，于是「多于一台 ⇒ 判不出来」这条
/// 只剩代码层面的推理，构造不出用例（`task-11-fix-report.md` §4 如实记着这一条）。
/// 抽成纯函数之后，单测可以直接喂构造切片（见本文件的 `held_pcm_resolution`）。
/// **判定规则一个字都没改**：`probe_via_proc` 只是把那个 `match` 换成了对它结果的 `match`。
///
/// ⚠️ **多于一台就判「判不出来」，不取第一个。** 同进程里若同时开着两个
/// `AudioEngine`（两台不同的卡），「第一个」是 `/proc/asound` 的**遍历顺序**
/// 决定的；而多子流的卡（`snd-aloop` 有 8 个子流）上打开另一台卡**会成功**
/// ⇒ 声音出到错误的设备，而采样率、xrun、时长**全都正常**。
/// 这正是本模块开头那段「宁可拿不到证据，也不猜」的同一条规矩。
/// （今天单引擎故不可达；写成显式的判定是为了让它**继续**不可达。）
///
/// ⚠️ **恰好一台时不做任何「这真是 `default` 那一台吗」的追问** —— 持有一台
/// **别的** engine 的 PCM 也满足「恰好一台」，那时会认到那一台上，而不是判不出来。
/// 理由与后果见 [`probe_default_target`] 的「前置条件」那一段。
#[cfg(target_os = "linux")]
#[derive(Debug)]
enum HeldTarget {
    /// 去重后**恰好一台** ⇒ 它是候选（还要拿去 cpal 的枚举里查一次才用得上）。
    One { card: u32, dev: u32 },
    /// 一台都没有 / 多于一台 ⇒ 判不出来。附带给用户看的原因。
    Unknown(String),
}

#[cfg(target_os = "linux")]
fn resolve_held(held: &[(u32, u32)]) -> HeldTarget {
    match held {
        [] => HeldTarget::Unknown(
            "默认设备背后不是一张物理卡（本进程没有持有任何 /proc/asound 里的播放 PCM）—— \
             这通常是 PipeWire/PulseAudio 这类服务型后端：硬件由**服务进程**持有。\
             进程内无法把 default 映射到 `hw:` 名字（原因与实测见 output_path.rs 的文档）"
                .to_owned(),
        ),
        [(card, dev)] => HeldTarget::One {
            card: *card,
            dev: *dev,
        },
        many => HeldTarget::Unknown(format!(
            "本进程同时持有多台播放 PCM（{}）—— 「default 落在哪一台」不再唯一，\
             判定为不可用（本模块宁可判不出来，也不认错设备）",
            many.iter()
                .map(|(c, d)| format!("{HW_PREFIX}CARD={c},DEV={d}"))
                .collect::<Vec<_>>()
                .join("、")
        )),
    }
}

#[cfg(not(target_os = "linux"))]
fn probe_via_proc(_host: &cpal::Host) -> Candidate {
    // 到不了这里（platform_note() 已经返回）。留一个诚实的兜底而不是 `unreachable!()`：
    // 万一将来 cfg 条件改动，这里给的是「没做」而不是一个崩溃。
    Candidate::Unavailable {
        reason: "本平台没有实现独占判定".to_owned(),
    }
}

/// 把用户写的 ALSA PCM 名字规范成 **cpal 枚举里的形式**。`None` = 不需要改 / 改不出来。
///
/// # 为什么需要它（实测的坑）
///
/// cpal 枚举物理设备时**只**产出 `hw:CARD=<数字>,DEV=<数字>` 一种形状
/// （源码注释原话 `Add hw:/plughw: for all physical devices with numeric index`），
/// 而 ALSA 自己认好几种写法 —— `aplay -D hw:3,0` 与 `aplay -D hw:CARD=Loopback,DEV=0`
/// 都照样能开。`AudioEngine::new_on` 是**按枚举到的名字**找设备的 ⇒
/// 用户（按 ALSA 文档或教程）写出的名字会得到「找不到输出设备」，**而设备明明在**。
/// （实测：Task 11 的控制者在验收说明里就写的是 `hw:CARD=Loopback,DEV=0`。）
///
/// | 用户写的 | 规范成 |
/// |---|---|
/// | `hw:3,0` / `hw:3`（位置写法） | `hw:CARD=3,DEV=0` |
/// | `hw:CARD=3`（缺 `DEV`） | `hw:CARD=3,DEV=0` |
/// | `hw:CARD=Loopback,DEV=0`（卡名） | `hw:CARD=<查到的号>,DEV=0` |
///
/// 查不出卡号、或本来就是规范形式、或根本不是 `hw:`/`plughw:` ⇒ `None`，
/// 调用方按原样报错 —— **绝不猜**。
///
/// 返回的字符串**一定是 cpal 枚举得出的形状**（`<前缀>:CARD=<数字>,DEV=<数字>`）：
/// 卡名查不到号、或卡号不是数字，都返回 `None` 而不是「补个 `DEV=0` 交差」——
/// 那种返回值拿到枚举里必然查不到，只是让上层多查一次。
///
/// # 两段各自的出处
///
/// - **数字写法**这一段是 cpal `canonical_pcm_id` 的**分支镜像**（已核实·源码
///   `cpal-0.18.2/src/host/alsa/mod.rs:1696-1711`）。cpal 也经公开的
///   `HostTrait::device_by_id`（同文件 `:164-169`）暴露了同一个归一化，
///   但**那条路只做名字归一化、不把卡名解析成卡号** —— `canonical_pcm_id`
///   在 `CARD=` 后面不是数字时最多补一个 `DEV=0`（同文件 `:1702-1705`：
///   `hw:CARD=Loopback` → `hw:CARD=Loopback,DEV=0`，写全了则**原样返回**），
///   名字里是卡名就还是卡名 ⇒ 匹配不上 cpal 枚举出的 `hw:CARD=<数字>,DEV=<数字>`
///   ⇒ 卡名那段无论如何都得自己写（查 `/proc/asound/cards` 换成卡号）。
///   两段都写成纯函数、各自可单测，比「先 `device_by_id` 再补一次卡名」少一层接力。
/// - 镜像若与 cpal 的真实行为有出入，后果**有上界**：这个返回值只用来
///   *查* cpal 的枚举，查不到就退回「原样报错」，与今天同一个下场，不会更坏。
/// - **卡名**那一段查 `/proc/asound/cards`（见 [`card_index_by_name`]）。
pub fn canonicalize_pcm_id(pcm_id: &str) -> Option<String> {
    // 先把数字写法补齐（`hw:3,0` → `hw:CARD=3,DEV=0`）。这一步也可能产出
    // `hw:CARD=Loopback,DEV=0` 这种**仍是卡名**的形式，正好交给第二段查号。
    let numeric = canonicalize_numeric_pcm_id(pcm_id);
    let base = numeric.as_deref().unwrap_or(pcm_id);
    // 卡名那一段对 `base` 与**原串**各试一次：`base` 可能已经是数字形式
    // （那时第二段返回 None），而原串可能本来就没有 `DEV=`，两处都试才不漏。
    canonicalize_card_name(base)
        .or_else(|| canonicalize_card_name(pcm_id))
        .or(numeric)
}

/// `hw:3,0` / `hw:CARD=3` 这类**数字**写法 → cpal 枚举的形式。`None` = 没改动。
///
/// **按分支**镜像 `cpal-0.18.2/src/host/alsa/mod.rs:1696-1711` 的两种形状
/// （`CARD=` 只缺 `DEV`、位置写法），但**两处刻意更严**：
///
/// 1. **判据不同**：cpal 认的是 `card_str.contains('=')`，这里认 `strip_prefix("CARD=")`
///    ⇒ `hw:FOO=3` 这种 cpal 会补成 `hw:FOO=3,DEV=0`，这里返回 `None`；
/// 2. **卡段必须是数字**：cpal 的 `CARD=` 分支不看 `=` 后面是什么，位置写法那支
///    也只要求 `DEV` 能解析 ⇒ `hw:CARD=Loopback`、`hw:,0`、`hw:abc,0` 这三种
///    cpal 都会补出一个**查不到设备**的名字，这里一律 `None`（卡名那种留给下一段）。
///
/// 剩下的差别只有「返回原串」vs「返回 `None`」：cpal 不需要改时回原串，这里回
/// `None`（调用方按「原样使用」处理）—— 对调用方是同一件事。
fn canonicalize_numeric_pcm_id(pcm_id: &str) -> Option<String> {
    let (prefix, rest) = pcm_id.split_once(':')?;
    let (card_str, device_str) = match rest.split_once(',') {
        Some((c, d)) => (c.trim(), d.trim()),
        None => (rest.trim(), "0"),
    };
    let canonical = if let Some(card) = card_str.strip_prefix("CARD=") {
        // `hw:CARD=3` —— 只缺 DEV。写了 DEV 就不用补（cpal 也是这么分的）。
        if rest.contains(',') {
            return None;
        }
        // ⚠️ **卡号必须是数字**，否则这一步什么都不做（留给卡名那一段）。
        //    cpal 在这里会把 `hw:CARD=Loopback` 也补成 `,DEV=0`，但补出来的
        //    仍然是**卡名**、仍然查不到设备 —— 返回它只会让上层白查一次。
        //    （刻意比 cpal 严：本函数只吐 cpal 枚举得出的形状，
        //      出处即 `Add hw:/plughw: for all physical devices with numeric index`。）
        card.trim().parse::<u32>().ok()?;
        format!("{prefix}:{rest},DEV=0")
    } else {
        // `hw:3,0` / `hw:3` —— 位置写法换成 CARD=/DEV=。
        // 两个位置都必须是数字（位置写法的 ALSA 语法本来就只收数字），
        // 而且卡号要**真的解析一次**：`hw:` / `hw:,0` 这种空卡段会在这里被挡掉
        // （cpal 会补出 `hw:CARD=,DEV=0` —— 一个查不到任何设备的名字）。
        // 前缀不校验（cpal 也不校验）：`default:0` 这种名字本来就打不开，
        // 规范不规范都是同一个下场。
        let card = card_str.parse::<u32>().ok()?;
        let device = device_str.parse::<u32>().ok()?;
        format!("{prefix}:CARD={card},DEV={device}")
    };
    (canonical != pcm_id).then_some(canonical)
}

/// 卡名 → 卡号那一段（`hw:CARD=Loopback,DEV=0` → `hw:CARD=3,DEV=0`）。
///
/// 只用 `/proc/asound/cards` 查；查不到返回 `None`，**绝不猜**。
fn canonicalize_card_name(pcm_id: &str) -> Option<String> {
    let (prefix, rest) = pcm_id.split_once(':')?;
    if prefix != "hw" && prefix != "plughw" {
        return None;
    }
    let (card_part, dev_part) = match rest.split_once(',') {
        Some((c, d)) => (c, Some(d)),
        None => (rest, None),
    };
    // `hw:1,0`（位置写法）与 `hw:CARD=1`（数字）都不用改。
    let card_name = card_part.trim().strip_prefix("CARD=")?.trim();
    if card_name.is_empty() || card_name.parse::<u32>().is_ok() {
        return None;
    }
    let dev = match dev_part {
        Some(d) => d.trim().strip_prefix("DEV=")?.trim().to_owned(),
        None => "0".to_owned(),
    };
    let index = card_index_by_name(card_name)?;
    Some(format!("{prefix}:CARD={index},DEV={dev}"))
}

/// 卡名（ALSA 的 card id，如 `Loopback`）→ 卡号。
#[cfg(target_os = "linux")]
fn card_index_by_name(name: &str) -> Option<u32> {
    let text = std::fs::read_to_string("/proc/asound/cards").ok()?;
    parse_cards(&text)
        .into_iter()
        .find(|(_, id)| id == name)
        .map(|(index, _)| index)
}

#[cfg(not(target_os = "linux"))]
fn card_index_by_name(_name: &str) -> Option<u32> {
    // 这条规范化是 Linux/ALSA 专有的（`/proc/asound`）；别的平台没有这个概念。
    None
}

/// 解析 `/proc/asound/cards`：`(卡号, 卡名)`。
///
/// 真实内容长这样（本机实测，fixture 见测试）：
///
/// ```text
///  0 [CAMERA         ]: USB-Audio - USB2.0 PC CAMERA
///                       Generic USB2.0 PC CAMERA at usb-0000:00:10.0-1, high speed
///  3 [Loopback       ]: Loopback - Loopback
///                       Loopback 1
/// ```
///
/// 只认形如 `<号> [<名>]:` 的行（卡名两侧有对齐空格，要 trim）；
/// 第二行是描述，必须**忽略**（那里也有 `-` 和空格，认错就会把描述当卡名）。
#[cfg(target_os = "linux")]
fn parse_cards(text: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((idx, rest)) = line.trim().split_once('[') else {
            continue;
        };
        let Ok(index) = idx.trim().parse::<u32>() else {
            continue;
        };
        let Some((name, tail)) = rest.split_once(']') else {
            continue;
        };
        // 卡名后面必须紧跟 `:`（描述行里没有这一对），否则这一行不是卡头。
        if !tail.trim_start().starts_with(':') {
            continue;
        }
        out.push((index, name.trim().to_owned()));
    }
    out
}

/// 本机可选的物理输出设备（`hw:`），给 UI 提示用。`(pcm_id, 描述)`。
///
/// 之所以要暴露它：拿不到独占时，「你本机有这些可选」比「不可用」有用得多 ——
/// 用户拿其中一个去设 [`EXCLUSIVE_DEVICE_ENV`] 就能拿到独占。
pub fn hardware_output_devices(host: &cpal::Host) -> Vec<(String, String)> {
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|d| {
            let id = pcm_id_of(&d);
            if !is_hardware_pcm_id(&id) {
                return None;
            }
            let name = d
                .description()
                .map(|x| x.name().to_owned())
                .unwrap_or_default();
            Some((id, name))
        })
        .collect()
}

/// 在 cpal 枚举的输出设备里找 `pcm_id` 那一个。
fn hardware_output_device(host: &cpal::Host, pcm_id: &str) -> Option<cpal::Device> {
    host.output_devices().ok()?.find(|d| pcm_id_of(d) == pcm_id)
}

/// 本机枚举到的**所有**输出设备的 `pcm_id`（给错误信息与诊断用）。
///
/// 「找不到设备」时最有用的一句话就是「本机有这些」—— 让用户能照着改，
/// 而不是去猜是拼错了还是设备真不在。
pub fn output_device_ids(host: &cpal::Host) -> Vec<String> {
    host.output_devices()
        .map(|devices| devices.map(|d| pcm_id_of(&d)).collect())
        .unwrap_or_default()
}

/// cpal 的 `Device` 在 ALSA 后端里**不暴露 PCM 名字**，唯一拿得到的地方是
/// `DeviceId::id()`（cpal 源码 `host/alsa/mod.rs:459` 就是拿 `pcm_id` 造的这个 id）。
/// 拿不到时退回描述名，再不行给一个占位串 —— **绝不 panic**：设备名只影响日志与显示。
pub fn pcm_id_of(device: &cpal::Device) -> String {
    if let Ok(id) = device.id() {
        return id.id().to_owned();
    }
    device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "<未知设备>".to_owned())
}

/// 扫 `/proc/asound`，返回**本进程**持有的**播放** PCM：`(卡号, 设备号)` 的
/// **去重集合**（顺序不定；空 = 一个都没有）。
///
/// 只读 `/proc`，不打开、不修改任何东西。`/proc/asound` 不存在时返回空表。
///
/// ⚠️ **返回集合而不是「第一个」**，理由见 [`probe_default_target`] 里那段
/// 「多于一台就判判不出来」：取第一个 = 让 `/proc/asound` 的遍历顺序决定
/// 「哪张卡是默认卡」，而那是会**静默送错设备**的猜。
/// 去重按 `(卡号, 设备号)`：多子流的卡（`snd-aloop` 有 8 个子流）上同一个
/// `hw:CARD=3,DEV=0` 只会算一台 —— 那是 ALSA 的子流分配，不是「两台设备」。
#[cfg(target_os = "linux")]
pub fn held_playback_pcms(pid: u32) -> Vec<(u32, u32)> {
    let mut found: Vec<(u32, u32)> = Vec::new();
    let Some(cards) = std::fs::read_dir("/proc/asound").ok() else {
        return found;
    };
    for card_entry in cards.filter_map(Result::ok) {
        let Some(card) = parse_card_dir(&card_entry.file_name().to_string_lossy()) else {
            continue;
        };
        let Ok(pcms) = std::fs::read_dir(card_entry.path()) else {
            continue;
        };
        for pcm_entry in pcms.filter_map(Result::ok) {
            let Some(dev) = parse_playback_pcm_dir(&pcm_entry.file_name().to_string_lossy()) else {
                continue;
            };
            let Ok(subs) = std::fs::read_dir(pcm_entry.path()) else {
                continue;
            };
            for sub in subs.filter_map(Result::ok) {
                let Ok(status) = std::fs::read_to_string(sub.path().join("status")) else {
                    continue;
                };
                if status_owner_pid(&status) == Some(pid) && !found.contains(&(card, dev)) {
                    found.push((card, dev));
                }
            }
        }
    }
    found
}

/// `/proc/asound` 里的目录名 `card1` → `Some(1)`。
///
/// 同一层还有 `cards` / `devices` / `hwdep` / `timers` 等**不是卡**的条目
/// （实测 `/proc/asound` 下就有这些），所以必须按前缀 `card` + 纯数字来判，
/// 不能「读到一个目录就当卡」。
#[cfg(target_os = "linux")]
fn parse_card_dir(name: &str) -> Option<u32> {
    name.strip_prefix("card")?.parse().ok()
}

/// 目录名 `pcm0p` → `Some(0)`；`pcm0c`（录音）→ `None`。
///
/// **只认 `p`**：我们要找的是「`default` 这个**播放**路径落在哪张卡上」，
/// 录音侧与我们无关（认错方向会给出一个能打开、但听不到任何东西的设备）。
#[cfg(target_os = "linux")]
fn parse_playback_pcm_dir(name: &str) -> Option<u32> {
    name.strip_prefix("pcm")?.strip_suffix('p')?.parse().ok()
}

/// 从 `/proc/asound/.../status` 的内容里取 `owner_pid`。
///
/// 文件长这样（PCM 被打开时；关闭时整个文件只有 `closed`）：
///
/// ```text
/// state: RUNNING
/// owner_pid   : 12345
/// trigger_time: 1234.567890
/// ```
///
/// 格式取自实测（fixture 见本文件的测试）。取不到就 `None` ⇒ 判定为「不是我们持有的」，
/// 也就是**宁可判不出来，也不认错**。
#[cfg(target_os = "linux")]
fn status_owner_pid(status: &str) -> Option<u32> {
    status
        .lines()
        .find_map(|l| l.trim().strip_prefix("owner_pid"))
        .and_then(|rest| rest.trim_start_matches([' ', '\t']).strip_prefix(':'))
        .and_then(|v| v.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **只有 `hw:` 是独占。** `plughw:` 名字里含 "hw:" 但系统插件在中间
    /// （plug 就是为了做格式/采样率转换而存在的），把它算成独占是**多报**。
    #[test]
    fn only_hw_prefix_counts_as_hardware() {
        assert!(is_hardware_pcm_id("hw:CARD=1,DEV=0"));
        assert!(is_hardware_pcm_id("hw:0,0"));
        for id in [
            "plughw:CARD=1,DEV=0",
            "default",
            "sysdefault:CARD=Generic",
            "front:CARD=Generic,DEV=0",
            "surround71:CARD=Generic,DEV=0",
            "iec958:CARD=Generic,DEV=0",
            "hdmi:CARD=NVidia,DEV=3",
            "dmix:CARD=1,DEV=0",
            "pipewire",
            "pulse",
            "null",
            "",
        ] {
            assert!(!is_hardware_pcm_id(id), "{id:?} 不该被判为独占");
        }
    }

    /// 环境变量：**只有 `hw:` 被接受**，其余是 `Invalid`（要告警，不能静默当共享设备用）。
    #[test]
    fn env_value_classification() {
        assert_eq!(classify_env_value(None), EnvPreference::NotSet);
        assert_eq!(classify_env_value(Some("")), EnvPreference::NotSet);
        assert_eq!(classify_env_value(Some("   ")), EnvPreference::NotSet);
        assert_eq!(
            classify_env_value(Some("  hw:CARD=3,DEV=0 ")),
            EnvPreference::Hardware("hw:CARD=3,DEV=0".to_owned()),
            "两侧空白要吃掉（用户从别处复制粘贴时很常见）"
        );
        for bad in ["default", "pulse", "plughw:CARD=1,DEV=0"] {
            match classify_env_value(Some(bad)) {
                EnvPreference::Invalid(msg) => {
                    assert!(msg.contains(bad), "说明里要点出用户给的那个值: {msg}")
                }
                other => panic!("{bad:?} 必须是 Invalid，实际 {other:?}"),
            }
        }
    }

    /// `/proc/asound` 的目录名解析：**只认卡与播放 PCM**。
    #[cfg(target_os = "linux")]
    #[test]
    fn proc_dir_names() {
        assert_eq!(parse_card_dir("card0"), Some(0));
        assert_eq!(parse_card_dir("card12"), Some(12));
        for not_a_card in ["cards", "devices", "hwdep", "timers", "card", "cardX", ""] {
            assert_eq!(parse_card_dir(not_a_card), None, "{not_a_card:?}");
        }
        assert_eq!(parse_playback_pcm_dir("pcm0p"), Some(0));
        assert_eq!(parse_playback_pcm_dir("pcm3p"), Some(3));
        for not_playback in ["pcm0c", "pcm0", "pcmXp", "pcmp", ""] {
            assert_eq!(
                parse_playback_pcm_dir(not_playback),
                None,
                "{not_playback:?} 不是播放 PCM"
            );
        }
    }

    /// `status` 的解析：**认不出就返回 `None`**（宁可判不出，不认错设备）。
    #[cfg(target_os = "linux")]
    #[test]
    fn status_owner_pid_parsing() {
        // 实测格式（PCM 被打开时）
        let running = "state: RUNNING\nowner_pid   : 12345\ntrigger_time: 1234.567890\n";
        assert_eq!(status_owner_pid(running), Some(12345));
        // PCM 关着的时候整个文件只有这一行 —— 必须解析成 None，不能当成 pid 0
        assert_eq!(status_owner_pid("closed\n"), None);
        // 只有 `owner_pid` **这一行**算；别的行里出现同样的字样不算
        assert_eq!(status_owner_pid("state: owner_pid : 7\n"), None);
        assert_eq!(status_owner_pid(""), None);
        assert_eq!(status_owner_pid("owner_pid: not-a-pid\n"), None);
    }

    /// 这条纯函数测试是 `/proc` 那三个解析函数的**真实 fixture**：
    /// 本机加载 `snd-aloop` 后，用 `parec` 之外的手段（我们自己的探针流）
    /// 持有 `hw:CARD=3,DEV=0` 时读到的 `status` 原文。
    /// 记在这里是为了让「解析格式」这件事可复核 —— 格式变了这条会红。
    #[cfg(target_os = "linux")]
    #[test]
    fn status_owner_pid_matches_real_loopback_fixture() {
        // 2026-09-26 采集自本机 /proc/asound/card3/pcm0p/sub0/status（snd-aloop 卡），
        // 由 `cargo run -p dsls-audio --example loopback_verify -- --hold hw:CARD=3,DEV=0`
        // 持有设备时读出。**逐字照抄**（只把 pid 换成 3597943 → 4242 保持稳定），
        // 因为这条测试要钉的就是「ALSA 到底怎么写的」。
        let real = "\
state: RUNNING
owner_pid   : 4242
trigger_time: 195036.032186199
tstamp      : 195036.332324121
delay       : 338
avail       : 174
avail_max   : 0
-----
hw_ptr      : 13230
appl_ptr    : 13568
";
        assert_eq!(status_owner_pid(real), Some(4242));
    }

    /// `/proc/asound/cards` 的解析：**只认卡头行，描述行必须忽略**。
    /// fixture 是 2026-09-26 本机的原文（`snd-aloop` 已加载时）。
    #[cfg(target_os = "linux")]
    #[test]
    fn parse_cards_from_real_fixture() {
        let real = "\
 0 [CAMERA         ]: USB-Audio - USB2.0 PC CAMERA
                      Generic USB2.0 PC CAMERA at usb-0000:00:10.0-1, high speed
 1 [Generic        ]: HDA-Intel - HD-Audio Generic
                      HD-Audio Generic at 0xfe200000 irq 16
 2 [NVidia         ]: HDA-Intel - HDA NVidia
                      HDA NVidia at 0xfe080000 irq 41
 3 [Loopback       ]: Loopback - Loopback
                      Loopback 1
";
        assert_eq!(
            parse_cards(real),
            vec![
                (0, "CAMERA".to_owned()),
                (1, "Generic".to_owned()),
                (2, "NVidia".to_owned()),
                (3, "Loopback".to_owned()),
            ],
            "卡名要 trim 掉对齐空格；第二行描述不能被当成卡（它也有 [ 吗？没有，但也要防）"
        );
        // 没有卡的机器、以及只有描述行的输入
        assert!(parse_cards("").is_empty());
        assert!(parse_cards("--- no soundcards ---\n").is_empty());
    }

    /// `resolve_held` 的三支：**构造切片就能钉住**，不需要真 `/proc`、真设备。
    ///
    /// 这条存在的理由：原来这段判定嵌在 `probe_via_proc` 里（要真扫 `/proc`、
    /// 真枚举 cpal 设备），于是「多于一台 ⇒ 判不出来」只能靠代码推理 ——
    /// `task-11-fix-report.md` §4 把它列进了「我不能排除的」。抽出来之后它可测了。
    #[cfg(target_os = "linux")]
    #[test]
    fn held_pcm_resolution() {
        assert!(
            matches!(resolve_held(&[]), HeldTarget::Unknown(_)),
            "一台都没持有 ⇒ 判不出来（服务型后端那条：硬件不在本进程名下）"
        );

        assert!(
            matches!(resolve_held(&[(3, 0)]), HeldTarget::One { card: 3, dev: 0 }),
            "恰好一台 ⇒ 就是它（下一站是 cpal 的枚举）"
        );
        // ⚠️ 这一条钉的是**一条已知的、比文档更弱的边界，不是 bug**：持有一台
        //    **别的** engine 的 PCM 也满足「恰好一台」⇒ 照样被当成 default 的目标。
        //    今天同时只有一个 AudioEngine 且它在探针之后才建流，故不可达；
        //    把「不可达」写成断言，是为了让它**继续**不可达（见 probe_default_target
        //    的「前置条件」那一段）。
        assert!(
            matches!(resolve_held(&[(2, 7)]), HeldTarget::One { card: 2, dev: 7 }),
            "单台就是别的 engine 的 PCM 时也会被认下 —— 如实钉住，不粉饰"
        );

        match resolve_held(&[(1, 0), (3, 0)]) {
            HeldTarget::Unknown(msg) => {
                assert!(
                    msg.contains("hw:CARD=1,DEV=0") && msg.contains("hw:CARD=3,DEV=0"),
                    "多台时必须把**每一台**都列进原因里（用户要能照着改），实得 {msg:?}"
                );
                assert!(
                    msg.contains("多台") || msg.contains("不再唯一"),
                    "原因要说明是「哪一台不再唯一」，不能只说「不可用」，实得 {msg:?}"
                );
            }
            other => panic!("多于一台 ⇒ 必须判不出来、不取第一个，实得 {other:?}"),
        }
        // 三台也一样：判定只看「几台」，与 `/proc` 的遍历顺序无关 ——
        // 而原因里必须三台**全在**（不是「第一个 + 省略号」）。
        match resolve_held(&[(0, 0), (2, 7), (3, 0)]) {
            HeldTarget::Unknown(msg) => assert!(
                ["hw:CARD=0,DEV=0", "hw:CARD=2,DEV=7", "hw:CARD=3,DEV=0"]
                    .iter()
                    .all(|p| msg.contains(p)),
                "三台时也要一台不落地列出来，实得 {msg:?}"
            ),
            other => panic!("三台 ⇒ 判不出来，实得 {other:?}"),
        }
    }

    /// 规范化：**只在确实需要换、且换得出来时才换**。
    #[test]
    fn canonicalize_only_when_it_helps() {
        // 已经是 cpal 枚举的形式 / 不是 PCM 名字 ⇒ 不该动（None 表示原样使用）
        for unchanged in [
            "hw:CARD=3,DEV=0",
            "plughw:CARD=1,DEV=0",
            "default",
            "pipewire",
            "",
            "hw",  // 没有 `:`
            "hw:", // 卡段是空的 —— 补成 `hw:CARD=,DEV=0` 只是噪声，见函数里的说明
        ] {
            assert_eq!(
                canonicalize_pcm_id(unchanged),
                None,
                "{unchanged:?} 不该被改写"
            );
        }

        // **数字写法必须补齐成 cpal 枚举的形式。** 这几条是本次修的：
        // 以前它们原样返回 None ⇒ `--device hw:0,0` 报「找不到输出设备」，
        // 而 `aplay -D hw:0,0` 认得这个名字（CLI 的帮助文本也把它称作
        // 「ALSA 的 PCM 名字」）。分支逐条对齐 cpal 的 `canonical_pcm_id`。
        for (input, want) in [
            ("hw:0,0", "hw:CARD=0,DEV=0"),
            ("hw:3", "hw:CARD=3,DEV=0"),
            ("hw:CARD=3", "hw:CARD=3,DEV=0"),
            ("plughw:3,1", "plughw:CARD=3,DEV=1"),
            ("plughw:CARD=1", "plughw:CARD=1,DEV=0"),
            // 空白要从两侧吃掉（用户从别处复制粘贴时很常见）
            ("hw: 3 , 0 ", "hw:CARD=3,DEV=0"),
        ] {
            assert_eq!(
                canonicalize_pcm_id(input).as_deref(),
                Some(want),
                "{input:?} 应被规范成 {want:?}"
            );
        }

        // 卡名形式：**本机查得到就换，查不到就 None**（绝不猜）
        assert_eq!(
            canonicalize_pcm_id("hw:CARD=NoSuchCardZZZ,DEV=0"),
            None,
            "卡名查不到时必须原样返回，让调用方报错"
        );
        // ⚠️ 别在这里断言「`hw:CARD=<真实卡名>` 查不到」—— 本机 `snd-aloop` 常驻，
        //    `Loopback` 是**查得到**的，那种断言会在这台机器上红、在没装 aloop 的
        //    机器上绿，正好把「实现了」判成「没实现」。下面的存在性检查才是对的写法。
        assert_eq!(
            canonicalize_pcm_id("hw:CARD=NoSuchCardZZZ"),
            None,
            "卡名 + 缺 DEV：补出 `,DEV=0` 之后仍然查不到卡号 ⇒ 必须返回 None"
        );
        // 本机真实存在的那张（snd-aloop 已加载时才在；不在时这条也应当返回 None，
        // 所以这里不断言它一定换得出来 —— 只在存在时检查结果形状）。
        if let Some(canonical) = canonicalize_pcm_id("hw:CARD=Loopback,DEV=0") {
            assert!(
                canonical.starts_with("hw:CARD=") && canonical.ends_with(",DEV=0"),
                "换出来的必须仍是 hw:CARD=<号>,DEV=0 的形状，实得 {canonical:?}"
            );
            assert!(
                canonical
                    .trim_start_matches("hw:CARD=")
                    .starts_with(|c: char| c.is_ascii_digit()),
                "CARD= 后面必须是**卡号**（cpal 枚举的形式），实得 {canonical:?}"
            );
            // 缺 `DEV=` 的卡名写法要与写全的**落到同一处** —— 两段规范化（先补 DEV、
            // 再换卡号）必须能接力，否则 `hw:CARD=Loopback` 会走到「找不到设备」。
            assert_eq!(
                canonicalize_pcm_id("hw:CARD=Loopback").as_deref(),
                Some(canonical.as_str()),
                "卡名 + 缺 DEV 应当与写全 DEV 的写法规范到同一个名字"
            );
        }
    }

    /// `Display` 必须把两条路径的**区别**写在最前面 —— 这是 CLI 打印的那一行。
    #[test]
    fn display_names_the_path_kind() {
        let ex = OutputPath::Exclusive {
            device: "hw:CARD=1,DEV=0".to_owned(),
            rate: 44100,
        };
        let sh = OutputPath::Shared {
            device: "default".to_owned(),
            rate: 48000,
        };
        assert!(ex.to_string().starts_with("Exclusive"));
        assert!(sh.to_string().starts_with("Shared"));
        assert!(ex.to_string().contains("hw:CARD=1,DEV=0"));
        assert!(sh.to_string().contains("48000"));
        assert!(ex.is_exclusive() && !sh.is_exclusive());
        assert_eq!(ex.device(), "hw:CARD=1,DEV=0");
        assert_eq!(sh.rate(), 48000);
    }

    /// 两条路径的边界说明**必须都提到「不等于逐位精确」这件事**（或它的等价说法），
    /// 否则 CLI 上就会出现一个没有边界的「独占」二字。
    #[test]
    fn caveats_state_the_boundary() {
        let ex = OutputPath::Exclusive {
            device: "hw:0,0".to_owned(),
            rate: 44100,
        };
        let sh = OutputPath::Shared {
            device: "default".to_owned(),
            rate: 44100,
        };
        assert!(
            ex.caveat().contains("不等于逐位精确"),
            "独占那一档必须写明它不等于逐位精确"
        );
        assert!(
            ex.caveat().contains("设备自己不做处理"),
            "独占那一档必须写明「设备本身不做处理」这条前提"
        );
        assert!(
            sh.caveat().contains("系统") && sh.caveat().contains("重采样"),
            "共享那一档必须写明系统可能替我们重采样"
        );
    }
}
