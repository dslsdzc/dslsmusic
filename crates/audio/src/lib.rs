//! 音频管线：**无锁环形缓冲 + symphonia 解码 + cpal 输出**。
//!
//! 本 crate 不得依赖任何 `ui-*` crate（设计文档 §2.3）。
//!
//! **实时约束（设计文档 §16.2）**：音频回调线程内禁止分配内存、加锁、IO。
//! 回调体是 [`output::fill_from_ring`]，它只做 `pop_slice` + 采样格式转换 +
//! 无分支的尾部填充；`scratch` 在**建流时**一次性分配好再 move 进闭包。
//!
//! # 采样率对齐（本 crate 最容易静默出错的地方）
//!
//! 全链路只有**一个**采样率进入 ring：**设备率**。
//!
//! ```text
//! 解码（源率）→ PcmGraph（源率）→ resample（源率→设备率）→ ring（设备率）→ 回调
//! ```
//!
//! 这条规则是硬的：`decode::decode_to_i16` 按**源**采样率产出样本，而 cpal 的流按
//! **建流时那个**采样率消费。两者不一致时，若中间什么都不做，就会**变速变调**
//! （44.1kHz 的素材在 48kHz 设备上是 1.0884 倍速、+1.46 半音），
//! **而 xrun 恒为 0、不报任何错** —— 实测一首 42.893 秒的歌 39.4 秒就放完了。
//!
//! [`AudioEngine::new`] 按用户裁定的「不是最省而是最好」处理这件事：
//!
//! 1. **先试以「源采样率」建流** —— 设备支持就**一个样本都不动**（直通）。
//!    **任何重采样都是失真，哪怕是高质量算法**，所以这一档在音质上就是最优。
//! 2. 设备不支持才退回设备默认率，**并且重采样**（[`resample::StreamResampler`]，
//!    高质量 FFT 同步重采样器 —— 不是线性插值那种）。
//!
//! 走的是哪条路由 [`AudioEngine::is_passthrough`] 如实报出来，CLI 会打印它。
//!
//! # 两道边界，别混为一谈
//!
//! - [`AudioEngine::is_passthrough`]：「**我们**没重采样」。
//! - [`AudioEngine::output_path`]：「**系统混音器**在不在我们与设备之间」。
//!
//! 第二道是 Task 11 加的。它的来历是实测：那 184 秒里 `is_passthrough() == true`，
//! 而 44.1kHz 仍然是 **PipeWire 在替我们重采样**到 48kHz 的硬件上 ——
//! 「我们没动」并不蕴含「没人动」。两条都得报出来，只说前一条就是给人一个
//! 「已经 bit-perfect」的错觉。边界说明见 [`OutputPath`] 的文档。

pub mod decode;
pub mod output;
pub mod output_path;
pub mod pcm;
pub mod resample;

pub use output::{AudioRing, AudioRingConsumer, AudioRingProducer};
pub use output_path::OutputPath;
pub use pcm::{PcmGraph, PcmStage};
pub use resample::StreamResampler;

use anyhow::Context;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use output_path::{Candidate, EnvPreference};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// ring 缓冲的时长（秒）。**按秒而不是按样本数**定容量：输出率可能是 44.1k 也可能
/// 是 48k，写死样本数会让「几秒缓冲」随设备而变（44.1k 的 4 秒样本数在 48k 上
/// 只有 3.675 秒）。
const RING_SECONDS: usize = 4;

pub struct AudioEngine {
    _stream: cpal::Stream,
    /// 生产端留在这里（解码线程用）。`HeapProd` 是 `!Sync`，所以
    /// `AudioEngine` 也 `!Sync` —— 它在单线程里使用，没有问题。
    producer: AudioRingProducer,
    /// 输出流实际建在哪个采样率上（`output_rate()`）。
    output_rate: u32,
    /// 输出流实际的声道数（`output_channels()`）。
    output_channels: u16,
    /// 输出流实际的采样格式（`output_format()`）。
    output_format: cpal::SampleFormat,
    /// 我们**实际**拿到的那条路径：系统混音器在不在中间（`output_path()`）。
    path: OutputPath,
    /// `None` = **直通**：设备支持源采样率，我们一个样本都没动。
    /// `Some` = 设备不支持，样本在进 ring 之前被换算到设备率。
    resampler: Option<StreamResampler>,
    /// 重采样输出的暂存。复用同一个 `Vec`，避免每次 `push_samples` 都分配。
    /// （这里在**解码线程**上，分配是允许的 —— §16.2 只约束回调线程；
    /// 但没有必要每包都分配一次。）
    conv: Vec<i16>,
    /// ring 容量（样本数），按**输出率**算出来的。
    ring_capacity: usize,
    /// 设备侧报错次数（`device_error_count()`）。与 `producer.underruns()` 共享同一个
    /// 计数器 —— 但**口径完全不同**，见那个方法的文档。
    device_errors: Arc<AtomicU64>,
}

impl AudioEngine {
    /// 按**源采样率 `desired_rate`** 尽量建一条直通流；设备不支持时才重采样。
    ///
    /// `desired_rate` 是**解码产出的**采样率（来自 `decode::probe_sample_rate`），
    /// 不是「想要多少」—— 建流成功后 [`AudioEngine::push_samples`] 收的就是这个
    /// 采样率的样本。
    ///
    /// `desired_rate == 0` 直接报错：0 不是「没意见」，是调用方的实现错误。
    /// 别把它当成「用设备默认率」—— 那样会静默退回本文件顶部描述的那个缺陷。
    ///
    /// # 自动选路：先试设备独占，拿不到再走系统默认（共享）
    ///
    /// - 独占候选从哪来：环境变量 `DSLS_AUDIO_DEVICE`（首选）或
    ///   [`output_path::probe_default_target`]（拿证据，不猜）。见那个函数的文档。
    /// - ⚠️ **独占失败是常态而不是异常**（`hw:` 被 PipeWire 或别的进程占着就打不开）。
    ///   失败只记一条 `warn` 然后走共享路径 —— **绝不因此不出声**（设计文档 §17：
    ///   能力缺失时降级，不要拒绝运行）。实际拿到哪条路由由
    ///   [`AudioEngine::output_path`] 如实报出，CLI 会打印它。
    /// - 「用户自己点了名」的设备走 [`AudioEngine::new_on`]，那条是**硬**语义。
    pub fn new(desired_rate: u32) -> anyhow::Result<Self> {
        Self::check_rate(desired_rate)?;
        let host = cpal::default_host();

        // ① **独占优先。** 这一档的意义是「系统混音器不在我们与设备之间」——
        //    它是 `is_passthrough()` 之外的另一半（见本模块文档）。
        if let Some(pcm_id) = preferred_exclusive(&host) {
            match Self::new_on(&pcm_id, desired_rate) {
                Ok(engine) => return Ok(engine),
                Err(e) => log::warn!(
                    "独占设备 {pcm_id} 打不开（{e}）—— **这是常态**（PipeWire 或别的进程\
                     正占着它）。退回共享路径；这不会是静音，但系统混音器会回到中间。"
                ),
            }
        }

        // ② 共享路径：系统默认设备。行为与 Task 9/10 逐字相同。
        let device = host
            .default_output_device()
            .context("没有可用的音频输出设备")?;
        Self::build_on_device(&device, desired_rate, false)
    }

