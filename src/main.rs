//! Titan AOSP 15 (API 35) Zero-Overhead PC Emulator
//! Entry point wiring Stage 1 (Shared Memory), Stage 2 (DX12 Zero-Copy Compositor),
//! Stage 3 (WHPX + Large Pages + P-Core Pinning + RawInput + IOCP), and Stage 4 (AOSP 15 Tuning).

mod stage1_memory;
mod stage2_renderer;
mod stage3_vm_input;
mod stage4_aosp15;
mod win_dx12;

use core::mem::size_of;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use stage1_memory::{
    IvshmemSharedRegion, SharedFrameDescriptor, LARGE_PAGE_SIZE,
};
use stage2_renderer::{Dx12Renderer, FramelessWindow, TelemetryHud};
use stage3_vm_input::{
    CpuPinningManager, LargePageArena, RawInputMapper, VirtioBlkIocpEngine, WhpxMicroVm,
    OVERLAPPED, OVERLAPPED_ENTRY,
};
use stage4_aosp15::Aosp15TuningSummary;
use win_dx12::{query_process_memory_mib, CloseHandle, DXGI_FORMAT_B8G8R8A8_UNORM};

struct SharedRegionSync(*const IvshmemSharedRegion);
unsafe impl Send for SharedRegionSync {}
unsafe impl Sync for SharedRegionSync {}

impl SharedRegionSync {
    #[inline(always)]
    fn as_ref(&self) -> &IvshmemSharedRegion {
        unsafe { &*self.0 }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let self_test = args.iter().any(|a| a == "--self-test");

    if let Err(err) = run(self_test) {
        eprintln!("[TITAN ERROR] {err}");
        std::process::exit(1);
    }
}

fn run(self_test: bool) -> Result<(), String> {
    // ------------------------------------------------------------------------
    // Stage 3: P-Core Pinning & 2 MB Large Page Arenas
    // ------------------------------------------------------------------------
    let cpu_mgr = CpuPinningManager::detect();
    let pinned_ok = cpu_mgr.pin_current_thread_to_p_cores();

    // Allocate 2 MB shared BAR2 control/ring region + compact 16 MB guest arena for self-test
    // (or configurable RAM slice when booting full AOSP kernel).
    let guest_ram_bytes = if self_test { 16 * 1024 * 1024 } else { 64 * 1024 * 1024 };
    let guest_ram = LargePageArena::allocate(guest_ram_bytes)?;
    let ivshmem_arena = LargePageArena::allocate(LARGE_PAGE_SIZE)?;

    // Stage 1: Initialize cache-line-isolated lock-free shared memory in place.
    let shm: &'static mut IvshmemSharedRegion = unsafe {
        IvshmemSharedRegion::init_in_place(ivshmem_arena.as_mut_ptr().cast(), guest_ram.len() as u64)
    };
    if !shm.validate_header() {
        return Err("IvshmemSharedRegion header validation failed".to_string());
    }

    // Probe Windows Hypervisor Platform (WHPX) & map GPA ranges if hypervisor is active.
    let whpx_probe = WhpxMicroVm::probe_and_provision(
        cpu_mgr.p_cores.len().clamp(1, 8) as u32,
        &guest_ram,
        &ivshmem_arena,
    );

    // ------------------------------------------------------------------------
    // Stage 2 & Stage 3: Frameless DX12 Window + RawInput Smart Keymapper
    // ------------------------------------------------------------------------
    let mapper = Arc::new(RawInputMapper::new());
    let shm_sync = SharedRegionSync(shm as *const IvshmemSharedRegion);
    let mapper_hook = Arc::clone(&mapper);
    let hook: Box<dyn Fn(isize) + Send + Sync> = Box::new(move |lparam| {
        mapper_hook.handle_wm_input(lparam, &shm_sync.as_ref().input_ring);
    });

    let window = FramelessWindow::create(
        "Titan AOSP 15 Zero-Overhead Emulator",
        1280,
        720,
        !self_test,
        Some(hook),
    )?;
    let raw_input_ok = RawInputMapper::register_devices(window.hwnd);

    let mut renderer = Dx12Renderer::new(window.hwnd, 1280, 720)?;
    let mut hud = TelemetryHud::new(renderer.tearing_supported);
    hud.large_pages_active = ivshmem_arena.large_pages_active;
    hud.p_core_count = cpu_mgr.p_cores.len();

    // Stage 4: Validate AOSP 15 (API 35) tuning artifacts.
    let aosp15 = Aosp15TuningSummary::current();
    aosp15.validate_artifacts()?;

