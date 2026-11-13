//! Stage 3: WHPX Micro-VM Controller, 2 MB Large Page Allocator, Hybrid P-Core Pinning,
//! Async `virtio-blk` IOCP Engine, and Sub-ms Lock-Free `RawInput` + Smart Keymapper.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use core::ffi::c_void;
use core::mem::{size_of, zeroed};
use core::ptr::{null, null_mut};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::stage1_memory::{
    ev_code, ev_type, GPA_IVSHMEM_BAR2_BASE, GPA_LOW_RAM_BASE, InputSpscRing, LARGE_PAGE_SIZE,
    VirtioInputEvent,
};
use crate::win_dx12::{
    utf16_null, BOOL, CloseHandle, GetCurrentProcess, HANDLE, HMODULE, HRESULT, HWND, LPARAM,
    QueryPerformanceCounter, QueryPerformanceFrequency, succeeded,
};

const MEM_COMMIT: u32 = 0x0000_1000;
const MEM_RESERVE: u32 = 0x0000_2000;
const MEM_RELEASE: u32 = 0x0000_8000;
const MEM_LARGE_PAGES: u32 = 0x2000_0000;
const PAGE_READWRITE: u32 = 0x04;

const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
const TOKEN_QUERY: u32 = 0x0008;
const SE_PRIVILEGE_ENABLED: u32 = 0x0000_0002;

const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const FILE_SHARE_READ: u32 = 0x0000_0001;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const OPEN_EXISTING: u32 = 3;
const CREATE_ALWAYS: u32 = 2;
const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
const INVALID_HANDLE_VALUE: HANDLE = (-1isize) as HANDLE;
const ERROR_IO_PENDING: u32 = 997;

const RID_INPUT: u32 = 0x1000_0003;
const RIM_TYPEMOUSE: u32 = 0;
const RIM_TYPEKEYBOARD: u32 = 1;
const RI_KEY_BREAK: u16 = 0x0001;
const RI_MOUSE_LEFT_BUTTON_DOWN: u16 = 0x0001;
const RI_MOUSE_LEFT_BUTTON_UP: u16 = 0x0002;
const RI_MOUSE_RIGHT_BUTTON_DOWN: u16 = 0x0004;
const RI_MOUSE_RIGHT_BUTTON_UP: u16 = 0x0008;

#[repr(C)]
#[derive(Clone, Copy)]
struct LUID_AND_ATTRIBUTES {
    Luid: crate::win_dx12::LUID,
    Attributes: u32,
}

#[repr(C)]
struct TOKEN_PRIVILEGES {
    PrivilegeCount: u32,
    Privileges: [LUID_AND_ATTRIBUTES; 1],
}