    /// **指定设备**建流（按 PCM 名字：`hw:CARD=1,DEV=0` / `plughw:…` / `default` /
    /// `pipewire` / …）。
    ///
    /// ⚠️ 与 [`AudioEngine::new`] 的区别是**语义**，不是实现：
    ///
    /// - 这里是「**我要这台设备**」⇒ 打不开就**报错**（找不到名字时会把本机有哪些
    ///   列在错误信息里）。静默换一台设备播放等于骗人 —— 用户点的是 A 卡，
    ///   声音从 B 卡出来，而所有指标都正常。
    /// - `new` 是「**能拿到什么就用什么**」⇒ 按能力降级（§17）。
    ///
    /// `hw:` 前缀的设备会被报成 [`OutputPath::Exclusive`]，其余报 `Shared`
    /// （判据见 [`output_path::is_hardware_pcm_id`]）。
    pub fn new_on(pcm_id: &str, desired_rate: u32) -> anyhow::Result<Self> {
        Self::check_rate(desired_rate)?;
        let host = cpal::default_host();
        let device = find_device(&host, pcm_id)?;
        Self::build_on_device(
            &device,
            desired_rate,
            output_path::is_hardware_pcm_id(pcm_id),
        )
    }

    fn check_rate(desired_rate: u32) -> anyhow::Result<()> {
        anyhow::ensure!(
            desired_rate > 0,
            "desired_rate 不能为 0（它不是「用默认率」的意思，见 AudioEngine::new 文档）"
        );
        Ok(())
    }

    /// 在**一个已选定的设备**上按音质优先级建流：
    ///
    /// ① **源采样率**（直通：我们一个样本都不动 —— 音质最优的一档，任何重采样
    ///    都是失真，哪怕是高质量算法）；
    /// ② 设备不支持才退回**设备默认率 + 重采样**。
    ///
    /// `exclusive` 只影响**报出来的** [`OutputPath`]，不影响建流行为 ——
    /// 而且它在 `build` 里还要再和「设备名确实是 `hw:`」与一次，见那里的注释。
    fn build_on_device(
        device: &cpal::Device,
        desired_rate: u32,
        exclusive: bool,
    ) -> anyhow::Result<Self> {
        let pcm_id = output_path::pcm_id_of(device);

        // ① **音质最优的一档**：按源采样率建流 ⇒ 零处理。
        if let Some(supported) = find_config_at_rate(device, desired_rate) {
            match Self::build(device, supported, desired_rate, exclusive) {
                Ok(engine) => return Ok(engine),
                Err(e) => log::warn!(
                    "按源采样率 {desired_rate}Hz 在 {pcm_id} 上建流失败（{e}）；\
                     退回设备默认率并重采样。这不会是静音，但会有一次重采样。"
                ),
            }
        } else {
            log::info!("设备 {pcm_id} 未报告 {desired_rate}Hz 下我们支持的输出配置");
        }

        // ② 兜底：设备默认率 + 重采样。
        //    ⚠️ **它在 `desired_rate` 恰好等于默认率时也会走到**（例如
        //    `supported_output_configs()` 枚举失败的后端）。`build` 里按
        //    「输出率 == 源率 ⇒ 直通」判，所以那种情况仍然不会白白重采样。
        let default = device.default_output_config()?;
        // ⚠️ 设备的**默认**配置也可能是单声道 —— 本机设备会把 1..64 声道的全部
        //    组合都报一遍，默认那一档只是它自己的偏好，不代表它只能给单声道。
        //    在**同一个采样率**上再找一个 ≥2 声道的配置；找到就用它。
        //    （采样率不变 ⇒ 重采样比率不变，所以这一步不会引入任何额外转换。）
        let chosen = find_config_at_rate(device, default.sample_rate()).unwrap_or(default);
        Self::build(device, chosen, desired_rate, exclusive)
    }

