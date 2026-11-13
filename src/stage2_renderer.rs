//! Stage 2: Zero-Copy DirectX 12 Core Renderer, External Texture Import,
//! Single-Pass Kawase Blur HUD, Collapsible Sidebar & Frameless Native Window.

use core::ffi::c_void;
use core::mem::{size_of, zeroed};
use core::ptr::{null, null_mut};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::stage1_memory::IvshmemSharedRegion;
use crate::win_dx12::*;

const BACKBUFFER_COUNT: usize = 3;
const MAX_UI_QUADS: usize = 2048;
const FRAMETIME_HISTORY: usize = 512;
const PIPELINE_CACHE_FILE: &str = "pipeline_cache.bin";
const PIPELINE_CACHE_MAGIC: u32 = 0x5450_4331; // "TPC1"

/// Embedded HLSL shader suite:
/// - `VSMain`: generates a screen quad (`VertexID 0..5`) per `UiInstance` (no vertex buffer needed for geometry).
/// - `PSGuest`: samples the zero-copy Android 15 external framebuffer texture (`t0`).
/// - `PSUi`: renders single-pass Kawase frosted-glass blur panels, SDF rounded rects,
///   frametime graph bars, and procedural 5x7 HUD glyph bitmaps (`mode == 2`).
const SHADER_HLSL: &str = r#"
Texture2D<float4> g_guestTex : register(t0);
SamplerState g_linearClamp : register(s0);

cbuffer FrameConstants : register(b0)
{
    float2 g_viewportSize;
    float2 g_guestSize;
    float  g_timeSec;
    float  g_blurRadius;
    uint   g_hudVisible;
    uint   g_sidebarOpen;
};

struct VSInput
{
    float4 rect     : INST_RECT;   // x, y, w, h in pixels
    float4 color    : INST_COLOR;  // RGBA
    float4 params   : INST_PARAMS; // radius_px, border_px, mode (0=glass,1=solid,2=glyph), extra
    uint2  glyph    : INST_GLYPH;  // 35-bit 5x7 glyph bitmap packed across (lo32, hi3)
};

struct VSOutput
{
    float4 pos      : SV_POSITION;
    float2 uv       : TEXCOORD0;
    float2 screenUv : TEXCOORD1;
    float4 rectSize : TEXCOORD2; // w, h, radius, border
    float4 color    : COLOR0;
    nointerpolation uint  mode  : TEXCOORD3;
    nointerpolation uint2 glyph : TEXCOORD4;
    nointerpolation float extra : TEXCOORD5;
};

VSOutput VSMain(uint vid : SV_VertexID, VSInput inst)
{
    static const float2 kCorners[6] = {
        float2(0.0, 0.0), float2(1.0, 0.0), float2(0.0, 1.0),
        float2(1.0, 0.0), float2(1.0, 1.0), float2(0.0, 1.0)
    };
    float2 c = kCorners[vid];
    float2 px = inst.rect.xy + c * inst.rect.zw;
    float2 ndc = float2((px.x / g_viewportSize.x) * 2.0 - 1.0,
                        1.0 - (px.y / g_viewportSize.y) * 2.0);

    VSOutput o;
    o.pos = float4(ndc, 0.0, 1.0);
    o.uv = c;
    o.screenUv = px / g_viewportSize;
    o.rectSize = float4(inst.rect.zw, inst.params.x, inst.params.y);
    o.color = inst.color;
    o.mode = (uint)(inst.params.z + 0.5);
    o.glyph = inst.glyph;
    o.extra = inst.params.w;
    return o;
}

