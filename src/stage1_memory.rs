//! Stage 1: Low-Level Architecture & Zero-Copy Shared Memory Layout.
//!
//! Defines the Guest Physical Address (GPA) map and cache-line-isolated (`64 B`)
//! lock-free Single-Producer Single-Consumer (SPSC) IPC primitives housed inside a
//! single 2 MB Large Page (`IvshmemSharedRegion`) shared across WHPX micro-VM guest
//! and Windows host compositor without VM-exit or syscall overhead on hot paths.

#![allow(dead_code)]
use core::mem::size_of;
use core::ptr;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// x86-64 2 MB Large Page size used for SLAT/EPT zero-miss backing.
pub const LARGE_PAGE_SIZE: usize = 2 * 1024 * 1024;

/// Guest Physical Address (GPA) base for main Android 15 RAM (above 2 GB PCI hole).
pub const GPA_LOW_RAM_BASE: u64 = 0x0000_0000_8000_0000;

/// Guest Physical Address (GPA) base for IVSHMEM BAR2 zero-copy control/ring page (2 MB).
pub const GPA_IVSHMEM_BAR2_BASE: u64 = 0x0000_0000_4000_0000;

/// Guest Physical Address (GPA) base for VirtIO MMIO device descriptors.
pub const GPA_VIRTIO_MMIO_BASE: u64 = 0x0000_0000_3000_0000;

/// Magic signature `"TITAN15\0"` stored at offset 0 of `IvshmemSharedRegion`.
pub const IVSHMEM_MAGIC: u64 = 0x5449_5441_4E31_3500;
pub const IVSHMEM_VERSION: u32 = 1;

/// Number of `VirtioInputEvent` slots in `InputSpscRing` (must be power of two).
pub const INPUT_RING_CAPACITY: usize = 4096;
const INPUT_RING_MASK: usize = INPUT_RING_CAPACITY - 1;

/// Number of stereo `f32` PCM frames in `VirtioSndRing` (~85 ms @ 48 kHz, sub-10 ms target fill).
pub const SND_RING_FRAMES: usize = 4096;
const SND_RING_MASK: usize = SND_RING_FRAMES - 1;

/// Byte capacity of `VenusCommandRing` (256 KB, power of two).
pub const VENUS_RING_BYTES: usize = 256 * 1024;
const VENUS_RING_MASK: usize = VENUS_RING_BYTES - 1;

/// Triple-buffered zero-copy frame slots.
pub const FRAME_SLOT_COUNT: usize = 3;

/// Linux `input_event` types mirrored over `virtio-input`.
pub mod ev_type {
    pub const EV_SYN: u16 = 0x00;
    pub const EV_KEY: u16 = 0x01;
    pub const EV_REL: u16 = 0x02;
    pub const EV_ABS: u16 = 0x03;
}

/// Linux `input_event` codes used for RawInput + Android multitouch / pointer passthrough.
pub mod ev_code {
    pub const SYN_REPORT: u16 = 0x00;

    pub const REL_X: u16 = 0x00;
    pub const REL_Y: u16 = 0x01;
    pub const REL_WHEEL: u16 = 0x08;

    pub const ABS_MT_SLOT: u16 = 0x2f;
    pub const ABS_MT_POSITION_X: u16 = 0x35;
    pub const ABS_MT_POSITION_Y: u16 = 0x36;
    pub const ABS_MT_TRACKING_ID: u16 = 0x39;

    pub const BTN_LEFT: u16 = 0x110;
    pub const BTN_RIGHT: u16 = 0x111;
    pub const BTN_MIDDLE: u16 = 0x112;
}

/// Venus / gfxstream pipeline cache status flags.
pub mod venus_flags {
    pub const PIPELINE_CACHE_READY: u32 = 1 << 0;
    pub const PIPELINE_CACHE_DIRTY: u32 = 1 << 1;
    pub const VULKAN_1_3_ACTIVE: u32 = 1 << 2;
    pub const ANGLE_GLES3_ACTIVE: u32 = 1 << 3;
}

/// Single 64-byte cache-line-aligned atomic counter preventing false sharing across cores.
#[repr(C, align(64))]
pub struct CacheAlignedAtomicU64 {
    pub value: AtomicU64,
    _pad: [u8; 56],
}