    /// 把一条**已选定的**输出配置变成能出声的引擎。
    ///
    /// 直通与否**由「输出率 == 源率」决定**，而不是由调用方传进来的意图决定 ——
    /// 这样上面那条兜底路径不会在采样率其实相同时白白插进一次重采样。
    fn build(
        device: &cpal::Device,
        supported: cpal::SupportedStreamConfig,
        desired_rate: u32,
        exclusive: bool,
    ) -> anyhow::Result<Self> {
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.config();
        let output_rate = config.sample_rate;
        let channels = config.channels;
        let pcm_id = output_path::pcm_id_of(device);

        // ⚠️ **报路径时宁可少报，也不多报。** 调用方说「这是独占候选」还不够 ——
        //    设备名必须真的落在 `hw:` 上，否则一律按 `Shared` 报。
        //    多报一次「独占」就是给用户一个假的 bit-perfect 承诺；
        //    少报一次（其实是独占、报成共享）只是保守，代价小得多。
        let path = if exclusive && output_path::is_hardware_pcm_id(&pcm_id) {
            OutputPath::Exclusive {
                device: pcm_id.clone(),
                rate: output_rate,
            }
        } else {
            OutputPath::Shared {
                device: pcm_id.clone(),
                rate: output_rate,
            }
        };

        let resampler = if output_rate == desired_rate {
            None
        } else {
            Some(StreamResampler::new(desired_rate, output_rate)?)
        };

        // ⚠️ cpal 0.18 已删除 `DeviceTrait::name()`（已核实·编译：
        //    error[E0599]: no method named `name` found for struct `Device`），
        //    换成 `description()?` 再取 `.name()`。
        // ⚠️ `SampleRate` 在 0.18 是**裸 `u32` 别名**，不是 newtype
        //    （已核实·源码 cpal-0.18.2/src/lib.rs:220-233），
        //    所以 `config.sample_rate().0` 编译不过，要写 `config.sample_rate`。
        match &resampler {
            None => log::info!(
                "音频输出: {} [{path}] @ {}Hz {}ch fmt={:?} | 直通：按源采样率建流，未重采样",
                device.description()?.name(),
                output_rate,
                channels,
                sample_format
            ),
            Some(rs) => log::info!(
                "音频输出: {} [{path}] @ {}Hz {}ch fmt={:?} | 重采样 {}Hz → {}Hz（启动延迟 {} 帧）",
                device.description()?.name(),
                output_rate,
                channels,
                sample_format,
                desired_rate,
                output_rate,
                rs.delay_frames()
            ),
        }

        // 缓冲时长按**输出率**折算 —— 见 `RING_SECONDS` 的说明。
        // **split 之后消费端移进回调，全程无锁。**
        let ring_capacity = output_rate as usize * resample::CHANNELS * RING_SECONDS;
        let (producer, consumer) = AudioRing::new(ring_capacity).split();

        // ⚠️ **不要硬写 `f32`。** `build_output_stream<T>` 是泛型，`T` 由调用方选；
        //    而设备默认格式是运行时才知道的（本机实测 F32/48000，但换台设备可能是
        //    别的）。硬写 f32 等于假设「所有设备的默认格式都是 F32」——
        //    在不是的设备上直接建流失败，**而这个任务的出口标准就是「出声」**。
        // 设备侧报错（ALSA 的 xrun 就走这条路）**只有在 cpal 的错误回调里看得见** ——
        // 它以前只进日志，于是「设备卡了、声音断了一下」在指标上是隐形的
        // （`xrun_count` 的口径明说了自己看不见它）。这里数起来，
        // 由 `device_error_count()` 如实报出。
        let device_errors = Arc::new(AtomicU64::new(0));
        let stream = build_stream_for(
            device,
            config,
            sample_format,
            consumer,
            Arc::clone(&device_errors),
        )?;
        stream.play()?;

        Ok(Self {
            _stream: stream,
            producer,
            output_rate,
            output_channels: channels,
            output_format: sample_format,
            path,
            resampler,
            conv: Vec::new(),
            ring_capacity,
            device_errors,
        })
    }

    /// 供解码线程推送样本。
    ///
    /// ⚠️ **收的是【源采样率】的样本**（即 `AudioEngine::new` 的 `desired_rate`），
    /// 不是设备率。直通时它们原样进 ring；否则在这里被换算到设备率。
    /// 换算在**解码线程**上做，所以 ring 里永远是设备率样本，
    /// 音频回调仍然只是 memcpy + 转换（§16.2）。
    pub fn push_samples(&mut self, data: &[i16]) {
        match &mut self.resampler {
            // 直通：一个样本都不动。零拷贝、零转换。
            None => self.producer.push_samples(data),
            Some(rs) => {
                self.conv.clear();
                rs.process(data, &mut self.conv);
                self.producer.push_samples(&self.conv);
            }
        }
    }

    /// **整首歌喂完之后必须调一次。**
    ///
    /// 重采样器是有状态的：内部延迟线里还压着 `output_delay()` 个输出帧，暂存里
    /// 还可能剩不足一整块的输入。不排空的话**结尾会被吞掉一截**，而「时长对不对」
    /// 正是本任务的验收标准。它同时把开头为对齐而丢掉的帧补回来，所以总输出帧数
    /// 恰好是 `ceil(输入帧数 × 比率)`。
    ///
    /// 直通时它什么都不做。
    ///
    /// **前置条件：此刻环里要放得下这点尾巴。** 排空产出的量是有界的
    /// （`output_delay()` 帧 + 不足一整块的输入，本模块默认配置下约 12 ms），
    /// 而 `push_samples` 在环满时是**丢弃**语义 —— 环真满的话尾巴会被丢掉，
    /// **而且是静默的**。CLI 的背压把环维持在 3/4（约 1 秒余量），远大于这个量；
    /// 直接调用本方法的代码要自己保证同一条。
    pub fn finish_stream(&mut self) {
        if let Some(rs) = &mut self.resampler {
            self.conv.clear();
            rs.flush(&mut self.conv);
            self.producer.push_samples(&self.conv);
        }
    }

    /// **我们**有没有重采样。
    ///
    /// ⚠️ **`true` 只代表「我们没重采样」，不代表「整条链路逐位精确」。**
    /// 样本出我们这一层之后还要经过系统混音器（PipeWire / PulseAudio / ALSA 的
    /// `plug` 插件），**它仍可能替我们重采样到硬件实际跑的采样率**。
    /// 后半句由 [`AudioEngine::output_path`] 回答（Task 11 加的）；而且就算拿到了
    /// `Exclusive`，也还要格式/采样率与源一致、设备自己不做处理 —— 见 [`OutputPath`]。
    ///
    /// 不说清这一点，会给人一种「已经 bit-perfect」的错觉，而那是假的。
    pub fn is_passthrough(&self) -> bool {
        self.resampler.is_none()
    }

    /// 输出流实际建在哪个采样率上。
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// 输出流实际的声道数。
    ///
    /// 与 [`AudioEngine::output_rate`] 同一个理由：**用户/调用方有权知道我们实际
    /// 建了什么流**。它不是「>= 2」就没事 —— 本机实测设备会报 1..64 声道的
    /// 全部组合，挑中单声道就等于把右声道整条丢掉，而采样率、xrun 全都正常。
    /// 把实际值报出来，这类事才可能被发现。
    pub fn output_channels(&self) -> u16 {
        self.output_channels
    }

    /// 输出流实际的采样格式（**设备侧**的，不是我们喂进去的 i16）。
    ///
    /// 为什么值得单独报出来：`Exclusive` 只保证「系统混音器不在中间」，
    /// 而逐位精确还要求**格式与源一致**。设备跑 f32/i32/I24 时，
    /// i16 → 那一档虽然是无损转换，但它仍然是一次转换 —— 这条只有报出来，
    /// 用户才可能知道。
    pub fn output_format(&self) -> cpal::SampleFormat {
        self.output_format
    }

