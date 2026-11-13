//! Stage 4: AOSP 15 (API 35) Max-FPS Tuning Inspection & Compiler Profile Validation.

pub const SURFACEFLINGER_PATCH: &str =
    include_str!("../aosp15-tuning/0001-SurfaceFlinger-240Hz-VRR-ZeroCopy-VirtIO-GPU.patch");
pub const BOARD_CONFIG_MK: &str = include_str!("../aosp15-tuning/BoardConfigTitan.mk");
pub const PRODUCT_MK: &str = include_str!("../aosp15-tuning/titan_x86_64.mk");
pub const INIT_TITAN_RC: &str = include_str!("../aosp15-tuning/init.titan.rc");

#[derive(Debug, Clone)]
pub struct Aosp15TuningSummary {
    pub api_level: u32,
    pub target_isa: &'static str,
    pub vrr_modes_hz: [u32; 3],
    pub lto_mode: &'static str,
    pub pgo_enabled: bool,
    pub vulkan_backend: &'static str,
    pub disabled_daemons: &'static [&'static str],
}

impl Aosp15TuningSummary {
    pub fn current() -> Self {
        Self {
            api_level: 35,
            target_isa: "x86-64-v3 (AVX2 + FMA + BMI2)",
            vrr_modes_hz: [120, 144, 240],
            lto_mode: "ThinLTO + FatLTO (Host)",
            pgo_enabled: true,
            vulkan_backend: "VirtIO-GPU Venus (Vulkan 1.3) + ANGLE (GLES 3.x)",
            disabled_daemons: &[
                "ril-daemon",
                "traced",
                "traced_probes",
                "statsd",
                "logd",
                "tombstoned",
                "incidentd",
                "update_engine",
            ],
        }
    }

    pub fn validate_artifacts(&self) -> Result<(), String> {
        for hz in self.vrr_modes_hz {
            if !SURFACEFLINGER_PATCH.contains(&hz.to_string()) {
                return Err(format!("SurfaceFlinger patch missing VRR mode {hz} Hz"));
            }
        }
        if !BOARD_CONFIG_MK.contains("-march=x86-64-v3") || !BOARD_CONFIG_MK.contains("-mavx2") {
            return Err("BoardConfigTitan.mk missing x86-64-v3 / AVX2 compiler flags".into());
        }
        if !BOARD_CONFIG_MK.contains("GLOBAL_THINLTO := true") || !BOARD_CONFIG_MK.contains("BOARD_USE_PGO := true") {
            return Err("BoardConfigTitan.mk missing LTO/PGO flags".into());
        }
        if !SURFACEFLINGER_PATCH.contains("LatchUnsignaledConfig::Always") {
            return Err("SurfaceFlinger patch missing LatchUnsignaledConfig::Always".into());
        }
        if !PRODUCT_MK.contains("ro.hardware.egl=angle") || !PRODUCT_MK.contains("ro.hardware.vulkan=venus") {
            return Err("titan_x86_64.mk missing Venus / ANGLE properties".into());
        }
        for daemon in self.disabled_daemons {
            if !INIT_TITAN_RC.contains(&format!("stop {daemon}")) {
                return Err(format!("init.titan.rc missing stop {daemon}"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aosp15_tuning_artifacts_validate() {
        let summary = Aosp15TuningSummary::current();
        assert_eq!(summary.api_level, 35);
        summary
            .validate_artifacts()
            .expect("all AOSP 15 tuning flags and patches present");
    }
}
