//! Minimal, zero-dependency Windows / DXGI / DirectX 12 / D3DCompiler FFI & COM bindings.
//!
//! All COM vtables and structs are verified directly against Windows SDK / MinGW-w64 headers
//! (`d3d12.h`, `dxgi1_5.h`, `d3dcommon.h`, `d3dcompiler.h`, `winuser.h`, `psapi.h`).

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use core::ffi::c_void;
use core::mem::{size_of, zeroed};
use core::ptr::null_mut;

pub type BOOL = i32;
pub type HRESULT = i32;
pub type HWND = *mut c_void;
pub type HINSTANCE = *mut c_void;
pub type HMODULE = *mut c_void;
pub type HANDLE = *mut c_void;
pub type HICON = *mut c_void;
pub type HCURSOR = *mut c_void;
pub type HBRUSH = *mut c_void;
pub type HMENU = *mut c_void;
pub type ATOM = u16;
pub type WPARAM = usize;
pub type LPARAM = isize;
pub type LRESULT = isize;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct GUID {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl GUID {
    pub const fn from_u128(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Self {
        Self {
            data1: d1,
            data2: d2,
            data3: d3,
            data4: d4,
        }
    }
}

pub const IID_IDXGI_ADAPTER1: GUID =
    GUID::from_u128(0x29038f61, 0x3839, 0x4626, [0x91, 0xfd, 0x08, 0x68, 0x79, 0x01, 0x1a, 0x05]);
pub const IID_IDXGI_FACTORY4: GUID =
    GUID::from_u128(0x1bc6ea02, 0xef36, 0x464f, [0xbf, 0x0c, 0x21, 0xca, 0x39, 0xe5, 0x16, 0x8a]);
pub const IID_IDXGI_FACTORY5: GUID =
    GUID::from_u128(0x7632e1f5, 0xee65, 0x4dca, [0x87, 0xfd, 0x84, 0xcd, 0x75, 0xf8, 0x83, 0x8d]);
pub const IID_IDXGI_SWAP_CHAIN3: GUID =
    GUID::from_u128(0x94d99bdb, 0xf1f8, 0x4ab0, [0xb2, 0x36, 0x7d, 0xa0, 0x17, 0x0e, 0xda, 0xb1]);

pub const IID_ID3D12_DEVICE: GUID =
    GUID::from_u128(0x189819f1, 0x1db6, 0x4b57, [0xbe, 0x54, 0x18, 0x21, 0x33, 0x9b, 0x85, 0xf7]);
pub const IID_ID3D12_COMMAND_QUEUE: GUID =
    GUID::from_u128(0x0ec870a6, 0x5d7e, 0x4c22, [0x8c, 0xfc, 0x5b, 0xaa, 0xe0, 0x76, 0x16, 0xed]);
pub const IID_ID3D12_DESCRIPTOR_HEAP: GUID =
    GUID::from_u128(0x8efb471d, 0x616c, 0x4f49, [0x90, 0xf7, 0x12, 0x7b, 0xb7, 0x63, 0xfa, 0x51]);
pub const IID_ID3D12_COMMAND_ALLOCATOR: GUID =
    GUID::from_u128(0x6102dee4, 0xaf59, 0x4b09, [0xb9, 0x99, 0xb4, 0x4d, 0x73, 0xf0, 0x9b, 0x24]);
pub const IID_ID3D12_GRAPHICS_COMMAND_LIST: GUID =
    GUID::from_u128(0x5b160d0f, 0xac1b, 0x4185, [0x8b, 0xa8, 0xb3, 0xae, 0x42, 0xa5, 0xa4, 0x55]);
pub const IID_ID3D12_ROOT_SIGNATURE: GUID =
    GUID::from_u128(0xc54a6b66, 0x72df, 0x4ee8, [0x8b, 0xe5, 0xa9, 0x46, 0xa1, 0x42, 0x92, 0x14]);
pub const IID_ID3D12_PIPELINE_STATE: GUID =
    GUID::from_u128(0x765a30f3, 0xf624, 0x4c6f, [0xa8, 0x28, 0xac, 0xe9, 0x48, 0x62, 0x24, 0x45]);
pub const IID_ID3D12_RESOURCE: GUID =
    GUID::from_u128(0x696442be, 0xa72e, 0x4059, [0xbc, 0x79, 0x5b, 0x5c, 0x98, 0x04, 0x0f, 0xad]);
pub const IID_ID3D12_FENCE: GUID =
    GUID::from_u128(0x0a753dcf, 0xc4d8, 0x4b91, [0xad, 0xf6, 0xbe, 0x5a, 0x60, 0xd9, 0x5a, 0x76]);

#[inline(always)]
pub fn succeeded(hr: HRESULT) -> bool {
    hr >= 0
}

/// Safe RAII wrapper for COM interface pointers (`T` starts with `*const Vtbl`).
#[repr(transparent)]
pub struct ComPtr<T> {
    ptr: *mut T,
}

impl<T> ComPtr<T> {
    #[inline]
    pub const fn null() -> Self {
        Self { ptr: null_mut() }
    }

    /// Wraps an already AddRef'd raw pointer.
    #[inline]
    pub unsafe fn from_raw(ptr: *mut T) -> Self {
        Self { ptr }
    }

    #[inline]
    pub fn as_ptr(&self) -> *mut T {
        self.ptr
    }

    #[inline]
    pub fn is_null(&self) -> bool {
        self.ptr.is_null()
    }

    #[inline]
    pub fn as_unknown_mut_ptr(&mut self) -> *mut *mut c_void {
        (&mut self.ptr as *mut *mut T).cast()
    }

    #[inline]
    pub unsafe fn query_interface<U>(&self, iid: &GUID) -> Result<ComPtr<U>, HRESULT> {
        if self.ptr.is_null() {
            return Err(-2147467261); // E_POINTER
        }
        let vtbl = *(self.ptr as *const *const IUnknownVtbl);
        let mut out: *mut c_void = null_mut();
        let hr = ((*vtbl).QueryInterface)(self.ptr.cast(), iid, &mut out);
        if succeeded(hr) {
            Ok(ComPtr::from_raw(out.cast()))
        } else {
            Err(hr)
        }
    }
}

impl<T> Drop for ComPtr<T> {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                let vtbl = *(self.ptr as *const *const IUnknownVtbl);
                ((*vtbl).Release)(self.ptr.cast());
            }
            self.ptr = null_mut();
        }
    }
}