    /// 我们**实际**拿到的那条输出路径：系统混音器在不在我们与设备之间。
    ///
    /// ⚠️ 这是与 [`AudioEngine::is_passthrough`] **并列的另一半**，两条都要看：
    /// 前者说「我们没重采样」，本方法说「系统有没有可能替我们重采样」。
    /// 边界说明（**`Exclusive` 不等于逐位精确**）见 [`OutputPath::caveat`] ——
    /// UI 打印这条路径时**必须**带上它。
    pub fn output_path(&self) -> &OutputPath {
        &self.path
    }

    /// ring 容量（样本数）。调用方按它做背压 —— **不要自己写死 `44100 * 2 * 4`**：
    /// 输出率随设备而变，写死的数字在 48kHz 设备上不是 4 秒。
    pub fn ring_capacity(&self) -> usize {
        self.ring_capacity
    }

    pub fn ring_available(&self) -> usize {
        self.producer.available()
    }

    /// **硬指标：必须为 0**（设计文档 §16.2）。
    ///
    /// ⚠️ 口径比名字窄：它**只数我们自己那个 ring 被抽干**，看不见 ALSA/PipeWire
    /// 设备侧的 xrun。设备卡顿而我方缓冲有货时，它照样是 0 ——
    /// 那一半由 [`AudioEngine::device_error_count`] 回答，**两个都要看**。
    pub fn xrun_count(&self) -> u64 {
        self.producer.underruns()
    }

    /// **设备侧**报错次数（ALSA 的 underrun/overrun 就从这里来）。
    ///
    /// 补的是 [`AudioEngine::xrun_count`] 那条明确写着的盲区：设备卡顿而我方缓冲
    /// 有货时，那个 0 什么都说明不了，而听感上是**断了一下**。实测：本机 CPU 满载时
    /// 跑回环验证，一次 1 秒的播放里它数到 **17**（而 `xrun_count` 是 0）。
    ///
    /// ⚠️ 口径：它是「cpal 的错误回调被调了几次」，不是「丢了多少样本」；
    /// 而且回调里现在只有 ALSA 的 xrun 会走到（设备拔出等也走同一条，本机没遇到过）。
    /// **必须为 0**，非零就说明这条实时路径被饿到过。
    pub fn device_error_count(&self) -> u64 {
        self.device_errors.load(Ordering::Relaxed)
    }
}

/// 首选拿来做**独占**的物理设备（`hw:`），没有就 `None`。
///
/// 两个来源，优先级是「用户说的」>「我们探测的」：
///
/// 1. 环境变量 `DSLS_AUDIO_DEVICE`（见 [`output_path::preferred_exclusive_from_env`]）——
///    设了非法值时**告警并忽略**，不要静默把它当共享设备用（那会背地里改掉路由）；
/// 2. [`output_path::probe_default_target`] —— 靠 `/proc/asound` 的**证据**
///    判断 `default` 背后是哪张卡，判不出来就如实说，**不猜**。
///
/// 拿不到独占时把「本机可选哪些 hw: 设备」写进日志：用户下一次就能直接用上。
/// 这条日志是这个功能**唯一**的可发现性来源（env 变量不是自解释的）。
fn preferred_exclusive(host: &cpal::Host) -> Option<String> {
    match output_path::preferred_exclusive_from_env() {
        EnvPreference::Hardware(pcm_id) => return Some(pcm_id),
        EnvPreference::Invalid(msg) => log::warn!("{msg}"),
        EnvPreference::NotSet => {}
    }

    let reason = match output_path::probe_default_target(host) {
        Candidate::Found { pcm_id } => return Some(pcm_id),
        Candidate::Unavailable { reason } => reason,
    };

    let options = output_path::hardware_output_devices(host);
    if options.is_empty() {
        log::info!("没有拿到设备独占（{reason}）；本机也没有可选的 `hw:` 输出设备。");
    } else {
        let list = options
            .iter()
            .map(|(id, name)| format!("{id}（{name}）"))
            .collect::<Vec<_>>()
            .join("，");
        log::info!(
            "没有拿到设备独占（{reason}）。本机可选的物理输出设备：{list}。\
             想指定其中一个：设 {}={}（或 CLI 的 --device，那条是硬性的：打不开就报错）。\
             ⚠️ `hw:` 是独占的，别的进程占着时打不开 —— 那时会如实退回共享路径。",
            output_path::EXCLUSIVE_DEVICE_ENV,
            options[0].0
        );
    }
    None
}

/// 按 PCM 名字在**枚举**里找一个输出设备。
///
/// 找不到时把本机有哪些设备列进错误信息 —— 这条信息就是用户下一步要用的东西，
/// 比「设备不存在」有用得多。
fn find_device(host: &cpal::Host, pcm_id: &str) -> anyhow::Result<cpal::Device> {
    // ⚠️ **不能只靠 `devices()` 枚举。** `default` 这个虚拟设备在某些 ALSA 配置下
    //    **不在**枚举列表里（本机实测：把 `pcm.!default` 定义成 `type hw; card 1`
    //    之后，"default" 这一条从 hint 列表里消失了），而 `default_output_device()`
    //    永远给得出它。所以先问它。
    if let Some(default) = host.default_output_device() {
        if output_path::pcm_id_of(&default) == pcm_id {
            return Ok(default);
        }
    }

    // ① 按枚举到的名字**原样**找。
    //    ⚠️ 这一步曾经被我删掉过（改这段时只顾着加下面的卡名解析，把原样查找整段
    //    替换掉了），后果是 `hw:CARD=3,DEV=0` 这种**枚举里明明有**的名字也报
    //    「找不到设备」—— 单元测试抓不到（`find_device` 要真设备），
    //    是回环示例一跑就红才暴露的。**先原样、再改写**，这个顺序不能反。
    if let Some(d) = lookup_pcm_id(host, pcm_id) {
        return Ok(d);
    }

    // ② **卡名换成卡号再找一次。** ALSA 文档与所有教程写的都是
    //    `hw:CARD=Loopback,DEV=0`（CARD 可以是卡名），而 cpal 枚举的**只有卡号**形式
    //    （`hw:CARD=3,DEV=0`）⇒ 不换的话，用户按文档写就会得到「找不到设备」，
    //    而设备明明在。实测：Task 11 的控制者在验收说明里写的就是卡名那种。
    if let Some(canonical) = output_path::canonicalize_card_name(pcm_id) {
        if let Some(d) = lookup_pcm_id(host, &canonical) {
            log::info!("设备名 {pcm_id} 按 ALSA 的卡名写法解析为 {canonical}");
            return Ok(d);
        }
    }

    let available = output_path::output_device_ids(host);
    anyhow::bail!(
        "找不到输出设备 {pcm_id:?}。本机枚举到的输出设备有：{}。\
         （提示：cpal 枚举物理设备用的是**卡号**，如 hw:CARD=3,DEV=0；\
         按 ALSA 文档写卡名，如 hw:CARD=Loopback,DEV=0，也会被解析，但卡名必须在 \
         /proc/asound/cards 里找得到。）",
        if available.is_empty() {
            "<一个都没有>".to_owned()
        } else {
            available.join(", ")
        }
    )
}