float sdRoundedBox(float2 p, float2 halfExtent, float r)
{
    float2 q = abs(p) - halfExtent + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

// Single-pass 9-tap rotated Kawase blur over the guest VRAM framebuffer texture.
float3 sampleKawaseBlur(float2 uv, float radiusPx)
{
    float2 texel = radiusPx / max(g_viewportSize, float2(1.0, 1.0));
    float3 sum = g_guestTex.Sample(g_linearClamp, uv).rgb * 0.20;
    sum += g_guestTex.Sample(g_linearClamp, uv + float2( texel.x,  texel.y)).rgb * 0.12;
    sum += g_guestTex.Sample(g_linearClamp, uv + float2(-texel.x,  texel.y)).rgb * 0.12;
    sum += g_guestTex.Sample(g_linearClamp, uv + float2( texel.x, -texel.y)).rgb * 0.12;
    sum += g_guestTex.Sample(g_linearClamp, uv + float2(-texel.x, -texel.y)).rgb * 0.12;
    sum += g_guestTex.Sample(g_linearClamp, uv + float2( texel.x * 1.85,  0.0)).rgb * 0.08;
    sum += g_guestTex.Sample(g_linearClamp, uv + float2(-texel.x * 1.85,  0.0)).rgb * 0.08;
    sum += g_guestTex.Sample(g_linearClamp, uv + float2( 0.0,  texel.y * 1.85)).rgb * 0.08;
    sum += g_guestTex.Sample(g_linearClamp, uv + float2( 0.0, -texel.y * 1.85)).rgb * 0.08;
    return sum;
}

float4 PSGuest(VSOutput i) : SV_TARGET
{
    float4 sampleCol = g_guestTex.Sample(g_linearClamp, i.uv);
    // Subtle dark bezel vignette at extreme edges so frameless shell feels crisp.
    float2 edge = min(i.uv, 1.0 - i.uv);
    float vignette = saturate(min(edge.x, edge.y) * 90.0);
    return float4(sampleCol.rgb * lerp(0.86, 1.0, vignette), 1.0);
}

float4 PSUi(VSOutput i) : SV_TARGET
{
    if (i.mode == 2)
    {
        // Procedural 5x7 crisp bitmap glyph cell stored in uint2(lo32, hi3).
        uint col = min((uint)floor(i.uv.x * 5.0), 4u);
        uint row = min((uint)floor(i.uv.y * 7.0), 6u);
        uint bitIdx = row * 5u + col;
        uint bitVal = (bitIdx < 32u) ? ((i.glyph.x >> bitIdx) & 1u)
                                     : ((i.glyph.y >> (bitIdx - 32u)) & 1u);
        if (bitVal == 0u) discard;
        return i.color;
    }

    float2 size = i.rectSize.xy;
    float radius = min(i.rectSize.z, min(size.x, size.y) * 0.5);
    float border = i.rectSize.w;
    float2 p = (i.uv - 0.5) * size;
    float dist = sdRoundedBox(p, size * 0.5, radius);
    float aa = 1.0;
    float alphaMask = 1.0 - smoothstep(-aa, aa, dist);
    if (alphaMask <= 0.001) discard;

    if (i.mode == 0)
    {
        // Hardware-accelerated single-pass Kawase frosted glass backdrop + obsidian tint.
        float3 blurred = sampleKawaseBlur(i.screenUv, max(g_blurRadius, 2.5));
        float3 tinted = lerp(blurred, i.color.rgb, i.color.a);
        float borderBand = smoothstep(-border - 1.0, -border + 0.5, dist) * step(dist, 0.75);
        float3 borderColor = float3(0.32, 0.62, 0.98);
        float3 finalRgb = lerp(tinted, borderColor, borderBand * 0.42);
        return float4(finalRgb, alphaMask * 0.96);
    }

    // Mode 1: Crisp SDF rounded shape (accent strip, bar, button, status pill).
    float borderBand = (border > 0.0)
        ? (smoothstep(-border - 0.8, -border + 0.4, dist) * 0.45)
        : 0.0;
    float3 rgb = lerp(i.color.rgb, float3(0.45, 0.78, 1.0), borderBand);
    return float4(rgb, i.color.a * alphaMask);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct FrameConstants {
    viewport_size: [f32; 2],
    guest_size: [f32; 2],
    time_sec: f32,
    blur_radius: f32,
    hud_visible: u32,
    sidebar_open: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UiInstance {
    rect: [f32; 4],
    color: [f32; 4],
    params: [f32; 4],
    glyph: [u32; 2],
}

/// Real-time telemetry & frame-pacing metrics for the HUD overlay.
pub struct TelemetryHud {
    frametimes_ms: [f32; FRAMETIME_HISTORY],
    sorted_scratch: Vec<f32>,
    cursor: usize,
    filled: usize,
    pub total_frames: u64,
    pub avg_fps: f32,
    pub one_percent_low_fps: f32,
    pub point_one_percent_low_fps: f32,
    pub last_frametime_ms: f32,
    pub reflex_latency_ms: f32,
    pub host_ram_mib: f64,
    pub host_private_mib: f64,
    pub target_vrr_hz: u32,
    pub tearing_supported: bool,
    pub hud_visible: bool,
    pub sidebar_open: bool,
    pub keymapper_active: bool,
    pub large_pages_active: bool,
    pub p_core_count: usize,
    pub external_handle_active: bool,
}

impl TelemetryHud {
    pub fn new(tearing_supported: bool) -> Self {
        Self {
            frametimes_ms: [4.166; FRAMETIME_HISTORY],
            sorted_scratch: Vec::with_capacity(FRAMETIME_HISTORY),
            cursor: 0,
            filled: 0,
            total_frames: 0,
            avg_fps: 240.0,
            one_percent_low_fps: 240.0,
            point_one_percent_low_fps: 240.0,
            last_frametime_ms: 4.166,
            reflex_latency_ms: 1.15,
            host_ram_mib: 0.0,
            host_private_mib: 0.0,
            target_vrr_hz: 240,
            tearing_supported,
            hud_visible: true,
            sidebar_open: true,
            keymapper_active: true,
            large_pages_active: false,
            p_core_count: 0,
            external_handle_active: false,
        }
    }

    pub fn record_frametime(&mut self, dt_ms: f32, reflex_us: u64) {
        let clamped = dt_ms.clamp(0.05, 500.0);
        self.frametimes_ms[self.cursor] = clamped;
        self.cursor = (self.cursor + 1) % FRAMETIME_HISTORY;
        if self.filled < FRAMETIME_HISTORY {
            self.filled += 1;
        }
        self.total_frames += 1;
        self.last_frametime_ms = clamped;
        if reflex_us > 0 {
            self.reflex_latency_ms = reflex_us as f32 / 1000.0;
        }

        // Recompute percentiles without dynamic heap growth.
        let n = self.filled;
        self.sorted_scratch.clear();
        self.sorted_scratch.extend_from_slice(&self.frametimes_ms[..n]);
        self.sorted_scratch
            .sort_unstable_by(|a, b| a.total_cmp(b));

        let sum: f32 = self.sorted_scratch.iter().sum();
        let avg_ms = (sum / n as f32).max(0.05);
        self.avg_fps = 1000.0 / avg_ms;

        let idx_1pct = ((n as f32 * 0.99) as usize).min(n - 1);
        let idx_01pct = ((n as f32 * 0.999) as usize).min(n - 1);
        self.one_percent_low_fps = 1000.0 / self.sorted_scratch[idx_1pct].max(0.05);
        self.point_one_percent_low_fps = 1000.0 / self.sorted_scratch[idx_01pct].max(0.05);

        if self.total_frames % 16 == 1 {
            let (ws, priv_mb) = query_process_memory_mib();
            self.host_ram_mib = ws;
            self.host_private_mib = priv_mb;
        }
    }

    pub fn cycle_vrr_target(&mut self) {
        self.target_vrr_hz = match self.target_vrr_hz {
            120 => 144,
            144 => 240,
            _ => 120,
        };
    }

    /// Returns the `count` most recent frametimes in chronological order.
    fn recent_samples(&self, count: usize) -> impl Iterator<Item = f32> + '_ {
        let take = count.min(self.filled);
        let start = (self.cursor + FRAMETIME_HISTORY - take) % FRAMETIME_HISTORY;
        (0..take).map(move |i| self.frametimes_ms[(start + i) % FRAMETIME_HISTORY])
    }
}

/// Disk-cached compiled DXBC bytecode bundle eliminating shader compilation stutters.
struct ShaderBundle {
    vs: Vec<u8>,
    ps_guest: Vec<u8>,
    ps_ui: Vec<u8>,
    loaded_from_disk: bool,
}

fn fnv1a_64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

impl ShaderBundle {
    fn load_or_compile(cache_path: &Path) -> Result<Self, String> {
        let src_bytes = SHADER_HLSL.as_bytes();
        let src_hash = fnv1a_64(src_bytes);

        if let Ok(blob) = fs::read(cache_path) {
            if let Some(parsed) = Self::deserialize(&blob, src_hash) {
                return Ok(parsed);
            }
        }

        let vs = compile_hlsl(src_bytes, b"VSMain\0", b"vs_5_0\0")?;
        let ps_guest = compile_hlsl(src_bytes, b"PSGuest\0", b"ps_5_0\0")?;
        let ps_ui = compile_hlsl(src_bytes, b"PSUi\0", b"ps_5_0\0")?;

        let bundle = Self {
            vs,
            ps_guest,
            ps_ui,
            loaded_from_disk: false,
        };
        let encoded = bundle.serialize(src_hash);
        let _ = fs::write(cache_path, encoded);
        Ok(bundle)
    }

    fn serialize(&self, hash: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(24 + self.vs.len() + self.ps_guest.len() + self.ps_ui.len());
        out.extend_from_slice(&PIPELINE_CACHE_MAGIC.to_le_bytes());
        out.extend_from_slice(&hash.to_le_bytes());
        out.extend_from_slice(&(self.vs.len() as u32).to_le_bytes());
        out.extend_from_slice(&(self.ps_guest.len() as u32).to_le_bytes());
        out.extend_from_slice(&(self.ps_ui.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.vs);
        out.extend_from_slice(&self.ps_guest);
        out.extend_from_slice(&self.ps_ui);
        out
    }

    fn deserialize(data: &[u8], expected_hash: u64) -> Option<Self> {
        if data.len() < 24 {
            return None;
        }
        let magic = u32::from_le_bytes(data[0..4].try_into().ok()?);
        let hash = u64::from_le_bytes(data[4..12].try_into().ok()?);
        if magic != PIPELINE_CACHE_MAGIC || hash != expected_hash {
            return None;
        }
        let vs_len = u32::from_le_bytes(data[12..16].try_into().ok()?) as usize;
        let pg_len = u32::from_le_bytes(data[16..20].try_into().ok()?) as usize;
        let pu_len = u32::from_le_bytes(data[20..24].try_into().ok()?) as usize;
        let total = 24usize.checked_add(vs_len)?.checked_add(pg_len)?.checked_add(pu_len)?;
        if data.len() != total {
            return None;
        }
        let mut off = 24;
        let vs = data[off..off + vs_len].to_vec();
        off += vs_len;
        let ps_guest = data[off..off + pg_len].to_vec();
        off += pg_len;
        let ps_ui = data[off..off + pu_len].to_vec();
        Some(Self {
            vs,
            ps_guest,
            ps_ui,
            loaded_from_disk: true,
        })
    }
}

fn compile_hlsl(src: &[u8], entry: &[u8], target: &[u8]) -> Result<Vec<u8>, String> {
    let mut code: *mut ID3DBlob = null_mut();
    let mut err: *mut ID3DBlob = null_mut();
    let flags = 1 << 15; // D3DCOMPILE_OPTIMIZATION_LEVEL3
    let hr = unsafe {
        D3DCompile(
            src.as_ptr().cast(),
            src.len(),
            b"titan_hud.hlsl\0".as_ptr(),
            null(),
            null_mut(),
            entry.as_ptr(),
            target.as_ptr(),
            flags,
            0,
            &mut code,
            &mut err,
        )
    };
    let code_ptr = unsafe { ComPtr::from_raw(code) };
    let err_ptr = unsafe { ComPtr::from_raw(err) };
    if !succeeded(hr) {
        let msg = String::from_utf8_lossy(err_ptr.as_bytes()).into_owned();
        return Err(format!("D3DCompile failed (0x{hr:08x}): {msg}"));
    }
    Ok(code_ptr.as_bytes().to_vec())
}

/// Complete DirectX 12 zero-copy compositor, external NT handle importer & UI renderer.
pub struct Dx12Renderer {
    pub width: u32,
    pub height: u32,
    pub adapter_name: String,
    pub cache_hit: bool,
    pub tearing_supported: bool,

    _factory: ComPtr<IDXGIFactory5>,
    device: ComPtr<ID3D12Device>,
    queue: ComPtr<ID3D12CommandQueue>,
    swapchain: ComPtr<IDXGISwapChain3>,
    waitable_swapchain: HANDLE,
    rtv_heap: ComPtr<ID3D12DescriptorHeap>,
    rtv_inc: u32,
    srv_heap: ComPtr<ID3D12DescriptorHeap>,
    srv_cpu_start: D3D12_CPU_DESCRIPTOR_HANDLE,
    srv_gpu_start: D3D12_GPU_DESCRIPTOR_HANDLE,

    backbuffers: [ComPtr<ID3D12Resource>; BACKBUFFER_COUNT],
    allocators: [ComPtr<ID3D12CommandAllocator>; BACKBUFFER_COUNT],
    cmd_list: ComPtr<ID3D12GraphicsCommandList>,

    root_sig: ComPtr<ID3D12RootSignature>,
    pso_guest: ComPtr<ID3D12PipelineState>,
    pso_ui: ComPtr<ID3D12PipelineState>,

    // Fallback GPU texture (used until guest attaches external DXGI/Vulkan shared handle).
    fallback_tex: ComPtr<ID3D12Resource>,
    external_tex: Option<ComPtr<ID3D12Resource>>,
    guest_width: u32,
    guest_height: u32,

    // Persistent upload instance ring (one slice per backbuffer).
    instance_upload: ComPtr<ID3D12Resource>,
    instance_mapped: *mut UiInstance,

    fence: ComPtr<ID3D12Fence>,
    fence_event: HANDLE,
    fence_values: [u64; BACKBUFFER_COUNT],
    next_fence_value: u64,

    start_qpc: i64,
    last_frame_qpc: i64,
    qpc_freq: i64,
}

impl Dx12Renderer {
    pub fn new(hwnd: HWND, width: u32, height: u32) -> Result<Self, String> {
        let mut factory_raw: *mut c_void = null_mut();
        let hr = unsafe { CreateDXGIFactory2(0, &IID_IDXGI_FACTORY5, &mut factory_raw) };
        if !succeeded(hr) {
            return Err(format!("CreateDXGIFactory2 failed: 0x{hr:08x}"));
        }
        let factory = unsafe { ComPtr::from_raw(factory_raw as *mut IDXGIFactory5) };

        unsafe {
            ((*(*factory.as_ptr()).lpVtbl).MakeWindowAssociation)(
                factory.as_ptr(),
                hwnd,
                DXGI_MWA_NO_ALT_ENTER,
            );
        }

        // Check Variable Refresh Rate / Tearing support.
        let mut allow_tearing: BOOL = 0;
        unsafe {
            let _ = ((*(*factory.as_ptr()).lpVtbl).CheckFeatureSupport)(
                factory.as_ptr(),
                DXGI_FEATURE_PRESENT_ALLOW_TEARING,
                (&mut allow_tearing as *mut BOOL).cast(),
                size_of::<BOOL>() as u32,
            );
        }
        let tearing_supported = allow_tearing != 0;

        // Pick hardware adapter first, fallback to WARP if headless CI / no GPU.
        let (device, adapter_name) = Self::create_best_device(&factory)?;

        let queue_desc = D3D12_COMMAND_QUEUE_DESC {
            Type: D3D12_COMMAND_LIST_TYPE_DIRECT,
            Priority: 100, // HIGH priority for sub-frame compositor
            Flags: 0,
            NodeMask: 0,
        };
        let mut queue = ComPtr::<ID3D12CommandQueue>::null();
        let hr = unsafe {
            ((*(*device.as_ptr()).lpVtbl).CreateCommandQueue)(
                device.as_ptr(),
                &queue_desc,
                &IID_ID3D12_COMMAND_QUEUE,
                queue.as_unknown_mut_ptr(),
            )
        };
        if !succeeded(hr) {
            return Err(format!("CreateCommandQueue failed: 0x{hr:08x}"));
        }

        let sc_flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT
            | if tearing_supported {
                DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING
            } else {
                0
            };
        let sc_desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: width.max(64),
            Height: height.max(64),
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            Stereo: 0,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: BACKBUFFER_COUNT as u32,
            Scaling: DXGI_SCALING_STRETCH,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            AlphaMode: DXGI_ALPHA_MODE_UNSPECIFIED,
            Flags: sc_flags,
        };
        let mut swapchain1 = ComPtr::<IUnknownVtbl>::null();
        let hr = unsafe {
            ((*(*factory.as_ptr()).lpVtbl).CreateSwapChainForHwnd)(
                factory.as_ptr(),
                queue.as_ptr().cast(),
                hwnd,
                &sc_desc,
                null(),
                null_mut(),
                swapchain1.as_unknown_mut_ptr(),
            )
        };
        if !succeeded(hr) {
            return Err(format!("CreateSwapChainForHwnd failed: 0x{hr:08x}"));
        }
        let swapchain: ComPtr<IDXGISwapChain3> = unsafe {
            swapchain1
                .query_interface(&IID_IDXGI_SWAP_CHAIN3)
                .map_err(|e| format!("QueryInterface(IDXGISwapChain3) failed: 0x{e:08x}"))?
        };

        let waitable_swapchain = unsafe {
            let _ = ((*(*swapchain.as_ptr()).lpVtbl).SetMaximumFrameLatency)(swapchain.as_ptr(), 1);
            ((*(*swapchain.as_ptr()).lpVtbl).GetFrameLatencyWaitableObject)(swapchain.as_ptr())
        };

        // RTV Descriptor Heap
        let rtv_desc = D3D12_DESCRIPTOR_HEAP_DESC {
            Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
            NumDescriptors: BACKBUFFER_COUNT as u32,
            Flags: 0,
            NodeMask: 0,
        };
        let mut rtv_heap = ComPtr::<ID3D12DescriptorHeap>::null();
        let hr = unsafe {
            ((*(*device.as_ptr()).lpVtbl).CreateDescriptorHeap)(
                device.as_ptr(),
                &rtv_desc,
                &IID_ID3D12_DESCRIPTOR_HEAP,
                rtv_heap.as_unknown_mut_ptr(),
            )
        };
        if !succeeded(hr) {
            return Err(format!("CreateDescriptorHeap(RTV) failed: 0x{hr:08x}"));
        }
        let rtv_inc = unsafe {
            ((*(*device.as_ptr()).lpVtbl).GetDescriptorHandleIncrementSize)(
                device.as_ptr(),
                D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
            )
        };

        // SRV Descriptor Heap (shader visible, slot 0 = active guest texture)
        let srv_desc = D3D12_DESCRIPTOR_HEAP_DESC {
            Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
            NumDescriptors: 2,
            Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
            NodeMask: 0,
        };
        let mut srv_heap = ComPtr::<ID3D12DescriptorHeap>::null();
        let hr = unsafe {
            ((*(*device.as_ptr()).lpVtbl).CreateDescriptorHeap)(
                device.as_ptr(),
                &srv_desc,
                &IID_ID3D12_DESCRIPTOR_HEAP,
                srv_heap.as_unknown_mut_ptr(),
            )
        };
        if !succeeded(hr) {
            return Err(format!("CreateDescriptorHeap(SRV) failed: 0x{hr:08x}"));
        }

        let mut srv_cpu_start = D3D12_CPU_DESCRIPTOR_HANDLE::default();
        let mut srv_gpu_start = D3D12_GPU_DESCRIPTOR_HANDLE::default();
        unsafe {
            ((*(*srv_heap.as_ptr()).lpVtbl).GetCPUDescriptorHandleForHeapStart)(
                srv_heap.as_ptr(),
                &mut srv_cpu_start,
            );
            ((*(*srv_heap.as_ptr()).lpVtbl).GetGPUDescriptorHandleForHeapStart)(
                srv_heap.as_ptr(),
                &mut srv_gpu_start,
            );
        }

        // Command allocators + graphics command list
        let mut allocators = [
            ComPtr::null(),
            ComPtr::null(),
            ComPtr::null(),
        ];
        for alloc in &mut allocators {
            let hr = unsafe {
                ((*(*device.as_ptr()).lpVtbl).CreateCommandAllocator)(
                    device.as_ptr(),
                    D3D12_COMMAND_LIST_TYPE_DIRECT,
                    &IID_ID3D12_COMMAND_ALLOCATOR,
                    alloc.as_unknown_mut_ptr(),
                )
            };
            if !succeeded(hr) {
                return Err(format!("CreateCommandAllocator failed: 0x{hr:08x}"));
            }
        }

        let mut cmd_list = ComPtr::<ID3D12GraphicsCommandList>::null();
        let hr = unsafe {
            ((*(*device.as_ptr()).lpVtbl).CreateCommandList)(
                device.as_ptr(),
                0,
                D3D12_COMMAND_LIST_TYPE_DIRECT,
                allocators[0].as_ptr(),
                null_mut(),
                &IID_ID3D12_GRAPHICS_COMMAND_LIST,
                cmd_list.as_unknown_mut_ptr(),
            )
        };
        if !succeeded(hr) {
            return Err(format!("CreateCommandList failed: 0x{hr:08x}"));
        }

        // Fence & Win32 event
        let mut fence = ComPtr::<ID3D12Fence>::null();
        let hr = unsafe {
            ((*(*device.as_ptr()).lpVtbl).CreateFence)(
                device.as_ptr(),
                0,
                D3D12_FENCE_FLAG_NONE,
                &IID_ID3D12_FENCE,
                fence.as_unknown_mut_ptr(),
            )
        };
        if !succeeded(hr) {
            return Err(format!("CreateFence failed: 0x{hr:08x}"));
        }
        let fence_event = unsafe { CreateEventW(null_mut(), 0, 0, null()) };

        // Root signature + PSOs from disk-cached HLSL
        let bundle = ShaderBundle::load_or_compile(Path::new(PIPELINE_CACHE_FILE))?;
        let root_sig = Self::create_root_signature(&device)?;
        let pso_guest = Self::create_pso(&device, &root_sig, &bundle.vs, &bundle.ps_guest, false)?;
        let pso_ui = Self::create_pso(&device, &root_sig, &bundle.vs, &bundle.ps_ui, true)?;

        // Persistent upload buffer for UI instance quads across 3 buffered frames
        let upload_bytes = (BACKBUFFER_COUNT * MAX_UI_QUADS * size_of::<UiInstance>()) as u64;
        let upload_props = D3D12_HEAP_PROPERTIES::new(D3D12_HEAP_TYPE_UPLOAD);
        let upload_desc = D3D12_RESOURCE_DESC::buffer(upload_bytes);
        let mut instance_upload = ComPtr::<ID3D12Resource>::null();
        let hr = unsafe {
            ((*(*device.as_ptr()).lpVtbl).CreateCommittedResource)(
                device.as_ptr(),
                &upload_props,
                D3D12_HEAP_FLAG_NONE,
                &upload_desc,
                D3D12_RESOURCE_STATE_GENERIC_READ,
                null(),
                &IID_ID3D12_RESOURCE,
                instance_upload.as_unknown_mut_ptr(),
            )
        };
        if !succeeded(hr) {
            return Err(format!("CreateCommittedResource(upload) failed: 0x{hr:08x}"));
        }
        let mut mapped_ptr: *mut c_void = null_mut();
        let read_none = D3D12_RANGE { Begin: 0, End: 0 };
        unsafe {
            ((*(*instance_upload.as_ptr()).lpVtbl).Map)(
                instance_upload.as_ptr(),
                0,
                &read_none,
                &mut mapped_ptr,
            );
        }

        // Create and populate fallback Android 15 preview framebuffer texture on GPU.
        let (guest_w, guest_h) = (1280u32, 720u32);
        let (fallback_tex, staging_tex) =
            Self::create_fallback_guest_texture(&device, &cmd_list, guest_w, guest_h)?;

        // Close & execute initial upload command list and wait once.
        unsafe {
            ((*(*cmd_list.as_ptr()).lpVtbl).Close)(cmd_list.as_ptr());
            let lists = [cmd_list.as_ptr()];
            ((*(*queue.as_ptr()).lpVtbl).ExecuteCommandLists)(queue.as_ptr(), 1, lists.as_ptr());
            ((*(*queue.as_ptr()).lpVtbl).Signal)(queue.as_ptr(), fence.as_ptr(), 1);
            if ((*(*fence.as_ptr()).lpVtbl).GetCompletedValue)(fence.as_ptr()) < 1 {
                ((*(*fence.as_ptr()).lpVtbl).SetEventOnCompletion)(fence.as_ptr(), 1, fence_event);
                WaitForSingleObject(fence_event, INFINITE);
            }
        }
        drop(staging_tex);

        let mut start_qpc = 0i64;
        let mut qpc_freq = 1i64;
        unsafe {
            QueryPerformanceCounter(&mut start_qpc);
            QueryPerformanceFrequency(&mut qpc_freq);
        }

        let mut renderer = Self {
            width: width.max(64),
            height: height.max(64),
            adapter_name,
            cache_hit: bundle.loaded_from_disk,
            tearing_supported,
            _factory: factory,
            device,
            queue,
            swapchain,
            waitable_swapchain,
            rtv_heap,
            rtv_inc,
            srv_heap,
            srv_cpu_start,
            srv_gpu_start,
            backbuffers: [ComPtr::null(), ComPtr::null(), ComPtr::null()],
            allocators,
            cmd_list,
            root_sig,
            pso_guest,
            pso_ui,
            fallback_tex,
            external_tex: None,
            guest_width: guest_w,
            guest_height: guest_h,
            instance_upload,
            instance_mapped: mapped_ptr.cast(),
            fence,
            fence_event,
            fence_values: [1, 1, 1],
            next_fence_value: 2,
            start_qpc,
            last_frame_qpc: start_qpc,
            qpc_freq,
        };

        renderer.bind_backbuffer_rtvs()?;
        renderer.update_guest_srv(renderer.fallback_tex.as_ptr(), DXGI_FORMAT_B8G8R8A8_UNORM);
        Ok(renderer)
    }

    fn create_best_device(factory: &ComPtr<IDXGIFactory5>) -> Result<(ComPtr<ID3D12Device>, String), String> {
        let mut idx = 0u32;
        loop {
            let mut adapter = ComPtr::<IDXGIAdapter1>::null();
            let hr = unsafe {
                ((*(*factory.as_ptr()).lpVtbl).EnumAdapters1)(
                    factory.as_ptr(),
                    idx,
                    (&mut adapter as *mut ComPtr<IDXGIAdapter1>).cast(),
                )
            };
            if !succeeded(hr) {
                break;
            }
            let mut desc: DXGI_ADAPTER_DESC1 = unsafe { zeroed() };
            unsafe {
                ((*(*adapter.as_ptr()).lpVtbl).GetDesc1)(adapter.as_ptr(), &mut desc);
            }
            // Skip software adapter on first pass; pick first hardware D3D12 adapter.
            if (desc.Flags & 2) == 0 {
                let mut dev = ComPtr::<ID3D12Device>::null();
                let hr_dev = unsafe {
                    D3D12CreateDevice(
                        adapter.as_ptr().cast(),
                        D3D_FEATURE_LEVEL_12_0,
                        &IID_ID3D12_DEVICE,
                        dev.as_unknown_mut_ptr(),
                    )
                };
                if succeeded(hr_dev) {
                    let end = desc.Description.iter().position(|&c| c == 0).unwrap_or(128);
                    let name = String::from_utf16_lossy(&desc.Description[..end]);
                    return Ok((dev, name));
                }
            }
            idx += 1;
        }

        // Headless fallback to WARP adapter if no hardware GPU is exposed.
        let mut warp = null_mut();
        let hr = unsafe {
            ((*(*factory.as_ptr()).lpVtbl).EnumWarpAdapter)(
                factory.as_ptr(),
                &IID_IDXGI_ADAPTER1,
                &mut warp,
            )
        };
        if succeeded(hr) {
            let warp_ptr = unsafe { ComPtr::from_raw(warp as *mut IDXGIAdapter1) };
            let mut dev = ComPtr::<ID3D12Device>::null();
            let hr_dev = unsafe {
                D3D12CreateDevice(
                    warp_ptr.as_ptr().cast(),
                    D3D_FEATURE_LEVEL_11_0,
                    &IID_ID3D12_DEVICE,
                    dev.as_unknown_mut_ptr(),
                )
            };
            if succeeded(hr_dev) {
                return Ok((dev, "Microsoft Basic Render Driver (WARP)".to_string()));
            }
        }
        Err("No DirectX 12 compatible adapter found".to_string())
    }

    fn create_root_signature(device: &ComPtr<ID3D12Device>) -> Result<ComPtr<ID3D12RootSignature>, String> {
        let range = D3D12_DESCRIPTOR_RANGE {
            RangeType: 0, // SRV
            NumDescriptors: 1,
            BaseShaderRegister: 0,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: 0,
        };
        let params = [
            D3D12_ROOT_PARAMETER {
                ParameterType: 1, // 32BIT_CONSTANTS (b0)
                u: D3D12_ROOT_PARAMETER_UNION {
                    Constants: D3D12_ROOT_CONSTANTS {
                        ShaderRegister: 0,
                        RegisterSpace: 0,
                        Num32BitValues: (size_of::<FrameConstants>() / 4) as u32,
                    },
                },
                ShaderVisibility: 0, // ALL
            },
            D3D12_ROOT_PARAMETER {
                ParameterType: 0, // DESCRIPTOR_TABLE (t0)
                u: D3D12_ROOT_PARAMETER_UNION {
                    DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                        NumDescriptorRanges: 1,
                        pDescriptorRanges: &range,
                    },
                },
                ShaderVisibility: 5, // PIXEL
            },
        ];
        let sampler = D3D12_STATIC_SAMPLER_DESC {
            Filter: 0x15, // MIN_MAG_MIP_LINEAR
            AddressU: 3,  // CLAMP
            AddressV: 3,
            AddressW: 3,
            MipLODBias: 0.0,
            MaxAnisotropy: 1,
            ComparisonFunc: 8, // ALWAYS
            BorderColor: 1,
            MinLOD: 0.0,
            MaxLOD: 1.0,
            ShaderRegister: 0,
            RegisterSpace: 0,
            ShaderVisibility: 5, // PIXEL
        };
        let rs_desc = D3D12_ROOT_SIGNATURE_DESC {
            NumParameters: params.len() as u32,
            pParameters: params.as_ptr(),
            NumStaticSamplers: 1,
            pStaticSamplers: &sampler,
            Flags: 1, // ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT
        };

        let mut blob: *mut ID3DBlob = null_mut();
        let mut err: *mut ID3DBlob = null_mut();
        let hr = unsafe {
            D3D12SerializeRootSignature(
                &rs_desc,
                D3D_ROOT_SIGNATURE_VERSION_1_0,
                &mut blob,
                &mut err,
            )
        };
        let blob_ptr = unsafe { ComPtr::from_raw(blob) };
        let _err_ptr = unsafe { ComPtr::from_raw(err) };
        if !succeeded(hr) {
            return Err(format!("D3D12SerializeRootSignature failed: 0x{hr:08x}"));
        }

        let mut rs = ComPtr::<ID3D12RootSignature>::null();
        let bytes = blob_ptr.as_bytes();
        let hr = unsafe {
            ((*(*device.as_ptr()).lpVtbl).CreateRootSignature)(
                device.as_ptr(),
                0,
                bytes.as_ptr().cast(),
                bytes.len(),
                &IID_ID3D12_ROOT_SIGNATURE,
                rs.as_unknown_mut_ptr(),
            )
        };
        if !succeeded(hr) {
            return Err(format!("CreateRootSignature failed: 0x{hr:08x}"));
        }
        Ok(rs)
    }

    fn create_pso(
        device: &ComPtr<ID3D12Device>,
        rs: &ComPtr<ID3D12RootSignature>,
        vs: &[u8],
        ps: &[u8],
        blend: bool,
    ) -> Result<ComPtr<ID3D12PipelineState>, String> {
        let input_elems = [
            D3D12_INPUT_ELEMENT_DESC {
                SemanticName: b"INST_RECT\0".as_ptr(),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 0,
                InputSlotClass: 1, // PER_INSTANCE_DATA
                InstanceDataStepRate: 1,
            },
            D3D12_INPUT_ELEMENT_DESC {
                SemanticName: b"INST_COLOR\0".as_ptr(),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 16,
                InputSlotClass: 1,
                InstanceDataStepRate: 1,
            },
            D3D12_INPUT_ELEMENT_DESC {
                SemanticName: b"INST_PARAMS\0".as_ptr(),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 32,
                InputSlotClass: 1,
                InstanceDataStepRate: 1,
            },
            D3D12_INPUT_ELEMENT_DESC {
                SemanticName: b"INST_GLYPH\0".as_ptr(),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32_UINT,
                InputSlot: 0,
                AlignedByteOffset: 48,
                InputSlotClass: 1,
                InstanceDataStepRate: 1,
            },
        ];

        let mut blend_desc = D3D12_BLEND_DESC::alpha_blend();
        if !blend {
            blend_desc.RenderTarget[0].BlendEnable = 0;
        }

        let mut desc: D3D12_GRAPHICS_PIPELINE_STATE_DESC = unsafe { zeroed() };
        desc.pRootSignature = rs.as_ptr();
        desc.VS = D3D12_SHADER_BYTECODE {
            pShaderBytecode: vs.as_ptr().cast(),
            BytecodeLength: vs.len(),
        };
        desc.PS = D3D12_SHADER_BYTECODE {
            pShaderBytecode: ps.as_ptr().cast(),
            BytecodeLength: ps.len(),
        };
        desc.BlendState = blend_desc;
        desc.SampleMask = u32::MAX;
        desc.RasterizerState = D3D12_RASTERIZER_DESC::default_no_cull();
        desc.DepthStencilState = D3D12_DEPTH_STENCIL_DESC {
            DepthEnable: 0,
            DepthWriteMask: 1,
            DepthFunc: 2, // LESS
            StencilEnable: 0,
            StencilReadMask: 0xFF,
            StencilWriteMask: 0xFF,
            FrontFace: D3D12_DEPTH_STENCILOP_DESC {
                StencilFailOp: 1,
                StencilDepthFailOp: 1,
                StencilPassOp: 1,
                StencilFunc: 8,
            },
            BackFace: D3D12_DEPTH_STENCILOP_DESC {
                StencilFailOp: 1,
                StencilDepthFailOp: 1,
                StencilPassOp: 1,
                StencilFunc: 8,
            },
        };
        desc.InputLayout = D3D12_INPUT_LAYOUT_DESC {
            pInputElementDescs: input_elems.as_ptr(),
            NumElements: input_elems.len() as u32,
        };
        desc.PrimitiveTopologyType = D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE;
        desc.NumRenderTargets = 1;
        desc.RTVFormats[0] = DXGI_FORMAT_B8G8R8A8_UNORM;
        desc.SampleDesc = DXGI_SAMPLE_DESC { Count: 1, Quality: 0 };

        let mut pso = ComPtr::<ID3D12PipelineState>::null();
        let hr = unsafe {
            ((*(*device.as_ptr()).lpVtbl).CreateGraphicsPipelineState)(
                device.as_ptr(),
                &desc,
                &IID_ID3D12_PIPELINE_STATE,
                pso.as_unknown_mut_ptr(),
            )
        };
        if !succeeded(hr) {
            return Err(format!("CreateGraphicsPipelineState failed: 0x{hr:08x}"));
        }
        Ok(pso)
    }

    fn create_fallback_guest_texture(
        device: &ComPtr<ID3D12Device>,
        cmd_list: &ComPtr<ID3D12GraphicsCommandList>,
        w: u32,
        h: u32,
    ) -> Result<(ComPtr<ID3D12Resource>, ComPtr<ID3D12Resource>), String> {
        let def_props = D3D12_HEAP_PROPERTIES::new(D3D12_HEAP_TYPE_DEFAULT);
        let tex_desc = D3D12_RESOURCE_DESC::tex2d(w, h, DXGI_FORMAT_B8G8R8A8_UNORM);
        let mut tex = ComPtr::<ID3D12Resource>::null();
        let hr = unsafe {
            ((*(*device.as_ptr()).lpVtbl).CreateCommittedResource)(
                device.as_ptr(),
                &def_props,
                D3D12_HEAP_FLAG_NONE,
                &tex_desc,
                D3D12_RESOURCE_STATE_COPY_DEST,
                null(),
                &IID_ID3D12_RESOURCE,
                tex.as_unknown_mut_ptr(),
            )
        };
        if !succeeded(hr) {
            return Err(format!("CreateCommittedResource(fallback tex) failed: 0x{hr:08x}"));
        }

        let row_pitch = ((w * 4 + 255) & !255) as usize;
        let upload_bytes = (row_pitch * h as usize) as u64;
        let up_props = D3D12_HEAP_PROPERTIES::new(D3D12_HEAP_TYPE_UPLOAD);
        let up_desc = D3D12_RESOURCE_DESC::buffer(upload_bytes);
        let mut staging = ComPtr::<ID3D12Resource>::null();
        let hr = unsafe {
            ((*(*device.as_ptr()).lpVtbl).CreateCommittedResource)(
                device.as_ptr(),
                &up_props,
                D3D12_HEAP_FLAG_NONE,
                &up_desc,
                D3D12_RESOURCE_STATE_GENERIC_READ,
                null(),
                &IID_ID3D12_RESOURCE,
                staging.as_unknown_mut_ptr(),
            )
        };
        if !succeeded(hr) {
            return Err(format!("CreateCommittedResource(staging) failed: 0x{hr:08x}"));
        }

        // Render an authentic Android 15 (Material You / API 35) dark desktop pattern into VRAM.
        let mut mapped: *mut u8 = null_mut();
        let read_none = D3D12_RANGE { Begin: 0, End: 0 };
        unsafe {
            ((*(*staging.as_ptr()).lpVtbl).Map)(
                staging.as_ptr(),
                0,
                &read_none,
                (&mut mapped as *mut *mut u8).cast(),
            );
            for y in 0..h {
                let row = core::slice::from_raw_parts_mut(mapped.add(y as usize * row_pitch), w as usize * 4);
                let fy = y as f32 / h as f32;
                for x in 0..w {
                    let fx = x as f32 / w as f32;
                    let dx = fx - 0.45;
                    let dy = fy - 0.48;
                    let radial = (1.0 - (dx * dx * 2.2 + dy * dy * 2.2)).max(0.0);
                    let grid = (((x % 64) == 0) || ((y % 64) == 0)) as i32 as f32 * 10.0;
                    let r = (14.0 + radial * 36.0 + fx * 18.0 + grid) as u8;
                    let g = (20.0 + radial * 62.0 + fy * 22.0 + grid) as u8;
                    let b = (36.0 + radial * 124.0 + (1.0 - fx) * 28.0 + grid * 1.4) as u8;
                    let idx = x as usize * 4;
                    row[idx] = b;
                    row[idx + 1] = g;
                    row[idx + 2] = r;
                    row[idx + 3] = 255;
                }
            }
            ((*(*staging.as_ptr()).lpVtbl).Unmap)(staging.as_ptr(), 0, null());
        }

        let dst_loc = D3D12_TEXTURE_COPY_LOCATION {
            pResource: tex.as_ptr(),
            Type: 0, // SUBRESOURCE_INDEX
            u: D3D12_TEXTURE_COPY_UNION { SubresourceIndex: 0 },
        };
        let src_loc = D3D12_TEXTURE_COPY_LOCATION {
            pResource: staging.as_ptr(),
            Type: 1, // PLACED_FOOTPRINT
            u: D3D12_TEXTURE_COPY_UNION {
                PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                    Offset: 0,
                    Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        Width: w,
                        Height: h,
                        Depth: 1,
                        RowPitch: row_pitch as u32,
                    },
                },
            },
        };
        let barrier = D3D12_RESOURCE_BARRIER::transition(
            tex.as_ptr(),
            D3D12_RESOURCE_STATE_COPY_DEST,
            D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE,
        );
        unsafe {
            ((*(*cmd_list.as_ptr()).lpVtbl).CopyTextureRegion)(
                cmd_list.as_ptr(),
                &dst_loc,
                0,
                0,
                0,
                &src_loc,
                null(),
            );
            ((*(*cmd_list.as_ptr()).lpVtbl).ResourceBarrier)(cmd_list.as_ptr(), 1, &barrier);
        }

        Ok((tex, staging))
    }

    fn bind_backbuffer_rtvs(&mut self) -> Result<(), String> {
        let mut rtv_start = D3D12_CPU_DESCRIPTOR_HANDLE::default();
        unsafe {
            ((*(*self.rtv_heap.as_ptr()).lpVtbl).GetCPUDescriptorHandleForHeapStart)(
                self.rtv_heap.as_ptr(),
                &mut rtv_start,
            );
        }
        for i in 0..BACKBUFFER_COUNT {
            let mut buf = ComPtr::<ID3D12Resource>::null();
            let hr = unsafe {
                ((*(*self.swapchain.as_ptr()).lpVtbl).GetBuffer)(
                    self.swapchain.as_ptr(),
                    i as u32,
                    &IID_ID3D12_RESOURCE,
                    buf.as_unknown_mut_ptr(),
                )
            };
            if !succeeded(hr) {
                return Err(format!("IDXGISwapChain3::GetBuffer({i}) failed: 0x{hr:08x}"));
            }
            let handle = D3D12_CPU_DESCRIPTOR_HANDLE {
                ptr: rtv_start.ptr + i * self.rtv_inc as usize,
            };
            unsafe {
                ((*(*self.device.as_ptr()).lpVtbl).CreateRenderTargetView)(
                    self.device.as_ptr(),
                    buf.as_ptr(),
                    null(),
                    handle,
                );
            }
            self.backbuffers[i] = buf;
        }
        Ok(())
    }

    fn update_guest_srv(&self, res: *mut ID3D12Resource, format: u32) {
        let srv_desc = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: format,
            ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            u: D3D12_SRV_UNION {
                Texture2D: D3D12_TEX2D_SRV {
                    MostDetailedMip: 0,
                    MipLevels: 1,
                    PlaneSlice: 0,
                    ResourceMinLODClamp: 0.0,
                },
            },
        };
        unsafe {
            ((*(*self.device.as_ptr()).lpVtbl).CreateShaderResourceView)(
                self.device.as_ptr(),
                res,
                &srv_desc,
                self.srv_cpu_start,
            );
        }
    }

    /// Imports an external Vulkan/DXGI NT Shared Handle (`VK_KHR_external_memory_win32`)
    /// directly into the DirectX 12 device with zero CPU copies.
    pub fn import_external_shared_texture(
        &mut self,
        nt_handle: HANDLE,
        width: u32,
        height: u32,
        format: u32,
    ) -> Result<(), String> {
        if nt_handle.is_null() {
            return Err("Null NT shared handle".to_string());
        }
        let mut imported = ComPtr::<ID3D12Resource>::null();
        let hr = unsafe {
            ((*(*self.device.as_ptr()).lpVtbl).OpenSharedHandle)(
                self.device.as_ptr(),
                nt_handle,
                &IID_ID3D12_RESOURCE,
                imported.as_unknown_mut_ptr(),
            )
        };
        if !succeeded(hr) {
            return Err(format!("ID3D12Device::OpenSharedHandle failed: 0x{hr:08x}"));
        }
        self.update_guest_srv(imported.as_ptr(), format);
        self.guest_width = width;
        self.guest_height = height;
        self.external_tex = Some(imported);
        Ok(())
    }

    /// Creates a real GPU shared texture + NT handle for zero-copy pipeline verification.
    pub fn create_self_test_shared_texture(&self, w: u32, h: u32) -> Result<HANDLE, String> {
        let props = D3D12_HEAP_PROPERTIES::new(D3D12_HEAP_TYPE_DEFAULT);
        let mut desc = D3D12_RESOURCE_DESC::tex2d(w, h, DXGI_FORMAT_B8G8R8A8_UNORM);
        desc.Flags = D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS;

        let mut tex = ComPtr::<ID3D12Resource>::null();
        let hr = unsafe {
            ((*(*self.device.as_ptr()).lpVtbl).CreateCommittedResource)(
                self.device.as_ptr(),
                &props,
                D3D12_HEAP_FLAG_SHARED,
                &desc,
                D3D12_RESOURCE_STATE_COMMON,
                null(),
                &IID_ID3D12_RESOURCE,
                tex.as_unknown_mut_ptr(),
            )
        };
        if !succeeded(hr) {
            return Err(format!("CreateCommittedResource(SHARED) failed: 0x{hr:08x}"));
        }

        let mut nt_handle: HANDLE = null_mut();
        const GENERIC_ALL: u32 = 0x1000_0000;
        let hr = unsafe {
            ((*(*self.device.as_ptr()).lpVtbl).CreateSharedHandle)(
                self.device.as_ptr(),
                tex.as_ptr().cast(),
                null(),
                GENERIC_ALL,
                null(),
                &mut nt_handle,
            )
        };
        if !succeeded(hr) {
            return Err(format!("CreateSharedHandle failed: 0x{hr:08x}"));
        }
        Ok(nt_handle)
    }

    pub fn poll_shared_mailbox(&mut self, shm: &IvshmemSharedRegion, hud: &mut TelemetryHud) {
        if let Some((_seq, desc)) = shm.frame_mailbox.acquire_latest() {
            if desc.nt_shared_handle != 0 {
                let handle = desc.nt_shared_handle as HANDLE;
                if self
                    .import_external_shared_texture(
                        handle,
                        desc.width.max(1),
                        desc.height.max(1),
                        if desc.dxgi_format == 0 {
                            DXGI_FORMAT_B8G8R8A8_UNORM
                        } else {
                            desc.dxgi_format
                        },
                    )
                    .is_ok()
                {
                    hud.external_handle_active = true;
                }
            }
            hud.target_vrr_hz = desc.vrr_refresh_hz.max(60);
            let reflex_us = desc.reflex_total_latency_us();
            if reflex_us > 0 {
                hud.reflex_latency_ms = reflex_us as f32 / 1000.0;
            }
        }
    }

    pub fn resize(&mut self, new_w: u32, new_h: u32) -> Result<(), String> {
        let w = new_w.max(64);
        let h = new_h.max(64);
        if w == self.width && h == self.height {
            return Ok(());
        }
        self.wait_for_gpu_idle();
        for bb in &mut self.backbuffers {
            *bb = ComPtr::null();
        }
        let sc_flags = DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT
            | if self.tearing_supported {
                DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING
            } else {
                0
            };
        let hr = unsafe {
            ((*(*self.swapchain.as_ptr()).lpVtbl).ResizeBuffers)(
                self.swapchain.as_ptr(),
                BACKBUFFER_COUNT as u32,
                w,
                h,
                DXGI_FORMAT_B8G8R8A8_UNORM,
                sc_flags,
            )
        };
        if !succeeded(hr) {
            return Err(format!("ResizeBuffers failed: 0x{hr:08x}"));
        }
        self.width = w;
        self.height = h;
        self.bind_backbuffer_rtvs()
    }

    pub fn render_frame(&mut self, hud: &mut TelemetryHud) -> Result<(), String> {
        // Wait for swapchain latency object before polling input/rendering.
        if !self.waitable_swapchain.is_null() {
            unsafe { WaitForSingleObject(self.waitable_swapchain, 16) };
        }

        let bb_idx = unsafe {
            ((*(*self.swapchain.as_ptr()).lpVtbl).GetCurrentBackBufferIndex)(self.swapchain.as_ptr())
        } as usize;

        // Wait if GPU is still executing commands for this backbuffer index.
        let target_fence = self.fence_values[bb_idx];
        unsafe {
            if ((*(*self.fence.as_ptr()).lpVtbl).GetCompletedValue)(self.fence.as_ptr()) < target_fence {
                ((*(*self.fence.as_ptr()).lpVtbl).SetEventOnCompletion)(
                    self.fence.as_ptr(),
                    target_fence,
                    self.fence_event,
                );
                WaitForSingleObject(self.fence_event, INFINITE);
            }
        }

        // Measure frametime via high-resolution QPC.
        let mut now_qpc = 0i64;
        unsafe { QueryPerformanceCounter(&mut now_qpc) };
        let dt_sec = ((now_qpc - self.last_frame_qpc).max(1) as f64) / (self.qpc_freq as f64);
        self.last_frame_qpc = now_qpc;
        let time_sec = ((now_qpc - self.start_qpc) as f64) / (self.qpc_freq as f64);
        hud.record_frametime((dt_sec * 1000.0) as f32, 0);

        // Build UI instance list directly into mapped upload memory (zero allocations).
        let base_slot = bb_idx * MAX_UI_QUADS;
        let inst_slice = unsafe {
            core::slice::from_raw_parts_mut(self.instance_mapped.add(base_slot), MAX_UI_QUADS)
        };
        let ui_count = self.build_ui_instances(inst_slice, hud);

        // Guest framebuffer destination rect (leaving space for collapsible sidebar when open).
        let sidebar_w = if hud.sidebar_open { 236.0 } else { 0.0 };
        let top_bar_h = 34.0;
        let view_w = (self.width as f32 - sidebar_w).max(64.0);
        let view_h = (self.height as f32 - top_bar_h).max(64.0);
        inst_slice[ui_count] = UiInstance {
            rect: [0.0, top_bar_h, view_w, view_h],
            color: [1.0, 1.0, 1.0, 1.0],
            params: [0.0, 0.0, 1.0, 0.0],
            glyph: [0, 0],
        };
        let guest_inst_idx = ui_count;

        let alloc = self.allocators[bb_idx].as_ptr();
        let cl = self.cmd_list.as_ptr();
        let bb = self.backbuffers[bb_idx].as_ptr();

        let mut rtv_start = D3D12_CPU_DESCRIPTOR_HANDLE::default();
        unsafe {
            ((*(*self.rtv_heap.as_ptr()).lpVtbl).GetCPUDescriptorHandleForHeapStart)(
                self.rtv_heap.as_ptr(),
                &mut rtv_start,
            );
        }
        let rtv_handle = D3D12_CPU_DESCRIPTOR_HANDLE {
            ptr: rtv_start.ptr + bb_idx * self.rtv_inc as usize,
        };

        let constants = FrameConstants {
            viewport_size: [self.width as f32, self.height as f32],
            guest_size: [self.guest_width as f32, self.guest_height as f32],
            time_sec: time_sec as f32,
            blur_radius: 5.5,
            hud_visible: hud.hud_visible as u32,
            sidebar_open: hud.sidebar_open as u32,
        };

        let upload_gpu_base = unsafe {
            ((*(*self.instance_upload.as_ptr()).lpVtbl).GetGPUVirtualAddress)(
                self.instance_upload.as_ptr(),
            )
        } + (base_slot * size_of::<UiInstance>()) as u64;

        let vp = D3D12_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: self.width as f32,
            Height: self.height as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        let scissor = RECT {
            left: 0,
            top: 0,
            right: self.width as i32,
            bottom: self.height as i32,
        };

        unsafe {
            ((*(*alloc).lpVtbl).Reset)(alloc);
            ((*(*cl).lpVtbl).Reset)(cl, alloc, self.pso_guest.as_ptr());

            let to_rt = D3D12_RESOURCE_BARRIER::transition(
                bb,
                D3D12_RESOURCE_STATE_PRESENT,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
            );
            ((*(*cl).lpVtbl).ResourceBarrier)(cl, 1, &to_rt);

            let clear = [0.035f32, 0.043, 0.063, 1.0];
            ((*(*cl).lpVtbl).ClearRenderTargetView)(cl, rtv_handle, &clear, 0, null());
            ((*(*cl).lpVtbl).OMSetRenderTargets)(cl, 1, &rtv_handle, 0, null());
            ((*(*cl).lpVtbl).RSSetViewports)(cl, 1, &vp);
            ((*(*cl).lpVtbl).RSSetScissorRects)(cl, 1, &scissor);

            let heaps = [self.srv_heap.as_ptr()];
            ((*(*cl).lpVtbl).SetDescriptorHeaps)(cl, 1, heaps.as_ptr());
            ((*(*cl).lpVtbl).SetGraphicsRootSignature)(cl, self.root_sig.as_ptr());
            ((*(*cl).lpVtbl).SetGraphicsRoot32BitConstants)(
                cl,
                0,
                (size_of::<FrameConstants>() / 4) as u32,
                (&constants as *const FrameConstants).cast(),
                0,
            );
            ((*(*cl).lpVtbl).SetGraphicsRootDescriptorTable)(cl, 1, self.srv_gpu_start);
            ((*(*cl).lpVtbl).IASetPrimitiveTopology)(cl, D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

            // Pass 1: Zero-Copy Guest Android 15 Framebuffer
            let guest_vbv = D3D12_VERTEX_BUFFER_VIEW {
                BufferLocation: upload_gpu_base + (guest_inst_idx * size_of::<UiInstance>()) as u64,
                SizeInBytes: size_of::<UiInstance>() as u32,
                StrideInBytes: size_of::<UiInstance>() as u32,
            };
            ((*(*cl).lpVtbl).IASetVertexBuffers)(cl, 0, 1, &guest_vbv);
            ((*(*cl).lpVtbl).DrawInstanced)(cl, 6, 1, 0, 0);

            // Pass 2: Kawase Frosted Glass Panels, SDF Chrome, Frametime Graph & HUD Text
            if ui_count > 0 {
                ((*(*cl).lpVtbl).SetPipelineState)(cl, self.pso_ui.as_ptr());
                let ui_vbv = D3D12_VERTEX_BUFFER_VIEW {
                    BufferLocation: upload_gpu_base,
                    SizeInBytes: (ui_count * size_of::<UiInstance>()) as u32,
                    StrideInBytes: size_of::<UiInstance>() as u32,
                };
                ((*(*cl).lpVtbl).IASetVertexBuffers)(cl, 0, 1, &ui_vbv);
                ((*(*cl).lpVtbl).DrawInstanced)(cl, 6, ui_count as u32, 0, 0);
            }

            let to_present = D3D12_RESOURCE_BARRIER::transition(
                bb,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_PRESENT,
            );
            ((*(*cl).lpVtbl).ResourceBarrier)(cl, 1, &to_present);
            ((*(*cl).lpVtbl).Close)(cl);

            let lists = [cl];
            ((*(*self.queue.as_ptr()).lpVtbl).ExecuteCommandLists)(
                self.queue.as_ptr(),
                1,
                lists.as_ptr(),
            );

            let present_flags = if self.tearing_supported {
                DXGI_PRESENT_ALLOW_TEARING
            } else {
                0
            };
            ((*(*self.swapchain.as_ptr()).lpVtbl).Present)(
                self.swapchain.as_ptr(),
                0,
                present_flags,
            );

            let fv = self.next_fence_value;
            self.next_fence_value += 1;
            self.fence_values[bb_idx] = fv;
            ((*(*self.queue.as_ptr()).lpVtbl).Signal)(
                self.queue.as_ptr(),
                self.fence.as_ptr(),
                fv,
            );
        }

        Ok(())
    }

    fn build_ui_instances(&self, out: &mut [UiInstance], hud: &TelemetryHud) -> usize {
        let mut n = 0usize;
        let w = self.width as f32;
        let h = self.height as f32;

        // Top frameless title bar (obsidian dark chrome + status strip)
        push_rect(out, &mut n, [0.0, 0.0, w, 34.0], [0.055, 0.070, 0.102, 0.96], 0.0, 0.0, 1);
        push_rect(out, &mut n, [0.0, 33.0, w, 1.0], [0.24, 0.56, 0.98, 0.45], 0.0, 0.0, 1);
        push_text(out, &mut n, 14.0, 10.0, 2.0, [0.88, 0.94, 1.0, 1.0], "TITAN AOSP 15 // API 35 // DX12 ZERO-COPY");

        // Close button pill in top-right corner
        push_rect(out, &mut n, [w - 38.0, 6.0, 28.0, 22.0], [0.65, 0.16, 0.22, 0.85], 6.0, 0.0, 1);
        push_text(out, &mut n, w - 29.0, 10.0, 2.0, [1.0, 0.92, 0.94, 1.0], "X");

        // Telemetry HUD Overlay (Kawase glass backdrop + metrics + frametime sparkline)
        if hud.hud_visible {
            let hx = 16.0;
            let hy = 50.0;
            let hw = 276.0;
            let hh = 188.0;
            push_rect(out, &mut n, [hx, hy, hw, hh], [0.05, 0.07, 0.11, 0.72], 12.0, 1.2, 0);

            let line0 = format!("FPS {:>5.0}   1% LOW {:>4.0}", hud.avg_fps, hud.one_percent_low_fps);
            let line1 = format!(
                "FT  {:>4.2} MS  VRR {} HZ{}",
                hud.last_frametime_ms,
                hud.target_vrr_hz,
                if hud.tearing_supported { " T" } else { "" }
            );
            let line2 = format!("RAM {:>4.1} MB  REFLEX {:>4.2} MS", hud.host_ram_mib, hud.reflex_latency_ms);
            let line3 = format!(
                "SHM {}  CACHE {}",
                if hud.external_handle_active { "NT-HANDLE" } else { "READY" },
                if self.cache_hit { "DISK-HIT" } else { "WARMED" }
            );
            push_text(out, &mut n, hx + 12.0, hy + 12.0, 2.0, [0.35, 0.88, 0.65, 1.0], &line0);
            push_text(out, &mut n, hx + 12.0, hy + 32.0, 2.0, [0.85, 0.91, 1.0, 1.0], &line1);
            push_text(out, &mut n, hx + 12.0, hy + 52.0, 2.0, [0.55, 0.78, 1.0, 1.0], &line2);
            push_text(out, &mut n, hx + 12.0, hy + 72.0, 1.7, [0.70, 0.78, 0.90, 1.0], &line3);

            // Frametime graph area
            let gx = hx + 12.0;
            let gy = hy + 98.0;
            let gw = hw - 24.0;
            let gh = 74.0;
            push_rect(out, &mut n, [gx, gy, gw, gh], [0.03, 0.04, 0.07, 0.75], 6.0, 1.0, 1);

            let bars = 64usize;
            let bar_w = (gw - 8.0) / bars as f32;
            let max_ft = 16.6f32;
            let mut bx = gx + 4.0;
            for ft in hud.recent_samples(bars) {
                let norm = (ft / max_ft).clamp(0.08, 1.0);
                let bh = norm * (gh - 8.0);
                let color = if ft <= 5.0 {
                    [0.24, 0.88, 0.60, 0.92]
                } else if ft <= 9.0 {
                    [0.95, 0.76, 0.25, 0.92]
                } else {
                    [0.96, 0.30, 0.36, 0.92]
                };
                push_rect(
                    out,
                    &mut n,
                    [bx, gy + gh - 4.0 - bh, (bar_w - 1.0).max(1.5), bh],
                    color,
                    1.0,
                    0.0,
                    1,
                );
                bx += bar_w;
            }
        }

        // Collapsible Quick-Action Sidebar
        if hud.sidebar_open {
            let sw = 236.0;
            let sx = w - sw;
            let sy = 34.0;
            let sh = (h - sy).max(64.0);
            push_rect(out, &mut n, [sx, sy, sw, sh], [0.055, 0.075, 0.115, 0.80], 0.0, 1.0, 0);
            push_text(out, &mut n, sx + 16.0, sy + 16.0, 2.0, [0.48, 0.78, 1.0, 1.0], "QUICK ACTIONS");

            let items = [
                ("F1  HUD OVERLAY", hud.hud_visible),
                ("F2  SIDEBAR", hud.sidebar_open),
                ("F3  VRR 120/144/240", true),
                ("F4  WASD KEYMAP", hud.keymapper_active),
                ("2MB LARGE PAGES", hud.large_pages_active),
            ];
            for (idx, (label, active)) in items.iter().enumerate() {
                let by = sy + 48.0 + idx as f32 * 48.0;
                let bg = if *active {
                    [0.14, 0.27, 0.48, 0.75]
                } else {
                    [0.09, 0.12, 0.18, 0.72]
                };
                push_rect(out, &mut n, [sx + 12.0, by, sw - 24.0, 38.0], bg, 8.0, 1.0, 1);
                let text_col = if *active {
                    [0.92, 0.97, 1.0, 1.0]
                } else {
                    [0.58, 0.65, 0.76, 1.0]
                };
                push_text(out, &mut n, sx + 22.0, by + 12.0, 1.8, text_col, label);
            }

            let cores = format!("P-CORES PINNED: {}", hud.p_core_count);
            push_text(out, &mut n, sx + 16.0, sy + 306.0, 1.7, [0.40, 0.88, 0.66, 1.0], &cores);
            push_text(out, &mut n, sx + 16.0, sy + 330.0, 1.7, [0.68, 0.76, 0.88, 1.0], "ESC TO EXIT");
        }

        n
    }

    pub fn wait_for_gpu_idle(&mut self) {
        let fv = self.next_fence_value;
        self.next_fence_value += 1;
        unsafe {
            if succeeded(((*(*self.queue.as_ptr()).lpVtbl).Signal)(
                self.queue.as_ptr(),
                self.fence.as_ptr(),
                fv,
            )) {
                if ((*(*self.fence.as_ptr()).lpVtbl).GetCompletedValue)(self.fence.as_ptr()) < fv {
                    ((*(*self.fence.as_ptr()).lpVtbl).SetEventOnCompletion)(
                        self.fence.as_ptr(),
                        fv,
                        self.fence_event,
                    );
                    WaitForSingleObject(self.fence_event, INFINITE);
                }
            }
        }
    }
}