#[repr(C)]
struct SYSTEM_CPU_SET_INFORMATION {
    Size: u32,
    Type: i32,
    Id: u32,
    Group: u16,
    LogicalProcessorIndex: u8,
    CoreIndex: u8,
    LastLevelCacheIndex: u8,
    NumaNodeIndex: u8,
    EfficiencyClass: u8,
    AllFlags: u8,
    Reserved: u32,
    AllocationTag: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RAWINPUTDEVICE {
    usUsagePage: u16,
    usUsage: u16,
    dwFlags: u32,
    hwndTarget: HWND,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RAWINPUTHEADER {
    dwType: u32,
    dwSize: u32,
    hDevice: HANDLE,
    wParam: usize,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RAWMOUSE {
    usFlags: u16,
    usButtonFlags: u16,
    usButtonData: u16,
    ulRawButtons: u32,
    lLastX: i32,
    lLastY: i32,
    ulExtraInformation: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RAWKEYBOARD {
    MakeCode: u16,
    Flags: u16,
    Reserved: u16,
    VKey: u16,
    Message: u32,
    ExtraInformation: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
union RAWINPUT_DATA {
    mouse: RAWMOUSE,
    keyboard: RAWKEYBOARD,
    _pad: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RAWINPUT {
    header: RAWINPUTHEADER,
    data: RAWINPUT_DATA,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct OVERLAPPED {
    pub Internal: usize,
    pub InternalHigh: usize,
    pub Offset: u32,
    pub OffsetHigh: u32,
    pub hEvent: HANDLE,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct OVERLAPPED_ENTRY {
    pub lpCompletionKey: usize,
    pub lpOverlapped: *mut OVERLAPPED,
    pub Internal: usize,
    pub dwNumberOfBytesTransferred: u32,
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(ProcessHandle: HANDLE, DesiredAccess: u32, TokenHandle: *mut HANDLE) -> BOOL;
    fn LookupPrivilegeValueW(
        lpSystemName: *const u16,
        lpName: *const u16,
        lpLuid: *mut crate::win_dx12::LUID,
    ) -> BOOL;
    fn AdjustTokenPrivileges(
        TokenHandle: HANDLE,
        DisableAllPrivileges: BOOL,
        NewState: *const TOKEN_PRIVILEGES,
        BufferLength: u32,
        PreviousState: *mut c_void,
        ReturnLength: *mut u32,
    ) -> BOOL;
}

#[link(name = "user32")]
extern "system" {
    fn RegisterRawInputDevices(
        pRawInputDevices: *const RAWINPUTDEVICE,
        uiNumDevices: u32,
        cbSize: u32,
    ) -> BOOL;
    fn GetRawInputData(
        hRawInput: HANDLE,
        uiCommand: u32,
        pData: *mut c_void,
        pcbSize: *mut u32,
        cbSizeHeader: u32,
    ) -> u32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetLastError() -> u32;
    fn GetCurrentThread() -> HANDLE;
    fn GetLargePageMinimum() -> usize;
    fn VirtualAlloc(lpAddress: *mut c_void, dwSize: usize, flAllocationType: u32, flProtect: u32) -> *mut c_void;
    fn VirtualFree(lpAddress: *mut c_void, dwSize: usize, dwFreeType: u32) -> BOOL;
    fn GetSystemCpuSetInformation(
        Information: *mut SYSTEM_CPU_SET_INFORMATION,
        BufferLength: u32,
        ReturnedLength: *mut u32,
        Process: HANDLE,
        Flags: u32,
    ) -> BOOL;
    fn SetThreadSelectedCpuSets(Thread: HANDLE, CpuSetIds: *const u32, CpuSetIdCount: u32) -> BOOL;
    fn SetThreadAffinityMask(hThread: HANDLE, dwThreadAffinityMask: usize) -> usize;
    fn LoadLibraryW(lpLibFileName: *const u16) -> HMODULE;
    fn FreeLibrary(hLibModule: HMODULE) -> BOOL;
    fn GetProcAddress(hModule: HMODULE, lpProcName: *const u8) -> *const c_void;
    fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: u32,
        dwShareMode: u32,
        lpSecurityAttributes: *const c_void,
        dwCreationDisposition: u32,
        dwFlagsAndAttributes: u32,
        hTemplateFile: HANDLE,
    ) -> HANDLE;
    fn ReadFile(
        hFile: HANDLE,
        lpBuffer: *mut c_void,
        nNumberOfBytesToRead: u32,
        lpNumberOfBytesRead: *mut u32,
        lpOverlapped: *mut OVERLAPPED,
    ) -> BOOL;
    fn WriteFile(
        hFile: HANDLE,
        lpBuffer: *const c_void,
        nNumberOfBytesToWrite: u32,
        lpNumberOfBytesWritten: *mut u32,
        lpOverlapped: *mut OVERLAPPED,
    ) -> BOOL;
    fn CreateIoCompletionPort(
        FileHandle: HANDLE,
        ExistingCompletionPort: HANDLE,
        CompletionKey: usize,
        NumberOfConcurrentThreads: u32,
    ) -> HANDLE;
    fn GetQueuedCompletionStatusEx(
        CompletionPort: HANDLE,
        lpCompletionPortEntries: *mut OVERLAPPED_ENTRY,
        ulCount: u32,
        ulNumEntriesRemoved: *mut u32,
        dwMilliseconds: u32,
        fAlertable: BOOL,
    ) -> BOOL;
}

// ============================================================================
// 1. 2 MB Large Page Memory Allocator (Zero EPT/SLAT TLB Miss Backing)
// ============================================================================

/// Owns a 2 MB-aligned guest RAM or shared IPC memory arena backed by Windows
/// `MEM_LARGE_PAGES` when `SeLockMemoryPrivilege` is granted, or aligned 4 KB fallback.
pub struct LargePageArena {
    raw_base: *mut u8,
    aligned_ptr: *mut u8,
    size_bytes: usize,
    pub large_pages_active: bool,
    pub large_page_min: usize,
}

impl LargePageArena {
    pub fn allocate(requested_bytes: usize) -> Result<Self, String> {
        let lp_min = unsafe { GetLargePageMinimum() }.max(LARGE_PAGE_SIZE);
        let size_bytes = ((requested_bytes + lp_min - 1) / lp_min) * lp_min;

        let priv_ok = Self::enable_lock_memory_privilege();
        if priv_ok {
            let ptr = unsafe {
                VirtualAlloc(
                    null_mut(),
                    size_bytes,
                    MEM_RESERVE | MEM_COMMIT | MEM_LARGE_PAGES,
                    PAGE_READWRITE,
                )
            } as *mut u8;
            if !ptr.is_null() {
                return Ok(Self {
                    raw_base: ptr,
                    aligned_ptr: ptr,
                    size_bytes,
                    large_pages_active: true,
                    large_page_min: lp_min,
                });
            }
        }

        // Standard aligned fallback (over-allocates by 2 MB to guarantee exact 2 MB alignment).
        let raw = unsafe {
            VirtualAlloc(
                null_mut(),
                size_bytes + lp_min,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            )
        } as *mut u8;
        if raw.is_null() {
            return Err("VirtualAlloc failed for guest memory arena".to_string());
        }
        let addr = raw as usize;
        let aligned = ((addr + lp_min - 1) & !(lp_min - 1)) as *mut u8;
        Ok(Self {
            raw_base: raw,
            aligned_ptr: aligned,
            size_bytes,
            large_pages_active: false,
            large_page_min: lp_min,
        })
    }

    fn enable_lock_memory_privilege() -> bool {
        let mut token: HANDLE = null_mut();
        if unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &mut token,
            )
        } == 0
        {
            return false;
        }
        let name = utf16_null("SeLockMemoryPrivilege");
        let mut luid = crate::win_dx12::LUID::default();
        let ok_lookup = unsafe { LookupPrivilegeValueW(null(), name.as_ptr(), &mut luid) };
        if ok_lookup == 0 {
            unsafe { CloseHandle(token) };
            return false;
        }
        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let ok_adj = unsafe {
            AdjustTokenPrivileges(token, 0, &tp, 0, null_mut(), null_mut())
        };
        let err = unsafe { GetLastError() };
        unsafe { CloseHandle(token) };
        ok_adj != 0 && err == 0
    }

    #[inline]
    pub fn as_mut_ptr(&self) -> *mut u8 {
        self.aligned_ptr
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.size_bytes
    }
}

impl Drop for LargePageArena {
    fn drop(&mut self) {
        if !self.raw_base.is_null() {
            unsafe { VirtualFree(self.raw_base.cast(), 0, MEM_RELEASE) };
            self.raw_base = null_mut();
            self.aligned_ptr = null_mut();
        }
    }
}

// ============================================================================
// 2. Hybrid CPU Topology & Strict P-Core Pinning
// ============================================================================

#[derive(Clone, Debug)]
pub struct CpuCoreInfo {
    pub cpu_set_id: u32,
    pub logical_idx: u8,
    pub core_idx: u8,
    pub efficiency_class: u8,
}

/// Discovers Performance (P) vs Efficiency (E) cores via `GetSystemCpuSetInformation`
/// and binds vCPU, render, and audio threads strictly to P-cores.
pub struct CpuPinningManager {
    pub p_cores: Vec<CpuCoreInfo>,
    pub e_cores: Vec<CpuCoreInfo>,
    pub max_efficiency_class: u8,
}

impl CpuPinningManager {
    pub fn detect() -> Self {
        let mut needed = 0u32;
        unsafe {
            GetSystemCpuSetInformation(null_mut(), 0, &mut needed, GetCurrentProcess(), 0);
        }
        if needed == 0 {
            return Self {
                p_cores: Vec::new(),
                e_cores: Vec::new(),
                max_efficiency_class: 0,
            };
        }

        let mut buf = vec![0u8; needed as usize];
        let ok = unsafe {
            GetSystemCpuSetInformation(
                buf.as_mut_ptr().cast(),
                needed,
                &mut needed,
                GetCurrentProcess(),
                0,
            )
        };
        if ok == 0 {
            return Self {
                p_cores: Vec::new(),
                e_cores: Vec::new(),
                max_efficiency_class: 0,
            };
        }

        let mut all = Vec::new();
        let mut offset = 0usize;
        while offset + size_of::<SYSTEM_CPU_SET_INFORMATION>() <= needed as usize {
            let entry = unsafe { &*(buf.as_ptr().add(offset) as *const SYSTEM_CPU_SET_INFORMATION) };
            if entry.Size == 0 {
                break;
            }
            if entry.Type == 0 {
                all.push(CpuCoreInfo {
                    cpu_set_id: entry.Id,
                    logical_idx: entry.LogicalProcessorIndex,
                    core_idx: entry.CoreIndex,
                    efficiency_class: entry.EfficiencyClass,
                });
            }
            offset += entry.Size as usize;
        }

        let max_eff = all.iter().map(|c| c.efficiency_class).max().unwrap_or(0);
        let mut p_cores = Vec::new();
        let mut e_cores = Vec::new();
        for c in all {
            if c.efficiency_class == max_eff {
                p_cores.push(c);
            } else {
                e_cores.push(c);
            }
        }

        Self {
            p_cores,
            e_cores,
            max_efficiency_class: max_eff,
        }
    }

    /// Pins the calling thread (e.g., DX12 render or WASAPI audio thread) across all P-cores.
    pub fn pin_current_thread_to_p_cores(&self) -> bool {
        if self.p_cores.is_empty() {
            return false;
        }
        let ids: Vec<u32> = self.p_cores.iter().map(|c| c.cpu_set_id).collect();
        let ok = unsafe {
            SetThreadSelectedCpuSets(GetCurrentThread(), ids.as_ptr(), ids.len() as u32)
        };
        let mut mask = 0usize;
        for c in &self.p_cores {
            if (c.logical_idx as usize) < usize::BITS as usize {
                mask |= 1usize << c.logical_idx;
            }
        }
        if mask != 0 {
            unsafe { SetThreadAffinityMask(GetCurrentThread(), mask) };
        }
        ok != 0
    }

    /// Strictly pins a guest vCPU thread to a single dedicated physical P-core.
    pub fn pin_vcpu_to_p_core(&self, vcpu_index: usize) -> bool {
        if self.p_cores.is_empty() {
            return false;
        }
        let chosen = &self.p_cores[vcpu_index % self.p_cores.len()];
        let ok = unsafe { SetThreadSelectedCpuSets(GetCurrentThread(), &chosen.cpu_set_id, 1) };
        if (chosen.logical_idx as usize) < usize::BITS as usize {
            unsafe { SetThreadAffinityMask(GetCurrentThread(), 1usize << chosen.logical_idx) };
        }
        ok != 0
    }
}

// ============================================================================
// 3. Windows Hypervisor Platform (WHPX) Micro-VM Controller
// ============================================================================

type WHV_PARTITION_HANDLE = *mut c_void;

const WHV_CAPABILITY_CODE_HYPERVISOR_PRESENT: u32 = 0x0000_0000;
const WHV_PARTITION_PROPERTY_CODE_PROCESSOR_COUNT: u32 = 0x0000_1FFF;
const WHV_MAP_GPA_RANGE_FLAG_READ: u32 = 0x1;
const WHV_MAP_GPA_RANGE_FLAG_WRITE: u32 = 0x2;
const WHV_MAP_GPA_RANGE_FLAG_EXECUTE: u32 = 0x4;

struct WhpxApi {
    module: HMODULE,
    get_capability: unsafe extern "system" fn(u32, *mut c_void, u32, *mut u32) -> HRESULT,
    create_partition: unsafe extern "system" fn(*mut WHV_PARTITION_HANDLE) -> HRESULT,
    setup_partition: unsafe extern "system" fn(WHV_PARTITION_HANDLE) -> HRESULT,
    delete_partition: unsafe extern "system" fn(WHV_PARTITION_HANDLE) -> HRESULT,
    set_partition_property: unsafe extern "system" fn(
        WHV_PARTITION_HANDLE,
        u32,
        *const c_void,
        u32,
    ) -> HRESULT,
    map_gpa_range: unsafe extern "system" fn(
        WHV_PARTITION_HANDLE,
        *mut c_void,
        u64,
        u64,
        u32,
    ) -> HRESULT,
    create_vp: unsafe extern "system" fn(WHV_PARTITION_HANDLE, u32, u32) -> HRESULT,
    delete_vp: unsafe extern "system" fn(WHV_PARTITION_HANDLE, u32) -> HRESULT,
}

impl WhpxApi {
    fn load() -> Result<Self, String> {
        let name = utf16_null("WinHvPlatform.dll");
        let module = unsafe { LoadLibraryW(name.as_ptr()) };
        if module.is_null() {
            return Err("WinHvPlatform.dll not found on host".to_string());
        }
        macro_rules! load_sym {
            ($sym:expr, $ty:ty) => {{
                let p = unsafe { GetProcAddress(module, $sym.as_ptr()) };
                if p.is_null() {
                    unsafe { FreeLibrary(module) };
                    return Err(format!("Missing WHPX export {}", String::from_utf8_lossy($sym)));
                }
                unsafe { core::mem::transmute::<*const c_void, $ty>(p) }
            }};
        }
        Ok(Self {
            module,
            get_capability: load_sym!(b"WHvGetCapability\0", _),
            create_partition: load_sym!(b"WHvCreatePartition\0", _),
            setup_partition: load_sym!(b"WHvSetupPartition\0", _),
            delete_partition: load_sym!(b"WHvDeletePartition\0", _),
            set_partition_property: load_sym!(b"WHvSetPartitionProperty\0", _),
            map_gpa_range: load_sym!(b"WHvMapGpaRange\0", _),
            create_vp: load_sym!(b"WHvCreateVirtualProcessor\0", _),
            delete_vp: load_sym!(b"WHvDeleteVirtualProcessor\0", _),
        })
    }
}

impl Drop for WhpxApi {
    fn drop(&mut self) {
        if !self.module.is_null() {
            unsafe { FreeLibrary(self.module) };
            self.module = null_mut();
        }
    }
}

/// Minimalist WHPX micro-VM partition controller (zero QEMU legacy devices).
pub struct WhpxMicroVm {
    api: WhpxApi,
    partition: WHV_PARTITION_HANDLE,
    pub vcpu_count: u32,
    pub hypervisor_present: bool,
    pub partition_ready: bool,
}

impl WhpxMicroVm {
    /// Probes WHPX capability and, if enabled on Windows, provisions a micro-VM
    /// partition with `vcpu_count` vCPUs and maps guest RAM + IVSHMEM 2 MB BAR2.
    pub fn probe_and_provision(
        vcpu_count: u32,
        guest_ram: &LargePageArena,
        ivshmem_bar2: &LargePageArena,
    ) -> Result<Self, String> {
        let api = WhpxApi::load()?;
        let mut present: u64 = 0;
        let mut written = 0u32;
        let hr = unsafe {
            (api.get_capability)(
                WHV_CAPABILITY_CODE_HYPERVISOR_PRESENT,
                (&mut present as *mut u64).cast(),
                size_of::<u64>() as u32,
                &mut written,
            )
        };
        let hypervisor_present = succeeded(hr) && (present & 1) != 0;
        if !hypervisor_present {
            return Ok(Self {
                api,
                partition: null_mut(),
                vcpu_count,
                hypervisor_present: false,
                partition_ready: false,
            });
        }

        let mut partition: WHV_PARTITION_HANDLE = null_mut();
        let hr = unsafe { (api.create_partition)(&mut partition) };
        if !succeeded(hr) {
            return Err(format!("WHvCreatePartition failed: 0x{hr:08x}"));
        }

        let vcpus = vcpu_count.max(1);
        let hr = unsafe {
            (api.set_partition_property)(
                partition,
                WHV_PARTITION_PROPERTY_CODE_PROCESSOR_COUNT,
                (&vcpus as *const u32).cast(),
                size_of::<u32>() as u32,
            )
        };
        if !succeeded(hr) {
            unsafe { (api.delete_partition)(partition) };
            return Err(format!("WHvSetPartitionProperty(ProcessorCount) failed: 0x{hr:08x}"));
        }

        let hr = unsafe { (api.setup_partition)(partition) };
        if !succeeded(hr) {
            unsafe { (api.delete_partition)(partition) };
            return Err(format!("WHvSetupPartition failed: 0x{hr:08x}"));
        }

        let flags = WHV_MAP_GPA_RANGE_FLAG_READ
            | WHV_MAP_GPA_RANGE_FLAG_WRITE
            | WHV_MAP_GPA_RANGE_FLAG_EXECUTE;
        let hr_ram = unsafe {
            (api.map_gpa_range)(
                partition,
                guest_ram.as_mut_ptr().cast(),
                GPA_LOW_RAM_BASE,
                guest_ram.len() as u64,
                flags,
            )
        };
        let hr_shm = unsafe {
            (api.map_gpa_range)(
                partition,
                ivshmem_bar2.as_mut_ptr().cast(),
                GPA_IVSHMEM_BAR2_BASE,
                ivshmem_bar2.len() as u64,
                WHV_MAP_GPA_RANGE_FLAG_READ | WHV_MAP_GPA_RANGE_FLAG_WRITE,
            )
        };
        if !succeeded(hr_ram) || !succeeded(hr_shm) {
            unsafe { (api.delete_partition)(partition) };
            return Err(format!(
                "WHvMapGpaRange failed (ram=0x{hr_ram:08x}, shm=0x{hr_shm:08x})"
            ));
        }

        for vp in 0..vcpus {
            let hr_vp = unsafe { (api.create_vp)(partition, vp, 0) };
            if !succeeded(hr_vp) {
                for cleanup_vp in 0..vp {
                    unsafe { (api.delete_vp)(partition, cleanup_vp) };
                }
                unsafe { (api.delete_partition)(partition) };
                return Err(format!("WHvCreateVirtualProcessor({vp}) failed: 0x{hr_vp:08x}"));
            }
        }

        Ok(Self {
            api,
            partition,
            vcpu_count: vcpus,
            hypervisor_present: true,
            partition_ready: true,
        })
    }
}

impl Drop for WhpxMicroVm {
    fn drop(&mut self) {
        if !self.partition.is_null() {
            for vp in 0..self.vcpu_count {
                unsafe { (self.api.delete_vp)(self.partition, vp) };
            }
            unsafe { (self.api.delete_partition)(self.partition) };
            self.partition = null_mut();
        }
    }
}

// ============================================================================
// 4. Sub-ms RawInput Polling & Smart Keymapper (WASD + Mouse Aim)
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeymapMode {
    /// Converts WASD -> Virtual Analog Joystick (MT Slot 0) & Mouse Delta -> Aim (MT Slot 1).
    GamepadTouch,
    /// Zero-latency Hardware Pointer passthrough (`EV_REL` + `EV_KEY`).
    RawPointer,
}

/// Lock-free input pipeline translating Windows `WM_INPUT` RawInput directly into
/// `InputSpscRing` events without syscalls or heap allocations.
pub struct RawInputMapper {
    pub keymap_enabled: AtomicBool,
    wasd_mask: std::sync::atomic::AtomicU8, // bit0=W, bit1=A, bit2=S, bit3=D
    aim_x: core::sync::atomic::AtomicI32,
    aim_y: core::sync::atomic::AtomicI32,
    qpc_freq: i64,
}

const TOUCH_MAX_COORD: i32 = 32767;
const WASD_CENTER_X: i32 = 6200;
const WASD_CENTER_Y: i32 = 24000;
const WASD_RADIUS: f32 = 3400.0;

impl RawInputMapper {
    pub fn new() -> Self {
        let mut freq = 1i64;
        unsafe { QueryPerformanceFrequency(&mut freq) };
        Self {
            keymap_enabled: AtomicBool::new(true),
            wasd_mask: std::sync::atomic::AtomicU8::new(0),
            aim_x: core::sync::atomic::AtomicI32::new(TOUCH_MAX_COORD / 2),
            aim_y: core::sync::atomic::AtomicI32::new(TOUCH_MAX_COORD / 2),
            qpc_freq: freq.max(1),
        }
    }

    pub fn register_devices(hwnd: HWND) -> bool {
        let devices = [
            RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: 0x02, // Mouse
                dwFlags: 0,
                hwndTarget: hwnd,
            },
            RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: 0x06, // Keyboard
                dwFlags: 0,
                hwndTarget: hwnd,
            },
        ];
        unsafe {
            RegisterRawInputDevices(
                devices.as_ptr(),
                devices.len() as u32,
                size_of::<RAWINPUTDEVICE>() as u32,
            ) != 0
        }
    }

    #[inline(always)]
    fn timestamp_ns(&self) -> u64 {
        let mut qpc = 0i64;
        unsafe { QueryPerformanceCounter(&mut qpc) };
        ((qpc as u128 * 1_000_000_000) / self.qpc_freq as u128) as u64
    }

    /// Returns normalized `(wasd_xy in [-1, 1], aim_uv in [0, 1], wasd_mask)` for live HUD overlays.
    pub fn normalized_state(&self) -> ([f32; 2], [f32; 2], u8) {
        let mask = self.wasd_mask.load(Ordering::Relaxed);
        let mut dx = 0.0f32;
        let mut dy = 0.0f32;
        if (mask & 1) != 0 { dy -= 1.0; }
        if (mask & 4) != 0 { dy += 1.0; }
        if (mask & 2) != 0 { dx -= 1.0; }
        if (mask & 8) != 0 { dx += 1.0; }
        let len = (dx * dx + dy * dy).sqrt();
        if len > 1.0 {
            dx /= len;
            dy /= len;
        }
        let ax = (self.aim_x.load(Ordering::Relaxed) as f32 / TOUCH_MAX_COORD as f32).clamp(0.0, 1.0);
        let ay = (self.aim_y.load(Ordering::Relaxed) as f32 / TOUCH_MAX_COORD as f32).clamp(0.0, 1.0);
        ([dx, dy], [ax, ay], mask)
    }

    /// Zero-allocation handler invoked directly from `WM_INPUT`.
    pub fn handle_wm_input(&self, lparam: LPARAM, ring: &InputSpscRing) {
        let mut raw: RAWINPUT = unsafe { zeroed() };
        let mut size = size_of::<RAWINPUT>() as u32;
        let read = unsafe {
            GetRawInputData(
                lparam as HANDLE,
                RID_INPUT,
                (&mut raw as *mut RAWINPUT).cast(),
                &mut size,
                size_of::<RAWINPUTHEADER>() as u32,
            )
        };
        if read == u32::MAX {
            return;
        }
        unsafe {
            match raw.header.dwType {
                RIM_TYPEMOUSE => {
                    let m = raw.data.mouse;
                    self.process_mouse_delta(m.lLastX, m.lLastY, m.usButtonFlags, ring);
                }
                RIM_TYPEKEYBOARD => {
                    let k = raw.data.keyboard;
                    let pressed = (k.Flags & RI_KEY_BREAK) == 0;
                    self.process_key(k.VKey, pressed, ring);
                }
                _ => {}
            }
        }
    }

    /// Translates raw keyboard scancodes/VKeys, updating WASD virtual stick in `GamepadTouch` mode.
    pub fn process_key(&self, vkey: u16, pressed: bool, ring: &InputSpscRing) {
        let ts = self.timestamp_ns();
        let bit = match vkey {
            0x57 => 1u8 << 0, // W
            0x41 => 1u8 << 1, // A
            0x53 => 1u8 << 2, // S
            0x44 => 1u8 << 3, // D
            _ => 0,
        };

        if bit != 0 && self.keymap_enabled.load(Ordering::Relaxed) {
            let prev = if pressed {
                self.wasd_mask.fetch_or(bit, Ordering::Relaxed)
            } else {
                self.wasd_mask.fetch_and(!bit, Ordering::Relaxed)
            };
            let next = if pressed { prev | bit } else { prev & !bit };
            if next != prev {
                self.emit_wasd_stick(next, ts, ring);
            }
            return;
        }

        let _ = ring.try_push_batch(&[
            VirtioInputEvent::new(ev_type::EV_KEY, vkey, pressed as i32, ts),
            VirtioInputEvent::syn(ts),
        ]);
    }

    fn emit_wasd_stick(&self, mask: u8, ts: u64, ring: &InputSpscRing) {
        if mask == 0 {
            let _ = ring.try_push_batch(&[
                VirtioInputEvent::new(ev_type::EV_ABS, ev_code::ABS_MT_SLOT, 0, ts),
                VirtioInputEvent::new(ev_type::EV_ABS, ev_code::ABS_MT_TRACKING_ID, -1, ts),
                VirtioInputEvent::syn(ts),
            ]);
            return;
        }
        let mut dx = 0.0f32;
        let mut dy = 0.0f32;
        if (mask & 1) != 0 { dy -= 1.0; }
        if (mask & 4) != 0 { dy += 1.0; }
        if (mask & 2) != 0 { dx -= 1.0; }
        if (mask & 8) != 0 { dx += 1.0; }
        let len = (dx * dx + dy * dy).sqrt();
        if len > 1.0 {
            dx /= len;
            dy /= len;
        }
        let tx = (WASD_CENTER_X as f32 + dx * WASD_RADIUS) as i32;
        let ty = (WASD_CENTER_Y as f32 + dy * WASD_RADIUS) as i32;
        let _ = ring.try_push_batch(&[
            VirtioInputEvent::new(ev_type::EV_ABS, ev_code::ABS_MT_SLOT, 0, ts),
            VirtioInputEvent::new(ev_type::EV_ABS, ev_code::ABS_MT_TRACKING_ID, 100, ts),
            VirtioInputEvent::new(ev_type::EV_ABS, ev_code::ABS_MT_POSITION_X, tx, ts),
            VirtioInputEvent::new(ev_type::EV_ABS, ev_code::ABS_MT_POSITION_Y, ty, ts),
            VirtioInputEvent::syn(ts),
        ]);
    }

    /// Emits raw mouse motion & button clicks either as aim touch (Slot 1) or relative HW cursor.
    pub fn process_mouse_delta(
        &self,
        dx: i32,
        dy: i32,
        button_flags: u16,
        ring: &InputSpscRing,
    ) {
        let ts = self.timestamp_ns();
        let mut batch = [VirtioInputEvent::default(); 10];
        let mut n = 0usize;

        if dx != 0 || dy != 0 {
            if self.keymap_enabled.load(Ordering::Relaxed) {
                let nx = (self.aim_x.load(Ordering::Relaxed) + dx * 16).clamp(0, TOUCH_MAX_COORD);
                let ny = (self.aim_y.load(Ordering::Relaxed) + dy * 16).clamp(0, TOUCH_MAX_COORD);
                self.aim_x.store(nx, Ordering::Relaxed);
                self.aim_y.store(ny, Ordering::Relaxed);
                batch[n] = VirtioInputEvent::new(ev_type::EV_ABS, ev_code::ABS_MT_SLOT, 1, ts);
                batch[n + 1] = VirtioInputEvent::new(ev_type::EV_ABS, ev_code::ABS_MT_TRACKING_ID, 101, ts);
                batch[n + 2] = VirtioInputEvent::new(ev_type::EV_ABS, ev_code::ABS_MT_POSITION_X, nx, ts);
                batch[n + 3] = VirtioInputEvent::new(ev_type::EV_ABS, ev_code::ABS_MT_POSITION_Y, ny, ts);
                n += 4;
            } else {
                if dx != 0 {
                    batch[n] = VirtioInputEvent::new(ev_type::EV_REL, ev_code::REL_X, dx, ts);
                    n += 1;
                }
                if dy != 0 {
                    batch[n] = VirtioInputEvent::new(ev_type::EV_REL, ev_code::REL_Y, dy, ts);
                    n += 1;
                }
            }
        }

        if (button_flags & RI_MOUSE_LEFT_BUTTON_DOWN) != 0 {
            batch[n] = VirtioInputEvent::new(ev_type::EV_KEY, ev_code::BTN_LEFT, 1, ts);
            n += 1;
        } else if (button_flags & RI_MOUSE_LEFT_BUTTON_UP) != 0 {
            batch[n] = VirtioInputEvent::new(ev_type::EV_KEY, ev_code::BTN_LEFT, 0, ts);
            n += 1;
        }
        if (button_flags & RI_MOUSE_RIGHT_BUTTON_DOWN) != 0 {
            batch[n] = VirtioInputEvent::new(ev_type::EV_KEY, ev_code::BTN_RIGHT, 1, ts);
            n += 1;
        } else if (button_flags & RI_MOUSE_RIGHT_BUTTON_UP) != 0 {
            batch[n] = VirtioInputEvent::new(ev_type::EV_KEY, ev_code::BTN_RIGHT, 0, ts);
            n += 1;
        }

        if n > 0 {
            batch[n] = VirtioInputEvent::syn(ts);
            let _ = ring.try_push_batch(&batch[..=n]);
        }
    }
}

// ============================================================================
// 5. Async `virtio-blk` Disk I/O Engine via Windows IOCP
// ============================================================================

/// Non-blocking Windows I/O Completion Port (`IOCP`) engine backing `virtio-blk`.
pub struct VirtioBlkIocpEngine {
    file_handle: HANDLE,
    iocp_handle: HANDLE,
}

impl VirtioBlkIocpEngine {
    pub fn open_or_create(path: &str) -> Result<Self, String> {
        let wpath = utf16_null(path);
        let mut fh = unsafe {
            CreateFileW(
                wpath.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        if fh == INVALID_HANDLE_VALUE {
            fh = unsafe {
                CreateFileW(
                    wpath.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    null(),
                    CREATE_ALWAYS,
                    FILE_FLAG_OVERLAPPED,
                    null_mut(),
                )
            };
        }
        if fh == INVALID_HANDLE_VALUE {
            return Err(format!("CreateFileW({path}) failed"));
        }
        let iocp = unsafe { CreateIoCompletionPort(fh, null_mut(), 1, 0) };
        if iocp.is_null() {
            unsafe { CloseHandle(fh) };
            return Err("CreateIoCompletionPort failed".to_string());
        }
        Ok(Self {
            file_handle: fh,
            iocp_handle: iocp,
        })
    }

    /// Queues an asynchronous disk write at `offset` without waiting.
    pub fn submit_write(&self, offset: u64, data: &[u8], ov: &mut OVERLAPPED) -> Result<(), String> {
        *ov = OVERLAPPED {
            Offset: (offset & 0xFFFF_FFFF) as u32,
            OffsetHigh: (offset >> 32) as u32,
            ..Default::default()
        };
        let mut written = 0u32;
        let ok = unsafe {
            WriteFile(
                self.file_handle,
                data.as_ptr().cast(),
                data.len() as u32,
                &mut written,
                ov,
            )
        };
        if ok == 0 && unsafe { GetLastError() } != ERROR_IO_PENDING {
            return Err(format!("IOCP WriteFile error: {}", unsafe { GetLastError() }));
        }
        Ok(())
    }

    /// Queues an asynchronous disk read at `offset` into `dst`.
    pub fn submit_read(&self, offset: u64, dst: &mut [u8], ov: &mut OVERLAPPED) -> Result<(), String> {
        *ov = OVERLAPPED {
            Offset: (offset & 0xFFFF_FFFF) as u32,
            OffsetHigh: (offset >> 32) as u32,
            ..Default::default()
        };
        let mut read = 0u32;
        let ok = unsafe {
            ReadFile(
                self.file_handle,
                dst.as_mut_ptr().cast(),
                dst.len() as u32,
                &mut read,
                ov,
            )
        };
        if ok == 0 && unsafe { GetLastError() } != ERROR_IO_PENDING {
            return Err(format!("IOCP ReadFile error: {}", unsafe { GetLastError() }));
        }
        Ok(())
    }

    /// Reaps up to `entries.len()` completed I/O operations in one kernel call.
    pub fn reap_completions(&self, entries: &mut [OVERLAPPED_ENTRY], timeout_ms: u32) -> usize {
        let mut removed = 0u32;
        let ok = unsafe {
            GetQueuedCompletionStatusEx(
                self.iocp_handle,
                entries.as_mut_ptr(),
                entries.len() as u32,
                &mut removed,
                timeout_ms,
                0,
            )
        };
        if ok != 0 {
            removed as usize
        } else {
            0
        }
    }
}

impl Drop for VirtioBlkIocpEngine {
    fn drop(&mut self) {
        if !self.iocp_handle.is_null() {
            unsafe { CloseHandle(self.iocp_handle) };
        }
        if self.file_handle != INVALID_HANDLE_VALUE && !self.file_handle.is_null() {
            unsafe { CloseHandle(self.file_handle) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage1_memory::IvshmemSharedRegion;

    #[test]
    fn large_page_arena_is_2mb_aligned() {
        let arena = LargePageArena::allocate(LARGE_PAGE_SIZE).expect("allocate 2MB arena");
        assert_eq!((arena.as_mut_ptr() as usize) % LARGE_PAGE_SIZE, 0);
        assert!(arena.len() >= LARGE_PAGE_SIZE);
    }

    #[test]
    fn cpu_pinning_detects_p_cores() {
        let mgr = CpuPinningManager::detect();
        assert!(!mgr.p_cores.is_empty());
        assert!(mgr.pin_current_thread_to_p_cores());
    }

    #[test]
    fn raw_input_smart_keymapper_wasd_and_mouse_aim() {
        let arena = LargePageArena::allocate(LARGE_PAGE_SIZE).unwrap();
        let shm = unsafe {
            IvshmemSharedRegion::init_in_place(arena.as_mut_ptr().cast(), 4 * 1024 * 1024 * 1024)
        };
        let mapper = RawInputMapper::new();

        // Press W + D -> normalized diagonal stick at Slot 0.
        mapper.process_key(0x57, true, &shm.input_ring);
        mapper.process_key(0x44, true, &shm.input_ring);
        // Move mouse right & down -> Aim at Slot 1 + Left click.
        mapper.process_mouse_delta(20, -10, RI_MOUSE_LEFT_BUTTON_DOWN, &shm.input_ring);

        let mut drained = [VirtioInputEvent::default(); 32];
        let count = shm.input_ring.try_pop_batch(&mut drained);
        assert!(count >= 15);
        assert!(drained[..count]
            .iter()
            .any(|e| e.event_type == ev_type::EV_ABS && e.code == ev_code::ABS_MT_SLOT && e.value == 0));
        assert!(drained[..count]
            .iter()
            .any(|e| e.event_type == ev_type::EV_ABS && e.code == ev_code::ABS_MT_SLOT && e.value == 1));
        assert!(drained[..count]
            .iter()
            .any(|e| e.event_type == ev_type::EV_KEY && e.code == ev_code::BTN_LEFT && e.value == 1));
    }

    #[test]
    fn virtio_blk_iocp_async_roundtrip() {
        let mut p = std::env::temp_dir();
        p.push("titan_virtio_blk_iocp_test.img");
        let path_str = p.to_str().unwrap();
        let _ = std::fs::remove_file(&p);

        {
            let engine = VirtioBlkIocpEngine::open_or_create(path_str).expect("open IOCP image");
            let write_buf = vec![0x5Au8; 4096];
            let mut ov_w = OVERLAPPED::default();
            engine.submit_write(0, &write_buf, &mut ov_w).expect("submit async write");
            let mut entries = [OVERLAPPED_ENTRY::default(); 4];
            let n = engine.reap_completions(&mut entries, 1000);
            assert_eq!(n, 1);
            assert_eq!(entries[0].dwNumberOfBytesTransferred, 4096);

            let mut read_buf = vec![0u8; 4096];
            let mut ov_r = OVERLAPPED::default();
            engine.submit_read(0, &mut read_buf, &mut ov_r).expect("submit async read");
            let n2 = engine.reap_completions(&mut entries, 1000);
            assert_eq!(n2, 1);
            assert_eq!(read_buf[0], 0x5A);
            assert_eq!(read_buf[4095], 0x5A);
        }
        let _ = std::fs::remove_file(&p);
    }
}