impl CacheAlignedAtomicU64 {
    #[inline(always)]
    pub const fn new(v: u64) -> Self {
        Self {
            value: AtomicU64::new(v),
            _pad: [0u8; 56],
        }
    }

    #[inline(always)]
    pub fn load_acquire(&self) -> u64 {
        self.value.load(Ordering::Acquire)
    }

    #[inline(always)]
    pub fn load_relaxed(&self) -> u64 {
        self.value.load(Ordering::Relaxed)
    }

    #[inline(always)]
    pub fn store_release(&self, v: u64) {
        self.value.store(v, Ordering::Release);
    }
}

/// 16-byte ABI-compatible `virtio_input_event` payload with nanosecond timestamp.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VirtioInputEvent {
    pub event_type: u16,
    pub code: u16,
    pub value: i32,
    pub timestamp_ns: u64,
}

impl VirtioInputEvent {
    #[inline(always)]
    pub const fn new(event_type: u16, code: u16, value: i32, timestamp_ns: u64) -> Self {
        Self {
            event_type,
            code,
            value,
            timestamp_ns,
        }
    }

    #[inline(always)]
    pub const fn syn(timestamp_ns: u64) -> Self {
        Self::new(ev_type::EV_SYN, ev_code::SYN_REPORT, 0, timestamp_ns)
    }
}

/// Lock-free SPSC ring buffer holding 4096 `VirtioInputEvent`s (64 KB payload).
///
/// Producer (Host RawInput thread) solely updates `head` with `Release` ordering.
/// Consumer (Guest `virtio-input` vCPU / driver) solely updates `tail` with `Release` ordering.
#[repr(C, align(64))]
pub struct InputSpscRing {
    head: CacheAlignedAtomicU64,
    tail: CacheAlignedAtomicU64,
    dropped_events: CacheAlignedAtomicU64,
    events: [VirtioInputEvent; INPUT_RING_CAPACITY],
}