impl Drop for Dx12Renderer {
    fn drop(&mut self) {
        self.wait_for_gpu_idle();
        if !self.fence_event.is_null() {
            unsafe { CloseHandle(self.fence_event) };
        }
        if !self.waitable_swapchain.is_null() {
            unsafe { CloseHandle(self.waitable_swapchain) };
        }
    }
}

#[inline]
fn push_rect(
    out: &mut [UiInstance],
    n: &mut usize,
    rect: [f32; 4],
    color: [f32; 4],
    radius: f32,
    border: f32,
    mode: u32,
) {
    if *n >= MAX_UI_QUADS - 1 {
        return;
    }
    out[*n] = UiInstance {
        rect,
        color,
        params: [radius, border, mode as f32, 0.0],
        glyph: [0, 0],
    };
    *n += 1;
}

fn push_text(
    out: &mut [UiInstance],
    n: &mut usize,
    mut x: f32,
    y: f32,
    scale: f32,
    color: [f32; 4],
    text: &str,
) {
    let cw = 5.0 * scale;
    let ch = 7.0 * scale;
    let adv = 6.0 * scale;
    for ch_val in text.bytes() {
        if *n >= MAX_UI_QUADS - 1 {
            break;
        }
        if ch_val != b' ' {
            let (lo, hi) = glyph_5x7_bits(ch_val);
            out[*n] = UiInstance {
                rect: [x, y, cw, ch],
                color,
                params: [0.0, 0.0, 2.0, 0.0],
                glyph: [lo, hi],
            };
            *n += 1;
        }
        x += adv;
    }
}

