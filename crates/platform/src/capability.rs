/// 渲染能力档。设计文档 §17.3。
///
/// **原则：能力不足时降级质量，而不是拒绝运行。**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTier {
    /// 完整：Rgba16Float 线性中间格式、7 通道色散、完整折射
    A,
    /// 降级：Rgba8Unorm + dithering、3 通道色散
    B,
    /// 最低：无中间格式、关闭色散与折射
    C,
}

#[derive(Debug, Clone)]
pub struct AdapterInfo {
    pub name: String,
    pub backend: String,
    pub is_software: bool,
}

/// 软件光栅化适配器名单。
///
/// **这条防线很重要**：wgpu 选中软件光栅化时**性能慢两个数量级且不报错**
/// （本机实测 Vulkan 设备列表里就有 llvmpipe）。设计文档 §16.0 坑 2。
const SOFTWARE_MARKERS: &[&str] = &["llvmpipe", "lavapipe", "swiftshader", "softpipe"];

pub fn is_software_adapter(name: &str) -> bool {
    let lower = name.to_lowercase();
    SOFTWARE_MARKERS.iter().any(|m| lower.contains(m))
}

pub fn detect_tier(info: &AdapterInfo, supports_16f_blend: bool) -> RenderTier {
    if info.is_software {
        return RenderTier::C;
    }
    if supports_16f_blend {
        RenderTier::A
    } else {
        RenderTier::B
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llvmpipe_is_software() {
        assert!(is_software_adapter("llvmpipe (LLVM 22.1.8, 256 bits)"));
    }

    #[test]
    fn lavapipe_is_software() {
        assert!(is_software_adapter("lavapipe"));
    }

    #[test]
    fn swiftshader_is_software() {
        assert!(is_software_adapter("SwiftShader Device (Subzero)"));
    }

    #[test]
    fn real_gpu_is_not_software() {
        assert!(!is_software_adapter("NVIDIA GeForce GTX 1650"));
        assert!(!is_software_adapter("AMD Radeon Graphics (RADV RENOIR)"));
        assert!(!is_software_adapter("Apple M2"));
    }

    #[test]
    fn software_detection_is_case_insensitive() {
        assert!(is_software_adapter("LLVMpipe"));
    }

    #[test]
    fn tier_a_when_16f_blend_supported() {
        let info = AdapterInfo {
            name: "NVIDIA GeForce GTX 1650".into(),
            backend: "Vulkan".into(),
            is_software: false,
        };
        assert_eq!(detect_tier(&info, true), RenderTier::A);
    }

    #[test]
    fn tier_b_when_no_16f_blend() {
        let info = AdapterInfo {
            name: "Mali-G52".into(),
            backend: "Gl".into(),
            is_software: false,
        };
        assert_eq!(detect_tier(&info, false), RenderTier::B);
    }

    #[test]
    fn tier_c_when_software() {
        let info = AdapterInfo {
            name: "llvmpipe".into(),
            backend: "Vulkan".into(),
            is_software: true,
        };
        assert_eq!(detect_tier(&info, true), RenderTier::C);
    }
}