impl InputSpscRing {
    pub fn reset(&mut self) {
        self.head.value.store(0, Ordering::Relaxed);
        self.tail.value.store(0, Ordering::Relaxed);
        self.dropped_events.value.store(0, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        let h = self.head.load_acquire();
        let t = self.tail.load_acquire();
        h.wrapping_sub(t) as usize
    }

    #[inline(always)]
    pub fn free_space(&self) -> usize {
        INPUT_RING_CAPACITY - self.len()
    }

    #[inline(always)]
    pub fn dropped_count(&self) -> u64 {
        self.dropped_events.load_relaxed()
    }

    /// Enqueues a single event without locks or allocations. Returns `false` if full.
    #[inline(always)]
    pub fn try_push(&self, event: VirtioInputEvent) -> bool {
        let head = self.head.load_relaxed();
        let tail = self.tail.load_acquire();
        if head.wrapping_sub(tail) >= INPUT_RING_CAPACITY as u64 {
            self.dropped_events.value.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let slot = (head as usize) & INPUT_RING_MASK;
        // SAFETY: SPSC invariant guarantees the consumer never reads indices >= head,
        // and we checked free space against `tail`.
        unsafe {
            let dst = self.events.as_ptr().add(slot) as *mut VirtioInputEvent;
            ptr::write_volatile(dst, event);
        }
        self.head.store_release(head.wrapping_add(1));
        true
    }

    /// Atomically publishes a batch of events (e.g., MT slot + X + Y + SYN_REPORT)
    /// so the guest never observes a torn multitouch packet.
    #[inline]
    pub fn try_push_batch(&self, batch: &[VirtioInputEvent]) -> bool {
        let count = batch.len() as u64;
        if count == 0 {
            return true;
        }
        let head = self.head.load_relaxed();
        let tail = self.tail.load_acquire();
        if (INPUT_RING_CAPACITY as u64).saturating_sub(head.wrapping_sub(tail)) < count {
            self.dropped_events
                .value
                .fetch_add(count, Ordering::Relaxed);
            return false;
        }
        for (i, &ev) in batch.iter().enumerate() {
            let slot = (head.wrapping_add(i as u64) as usize) & INPUT_RING_MASK;
            // SAFETY: Free slots in [head, head + count) are exclusively owned by producer.
            unsafe {
                let dst = self.events.as_ptr().add(slot) as *mut VirtioInputEvent;
                ptr::write_volatile(dst, ev);
            }
        }
        self.head.store_release(head.wrapping_add(count));
        true
    }

    /// Dequeues a single event on the consumer side.
    #[inline(always)]
    pub fn try_pop(&self) -> Option<VirtioInputEvent> {
        let tail = self.tail.load_relaxed();
        let head = self.head.load_acquire();
        if tail == head {
            return None;
        }
        let slot = (tail as usize) & INPUT_RING_MASK;
        // SAFETY: Producer completed writing `slot` before advancing `head` with Release.
        let ev = unsafe { ptr::read_volatile(self.events.as_ptr().add(slot)) };
        self.tail.store_release(tail.wrapping_add(1));
        Some(ev)
    }

    /// Dequeues up to `out.len()` events in one Acquire/Release pair.
    #[inline]
    pub fn try_pop_batch(&self, out: &mut [VirtioInputEvent]) -> usize {
        let tail = self.tail.load_relaxed();
        let head = self.head.load_acquire();
        let avail = head.wrapping_sub(tail) as usize;
        let to_read = avail.min(out.len());
        for (i, dst) in out.iter_mut().take(to_read).enumerate() {
            let slot = (tail.wrapping_add(i as u64) as usize) & INPUT_RING_MASK;
            // SAFETY: `[tail, tail + to_read)` is published and immutable until `tail` advances.
            *dst = unsafe { ptr::read_volatile(self.events.as_ptr().add(slot)) };
        }
        if to_read > 0 {
            self.tail.store_release(tail.wrapping_add(to_read as u64));
        }
        to_read
    }
}

/// Zero-copy GPU framebuffer descriptor shared between Guest `SurfaceFlinger/HWC3`
/// and Host DirectX 12 SwapChain (`VK_KHR_external_memory_win32` / DXGI NT Handle).
#[repr(C, align(64))]
#[derive(Clone, Copy, Debug, Default)]
pub struct SharedFrameDescriptor {
    /// NT Shared Handle (`HANDLE` cast to `u64`) for `ID3D12Device::OpenSharedHandle`.
    pub nt_shared_handle: u64,
    /// `DXGI_FORMAT` (e.g. `DXGI_FORMAT_B8G8R8A8_UNORM = 87` or `R8G8B8A8_UNORM = 28`).
    pub dxgi_format: u32,
    pub width: u32,
    pub height: u32,
    pub row_pitch: u32,
    /// Target Variable Refresh Rate (120 / 144 / 240 Hz).
    pub vrr_refresh_hz: u32,
    /// Monotonic fence value signaled by guest GPU (`gfxstream`/Vulkan) upon frame completion.
    pub acquire_fence_value: u64,
    /// Monotonic fence value signaled by host DX12 queue after blit/compose completes.
    pub release_fence_value: u64,
    pub frame_index: u64,
    /// Guest `QueryPerformanceCounter` timestamp at `HWC3::presentDisplay`.
    pub guest_present_qpc: u64,
    /// Reflex / Anti-Lag simulation start timestamp (microseconds).
    pub reflex_sim_start_us: u64,
    /// Reflex / Anti-Lag render submission timestamp (microseconds).
    pub reflex_render_submit_us: u64,
    /// Reflex / Anti-Lag host SwapChain present timestamp (microseconds).
    pub reflex_present_us: u64,
    /// Packed HDR10 / color space flags.
    pub hdr_metadata_packed: u32,
    pub _reserved: u32,
}

impl SharedFrameDescriptor {
    /// Returns total end-to-end Reflex pipeline latency (`present - sim_start`) in microseconds.
    #[inline]
    pub const fn reflex_total_latency_us(&self) -> u64 {
        self.reflex_present_us
            .saturating_sub(self.reflex_sim_start_us)
    }
}

/// Triple-buffered lock-free mailbox enabling non-blocking guest HWC3 presentation
/// and host compositor consumption without ever dropping into OS locks.
#[repr(C, align(64))]
pub struct FrameMailbox {
    /// Index (`0..FRAME_SLOT_COUNT`) of the most recently published complete frame.
    published_slot: CacheAlignedAtomicU64,
    /// Monotonically incremented frame sequence number.
    published_seq: CacheAlignedAtomicU64,
    /// Last sequence number consumed by the host DX12 renderer.
    consumed_seq: CacheAlignedAtomicU64,
    pub slots: [SharedFrameDescriptor; FRAME_SLOT_COUNT],
}

impl FrameMailbox {
    pub fn reset(&mut self) {
        self.published_slot.value.store(0, Ordering::Relaxed);
        self.published_seq.value.store(0, Ordering::Relaxed);
        self.consumed_seq.value.store(0, Ordering::Relaxed);
        for slot in &mut self.slots {
            *slot = SharedFrameDescriptor::default();
        }
    }

    /// Guest publishes a completed frame into the next triple-buffer slot.
    #[inline]
    pub fn publish(&self, desc: SharedFrameDescriptor) -> u64 {
        let next_seq = self.published_seq.load_relaxed().wrapping_add(1);
        let slot_idx = (next_seq as usize) % FRAME_SLOT_COUNT;
        // SAFETY: Triple-buffer ensures writer writes to `(seq + 1) % 3` while reader
        // only inspects the previously published slot.
        unsafe {
            let dst = self.slots.as_ptr().add(slot_idx) as *mut SharedFrameDescriptor;
            ptr::write_volatile(dst, desc);
        }
        self.published_slot.store_release(slot_idx as u64);
        self.published_seq.store_release(next_seq);
        next_seq
    }

    /// Host polls the latest published frame if a newer sequence has arrived.
    #[inline]
    pub fn acquire_latest(&self) -> Option<(u64, SharedFrameDescriptor)> {
        let seq = self.published_seq.load_acquire();
        let last = self.consumed_seq.load_relaxed();
        if seq == 0 || seq == last {
            return None;
        }
        let slot_idx = (self.published_slot.load_acquire() as usize) % FRAME_SLOT_COUNT;
        // SAFETY: `published_slot` and `published_seq` were published with Release.
        let desc = unsafe { ptr::read_volatile(self.slots.as_ptr().add(slot_idx)) };
        self.consumed_seq.store_release(seq);
        Some((seq, desc))
    }

    /// Reads the currently active descriptor without advancing `consumed_seq`.
    #[inline]
    pub fn peek_current(&self) -> SharedFrameDescriptor {
        let slot_idx = (self.published_slot.load_acquire() as usize) % FRAME_SLOT_COUNT;
        unsafe { ptr::read_volatile(self.slots.as_ptr().add(slot_idx)) }
    }
}

/// 256 KB lock-free SPSC byte ring for `VirtIO-GPU (Venus / gfxstream)` Vulkan 1.3 command streams.
#[repr(C, align(64))]
pub struct VenusCommandRing {
    head: CacheAlignedAtomicU64,
    tail: CacheAlignedAtomicU64,
    pub flags: AtomicU32,
    pub active_pipelines: AtomicU32,
    _pad: [u8; 56],
    buffer: [u8; VENUS_RING_BYTES],
}

impl VenusCommandRing {
    pub fn reset(&mut self) {
        self.head.value.store(0, Ordering::Relaxed);
        self.tail.value.store(0, Ordering::Relaxed);
        self.flags
            .store(venus_flags::VULKAN_1_3_ACTIVE | venus_flags::ANGLE_GLES3_ACTIVE, Ordering::Relaxed);
        self.active_pipelines.store(0, Ordering::Relaxed);
    }

    #[inline(always)]
    pub fn available_bytes(&self) -> usize {
        let h = self.head.load_acquire();
        let t = self.tail.load_acquire();
        h.wrapping_sub(t) as usize
    }

    #[inline(always)]
    pub fn free_bytes(&self) -> usize {
        VENUS_RING_BYTES - self.available_bytes()
    }

    /// Enqueues a length-prefixed (`u32` LE) Venus Vulkan 1.3 command packet.
    pub fn try_push_packet(&self, payload: &[u8]) -> bool {
        let needed = 4 + payload.len();
        if payload.len() > u32::MAX as usize || needed > self.free_bytes() {
            return false;
        }
        let head = self.head.load_relaxed();
        let len_bytes = (payload.len() as u32).to_le_bytes();
        self.write_wrap(head, &len_bytes);
        self.write_wrap(head.wrapping_add(4), payload);
        self.head.store_release(head.wrapping_add(needed as u64));
        true
    }

    /// Dequeues a length-prefixed Venus command packet into `out`, returning payload length.
    pub fn try_pop_packet(&self, out: &mut [u8]) -> Option<usize> {
        let tail = self.tail.load_relaxed();
        let head = self.head.load_acquire();
        let avail = head.wrapping_sub(tail) as usize;
        if avail < 4 {
            return None;
        }
        let mut len_bytes = [0u8; 4];
        self.read_wrap(tail, &mut len_bytes);
        let payload_len = u32::from_le_bytes(len_bytes) as usize;
        if avail < 4 + payload_len || out.len() < payload_len {
            return None;
        }
        self.read_wrap(tail.wrapping_add(4), &mut out[..payload_len]);
        self.tail
            .store_release(tail.wrapping_add((4 + payload_len) as u64));
        Some(payload_len)
    }

    #[inline]
    fn write_wrap(&self, offset: u64, src: &[u8]) {
        let start = (offset as usize) & VENUS_RING_MASK;
        let first = src.len().min(VENUS_RING_BYTES - start);
        // SAFETY: Caller verified free space >= written slice length.
        unsafe {
            let base = self.buffer.as_ptr() as *mut u8;
            ptr::copy_nonoverlapping(src.as_ptr(), base.add(start), first);
            if first < src.len() {
                ptr::copy_nonoverlapping(src.as_ptr().add(first), base, src.len() - first);
            }
        }
    }

    #[inline]
    fn read_wrap(&self, offset: u64, dst: &mut [u8]) {
        let start = (offset as usize) & VENUS_RING_MASK;
        let first = dst.len().min(VENUS_RING_BYTES - start);
        // SAFETY: Caller verified available bytes >= read slice length.
        unsafe {
            let base = self.buffer.as_ptr();
            ptr::copy_nonoverlapping(base.add(start), dst.as_mut_ptr(), first);
            if first < dst.len() {
                ptr::copy_nonoverlapping(base, dst.as_mut_ptr().add(first), dst.len() - first);
            }
        }
    }
}

/// Lock-free stereo `f32` audio ring (`AAudio -> virtio-snd -> WASAPI Exclusive/Shared`, <10 ms).
#[repr(C, align(64))]
pub struct VirtioSndRing {
    head: CacheAlignedAtomicU64,
    tail: CacheAlignedAtomicU64,
    pub sample_rate_hz: AtomicU32,
    pub period_frames: AtomicU32,
    _pad: [u8; 56],
    frames: [[f32; 2]; SND_RING_FRAMES],
}

impl VirtioSndRing {
    pub fn reset(&mut self) {
        self.head.value.store(0, Ordering::Relaxed);
        self.tail.value.store(0, Ordering::Relaxed);
        self.sample_rate_hz.store(48_000, Ordering::Relaxed);
        // 192 frames @ 48 kHz = 4.0 ms period (< 10 ms target).
        self.period_frames.store(192, Ordering::Relaxed);
    }

    #[inline]
    pub fn available_frames(&self) -> usize {
        self.head
            .load_acquire()
            .wrapping_sub(self.tail.load_acquire()) as usize
    }

    /// Pushes as many stereo frames as fit without blocking; returns count written.
    pub fn push_frames(&self, src: &[[f32; 2]]) -> usize {
        let head = self.head.load_relaxed();
        let tail = self.tail.load_acquire();
        let free = SND_RING_FRAMES.saturating_sub(head.wrapping_sub(tail) as usize);
        let n = src.len().min(free);
        for (i, &frame) in src.iter().take(n).enumerate() {
            let slot = (head.wrapping_add(i as u64) as usize) & SND_RING_MASK;
            unsafe {
                let dst = self.frames.as_ptr().add(slot) as *mut [f32; 2];
                ptr::write_volatile(dst, frame);
            }
        }
        if n > 0 {
            self.head.store_release(head.wrapping_add(n as u64));
        }
        n
    }

    /// Pops up to `dst.len()` stereo frames into the WASAPI render buffer.
    pub fn pop_frames(&self, dst: &mut [[f32; 2]]) -> usize {
        let tail = self.tail.load_relaxed();
        let head = self.head.load_acquire();
        let avail = head.wrapping_sub(tail) as usize;
        let n = dst.len().min(avail);
        for (i, slot_out) in dst.iter_mut().take(n).enumerate() {
            let slot = (tail.wrapping_add(i as u64) as usize) & SND_RING_MASK;
            *slot_out = unsafe { ptr::read_volatile(self.frames.as_ptr().add(slot)) };
        }
        if n > 0 {
            self.tail.store_release(tail.wrapping_add(n as u64));
        }
        n
    }

    /// Computes theoretical ring buffer latency in milliseconds at the configured sample rate.
    pub fn period_latency_ms(&self) -> f64 {
        let sr = self.sample_rate_hz.load(Ordering::Relaxed).max(1) as f64;
        let pf = self.period_frames.load(Ordering::Relaxed) as f64;
        (pf / sr) * 1000.0
    }
}

/// Complete IVSHMEM BAR2 control & lock-free ring page mapped at `GPA_IVSHMEM_BAR2_BASE`.
///
/// Strictly sized to fit inside a single 2 MB Large Page (`<= LARGE_PAGE_SIZE`).
#[repr(C, align(4096))]
pub struct IvshmemSharedRegion {
    pub magic: u64,
    pub version: u32,
    pub region_bytes: u32,
    pub guest_ram_bytes: u64,
    pub host_feature_flags: AtomicU32,
    pub guest_ready_flags: AtomicU32,
    _header_pad: [u8; 32],
    pub frame_mailbox: FrameMailbox,
    pub input_ring: InputSpscRing,
    pub audio_ring: VirtioSndRing,
    pub venus_ring: VenusCommandRing,
}

impl IvshmemSharedRegion {
    /// Initializes an `IvshmemSharedRegion` directly inside a pre-allocated 2 MB page
    /// without overflowing the thread stack.
    ///
    /// # Safety
    /// `ptr` must point to writable, 4096-byte aligned memory of at least `size_of::<Self>()` bytes.
    pub unsafe fn init_in_place(ptr: *mut Self, guest_ram_bytes: u64) -> &'static mut Self {
        ptr::write_bytes(ptr as *mut u8, 0, size_of::<Self>());
        let region = &mut *ptr;
        region.magic = IVSHMEM_MAGIC;
        region.version = IVSHMEM_VERSION;
        region.region_bytes = size_of::<Self>() as u32;
        region.guest_ram_bytes = guest_ram_bytes;
        region.host_feature_flags.store(0b111, Ordering::Relaxed);
        region.guest_ready_flags.store(0, Ordering::Relaxed);
        region.frame_mailbox.reset();
        region.input_ring.reset();
        region.audio_ring.reset();
        region.venus_ring.reset();
        region
    }

    #[inline]
    pub fn validate_header(&self) -> bool {
        self.magic == IVSHMEM_MAGIC
            && self.version == IVSHMEM_VERSION
            && self.region_bytes as usize == size_of::<Self>()
            && size_of::<Self>() <= LARGE_PAGE_SIZE
    }
}

/// Compile-time guarantee that the entire shared memory IPC surface fits inside one 2 MB Large Page.
const _: () = assert!(size_of::<IvshmemSharedRegion>() <= LARGE_PAGE_SIZE);
const _: () = assert!(size_of::<CacheAlignedAtomicU64>() == 64);
const _: () = assert!(size_of::<VirtioInputEvent>() == 16);

#[cfg(test)]
mod tests {
    use super::*;
    use std::alloc::{alloc_zeroed, dealloc, Layout};