/// 在枚举里按 `pcm_id` 原样找一个输出设备（不做任何名字改写）。
fn lookup_pcm_id(host: &cpal::Host, pcm_id: &str) -> Option<cpal::Device> {
    host.output_devices()
        .ok()?
        .find(|d| output_path::pcm_id_of(d) == pcm_id)
}

/// 在设备报告的、**我们确实建得出流**的配置里，找一个能跑 `rate` 的。
///
/// 排序依据见 [`config_rank`]；`None` 表示「没有可用的」，调用方应退回默认率 + 重采样。
///
/// ⚠️ `supported_output_configs()` 在少数后端上会**直接报错**（枚举不出来）。
/// 那不该让播放失败 —— `.ok()?` 让它退回默认配置那条路，而
/// [`AudioEngine::build`] 会按「输出率 == 源率」重新判一次直通。
fn find_config_at_rate(device: &cpal::Device, rate: u32) -> Option<cpal::SupportedStreamConfig> {
    let default = device.default_output_config().ok();
    let mut best: Option<((u8, u8), cpal::SupportedStreamConfig)> = None;
    for range in device.supported_output_configs().ok()? {
        let format = range.sample_format();
        let channels = range.channels();
        // ⚠️ **单声道配置直接跳过。** 我们的内容恒为 2 声道（`resample::CHANNELS`），
        //    而回调是「ch0←L、ch1←R、其余静音」的上混 —— 建成单声道流等于
        //    **右声道被整条丢掉**，而采样率是对的、xrun 是 0，又是一个
        //    「指标全正常但声音是错的」。宁可退回默认率 + 一次高质量重采样。
        //    （本机实测：设备确实同时报了 1ch 与 2ch 的 48000Hz 配置，
        //      早先只按格式排序会挑中 1ch 那条。）
        if channels < resample::CHANNELS as u16 || !is_supported_format(format) {
            continue;
        }
        let Some(config) = range.try_with_sample_rate(rate) else {
            continue;
        };
        let rank = (
            channel_rank(channels, default.as_ref().map(|c| c.channels())),
            format_rank(format, default.as_ref().map(|c| c.sample_format())),
        );
        if best.as_ref().is_none_or(|(best_rank, _)| rank < *best_rank) {
            best = Some((rank, config));
        }
    }
    best.map(|(_, config)| config)
}

/// `build_stream_for` 能处理的采样格式集合。**必须与它 match 的臂一致**：
/// 这里说「支持」而那里掉进 `other` 分支，就等于白建一次流再失败。
///
/// 不一致也不会静音 —— [`AudioEngine::new`] 会把建流失败接住、退回默认配置 ——
/// 但会白多一次失败尝试，而且日志会难读。改 `build_stream_for` 时记得回来同步这里。
fn is_supported_format(format: cpal::SampleFormat) -> bool {
    use cpal::SampleFormat as F;
    matches!(format, F::F32 | F::I16 | F::I32 | F::I24 | F::U8)
}

/// 声道数的偏好，**越小越好**：
///
/// - **0：设备默认的声道数**（且 ≥2）。设备自己选的那一档最省转换。
/// - **1：其余 ≥2 声道的配置。** 我们的 2 声道内容能完整送上，只是可能与
///   设备默认的通道布局不同。
/// - **2：单声道。** 右声道会被丢掉 —— `find_config_at_rate` 现在直接跳过它们，
///   这个档位留着是为了让规则本身完整、可测。
fn channel_rank(channels: u16, default_channels: Option<u16>) -> u8 {
    if channels < resample::CHANNELS as u16 {
        2
    } else if Some(channels) == default_channels {
        0
    } else {
        1
    }
}

/// 采样格式的偏好，**越小越好**。
///
/// - **0：设备的默认格式。** 那是设备自己选的、需要最少转换的一档。
/// - **1：i16 → T 无损**的其余格式。`i16 → i32`（`<<16`）、`i16 → I24`（`<<8`）、
///   `i16 → f32`（`/32768`）都是**精确**的，所以这几档之间没有音质差别。
/// - **2：`u8`。** 8 位只有 256 级，i16 进去要丢 8 位 —— 设备只给 u8 时也只能用
///   它（总比不出声好），但**有别的选择时不该选它**。
fn format_rank(format: cpal::SampleFormat, default_format: Option<cpal::SampleFormat>) -> u8 {
    if Some(format) == default_format {
        0
    } else if format == cpal::SampleFormat::U8 {
        2
    } else {
        1
    }
}

/// 按**设备自己报的**采样格式分发。一份回调体覆盖全部受支持格式。
///
/// ⚠️ **不要照抄 cpal 官方文档的示例**（`cpal-0.18.2/src/lib.rs:104-124`）：
/// 它只分支 `F32/I16/U16`、其余 `panic!`——而**本机实测 `u16` 会被拒**
/// （`Sample format u16 is not supported in any byte order`），
/// 照抄会在本机直接 panic。这里一律返回 `Err`，绝不 panic。
fn build_stream_for(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    format: cpal::SampleFormat,
    consumer: AudioRingConsumer,
    device_errors: Arc<AtomicU64>,
) -> anyhow::Result<cpal::Stream> {
    use cpal::SampleFormat as F;
    match format {
        F::F32 => build_output::<f32>(device, config, consumer, device_errors),
        F::I16 => build_output::<i16>(device, config, consumer, device_errors),
        F::I32 => build_output::<i32>(device, config, consumer, device_errors),
        // I24 本机设备确实报告支持，**漏了它会掉进 other 分支**
        F::I24 => build_output::<cpal::I24>(device, config, consumer, device_errors),
        F::U8 => build_output::<u8>(device, config, consumer, device_errors),
        other => {
            anyhow::bail!("不支持的输出采样格式 {other:?}。请把这个值报上来，不要静默降级到无声。")
        }
    }
}

