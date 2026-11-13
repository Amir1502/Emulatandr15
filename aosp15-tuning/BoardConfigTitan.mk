# Titan Zero-Overhead Android 15 (AOSP API 35) Board Configuration
TARGET_ARCH := x86_64
TARGET_ARCH_VARIANT := x86_64
TARGET_CPU_ABI := x86_64
TARGET_2ND_ARCH := x86
TARGET_2ND_ARCH_VARIANT := x86_64
TARGET_2ND_CPU_ABI := x86

# Target x86-64-v3 (AVX2, FMA, BMI1/BMI2, MOVBE) + ThinLTO + PGO
TARGET_GLOBAL_CFLAGS += -O3 -march=x86-64-v3 -mavx2 -mfma -fomit-frame-pointer
TARGET_GLOBAL_CPPFLAGS += -O3 -march=x86-64-v3 -mavx2 -mfma -fomit-frame-pointer
ENABLE_LTO := true
GLOBAL_THINLTO := true
PGO_PROFILE_DIR := vendor/titan/pgo-profiles
BOARD_USE_PGO := true

# Graphics: VirtIO-GPU (Venus Vulkan 1.3) + ANGLE (GLES 3.2 -> Vulkan) + HWC3
BOARD_USES_VIRTIO_GPU_VENUS := true
BOARD_GPU_DRIVERS := venus
TARGET_USES_VULKAN := true
TARGET_USES_HWC3 := true
USE_OPENGL_RENDERER := false
OVERRIDE_RS_DRIVER := libRSDriver_arm.so

# Kernel & Memory: 2MB HugePages / Large Pages + VirtIO-BLK + VirtIO-SND
BOARD_KERNEL_CMDLINE += transparent_hugepage=always hugepagesz=2M default_hugepagesz=2M
BOARD_KERNEL_CMDLINE += mitigations=off tsc=reliable clocksource=tsc-early
BOARD_KERNEL_CMDLINE += pci=nomsi,noaer virtio_mmio.device=4K@0x30000000:5
BOARD_KERNEL_CMDLINE += quiet loglevel=3 androidboot.hardware=titan_x86_64