    struct OwnedRegion {
        ptr: *mut IvshmemSharedRegion,
        layout: Layout,
    }

    impl OwnedRegion {
        fn new(guest_ram_bytes: u64) -> Self {
            let layout = Layout::from_size_align(LARGE_PAGE_SIZE, 4096).unwrap();
            let raw = unsafe { alloc_zeroed(layout) as *mut IvshmemSharedRegion };
            assert!(!raw.is_null());
            unsafe {
                IvshmemSharedRegion::init_in_place(raw, guest_ram_bytes);
            }
            Self { ptr: raw, layout }
        }

        fn region(&self) -> &IvshmemSharedRegion {
            unsafe { &*self.ptr }
        }
    }

    impl Drop for OwnedRegion {
        fn drop(&mut self) {
            unsafe { dealloc(self.ptr as *mut u8, self.layout) };
        }
    }

    #[test]
    fn shared_region_fits_single_large_page_and_validates() {
        assert!(size_of::<IvshmemSharedRegion>() <= LARGE_PAGE_SIZE);
        assert_eq!(core::mem::align_of::<InputSpscRing>(), 64);
        assert_eq!(core::mem::align_of::<SharedFrameDescriptor>(), 64);

        let owned = OwnedRegion::new(4 * 1024 * 1024 * 1024);
        let region = owned.region();
        assert!(region.validate_header());
        assert!(region.audio_ring.period_latency_ms() < 10.0);
    }