/// Encodes an ASCII character into a 35-bit 5x7 bitmap `(lo32, hi3)`.
fn glyph_5x7_bits(b: u8) -> (u32, u32) {
    let rows: [u8; 7] = match b.to_ascii_uppercase() {
        b'0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        b'1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        b'2' => [0b01110, 0b10001, 0b00001, 0b00110, 0b01000, 0b10000, 0b11111],
        b'3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        b'4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        b'5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        b'6' => [0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        b'7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        b'8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        b'9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110],
        b'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        b'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        b'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        b'D' => [0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100],
        b'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        b'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        b'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110],
        b'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        b'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        b'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        b'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        b'M' => [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001],
        b'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        b'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        b'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        b'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        b'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        b'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        b'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        b'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        b'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        b'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        b'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        b'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        b'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        b'.' => [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100],
        b':' => [0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b01100, 0b00000],
        b'-' => [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000],
        b'/' => [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000],
        b'%' => [0b11001, 0b11010, 0b00010, 0b00100, 0b01000, 0b01011, 0b10011],
        _ => [0; 7],
    };
    let mut bits: u64 = 0;
    for (r, &row) in rows.iter().enumerate() {
        // Flip bit order so col=0 is bit 0 (leftmost pixel).
        let mut rev = 0u64;
        for c in 0..5 {
            if (row & (1 << (4 - c))) != 0 {
                rev |= 1 << c;
            }
        }
        bits |= rev << (r * 5);
    }
    ((bits & 0xFFFF_FFFF) as u32, ((bits >> 32) & 0x7) as u32)
}

