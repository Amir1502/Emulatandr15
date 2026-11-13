# Titan Minimalist AOSP 15 (API 35) Product Definition — Zero Bloat Profile
$(call inherit-product, $(SRC_TARGET_DIR)/product/core_64_bit_only.mk)
$(call inherit-product, $(SRC_TARGET_DIR)/product/aosp_base.mk)

PRODUCT_NAME := titan_x86_64
PRODUCT_DEVICE := titan_pc
PRODUCT_BRAND := Titan
PRODUCT_MODEL := Titan AOSP 15 Zero-Overhead PC
PRODUCT_MANUFACTURER := TitanSystems

# Vulkan 1.3 + ANGLE (OpenGL ES 3.x -> Vulkan 1.3 translation)
PRODUCT_PROPERTY_OVERRIDES += \
    ro.hardware.vulkan=venus \
    ro.hardware.egl=angle \
    ro.gfx.angle.supported=true \
    persist.graphics.vulkan.disable_protected_contents=true \
    ro.surface_flinger.use_content_detection_for_refresh_rate=true \
    ro.surface_flinger.set_idle_timer_ms=0 \
    ro.surface_flinger.set_touch_timer_ms=0 \
    ro.surface_flinger.max_frame_buffer_acquired_buffers=3 \
    debug.sf.latch_unsignaled=1 \
    debug.sf.auto_latch_unsignaled=1 \
    debug.sf.enable_gl_backpressure=0 \
    debug.sf.early.app.duration=750000 \
    debug.sf.early.sf.duration=500000 \
    debug.sf.late.app.duration=750000 \
    debug.sf.late.sf.duration=500000 \
    debug.sf.disable_backpressure=1 \
    debug.hwui.renderer=skiavk \
    aaudio.hw_burst_min_usec=2000 \
    aaudio.mmap_policy=2 \
    aaudio.mmap_exclusive_policy=2

# Disable non-essential background Android daemons to preserve 100% P-core budget
PRODUCT_PROPERTY_OVERRIDES += \
    ro.config.low_ram=false \
    dalvik.vm.dex2oat-threads=4 \
    dalvik.vm.image-dex2oat-threads=4 \
    persist.sys.dalvik.vm.lib.2=libart.so \
    ro.telephony.disable=true \
    config.disable_atlas=true \
    persist.traced.enable=0 \
    debug.atrace.tags.enableflags=0