    #[test]
    fn input_spsc_ring_wraparound_and_batch_order() {
        let owned = OwnedRegion::new(4096);
        let ring = &owned.region().input_ring;

        // Fill half, drain half, then push across boundary multiple times to exercise wrap-around.
        for round in 0..4u64 {
            let mut batch = [VirtioInputEvent::default(); 3000];
            for (i, item) in batch.iter_mut().enumerate() {
                *item = VirtioInputEvent::new(
                    ev_type::EV_ABS,
                    ev_code::ABS_MT_POSITION_X,
                    (round * 3000 + i as u64) as i32,
                    i as u64 * 100,
                );
            }
            assert!(ring.try_push_batch(&batch));
            assert_eq!(ring.len(), 3000);

            let mut out = vec![VirtioInputEvent::default(); 3000];
            assert_eq!(ring.try_pop_batch(&mut out), 3000);
            assert_eq!(ring.len(), 0);
            assert_eq!(out[0].value, (round * 3000) as i32);
            assert_eq!(out[2999].value, (round * 3000 + 2999) as i32);
        }
        assert_eq!(ring.dropped_count(), 0);
    }

    #[test]
    fn frame_mailbox_triple_buffer_and_venus_packet_wrap() {
        let owned = OwnedRegion::new(4096);
        let region = owned.region();

        for i in 1..=5u64 {
            let desc = SharedFrameDescriptor {
                nt_shared_handle: 0x1000 + i,
                dxgi_format: 87,
                width: 2560,
                height: 1440,
                row_pitch: 2560 * 4,
                vrr_refresh_hz: 240,
                acquire_fence_value: i * 10,
                release_fence_value: i * 10 - 1,
                frame_index: i,
                guest_present_qpc: 99_000 + i,
                reflex_sim_start_us: 1_000,
                reflex_render_submit_us: 1_650,
                reflex_present_us: 2_100,
                hdr_metadata_packed: 0,
                _reserved: 0,
            };
            region.frame_mailbox.publish(desc);
        }
        let (seq, latest) = region.frame_mailbox.acquire_latest().expect("latest frame");
        assert_eq!(seq, 5);
        assert_eq!(latest.frame_index, 5);
        assert_eq!(latest.vrr_refresh_hz, 240);
        assert_eq!(latest.reflex_total_latency_us(), 1_100);
        assert!(region.frame_mailbox.acquire_latest().is_none());

        // Exercise Venus ring packet wrap-around past 256 KB.
        let payload = vec![0xA5u8; 64 * 1024];
        let mut scratch = vec![0u8; 64 * 1024];
        for _ in 0..8 {
            assert!(region.venus_ring.try_push_packet(&payload));
            let n = region.venus_ring.try_pop_packet(&mut scratch).unwrap();
            assert_eq!(n, payload.len());
            assert_eq!(scratch[0], 0xA5);
            assert_eq!(scratch[n - 1], 0xA5);
        }
    }

    #[test]
    fn virtio_snd_ring_roundtrip() {
        let owned = OwnedRegion::new(4096);
        let snd = &owned.region().audio_ring;
        let samples = [[0.25f32, -0.25f32]; 256];
        assert_eq!(snd.push_frames(&samples), 256);
        assert_eq!(snd.available_frames(), 256);
        let mut out = [[0.0f32; 2]; 256];
        assert_eq!(snd.pop_frames(&mut out), 256);
        assert_eq!(out[255], [0.25, -0.25]);
    }
}