/// Shared UI event flags updated by the Win32 WndProc without locks.
pub struct WindowState {
    pub should_quit: AtomicBool,
    pub toggle_hud: AtomicBool,
    pub toggle_sidebar: AtomicBool,
    pub cycle_vrr: AtomicBool,
    pub toggle_keymapper: AtomicBool,
    pub resized: AtomicBool,
    pub width: std::sync::atomic::AtomicU32,
    pub height: std::sync::atomic::AtomicU32,
    pub raw_input_hook: Option<Box<dyn Fn(LPARAM) + Send + Sync>>,
}

impl WindowState {
    pub fn new(width: u32, height: u32, raw_input_hook: Option<Box<dyn Fn(LPARAM) + Send + Sync>>) -> Arc<Self> {
        Arc::new(Self {
            should_quit: AtomicBool::new(false),
            toggle_hud: AtomicBool::new(false),
            toggle_sidebar: AtomicBool::new(false),
            cycle_vrr: AtomicBool::new(false),
            toggle_keymapper: AtomicBool::new(false),
            resized: AtomicBool::new(false),
            width: std::sync::atomic::AtomicU32::new(width),
            height: std::sync::atomic::AtomicU32::new(height),
            raw_input_hook,
        })
    }
}

/// Borderless, dark-mode Win32 native host window with `WM_NCHITTEST` drag/resize.
pub struct FramelessWindow {
    pub hwnd: HWND,
    pub state: Arc<WindowState>,
}