fn build_output<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut consumer: AudioRingConsumer,
    device_errors: Arc<AtomicU64>,
) -> anyhow::Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::Sample + cpal::FromSample<i16> + Send + 'static,
{
    // 回调内**绝不分配**：scratch 在这里一次性分配好，再 move 进闭包。
    // （旧写法在回调里 `scratch.resize()`——虽然容量预留着不会真重分配，
    //   但那是把「不分配」这条硬约束押在容量估算上，换个大缓冲就破了。）
    let mut scratch: Vec<i16> = vec![0; 8192];
    // 通道数要在 move 进闭包**之前**取出来（`config` 下面会被按值消费掉）
    let channels = config.channels as usize;

    // ⚠️ 0.18 的 `config` **按值传**（旧版是引用），且多了第 4 个参数
    //    `timeout: Option<Duration>`；错误类型没有 `BuildStreamError` 了，
    //    统一是 `cpal::Error`。已核实·源码 cpal-0.18.2/src/traits.rs:318-329。
    let stream = device.build_output_stream(
        config,
        move |out: &mut [T], _: &cpal::OutputCallbackInfo| {
            fill_from_ring::<T>(out, channels, &mut consumer, &mut scratch);
        },
        // 错误回调：计数 + 日志。**只做一次原子加**（无分配、无锁、无 IO），
        // 这条回调可能与音频线程同源。
        move |err| {
            device_errors.fetch_add(1, Ordering::Relaxed);
            log::error!("音频流错误: {err}");
        },
        None, // timeout：None = 无限等待后端初始化
    )?;
    Ok(stream)
}