#[repr(C)]
pub struct IUnknownVtbl {
    pub QueryInterface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    pub AddRef: unsafe extern "system" fn(*mut c_void) -> u32,
    pub Release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
pub struct ID3DBlobVtbl {
    pub unk: IUnknownVtbl,
    pub GetBufferPointer: unsafe extern "system" fn(*mut ID3DBlob) -> *mut c_void,
    pub GetBufferSize: unsafe extern "system" fn(*mut ID3DBlob) -> usize,
}

#[repr(C)]
pub struct ID3DBlob {
    pub lpVtbl: *const ID3DBlobVtbl,
}

impl ComPtr<ID3DBlob> {
    pub fn as_bytes(&self) -> &[u8] {
        if self.ptr.is_null() {
            return &[];
        }
        unsafe {
            let p = ((*(*self.ptr).lpVtbl).GetBufferPointer)(self.ptr) as *const u8;
            let len = ((*(*self.ptr).lpVtbl).GetBufferSize)(self.ptr);
            core::slice::from_raw_parts(p, len)
        }
    }
}

// ============================================================================
// Win32 Types & Constants
// ============================================================================

pub const WS_POPUP: u32 = 0x8000_0000;
pub const WS_VISIBLE: u32 = 0x1000_0000;
pub const WS_MINIMIZEBOX: u32 = 0x0002_0000;
pub const WS_MAXIMIZEBOX: u32 = 0x0001_0000;
pub const WS_THICKFRAME: u32 = 0x0004_0000;
pub const WS_EX_APPWINDOW: u32 = 0x0004_0000;

pub const SW_SHOW: i32 = 5;
pub const SW_HIDE: i32 = 0;

pub const PM_REMOVE: u32 = 0x0001;
pub const WM_DESTROY: u32 = 0x0002;
pub const WM_SIZE: u32 = 0x0005;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_QUIT: u32 = 0x0012;
pub const WM_NCHITTEST: u32 = 0x0084;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_INPUT: u32 = 0x00FF;

pub const HTCLIENT: isize = 1;
pub const HTCAPTION: isize = 2;
pub const HTLEFT: isize = 10;
pub const HTRIGHT: isize = 11;
pub const HTTOP: isize = 12;
pub const HTTOPLEFT: isize = 13;
pub const HTTOPRIGHT: isize = 14;
pub const HTBOTTOM: isize = 15;
pub const HTBOTTOMLEFT: isize = 16;
pub const HTBOTTOMRIGHT: isize = 17;

pub const VK_ESCAPE: usize = 0x1B;
pub const VK_TAB: usize = 0x09;
pub const VK_F1: usize = 0x70;
pub const VK_F2: usize = 0x71;
pub const VK_F3: usize = 0x72;
pub const VK_F4: usize = 0x73;

pub const CS_OWNDC: u32 = 0x0020;
pub const CS_HREDRAW: u32 = 0x0002;
pub const CS_VREDRAW: u32 = 0x0001;
pub const IDC_ARROW: *const u16 = 32512usize as *const u16;

pub const DWMWA_USE_IMMERSIVE_DARK_MODE: u32 = 20;
pub const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
pub const DWMWCP_ROUND: u32 = 2;

pub const INFINITE: u32 = 0xFFFF_FFFF;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct POINT {
    pub x: i32,
    pub y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct RECT {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MSG {
    pub hwnd: HWND,
    pub message: u32,
    pub wParam: WPARAM,
    pub lParam: LPARAM,
    pub time: u32,
    pub pt: POINT,
}

#[repr(C)]
pub struct WNDCLASSEXW {
    pub cbSize: u32,
    pub style: u32,
    pub lpfnWndProc: Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>,
    pub cbClsExtra: i32,
    pub cbWndExtra: i32,
    pub hInstance: HINSTANCE,
    pub hIcon: HICON,
    pub hCursor: HCURSOR,
    pub hbrBackground: HBRUSH,
    pub lpszMenuName: *const u16,
    pub lpszClassName: *const u16,
    pub hIconSm: HICON,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct LUID {
    pub LowPart: u32,
    pub HighPart: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct PROCESS_MEMORY_COUNTERS_EX {
    pub cb: u32,
    pub PageFaultCount: u32,
    pub PeakWorkingSetSize: usize,
    pub WorkingSetSize: usize,
    pub QuotaPeakPagedPoolUsage: usize,
    pub QuotaPagedPoolUsage: usize,
    pub QuotaPeakNonPagedPoolUsage: usize,
    pub QuotaNonPagedPoolUsage: usize,
    pub PagefileUsage: usize,
    pub PeakPagefileUsage: usize,
    pub PrivateUsage: usize,
}

// ============================================================================
// DXGI & DirectX 12 Structs & Constants
// ============================================================================

pub const DXGI_FORMAT_UNKNOWN: u32 = 0;
pub const DXGI_FORMAT_R32G32B32A32_FLOAT: u32 = 2;
pub const DXGI_FORMAT_R32G32_FLOAT: u32 = 16;
pub const DXGI_FORMAT_R32G32_UINT: u32 = 17;
pub const DXGI_FORMAT_R8G8B8A8_UNORM: u32 = 28;
pub const DXGI_FORMAT_B8G8R8A8_UNORM: u32 = 87;

pub const DXGI_USAGE_RENDER_TARGET_OUTPUT: u32 = 1 << (1 + 4);
pub const DXGI_SCALING_STRETCH: u32 = 0;
pub const DXGI_SWAP_EFFECT_FLIP_DISCARD: u32 = 4;
pub const DXGI_ALPHA_MODE_UNSPECIFIED: u32 = 0;

pub const DXGI_SWAP_CHAIN_FLAG_FRAME_LATENCY_WAITABLE_OBJECT: u32 = 64;
pub const DXGI_SWAP_CHAIN_FLAG_ALLOW_TEARING: u32 = 2048;
pub const DXGI_PRESENT_ALLOW_TEARING: u32 = 0x0000_0200;
pub const DXGI_FEATURE_PRESENT_ALLOW_TEARING: u32 = 0;
pub const DXGI_MWA_NO_ALT_ENTER: u32 = 1 << 1;

pub const D3D_FEATURE_LEVEL_12_0: u32 = 0xc000;
pub const D3D_FEATURE_LEVEL_11_0: u32 = 0xb000;

pub const D3D12_COMMAND_LIST_TYPE_DIRECT: u32 = 0;
pub const D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV: u32 = 0;
pub const D3D12_DESCRIPTOR_HEAP_TYPE_RTV: u32 = 2;
pub const D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE: u32 = 1;

pub const D3D12_HEAP_TYPE_DEFAULT: u32 = 1;
pub const D3D12_HEAP_TYPE_UPLOAD: u32 = 2;
pub const D3D12_HEAP_FLAG_NONE: u32 = 0;
pub const D3D12_HEAP_FLAG_SHARED: u32 = 1;

pub const D3D12_RESOURCE_DIMENSION_BUFFER: u32 = 1;
pub const D3D12_RESOURCE_DIMENSION_TEXTURE2D: u32 = 3;
pub const D3D12_TEXTURE_LAYOUT_UNKNOWN: u32 = 0;
pub const D3D12_TEXTURE_LAYOUT_ROW_MAJOR: u32 = 1;
pub const D3D12_RESOURCE_FLAG_NONE: u32 = 0;
pub const D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS: u32 = 0x20;

pub const D3D12_RESOURCE_STATE_COMMON: u32 = 0;
pub const D3D12_RESOURCE_STATE_PRESENT: u32 = 0;
pub const D3D12_RESOURCE_STATE_RENDER_TARGET: u32 = 0x4;
pub const D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE: u32 = 0x80;
pub const D3D12_RESOURCE_STATE_COPY_DEST: u32 = 0x400;
pub const D3D12_RESOURCE_STATE_GENERIC_READ: u32 = 0x1 | 0x2 | 0x40 | 0x80 | 0x200 | 0x800;

pub const D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING: u32 = 5768;
pub const D3D12_SRV_DIMENSION_TEXTURE2D: u32 = 4;

pub const D3D12_FENCE_FLAG_NONE: u32 = 0;
pub const D3D12_FENCE_FLAG_SHARED: u32 = 1;

pub const D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST: u32 = 4;
pub const D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE: u32 = 3;
pub const D3D_ROOT_SIGNATURE_VERSION_1_0: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DXGI_SAMPLE_DESC {
    pub Count: u32,
    pub Quality: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DXGI_SWAP_CHAIN_DESC1 {
    pub Width: u32,
    pub Height: u32,
    pub Format: u32,
    pub Stereo: BOOL,
    pub SampleDesc: DXGI_SAMPLE_DESC,
    pub BufferUsage: u32,
    pub BufferCount: u32,
    pub Scaling: u32,
    pub SwapEffect: u32,
    pub AlphaMode: u32,
    pub Flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DXGI_ADAPTER_DESC1 {
    pub Description: [u16; 128],
    pub VendorId: u32,
    pub DeviceId: u32,
    pub SubSysId: u32,
    pub Revision: u32,
    pub DedicatedVideoMemory: usize,
    pub DedicatedSystemMemory: usize,
    pub SharedSystemMemory: usize,
    pub AdapterLuid: LUID,
    pub Flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_CPU_DESCRIPTOR_HANDLE {
    pub ptr: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct D3D12_GPU_DESCRIPTOR_HANDLE {
    pub ptr: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_COMMAND_QUEUE_DESC {
    pub Type: u32,
    pub Priority: i32,
    pub Flags: u32,
    pub NodeMask: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_DESCRIPTOR_HEAP_DESC {
    pub Type: u32,
    pub NumDescriptors: u32,
    pub Flags: u32,
    pub NodeMask: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_HEAP_PROPERTIES {
    pub Type: u32,
    pub CPUPageProperty: u32,
    pub MemoryPoolPreference: u32,
    pub CreationNodeMask: u32,
    pub VisibleNodeMask: u32,
}

impl D3D12_HEAP_PROPERTIES {
    pub const fn new(heap_type: u32) -> Self {
        Self {
            Type: heap_type,
            CPUPageProperty: 0,
            MemoryPoolPreference: 0,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_RESOURCE_DESC {
    pub Dimension: u32,
    pub Alignment: u64,
    pub Width: u64,
    pub Height: u32,
    pub DepthOrArraySize: u16,
    pub MipLevels: u16,
    pub Format: u32,
    pub SampleDesc: DXGI_SAMPLE_DESC,
    pub Layout: u32,
    pub Flags: u32,
}

impl D3D12_RESOURCE_DESC {
    pub const fn buffer(size: u64) -> Self {
        Self {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Alignment: 0,
            Width: size,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: DXGI_FORMAT_UNKNOWN,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: D3D12_RESOURCE_FLAG_NONE,
        }
    }

    pub const fn tex2d(width: u32, height: u32, format: u32) -> Self {
        Self {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Alignment: 0,
            Width: width as u64,
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: D3D12_RESOURCE_FLAG_NONE,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_RANGE {
    pub Begin: usize,
    pub End: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_RESOURCE_TRANSITION_BARRIER {
    pub pResource: *mut ID3D12Resource,
    pub Subresource: u32,
    pub StateBefore: u32,
    pub StateAfter: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_RESOURCE_BARRIER {
    pub Type: u32,
    pub Flags: u32,
    pub Transition: D3D12_RESOURCE_TRANSITION_BARRIER,
}

impl D3D12_RESOURCE_BARRIER {
    pub const fn transition(res: *mut ID3D12Resource, before: u32, after: u32) -> Self {
        Self {
            Type: 0, // D3D12_RESOURCE_BARRIER_TYPE_TRANSITION
            Flags: 0,
            Transition: D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: res,
                Subresource: 0xFFFF_FFFF, // D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES
                StateBefore: before,
                StateAfter: after,
            },
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_SUBRESOURCE_FOOTPRINT {
    pub Format: u32,
    pub Width: u32,
    pub Height: u32,
    pub Depth: u32,
    pub RowPitch: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
    pub Offset: u64,
    pub Footprint: D3D12_SUBRESOURCE_FOOTPRINT,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union D3D12_TEXTURE_COPY_UNION {
    pub PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT,
    pub SubresourceIndex: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_TEXTURE_COPY_LOCATION {
    pub pResource: *mut ID3D12Resource,
    pub Type: u32,
    pub u: D3D12_TEXTURE_COPY_UNION,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_TEX2D_SRV {
    pub MostDetailedMip: u32,
    pub MipLevels: u32,
    pub PlaneSlice: u32,
    pub ResourceMinLODClamp: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union D3D12_SRV_UNION {
    pub Texture2D: D3D12_TEX2D_SRV,
    pub _pad: [u64; 3], // Matches 8-byte alignment & 24-byte size of largest union member
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_SHADER_RESOURCE_VIEW_DESC {
    pub Format: u32,
    pub ViewDimension: u32,
    pub Shader4ComponentMapping: u32,
    pub u: D3D12_SRV_UNION,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_VIEWPORT {
    pub TopLeftX: f32,
    pub TopLeftY: f32,
    pub Width: f32,
    pub Height: f32,
    pub MinDepth: f32,
    pub MaxDepth: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_VERTEX_BUFFER_VIEW {
    pub BufferLocation: u64,
    pub SizeInBytes: u32,
    pub StrideInBytes: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_SHADER_BYTECODE {
    pub pShaderBytecode: *const c_void,
    pub BytecodeLength: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_DESCRIPTOR_RANGE {
    pub RangeType: u32,
    pub NumDescriptors: u32,
    pub BaseShaderRegister: u32,
    pub RegisterSpace: u32,
    pub OffsetInDescriptorsFromTableStart: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_ROOT_DESCRIPTOR_TABLE {
    pub NumDescriptorRanges: u32,
    pub pDescriptorRanges: *const D3D12_DESCRIPTOR_RANGE,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_ROOT_CONSTANTS {
    pub ShaderRegister: u32,
    pub RegisterSpace: u32,
    pub Num32BitValues: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union D3D12_ROOT_PARAMETER_UNION {
    pub DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE,
    pub Constants: D3D12_ROOT_CONSTANTS,
    pub _pad: [u64; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_ROOT_PARAMETER {
    pub ParameterType: u32,
    pub u: D3D12_ROOT_PARAMETER_UNION,
    pub ShaderVisibility: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_STATIC_SAMPLER_DESC {
    pub Filter: u32,
    pub AddressU: u32,
    pub AddressV: u32,
    pub AddressW: u32,
    pub MipLODBias: f32,
    pub MaxAnisotropy: u32,
    pub ComparisonFunc: u32,
    pub BorderColor: u32,
    pub MinLOD: f32,
    pub MaxLOD: f32,
    pub ShaderRegister: u32,
    pub RegisterSpace: u32,
    pub ShaderVisibility: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_ROOT_SIGNATURE_DESC {
    pub NumParameters: u32,
    pub pParameters: *const D3D12_ROOT_PARAMETER,
    pub NumStaticSamplers: u32,
    pub pStaticSamplers: *const D3D12_STATIC_SAMPLER_DESC,
    pub Flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_RENDER_TARGET_BLEND_DESC {
    pub BlendEnable: BOOL,
    pub LogicOpEnable: BOOL,
    pub SrcBlend: u32,
    pub DestBlend: u32,
    pub BlendOp: u32,
    pub SrcBlendAlpha: u32,
    pub DestBlendAlpha: u32,
    pub BlendOpAlpha: u32,
    pub LogicOp: u32,
    pub RenderTargetWriteMask: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_BLEND_DESC {
    pub AlphaToCoverageEnable: BOOL,
    pub IndependentBlendEnable: BOOL,
    pub RenderTarget: [D3D12_RENDER_TARGET_BLEND_DESC; 8],
}

impl D3D12_BLEND_DESC {
    pub fn alpha_blend() -> Self {
        let mut s: Self = unsafe { zeroed() };
        s.RenderTarget[0] = D3D12_RENDER_TARGET_BLEND_DESC {
            BlendEnable: 1,
            LogicOpEnable: 0,
            SrcBlend: 5,       // D3D12_BLEND_SRC_ALPHA
            DestBlend: 6,      // D3D12_BLEND_INV_SRC_ALPHA
            BlendOp: 1,        // D3D12_BLEND_OP_ADD
            SrcBlendAlpha: 2,  // D3D12_BLEND_ONE
            DestBlendAlpha: 6, // D3D12_BLEND_INV_SRC_ALPHA
            BlendOpAlpha: 1,   // D3D12_BLEND_OP_ADD
            LogicOp: 4,        // D3D12_LOGIC_OP_NOOP
            RenderTargetWriteMask: 0x0F,
        };
        s
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_RASTERIZER_DESC {
    pub FillMode: u32,
    pub CullMode: u32,
    pub FrontCounterClockwise: BOOL,
    pub DepthBias: i32,
    pub DepthBiasClamp: f32,
    pub SlopeScaledDepthBias: f32,
    pub DepthClipEnable: BOOL,
    pub MultisampleEnable: BOOL,
    pub AntialiasedLineEnable: BOOL,
    pub ForcedSampleCount: u32,
    pub ConservativeRaster: u32,
}

impl D3D12_RASTERIZER_DESC {
    pub const fn default_no_cull() -> Self {
        Self {
            FillMode: 3, // SOLID
            CullMode: 1, // NONE
            FrontCounterClockwise: 0,
            DepthBias: 0,
            DepthBiasClamp: 0.0,
            SlopeScaledDepthBias: 0.0,
            DepthClipEnable: 1,
            MultisampleEnable: 0,
            AntialiasedLineEnable: 0,
            ForcedSampleCount: 0,
            ConservativeRaster: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_DEPTH_STENCILOP_DESC {
    pub StencilFailOp: u32,
    pub StencilDepthFailOp: u32,
    pub StencilPassOp: u32,
    pub StencilFunc: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_DEPTH_STENCIL_DESC {
    pub DepthEnable: BOOL,
    pub DepthWriteMask: u32,
    pub DepthFunc: u32,
    pub StencilEnable: BOOL,
    pub StencilReadMask: u8,
    pub StencilWriteMask: u8,
    pub FrontFace: D3D12_DEPTH_STENCILOP_DESC,
    pub BackFace: D3D12_DEPTH_STENCILOP_DESC,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_STREAM_OUTPUT_DESC {
    pub pSODeclaration: *const c_void,
    pub NumEntries: u32,
    pub pBufferStrides: *const u32,
    pub NumStrides: u32,
    pub RasterizedStream: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_INPUT_ELEMENT_DESC {
    pub SemanticName: *const u8,
    pub SemanticIndex: u32,
    pub Format: u32,
    pub InputSlot: u32,
    pub AlignedByteOffset: u32,
    pub InputSlotClass: u32,
    pub InstanceDataStepRate: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_INPUT_LAYOUT_DESC {
    pub pInputElementDescs: *const D3D12_INPUT_ELEMENT_DESC,
    pub NumElements: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_CACHED_PIPELINE_STATE {
    pub pCachedBlob: *const c_void,
    pub CachedBlobSizeInBytes: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct D3D12_GRAPHICS_PIPELINE_STATE_DESC {
    pub pRootSignature: *mut ID3D12RootSignature,
    pub VS: D3D12_SHADER_BYTECODE,
    pub PS: D3D12_SHADER_BYTECODE,
    pub DS: D3D12_SHADER_BYTECODE,
    pub HS: D3D12_SHADER_BYTECODE,
    pub GS: D3D12_SHADER_BYTECODE,
    pub StreamOutput: D3D12_STREAM_OUTPUT_DESC,
    pub BlendState: D3D12_BLEND_DESC,
    pub SampleMask: u32,
    pub RasterizerState: D3D12_RASTERIZER_DESC,
    pub DepthStencilState: D3D12_DEPTH_STENCIL_DESC,
    pub InputLayout: D3D12_INPUT_LAYOUT_DESC,
    pub IBStripCutValue: u32,
    pub PrimitiveTopologyType: u32,
    pub NumRenderTargets: u32,
    pub RTVFormats: [u32; 8],
    pub DSVFormat: u32,
    pub SampleDesc: DXGI_SAMPLE_DESC,
    pub NodeMask: u32,
    pub CachedPSO: D3D12_CACHED_PIPELINE_STATE,
    pub Flags: u32,
}

// ============================================================================
// COM Interface Vtables (Verified Slot-by-Slot)
// ============================================================================

#[repr(C)]
pub struct IDXGIAdapter1Vtbl {
    pub unk: IUnknownVtbl,
    pub _dxgi_obj: [usize; 4],
    pub EnumOutputs: usize,
    pub GetDesc: usize,
    pub CheckInterfaceSupport: usize,
    pub GetDesc1: unsafe extern "system" fn(*mut IDXGIAdapter1, *mut DXGI_ADAPTER_DESC1) -> HRESULT,
}

#[repr(C)]
pub struct IDXGIAdapter1 {
    pub lpVtbl: *const IDXGIAdapter1Vtbl,
}

#[repr(C)]
pub struct IDXGIFactory5Vtbl {
    pub unk: IUnknownVtbl,
    pub _dxgi_obj: [usize; 4],
    pub EnumAdapters: usize,
    pub MakeWindowAssociation: unsafe extern "system" fn(*mut IDXGIFactory5, HWND, u32) -> HRESULT,
    pub GetWindowAssociation: usize,
    pub CreateSwapChain: usize,
    pub CreateSoftwareAdapter: usize,
    pub EnumAdapters1:
        unsafe extern "system" fn(*mut IDXGIFactory5, u32, *mut *mut IDXGIAdapter1) -> HRESULT,
    pub IsCurrent: usize,
    pub IsWindowedStereoEnabled: usize,
    pub CreateSwapChainForHwnd: unsafe extern "system" fn(
        *mut IDXGIFactory5,
        *mut c_void,
        HWND,
        *const DXGI_SWAP_CHAIN_DESC1,
        *const c_void,
        *mut c_void,
        *mut *mut c_void,
    ) -> HRESULT,
    pub _factory2_rest: [usize; 9],
    pub GetCreationFlags: usize,
    pub EnumAdapterByLuid: usize,
    pub EnumWarpAdapter:
        unsafe extern "system" fn(*mut IDXGIFactory5, *const GUID, *mut *mut c_void) -> HRESULT,
    pub CheckFeatureSupport:
        unsafe extern "system" fn(*mut IDXGIFactory5, u32, *mut c_void, u32) -> HRESULT,
}

#[repr(C)]
pub struct IDXGIFactory5 {
    pub lpVtbl: *const IDXGIFactory5Vtbl,
}

#[repr(C)]
pub struct IDXGISwapChain3Vtbl {
    pub unk: IUnknownVtbl,
    pub _dxgi_obj: [usize; 4],
    pub GetDevice: usize,
    pub Present: unsafe extern "system" fn(*mut IDXGISwapChain3, u32, u32) -> HRESULT,
    pub GetBuffer: unsafe extern "system" fn(
        *mut IDXGISwapChain3,
        u32,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    pub SetFullscreenState: usize,
    pub GetFullscreenState: usize,
    pub GetDesc: usize,
    pub ResizeBuffers:
        unsafe extern "system" fn(*mut IDXGISwapChain3, u32, u32, u32, u32, u32) -> HRESULT,
    pub ResizeTarget: usize,
    pub GetContainingOutput: usize,
    pub GetFrameStatistics: usize,
    pub GetLastPresentCount: usize,
    pub _sc1_rest: [usize; 11],
    pub SetSourceSize: usize,
    pub GetSourceSize: usize,
    pub SetMaximumFrameLatency: unsafe extern "system" fn(*mut IDXGISwapChain3, u32) -> HRESULT,
    pub GetMaximumFrameLatency: usize,
    pub GetFrameLatencyWaitableObject: unsafe extern "system" fn(*mut IDXGISwapChain3) -> HANDLE,
    pub SetMatrixTransform: usize,
    pub GetMatrixTransform: usize,
    pub GetCurrentBackBufferIndex: unsafe extern "system" fn(*mut IDXGISwapChain3) -> u32,
}

#[repr(C)]
pub struct IDXGISwapChain3 {
    pub lpVtbl: *const IDXGISwapChain3Vtbl,
}

#[repr(C)]
pub struct ID3D12ResourceVtbl {
    pub unk: IUnknownVtbl,
    pub _d3d12_obj: [usize; 4],
    pub GetDevice: usize,
    pub Map: unsafe extern "system" fn(
        *mut ID3D12Resource,
        u32,
        *const D3D12_RANGE,
        *mut *mut c_void,
    ) -> HRESULT,
    pub Unmap: unsafe extern "system" fn(*mut ID3D12Resource, u32, *const D3D12_RANGE),
    pub GetDesc: unsafe extern "system" fn(
        *mut ID3D12Resource,
        *mut D3D12_RESOURCE_DESC,
    ) -> *mut D3D12_RESOURCE_DESC,
    pub GetGPUVirtualAddress: unsafe extern "system" fn(*mut ID3D12Resource) -> u64,
}

#[repr(C)]
pub struct ID3D12Resource {
    pub lpVtbl: *const ID3D12ResourceVtbl,
}

#[repr(C)]
pub struct ID3D12DescriptorHeapVtbl {
    pub unk: IUnknownVtbl,
    pub _d3d12_obj: [usize; 4],
    pub GetDevice: usize,
    pub GetDesc: usize,
    pub GetCPUDescriptorHandleForHeapStart: unsafe extern "system" fn(
        *mut ID3D12DescriptorHeap,
        *mut D3D12_CPU_DESCRIPTOR_HANDLE,
    ) -> *mut D3D12_CPU_DESCRIPTOR_HANDLE,
    pub GetGPUDescriptorHandleForHeapStart: unsafe extern "system" fn(
        *mut ID3D12DescriptorHeap,
        *mut D3D12_GPU_DESCRIPTOR_HANDLE,
    ) -> *mut D3D12_GPU_DESCRIPTOR_HANDLE,
}

#[repr(C)]
pub struct ID3D12DescriptorHeap {
    pub lpVtbl: *const ID3D12DescriptorHeapVtbl,
}

#[repr(C)]
pub struct ID3D12CommandAllocatorVtbl {
    pub unk: IUnknownVtbl,
    pub _d3d12_obj: [usize; 4],
    pub GetDevice: usize,
    pub Reset: unsafe extern "system" fn(*mut ID3D12CommandAllocator) -> HRESULT,
}

#[repr(C)]
pub struct ID3D12CommandAllocator {
    pub lpVtbl: *const ID3D12CommandAllocatorVtbl,
}

#[repr(C)]
pub struct ID3D12CommandQueueVtbl {
    pub unk: IUnknownVtbl,
    pub _d3d12_obj: [usize; 4],
    pub GetDevice: usize,
    pub UpdateTileMappings: usize,
    pub CopyTileMappings: usize,
    pub ExecuteCommandLists:
        unsafe extern "system" fn(*mut ID3D12CommandQueue, u32, *const *mut ID3D12GraphicsCommandList),
    pub SetMarker: usize,
    pub BeginEvent: usize,
    pub EndEvent: usize,
    pub Signal: unsafe extern "system" fn(*mut ID3D12CommandQueue, *mut ID3D12Fence, u64) -> HRESULT,
    pub Wait: unsafe extern "system" fn(*mut ID3D12CommandQueue, *mut ID3D12Fence, u64) -> HRESULT,
}

#[repr(C)]
pub struct ID3D12CommandQueue {
    pub lpVtbl: *const ID3D12CommandQueueVtbl,
}

#[repr(C)]
pub struct ID3D12FenceVtbl {
    pub unk: IUnknownVtbl,
    pub _d3d12_obj: [usize; 4],
    pub GetDevice: usize,
    pub GetCompletedValue: unsafe extern "system" fn(*mut ID3D12Fence) -> u64,
    pub SetEventOnCompletion:
        unsafe extern "system" fn(*mut ID3D12Fence, u64, HANDLE) -> HRESULT,
    pub Signal: unsafe extern "system" fn(*mut ID3D12Fence, u64) -> HRESULT,
}

#[repr(C)]
pub struct ID3D12Fence {
    pub lpVtbl: *const ID3D12FenceVtbl,
}

#[repr(C)]
pub struct ID3D12RootSignature {
    pub lpVtbl: *const IUnknownVtbl,
}

#[repr(C)]
pub struct ID3D12PipelineState {
    pub lpVtbl: *const IUnknownVtbl,
}

#[repr(C)]
pub struct ID3D12GraphicsCommandListVtbl {
    pub unk: IUnknownVtbl,
    pub _d3d12_obj: [usize; 4],
    pub GetDevice: usize,
    pub GetType: usize,
    pub Close: unsafe extern "system" fn(*mut ID3D12GraphicsCommandList) -> HRESULT,
    pub Reset: unsafe extern "system" fn(
        *mut ID3D12GraphicsCommandList,
        *mut ID3D12CommandAllocator,
        *mut ID3D12PipelineState,
    ) -> HRESULT,
    pub ClearState: usize,
    pub DrawInstanced: unsafe extern "system" fn(*mut ID3D12GraphicsCommandList, u32, u32, u32, u32),
    pub DrawIndexedInstanced: usize,
    pub Dispatch: usize,
    pub CopyBufferRegion: usize,
    pub CopyTextureRegion: unsafe extern "system" fn(
        *mut ID3D12GraphicsCommandList,
        *const D3D12_TEXTURE_COPY_LOCATION,
        u32,
        u32,
        u32,
        *const D3D12_TEXTURE_COPY_LOCATION,
        *const c_void,
    ),
    pub CopyResource: usize,
    pub CopyTiles: usize,
    pub ResolveSubresource: usize,
    pub IASetPrimitiveTopology: unsafe extern "system" fn(*mut ID3D12GraphicsCommandList, u32),
    pub RSSetViewports:
        unsafe extern "system" fn(*mut ID3D12GraphicsCommandList, u32, *const D3D12_VIEWPORT),
    pub RSSetScissorRects:
        unsafe extern "system" fn(*mut ID3D12GraphicsCommandList, u32, *const RECT),
    pub OMSetBlendFactor: usize,
    pub OMSetStencilRef: usize,
    pub SetPipelineState:
        unsafe extern "system" fn(*mut ID3D12GraphicsCommandList, *mut ID3D12PipelineState),
    pub ResourceBarrier: unsafe extern "system" fn(
        *mut ID3D12GraphicsCommandList,
        u32,
        *const D3D12_RESOURCE_BARRIER,
    ),
    pub ExecuteBundle: usize,
    pub SetDescriptorHeaps: unsafe extern "system" fn(
        *mut ID3D12GraphicsCommandList,
        u32,
        *const *mut ID3D12DescriptorHeap,
    ),
    pub SetComputeRootSignature: usize,
    pub SetGraphicsRootSignature:
        unsafe extern "system" fn(*mut ID3D12GraphicsCommandList, *mut ID3D12RootSignature),
    pub SetComputeRootDescriptorTable: usize,
    pub SetGraphicsRootDescriptorTable: unsafe extern "system" fn(
        *mut ID3D12GraphicsCommandList,
        u32,
        D3D12_GPU_DESCRIPTOR_HANDLE,
    ),
    pub SetComputeRoot32BitConstant: usize,
    pub SetGraphicsRoot32BitConstant: usize,
    pub SetComputeRoot32BitConstants: usize,
    pub SetGraphicsRoot32BitConstants:
        unsafe extern "system" fn(*mut ID3D12GraphicsCommandList, u32, u32, *const c_void, u32),
    pub SetComputeRootConstantBufferView: usize,
    pub SetGraphicsRootConstantBufferView: usize,
    pub SetComputeRootShaderResourceView: usize,
    pub SetGraphicsRootShaderResourceView: usize,
    pub SetComputeRootUnorderedAccessView: usize,
    pub SetGraphicsRootUnorderedAccessView: usize,
    pub IASetIndexBuffer: usize,
    pub IASetVertexBuffers: unsafe extern "system" fn(
        *mut ID3D12GraphicsCommandList,
        u32,
        u32,
        *const D3D12_VERTEX_BUFFER_VIEW,
    ),
    pub SOSetTargets: usize,
    pub OMSetRenderTargets: unsafe extern "system" fn(
        *mut ID3D12GraphicsCommandList,
        u32,
        *const D3D12_CPU_DESCRIPTOR_HANDLE,
        BOOL,
        *const D3D12_CPU_DESCRIPTOR_HANDLE,
    ),
    pub ClearDepthStencilView: usize,
    pub ClearRenderTargetView: unsafe extern "system" fn(
        *mut ID3D12GraphicsCommandList,
        D3D12_CPU_DESCRIPTOR_HANDLE,
        *const [f32; 4],
        u32,
        *const RECT,
    ),
}

#[repr(C)]
pub struct ID3D12GraphicsCommandList {
    pub lpVtbl: *const ID3D12GraphicsCommandListVtbl,
}

#[repr(C)]
pub struct ID3D12DeviceVtbl {
    pub unk: IUnknownVtbl,
    pub _d3d12_obj: [usize; 4],
    pub GetNodeCount: usize,
    pub CreateCommandQueue: unsafe extern "system" fn(
        *mut ID3D12Device,
        *const D3D12_COMMAND_QUEUE_DESC,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateCommandAllocator: unsafe extern "system" fn(
        *mut ID3D12Device,
        u32,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateGraphicsPipelineState: unsafe extern "system" fn(
        *mut ID3D12Device,
        *const D3D12_GRAPHICS_PIPELINE_STATE_DESC,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateComputePipelineState: usize,
    pub CreateCommandList: unsafe extern "system" fn(
        *mut ID3D12Device,
        u32,
        u32,
        *mut ID3D12CommandAllocator,
        *mut ID3D12PipelineState,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CheckFeatureSupport: usize,
    pub CreateDescriptorHeap: unsafe extern "system" fn(
        *mut ID3D12Device,
        *const D3D12_DESCRIPTOR_HEAP_DESC,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    pub GetDescriptorHandleIncrementSize:
        unsafe extern "system" fn(*mut ID3D12Device, u32) -> u32,
    pub CreateRootSignature: unsafe extern "system" fn(
        *mut ID3D12Device,
        u32,
        *const c_void,
        usize,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateConstantBufferView: usize,
    pub CreateShaderResourceView: unsafe extern "system" fn(
        *mut ID3D12Device,
        *mut ID3D12Resource,
        *const D3D12_SHADER_RESOURCE_VIEW_DESC,
        D3D12_CPU_DESCRIPTOR_HANDLE,
    ),
    pub CreateUnorderedAccessView: usize,
    pub CreateRenderTargetView: unsafe extern "system" fn(
        *mut ID3D12Device,
        *mut ID3D12Resource,
        *const c_void,
        D3D12_CPU_DESCRIPTOR_HANDLE,
    ),
    pub CreateDepthStencilView: usize,
    pub CreateSampler: usize,
    pub CopyDescriptors: usize,
    pub CopyDescriptorsSimple: usize,
    pub GetResourceAllocationInfo: usize,
    pub GetCustomHeapProperties: usize,
    pub CreateCommittedResource: unsafe extern "system" fn(
        *mut ID3D12Device,
        *const D3D12_HEAP_PROPERTIES,
        u32,
        *const D3D12_RESOURCE_DESC,
        u32,
        *const c_void,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    pub CreateHeap: usize,
    pub CreatePlacedResource: usize,
    pub CreateReservedResource: usize,
    pub CreateSharedHandle: unsafe extern "system" fn(
        *mut ID3D12Device,
        *mut c_void,
        *const c_void,
        u32,
        *const u16,
        *mut HANDLE,
    ) -> HRESULT,
    pub OpenSharedHandle: unsafe extern "system" fn(
        *mut ID3D12Device,
        HANDLE,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
    pub OpenSharedHandleByName: usize,
    pub MakeResident: usize,
    pub Evict: usize,
    pub CreateFence: unsafe extern "system" fn(
        *mut ID3D12Device,
        u64,
        u32,
        *const GUID,
        *mut *mut c_void,
    ) -> HRESULT,
}

#[repr(C)]
pub struct ID3D12Device {
    pub lpVtbl: *const ID3D12DeviceVtbl,
}

// ============================================================================
// External DLL Entry Points
// ============================================================================

#[link(name = "d3d12")]
extern "system" {
    pub fn D3D12CreateDevice(
        pAdapter: *mut c_void,
        MinimumFeatureLevel: u32,
        riid: *const GUID,
        ppDevice: *mut *mut c_void,
    ) -> HRESULT;

    pub fn D3D12SerializeRootSignature(
        pRootSignature: *const D3D12_ROOT_SIGNATURE_DESC,
        Version: u32,
        ppBlob: *mut *mut ID3DBlob,
        ppErrorBlob: *mut *mut ID3DBlob,
    ) -> HRESULT;
}

#[link(name = "dxgi")]
extern "system" {
    pub fn CreateDXGIFactory2(
        Flags: u32,
        riid: *const GUID,
        ppFactory: *mut *mut c_void,
    ) -> HRESULT;
}

#[link(name = "d3dcompiler_47")]
extern "system" {
    pub fn D3DCompile(
        pSrcData: *const c_void,
        SrcDataSize: usize,
        pSourceName: *const u8,
        pDefines: *const c_void,
        pInclude: *mut c_void,
        pEntrypoint: *const u8,
        pTarget: *const u8,
        Flags1: u32,
        Flags2: u32,
        ppCode: *mut *mut ID3DBlob,
        ppErrorMsgs: *mut *mut ID3DBlob,
    ) -> HRESULT;
}

#[link(name = "dwmapi")]
extern "system" {
    pub fn DwmSetWindowAttribute(
        hwnd: HWND,
        dwAttribute: u32,
        pvAttribute: *const c_void,
        cbAttribute: u32,
    ) -> HRESULT;
}

#[link(name = "user32")]
extern "system" {
    pub fn RegisterClassExW(lpwcx: *const WNDCLASSEXW) -> ATOM;
    pub fn CreateWindowExW(
        dwExStyle: u32,
        lpClassName: *const u16,
        lpWindowName: *const u16,
        dwStyle: u32,
        X: i32,
        Y: i32,
        nWidth: i32,
        nHeight: i32,
        hWndParent: HWND,
        hMenu: HMENU,
        hInstance: HINSTANCE,
        lpParam: *mut c_void,
    ) -> HWND;
    pub fn DestroyWindow(hWnd: HWND) -> BOOL;
    pub fn DefWindowProcW(hWnd: HWND, Msg: u32, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
    pub fn ShowWindow(hWnd: HWND, nCmdShow: i32) -> BOOL;
    pub fn PeekMessageW(
        lpMsg: *mut MSG,
        hWnd: HWND,
        wMsgFilterMin: u32,
        wMsgFilterMax: u32,
        wRemoveMsg: u32,
    ) -> BOOL;
    pub fn TranslateMessage(lpMsg: *const MSG) -> BOOL;
    pub fn DispatchMessageW(lpMsg: *const MSG) -> LRESULT;
    pub fn PostQuitMessage(nExitCode: i32);
    pub fn LoadCursorW(hInstance: HINSTANCE, lpCursorName: *const u16) -> HCURSOR;
    pub fn GetClientRect(hWnd: HWND, lpRect: *mut RECT) -> BOOL;
    pub fn GetWindowRect(hWnd: HWND, lpRect: *mut RECT) -> BOOL;
    pub fn ScreenToClient(hWnd: HWND, lpPoint: *mut POINT) -> BOOL;
    pub fn GetWindowLongPtrW(hWnd: HWND, nIndex: i32) -> isize;
    pub fn SetWindowLongPtrW(hWnd: HWND, nIndex: i32, dwNewLong: isize) -> isize;
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetModuleHandleW(lpModuleName: *const u16) -> HMODULE;
    pub fn GetCurrentProcess() -> HANDLE;
    pub fn CreateEventW(
        lpEventAttributes: *mut c_void,
        bManualReset: BOOL,
        bInitialState: BOOL,
        lpName: *const u16,
    ) -> HANDLE;
    pub fn WaitForSingleObject(hHandle: HANDLE, dwMilliseconds: u32) -> u32;
    pub fn CloseHandle(hObject: HANDLE) -> BOOL;
    pub fn QueryPerformanceCounter(lpPerformanceCount: *mut i64) -> BOOL;
    pub fn QueryPerformanceFrequency(lpFrequency: *mut i64) -> BOOL;
    pub fn K32GetProcessMemoryInfo(
        Process: HANDLE,
        ppsmemCounters: *mut PROCESS_MEMORY_COUNTERS_EX,
        cb: u32,
    ) -> BOOL;
}

pub fn utf16_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(core::iter::once(0)).collect()
}

/// Queries physical RAM WorkingSet and PrivateUsage in MiB via `K32GetProcessMemoryInfo`.
pub fn query_process_memory_mib() -> (f64, f64) {
    let mut pmc = PROCESS_MEMORY_COUNTERS_EX {
        cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut pmc,
            size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    };
    if ok != 0 {
        let ws = pmc.WorkingSetSize as f64 / (1024.0 * 1024.0);
        let priv_bytes = pmc.PrivateUsage as f64 / (1024.0 * 1024.0);
        (ws, priv_bytes)
    } else {
        (0.0, 0.0)
    }
}
