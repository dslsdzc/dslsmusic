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

const SHARED_CAVEAT: &str = "共享（经系统混音器）。**系统可能替我们重采样**，我们看不见也管不着 —— \
     这一档只能保证「我们交出去的是源采样率的样本」。要绕开系统混音器，\
     见 DSLS_AUDIO_DEVICE（下面的日志里会列出本机可选的 hw: 设备）。";

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
    let found = held_playback_pcm(me);
    // 诊断用：探针还活着的时候把「本进程持有哪个 PCM」记下来。
    log::debug!("探针期间本进程（pid {me}）持有的播放 PCM: {found:?}");
    drop(probe);

    let Some((card, dev)) = found else {
        return Candidate::Unavailable {
            reason: format!(
                "默认设备背后不是一张物理卡（本进程没有持有任何 /proc/asound 里的播放 PCM）—— \
                 这通常是 PipeWire/PulseAudio 这类服务型后端：硬件由**服务进程**持有。\
                 进程内无法把 default 映射到 hw:{HW_PREFIX} 名字（原因与实测见 output_path.rs 的文档）"
            ),
        };
    };

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

#[cfg(not(target_os = "linux"))]
fn probe_via_proc(_host: &cpal::Host) -> Candidate {
    // 到不了这里（platform_note() 已经返回）。留一个诚实的兜底而不是 `unreachable!()`：
    // 万一将来 cfg 条件改动，这里给的是「没做」而不是一个崩溃。
    Candidate::Unavailable {
        reason: "本平台没有实现独占判定".to_owned(),
    }
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

/// 扫 `/proc/asound`，返回**本进程**持有的**播放** PCM：`(卡号, 设备号)`。
///
/// 只读 `/proc`，不打开、不修改任何东西。找不到（含 `/proc/asound` 不存在）返回 `None`。
#[cfg(target_os = "linux")]
pub fn held_playback_pcm(pid: u32) -> Option<(u32, u32)> {
    let cards = std::fs::read_dir("/proc/asound").ok()?;
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
                if status_owner_pid(&status) == Some(pid) {
                    return Some((card, dev));
                }
            }
        }
    }
    None
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