/// **唯一的回调体。** 一份代码覆盖 f32 / i16 / i32 / I24 / u8。
///
/// 用**分块 `pop_slice`** 而不是逐样本 `try_pop`——实测快约 50 倍
/// （0.4 ns/样本 vs 20.0 ns/样本，各 800 万样本）。本函数全程无锁、
/// 无分配、无 IO（设计文档 §16.2）。
///
/// 前置条件（唯一的调用点 `build_output` 两条都满足；违反了会 panic）：
/// `channels >= 1`，且 `scratch.len() >= 2`（至少要装得下一帧立体声）。
///
/// 环里承载的是**2 声道交错**的样本（`decode::decode_to_i16` 的契约）。推入的
/// 样本数**允许是奇数**（切片边界随调用方），本函数按**整帧**消费，见下面的注释。
fn fill_from_ring<T>(
    out: &mut [T],
    channels: usize,
    consumer: &mut AudioRingConsumer,
    scratch: &mut [i16],
) where
    T: cpal::SizedSample + cpal::Sample + cpal::FromSample<i16>,
{
    let total_frames = out.len() / channels;
    let cap_frames = (scratch.len() / 2).max(1); // scratch 存的是立体声样本
    let mut frame = 0usize;
    let mut starved = false;

    // ⚠️ **尾部槽位必须先写。** 若 `out.len()` 不是 `channels` 的整数倍，
    //    最后那几个槽位**永远不会被下面的循环写到**，会留下 cpal 缓冲里的旧值
    //    （听感＝爆音）。先整体铺静音、再让循环覆盖，是唯一稳妥的顺序。
    out.fill(T::EQUILIBRIUM);

    while frame < total_frames {
        let want_frames = (total_frames - frame).min(cap_frames);
        let want = want_frames * 2;
        // ⚠️ **只取整帧。** 环里可能装着**奇数**个样本：push 端按任意边界切分
        //    同一个立体声流是合法的（先推 [L0,R0,L1]，再推 [R1,L2,R2]）。
        //    按 `available()` 把 pop 长度向下取到偶数，那**半个样本就留在环里**，
        //    下一次回调自然接在它后面。
        //    反过来（直接 pop 奇数个再 `got / 2`）会把它**丢掉** —— 而它后面的
        //    样本还在环里 ⇒ 此后所有 L/R 配对整体错位，听感是**声道互换**，
        //    而且**不可恢复**（比丢一个样本难查得多）。
        let avail = consumer.available();
        // `want` 本身是偶数，只有 `avail` 更小时才可能算出半个帧 ⇒ 清掉最低位。
        let n = want.min(avail) & !1;
        let got = consumer.pop_raw(&mut scratch[..n]);
        let got_frames = got / 2;

        for f in 0..want_frames {
            let (l, r) = if f < got_frames {
                (scratch[f * 2], scratch[f * 2 + 1])
            } else {
                (0, 0) // i16 域的静音；转换到 T 后自然就是该格式的静音
            };
            let base = (frame + f) * channels;
            for ch in 0..channels {
                // 上混：0→左、1→右，其余声道静音
                let v = if ch == 0 {
                    l
                } else if ch == 1 {
                    r
                } else {
                    0
                };
                out[base + ch] = T::from_sample(v);
            }
        }

        if got < want {
            starved = true;
        }
        frame += got_frames;
        if starved {
            break;
        }
    }

    if starved {
        consumer.count_underruns(1);
        // ⚠️ 静音值随 T 而变：f32→0.0、i16→0、**u8→128**。
        //    手写 `*s = 0` 在 u8 设备上会产生满幅直流噪声。必须用 EQUILIBRIUM。
        //    （上面的预填充在语义上已经覆盖到这里；保留这一句是为了让
        //      「饿死 ⇒ 后半块是静音」这条不变量在本地可见。）
        out[frame * channels..].fill(T::EQUILIBRIUM);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 以下是 `fill_from_ring`（音频回调体）的测试 ──
    //
    // `output::tests` 那 4 个测试只覆盖 `AudioRing` 本身；`fill_from_ring`
    // 是唯一真正在音频线程里跑的函数，却一行都没被执行过。它同时是本任务
    // 最容易出错的地方（越界、留垃圾值、静音补在错误的域、欠载计数口径），
    // 所以单独测。
    //
    // 这 4 个测试是**实现时补的**，brief 的 Step 1 里没有。它们不改变任何
    // brief 要求的接口或行为，只是把「回调必须填满整块缓冲」这条已经写在
    // brief 正文里的要求变成可执行的断言。

    /// 回调必须把**整块** `out` 写满，且尾部静音要在**目标格式 `T` 的域**里补。
    #[test]
    fn callback_fills_silence_in_target_format_not_zero() {
        let ring = AudioRing::new(16);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[1000, -1000, 2000, -2000]); // 2 帧立体声
        let mut scratch = vec![0i16; 8]; // 4 帧容量
        let mut out = [123u8; 8]; // 4 帧 × 2ch，故意预填垃圾值

        fill_from_ring::<u8>(&mut out, 2, &mut cons, &mut scratch);

        // 头两帧是真实数据，走的是与回调同一条 `T::from_sample` 通路
        use cpal::Sample; // `u8::from_sample` 需要 trait 在作用域里
        assert_eq!(out[0], u8::from_sample(1000i16));
        assert_eq!(out[1], u8::from_sample(-1000i16));
        assert_eq!(out[2], u8::from_sample(2000i16));
        assert_eq!(out[3], u8::from_sample(-2000i16));
        // 尾两帧：**u8 的静音是 128 不是 0**。写 0 是满幅直流噪声（爆音）。
        // 这里硬写 128 而不是引用 `T::EQUILIBRIUM`，避免「实现和断言一起错」。
        assert_eq!(&out[4..], &[128u8; 4], "u8 静音必须是 128");
        assert_eq!(cons.underruns(), 1, "一次回调只记一次欠载事件");
    }

    /// scratch 小于本次请求时，分块循环必须继续取，不能截断成一次。
    #[test]
    fn callback_chunks_when_scratch_is_smaller_than_request() {
        let ring = AudioRing::new(64);
        let (mut prod, mut cons) = ring.split();
        let data: Vec<i16> = (0..40).map(|i| i as i16).collect(); // 20 帧立体声
        prod.push_samples(&data);

        let mut scratch = vec![0i16; 4]; // 只能装 2 帧，远小于 20 帧的请求
        let mut out = [0i16; 40];
        fill_from_ring::<i16>(&mut out, 2, &mut cons, &mut scratch);

        assert_eq!(out, data[..], "小 scratch 也必须填满整块，不能截断");
        assert_eq!(cons.underruns(), 0, "数据够时不该记欠载");
    }

    /// 欠载的计数口径是**每次回调一次事件**，不是「缺几个样本算几次」。
    #[test]
    fn callback_counts_one_underrun_event_per_callback() {
        let ring = AudioRing::new(16);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[1, 2]); // 只有 1 帧

        let mut scratch = vec![0i16; 8];
        let mut out = [7i16; 40]; // 要 20 帧，预填 7 用来暴露「没写到的槽位」
        fill_from_ring::<i16>(&mut out, 2, &mut cons, &mut scratch);

        assert_eq!(out[..2], [1, 2]);
        assert!(
            out[2..].iter().all(|&s| s == 0),
            "尾部必须全部补静音，不能留下 out 的原值 7（那是爆音）"
        );
        assert_eq!(cons.underruns(), 1, "口径：一次回调 = 一次事件");
    }

    /// 单声道设备只有 ch0；右声道数据按「上混」规则被丢掉，但不能越界。
    #[test]
    fn callback_handles_mono_without_indexing_past_end() {
        let ring = AudioRing::new(16);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[11, 22, 33, 44]); // 2 帧立体声

        let mut scratch = vec![0i16; 8];
        let mut out = [0i16; 2]; // 2 帧 × 1ch
        fill_from_ring::<i16>(&mut out, 1, &mut cons, &mut scratch);

        assert_eq!(out, [11, 33], "单声道取每帧的左声道");
        assert_eq!(cons.underruns(), 0);
    }

    /// `I24` 是本机设备**确实报告支持**的格式（brief 特别点名：漏了会掉进 fallback），
    /// 但它永远轮不到在 F32 设备上被真机调用 —— 所以在这里实例化一次，
    /// 别让它只停留在「编译过」。
    #[test]
    fn callback_works_for_i24_target_format() {
        use cpal::{I24, Sample as _};

        let ring = AudioRing::new(16);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[0, -32768, 32767, 100]); // 2 帧立体声
        let mut scratch = vec![0i16; 8];
        let mut out = [I24::EQUILIBRIUM; 4];
        fill_from_ring::<I24>(&mut out, 2, &mut cons, &mut scratch);

        // ⚠️ 期望值**硬写 I24 的位模式**，不要写 `I24::from_sample(同一个输入)` ——
        //    那**就是实现里的表达式**（`fill_from_ring` 里的 `T::from_sample(v)`），
        //    只能抓路由/索引错，抓不到转换错（「把同样的算术抄一遍」）。
        //    I24 是「i32 里放 24 位有效位」，i16 → I24 是 `<< 8`
        //    （已核实 dasp_sample-0.11.0/src/conv.rs:186 `I24::new_unchecked((s as i32) << 8)`）：
        //    -32768 → -8388608（= I24::MIN）、32767 → 8388352、100 → 25600。
        assert_eq!(out[0], I24::EQUILIBRIUM, "静音仍是 0");
        assert_eq!(
            out[1],
            I24::new_unchecked(-8_388_608),
            "负满幅 = -32768 << 8"
        );
        assert_eq!(out[2], I24::new_unchecked(8_388_352), "正满幅 = 32767 << 8");
        assert_eq!(out[3], I24::new_unchecked(25_600), "100 << 8");
        assert_eq!(cons.underruns(), 0);
        // 回调整体在 i16 域里补静音，靠 `from_sample` 转成目标格式的静音值。
        // I24 的静音是 0（不是 u8 的 128）—— 这条把该假设对 I24 钉死。
        assert_eq!(
            I24::from_sample(0i16),
            I24::EQUILIBRIUM,
            "i16 域的静音在 I24 上应映射到 I24 的静音"
        );
    }

    /// 环里只剩**半个**立体声帧时，那半个样本必须**留在环里**，不能取出来丢掉。
    ///
    /// 丢掉它不会当场出错（本次回调只是补静音 + 记一次欠载），但它后面的样本
    /// 还在环里 ⇒ **此后所有 L/R 配对整体错位**，听感是声道互换，且不可恢复。
    #[test]
    fn callback_leaves_a_half_frame_in_the_ring_instead_of_dropping_it() {
        let ring = AudioRing::new(16);
        let (mut prod, mut cons) = ring.split();
        prod.push_samples(&[1, 2, 3]); // 1.5 帧：3 是下一帧的左声道
        let mut scratch = vec![0i16; 8];
        let mut out = [7i16; 4]; // 2 帧，预填 7 用来暴露「没写到的槽位」

        fill_from_ring::<i16>(&mut out, 2, &mut cons, &mut scratch);
        assert_eq!(out, [1, 2, 0, 0], "只有整帧可用，尾部补静音");
        assert_eq!(cons.underruns(), 1, "半个帧也算饿死");
        assert_eq!(
            cons.available(),
            1,
            "那半个样本必须还在环里，不能被取走丢掉"
        );

        // 下一帧的右声道到了：它必须**接在那半个样本后面**。
        // 若上一步把 3 丢了，这里会读成 [4, 5, ...] —— 也就是 L/R 互换。
        prod.push_samples(&[4, 5, 6]);
        fill_from_ring::<i16>(&mut out, 2, &mut cons, &mut scratch);
        assert_eq!(out, [3, 4, 5, 6], "半个样本接下一帧 ⇒ L/R 配对不错位");
        assert_eq!(cons.underruns(), 1, "这次数据够，不再记欠载");
    }

    // ── 以下是**设备能力判定**的纯函数测试（不需要声卡） ──
    //
    // `find_config_at_rate` 本身要真设备才跑得起来（`AudioEngine::new` 那一路是
    // 手动冒烟 + CLI 掐表验的），但它的两个判据是纯函数，而且正是「会不会静默退回
    // 那个变速变调的缺陷」的分界线，所以单独钉住。

    /// `is_supported_format` 必须与 `build_stream_for` 的 match 臂**逐条对齐**：
    /// 说「支持」而那里掉进 `other` 分支，就会白建一次流再失败。
    #[test]
    fn supported_format_set_matches_build_stream_for_arms() {
        use cpal::SampleFormat as F;
        for f in [F::F32, F::I16, F::I32, F::I24, F::U8] {
            assert!(is_supported_format(f), "{f:?} 在 build_stream_for 里有分支");
        }
        // 其余变体（0.18 一共 15 个）都必须被判为「不支持」，
        // 否则我们会拿一个注定失败去建流。
        for f in [
            F::I8,
            F::I64,
            F::U16,
            F::U24,
            F::U32,
            F::U64,
            F::F64,
            F::DsdU8,
            F::DsdU16,
            F::DsdU32,
        ] {
            assert!(
                !is_supported_format(f),
                "{f:?} 在 build_stream_for 里没有分支"
            );
        }
    }

    /// 采样格式的偏好次序。这是「直通那一档到底有多直」的最后一道关：
    /// 选到 `u8` 就等于把 16 位源砍成 8 位，**而采样率是对的、xrun 是 0** ——
    /// 又一个「指标全正常但声音是错的」。
    #[test]
    fn format_rank_prefers_device_default_then_lossless_then_u8() {
        use cpal::SampleFormat as F;

        // 默认格式永远第一
        for d in [F::F32, F::I16, F::U8, F::I32] {
            assert_eq!(format_rank(d, Some(d)), 0, "{d:?} 是默认格式时排第一");
        }
        // 非默认时：u8 有损，排最后；其余（i16→T 无损）排中间。
        // 这里用**永远不会出现在候选里**的 `F64` 当「设备的默认格式」，
        // 否则循环到自己那一档时会因为「它就是默认」而拿到 0 —— 那是另一条规则，
        // 会让这条断言测不到「无损 vs 有损」这件事。
        let other_default = Some(F::F64);
        assert!(
            format_rank(F::U8, other_default) > format_rank(F::I16, other_default),
            "u8 有损，必须比无损格式差"
        );
        for f in [F::F32, F::I16, F::I32, F::I24] {
            assert_eq!(
                format_rank(f, other_default),
                1,
                "{f:?} 相对 i16 是无损的，应与其它无损格式同级"
            );
        }
        // 设备没报默认格式时（`default_output_config()` 失败）也不能把 u8 排前面
        assert!(format_rank(F::U8, None) > format_rank(F::F32, None));
    }

    /// 声道数的偏好：**能装下 2 声道的排前面，单声道排最后**。
    ///
    /// 这条的来历是实测：本机设备同时报了 1ch 与 2ch 的 48000Hz 配置，
    /// 而早先只按采样格式排序会**挑中 1ch 那条** —— 右声道被整条丢掉，
    /// 而采样率是对的、xrun 是 0。CLI 实测日志里那行 `@ 48000Hz 1ch` 就是它。
    #[test]
    fn channel_rank_prefers_stereo_over_mono() {
        assert_eq!(channel_rank(2, Some(2)), 0, "默认就是 2 声道时排第一");
        assert_eq!(channel_rank(6, Some(6)), 0);
        assert!(
            channel_rank(2, Some(6)) < channel_rank(1, Some(6)),
            "设备默认是 6 声道时，2 声道仍必须优于单声道（单声道会丢右声道）"
        );
        assert!(
            channel_rank(1, Some(1)) > channel_rank(2, Some(1)),
            "就算设备自己默认单声道，2 声道也比它好 —— 我们的内容本来就是 2 声道"
        );
        assert!(channel_rank(0, None) > channel_rank(2, None));
    }

    /// 组合起来的偏好次序：**声道数优先于采样格式**。
    ///
    /// 一个「2 声道 + u8」必须赢过「1 声道 + F32」：丢一个声道比 8 位量化严重得多。
    #[test]
    fn stereo_beats_mono_even_with_a_worse_sample_format() {
        use cpal::SampleFormat as F;
        let stereo_u8 = (channel_rank(2, Some(1)), format_rank(F::U8, Some(F::F32)));
        let mono_f32 = (channel_rank(1, Some(1)), format_rank(F::F32, Some(F::F32)));
        assert!(stereo_u8 < mono_f32, "2 声道 u8 应优于 1 声道 F32");
    }
}