impl FramelessWindow {
    pub fn create(
        title: &str,
        width: u32,
        height: u32,
        visible: bool,
        raw_input_hook: Option<Box<dyn Fn(LPARAM) + Send + Sync>>,
    ) -> Result<Self, String> {
        let state = WindowState::new(width, height, raw_input_hook);
        let hinst = unsafe { GetModuleHandleW(null()) };
        let class_name = utf16_null("TitanAosp15Shell");
        let title_w = utf16_null(title);

        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_OWNDC | CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst,
            hIcon: null_mut(),
            hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: null_mut(),
        };
        unsafe { RegisterClassExW(&wc) };

        let state_ptr = Arc::into_raw(Arc::clone(&state)) as *mut c_void;
        let style = WS_POPUP
            | WS_MINIMIZEBOX
            | WS_MAXIMIZEBOX
            | WS_THICKFRAME
            | if visible { WS_VISIBLE } else { 0 };
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_APPWINDOW,
                class_name.as_ptr(),
                title_w.as_ptr(),
                style,
                120,
                80,
                width as i32,
                height as i32,
                null_mut(),
                null_mut(),
                hinst,
                state_ptr,
            )
        };
        if hwnd.is_null() {
            unsafe { drop(Arc::from_raw(state_ptr as *const WindowState)) };
            return Err("CreateWindowExW failed".to_string());
        }

        let dark: BOOL = 1;
        let round: u32 = DWMWCP_ROUND;
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                (&dark as *const BOOL).cast(),
                size_of::<BOOL>() as u32,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                (&round as *const u32).cast(),
                size_of::<u32>() as u32,
            );
            ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE });
        }

        Ok(Self { hwnd, state })
    }

    pub fn pump_messages(&self) -> bool {
        let mut msg: MSG = unsafe { zeroed() };
        unsafe {
            while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                if msg.message == WM_QUIT {
                    self.state.should_quit.store(true, Ordering::Relaxed);
                    return false;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        !self.state.should_quit.load(Ordering::Relaxed)
    }

    pub fn sync_hud_flags(&self, hud: &mut TelemetryHud) {
        if self.state.toggle_hud.swap(false, Ordering::AcqRel) {
            hud.hud_visible = !hud.hud_visible;
        }
        if self.state.toggle_sidebar.swap(false, Ordering::AcqRel) {
            hud.sidebar_open = !hud.sidebar_open;
        }
        if self.state.cycle_vrr.swap(false, Ordering::AcqRel) {
            hud.cycle_vrr_target();
        }
        if self.state.toggle_keymapper.swap(false, Ordering::AcqRel) {
            hud.keymapper_active = !hud.keymapper_active;
        }
    }
}

impl Drop for FramelessWindow {
    fn drop(&mut self) {
        if !self.hwnd.is_null() {
            unsafe {
                let raw = GetWindowLongPtrW(self.hwnd, -21 /* GWLP_USERDATA */);
                if raw != 0 {
                    SetWindowLongPtrW(self.hwnd, -21, 0);
                    drop(Arc::from_raw(raw as *const WindowState));
                }
                DestroyWindow(self.hwnd);
            }
            self.hwnd = null_mut();
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    const WM_NCCREATE: u32 = 0x0081;
    const GWLP_USERDATA: i32 = -21;

    if msg == WM_NCCREATE {
        let createstruct = &*(lparam as *const *const c_void);
        // CREATESTRUCTW.lpCreateParams is the first pointer-sized field at offset 0.
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*createstruct) as isize);
        return 1;
    }

    let raw_state = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    let state = if raw_state != 0 {
        Some(&*(raw_state as *const WindowState))
    } else {
        None
    };

    match msg {
        WM_INPUT => {
            if let Some(st) = state {
                if let Some(hook) = &st.raw_input_hook {
                    hook(lparam);
                }
            }
            0
        }
        WM_NCHITTEST => {
            let sx = (lparam & 0xFFFF) as i16 as i32;
            let sy = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let mut wr = RECT::default();
            GetWindowRect(hwnd, &mut wr);
            let border = 6;
            let left = sx < wr.left + border;
            let right = sx >= wr.right - border;
            let top = sy < wr.top + border;
            let bottom = sy >= wr.bottom - border;
            match (left, right, top, bottom) {
                (true, _, true, _) => HTTOPLEFT,
                (_, true, true, _) => HTTOPRIGHT,
                (true, _, _, true) => HTBOTTOMLEFT,
                (_, true, _, true) => HTBOTTOMRIGHT,
                (true, _, _, _) => HTLEFT,
                (_, true, _, _) => HTRIGHT,
                (_, _, true, _) => HTTOP,
                (_, _, _, true) => HTBOTTOM,
                _ => {
                    let mut pt = POINT { x: sx, y: sy };
                    ScreenToClient(hwnd, &mut pt);
                    let w = wr.right - wr.left;
                    if pt.y >= 0 && pt.y < 34 && pt.x < w - 44 {
                        HTCAPTION
                    } else {
                        HTCLIENT
                    }
                }
            }
        }
        WM_LBUTTONDOWN => {
            let x = (lparam & 0xFFFF) as i16 as i32;
            let y = ((lparam >> 16) & 0xFFFF) as i16 as i32;
            let mut rc = RECT::default();
            GetClientRect(hwnd, &mut rc);
            let w = rc.right - rc.left;
            if y < 34 && x >= w - 44 {
                if let Some(st) = state {
                    st.should_quit.store(true, Ordering::Relaxed);
                }
                PostQuitMessage(0);
                return 0;
            }
            if let Some(st) = state {
                if x >= w - 236 && y >= 82 && y <= 316 {
                    let btn = (y - 82) / 48;
                    match btn {
                        0 => { st.toggle_hud.store(true, Ordering::Release); }
                        1 => { st.toggle_sidebar.store(true, Ordering::Release); }
                        2 => { st.cycle_vrr.store(true, Ordering::Release); }
                        3 => { st.toggle_keymapper.store(true, Ordering::Release); }
                        _ => {}
                    }
                }
            }
            0
        }
        WM_KEYDOWN => {
            if let Some(st) = state {
                match wparam {
                    VK_ESCAPE => {
                        st.should_quit.store(true, Ordering::Relaxed);
                        PostQuitMessage(0);
                    }
                    VK_F1 => { st.toggle_hud.store(true, Ordering::Release); }
                    VK_F2 | VK_TAB => { st.toggle_sidebar.store(true, Ordering::Release); }
                    VK_F3 => { st.cycle_vrr.store(true, Ordering::Release); }
                    VK_F4 => { st.toggle_keymapper.store(true, Ordering::Release); }
                    _ => {}
                }
            }
            0
        }
        WM_SIZE => {
            if let Some(st) = state {
                let w = (lparam & 0xFFFF) as u32;
                let h = ((lparam >> 16) & 0xFFFF) as u32;
                if w >= 64 && h >= 64 {
                    st.width.store(w, Ordering::Relaxed);
                    st.height.store(h, Ordering::Relaxed);
                    st.resized.store(true, Ordering::Release);
                }
            }
            0
        }
        WM_CLOSE | WM_DESTROY => {
            if let Some(st) = state {
                st.should_quit.store(true, Ordering::Relaxed);
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn shader_disk_cache_roundtrip_and_hlsl_compile() {
        let mut cache_file = std::env::temp_dir();
        cache_file.push("titan_test_pipeline_cache.bin");
        let _ = fs::remove_file(&cache_file);

        let b1 = ShaderBundle::load_or_compile(&cache_file).expect("first HLSL compile");
        assert!(!b1.loaded_from_disk);
        assert!(!b1.vs.is_empty() && !b1.ps_guest.is_empty() && !b1.ps_ui.is_empty());

        let b2 = ShaderBundle::load_or_compile(&cache_file).expect("disk cache hit");
        assert!(b2.loaded_from_disk);
        assert_eq!(b1.vs, b2.vs);
        let _ = fs::remove_file(&cache_file);
    }

    #[test]
    fn dx12_renderer_and_external_shared_handle_import() {
        let win = FramelessWindow::create("TitanTest", 640, 360, false, None)
            .expect("create hidden frameless window");
        let mut renderer = Dx12Renderer::new(win.hwnd, 640, 360).expect("init DX12 renderer");

        let shared_handle = renderer
            .create_self_test_shared_texture(1280, 720)
            .expect("create DXGI/D3D12 shared handle");
        renderer
            .import_external_shared_texture(shared_handle, 1280, 720, DXGI_FORMAT_B8G8R8A8_UNORM)
            .expect("open shared handle");
        unsafe { CloseHandle(shared_handle) };

        let mut hud = TelemetryHud::new(renderer.tearing_supported);
        hud.external_handle_active = true;
        for _ in 0..8 {
            renderer.render_frame(&mut hud).expect("render frame");
        }
        assert_eq!(hud.total_frames, 8);
        assert!(PathBuf::from(PIPELINE_CACHE_FILE).exists());
    }
}