    if self_test {
        let (shell_pre_ws, shell_pre_priv) = query_process_memory_mib();

        // Demonstrate true zero-copy external NT handle import across GPU heap -> DX12 SRV.
        let demo_shared_nt_handle = renderer.create_self_test_shared_texture(1280, 720)?;
        shm.frame_mailbox.publish(SharedFrameDescriptor {
            nt_shared_handle: demo_shared_nt_handle as u64,
            dxgi_format: DXGI_FORMAT_B8G8R8A8_UNORM,
            width: 1280,
            height: 720,
            row_pitch: 1280 * 4,
            vrr_refresh_hz: 240,
            acquire_fence_value: 1,
            release_fence_value: 0,
            frame_index: 1,
            guest_present_qpc: 1,
            reflex_sim_start_us: 1000,
            reflex_render_submit_us: 1420,
            reflex_present_us: 1850,
            hdr_metadata_packed: 0,
            _reserved: 0,
        });
        renderer.poll_shared_mailbox(shm, &mut hud);

        // Exercise lock-free WASD + mouse aim keymapper into Stage 1 SPSC ring.
        mapper.process_key(0x57, true, &shm.input_ring); // W
        mapper.process_key(0x44, true, &shm.input_ring); // D
        mapper.process_mouse_delta(14, -6, 0x0001, &shm.input_ring);
        let queued_input_events = shm.input_ring.len();

        // Exercise IOCP async virtio-blk write/read.
        let mut blk_path = std::env::temp_dir();
        blk_path.push("titan_selftest_blk.img");
        let iocp_bytes = {
            let blk = VirtioBlkIocpEngine::open_or_create(blk_path.to_str().unwrap())?;
            let sector = vec![0xE1u8; 4096];
            let mut ov = OVERLAPPED::default();
            blk.submit_write(0, &sector, &mut ov)?;
            let mut comp = [OVERLAPPED_ENTRY::default(); 2];
            let n = blk.reap_completions(&mut comp, 1000);
            if n == 1 { comp[0].dwNumberOfBytesTransferred } else { 0 }
        };
        let _ = std::fs::remove_file(&blk_path);

        // Render 120 live DirectX 12 frames (Kawase blur + SDF HUD + frametime graph + sidebar).
        for _ in 0..120 {
            window.pump_messages();
            renderer.render_frame(&mut hud)?;
        }
        renderer.wait_for_gpu_idle();
        unsafe { CloseHandle(demo_shared_nt_handle) };

        let (ws_mib, priv_mib) = query_process_memory_mib();
        println!("=== TITAN AOSP 15 ZERO-OVERHEAD SELF-TEST ===");
        println!("Stage 1 | IvshmemSharedRegion : {} bytes (<= {} Large Page)", size_of::<IvshmemSharedRegion>(), LARGE_PAGE_SIZE);
        println!("Stage 1 | SPSC Input Events   : {} events queued lock-free", queued_input_events);
        println!("Stage 1 | Audio Ring Latency  : {:.2} ms ({} frames @ 48kHz)", shm.audio_ring.period_latency_ms(), 192);
        println!("Stage 2 | GPU Adapter         : {}", renderer.adapter_name);
        println!("Stage 2 | VRR Tearing Support : {}", renderer.tearing_supported);
        println!("Stage 2 | External NT Import  : {} (DXGI/Vulkan Zero-Copy)", hud.external_handle_active);
        println!("Stage 2 | Shader Disk Cache   : {}", if renderer.cache_hit { "HIT (pipeline_cache.bin)" } else { "COMPILED & CACHED" });
        println!("Stage 2 | 120-Frame Telemetry : Avg {:.1} FPS | 1% Low {:.1} FPS | Last {:.2} ms | Reflex {:.2} ms",
            hud.avg_fps, hud.one_percent_low_fps, hud.last_frametime_ms, hud.reflex_latency_ms);
        println!("Stage 2 | Shell RAM Footprint : WorkingSet {:.2} MiB (pre {:.2} MiB) | Private {:.2} MiB (pre {:.2} MiB)",
            ws_mib, shell_pre_ws, priv_mib, shell_pre_priv);
        println!("Stage 3 | CPU Topology        : {} P-cores, {} E-cores (pinned={})", cpu_mgr.p_cores.len(), cpu_mgr.e_cores.len(), pinned_ok);
        println!("Stage 3 | 2MB Large Pages     : {} (2MB aligned={})", ivshmem_arena.large_pages_active, (ivshmem_arena.as_mut_ptr() as usize) % LARGE_PAGE_SIZE == 0);
        match &whpx_probe {
            Ok(vm) => println!("Stage 3 | WHPX Micro-VM       : hypervisor_present={}, partition_ready={}, vcpus={}", vm.hypervisor_present, vm.partition_ready, vm.vcpu_count),
            Err(e) => println!("Stage 3 | WHPX Micro-VM       : unavailable ({e})"),
        }
        println!("Stage 3 | RawInput + IOCP Blk : rawinput_reg={}, iocp_xfer={} bytes", raw_input_ok, iocp_bytes);
        println!("Stage 4 | AOSP API {} Tuning  : {} | {} | LTO: {} | PGO: {}",
            aosp15.api_level, aosp15.target_isa, aosp15.vulkan_backend, aosp15.lto_mode, aosp15.pgo_enabled);
        return Ok(());
    }

    // Interactive Desktop Loop (0% idle overhead via WaitForSingleObject on Waitable SwapChain).
    let mut running = true;
    while running {
        running = window.pump_messages();
        if !running {
            break;
        }
        window.sync_hud_flags(&mut hud);
        mapper
            .keymap_enabled
            .store(hud.keymapper_active, Ordering::Relaxed);

        if window.state.resized.swap(false, Ordering::AcqRel) {
            let nw = window.state.width.load(Ordering::Relaxed);
            let nh = window.state.height.load(Ordering::Relaxed);
            renderer.resize(nw, nh)?;
        }

        renderer.poll_shared_mailbox(shm, &mut hud);
        renderer.render_frame(&mut hud)?;
    }

    renderer.wait_for_gpu_idle();
    Ok(())
}
