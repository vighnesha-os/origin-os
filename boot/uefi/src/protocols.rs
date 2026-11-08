//! The UEFI 2.11 boot services and protocols the loader uses to find, read and leave for
//! the Framekernel (ADR-0002 B1, B3, B5).
//!
//! References: UEFI 2.11 §4.4 `EFI_BOOT_SERVICES`, §4.6 `EFI_CONFIGURATION_TABLE`, §7.2
//! memory allocation services, §9.1 `EFI_LOADED_IMAGE_PROTOCOL`, §13.4
//! `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL`, §13.5 `EFI_FILE_PROTOCOL` and `EFI_FILE_INFO`.
//! Members the loader does not call are opaque pointers. The field offsets are pinned at
//! compile time below, and the QEMU + OVMF boot test checks them end to end.

use core::ffi::c_void;

use crate::uefi::{Handle, Status, TableHeader};

/// `EFI_GUID`: one 32-bit, two 16-bit and eight 8-bit fields, stored little-endian.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Guid {
    /// First field.
    pub data1: u32,
    /// Second field.
    pub data2: u16,
    /// Third field.
    pub data3: u16,
    /// Last eight bytes.
    pub data4: [u8; 8],
}

/// `EFI_LOADED_IMAGE_PROTOCOL_GUID`.
pub const LOADED_IMAGE_PROTOCOL: Guid = Guid {
    data1: 0x5B1B_31A1,
    data2: 0x9562,
    data3: 0x11D2,
    data4: [0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B],
};
/// `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_GUID`.
pub const SIMPLE_FILE_SYSTEM_PROTOCOL: Guid = Guid {
    data1: 0x964E_5B22,
    data2: 0x6459,
    data3: 0x11D2,
    data4: [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B],
};
/// `EFI_FILE_INFO_ID`.
pub const FILE_INFO: Guid = Guid {
    data1: 0x0957_6E92,
    data2: 0x6D3F,
    data3: 0x11D2,
    data4: [0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B],
};
/// `EFI_ACPI_20_TABLE_GUID`: the configuration-table entry for the ACPI 2.0+ RSDP.
pub const ACPI_20_TABLE: Guid = Guid {
    data1: 0x8868_E871,
    data2: 0xE4F1,
    data3: 0x11D3,
    data4: [0xBC, 0x22, 0x00, 0x80, 0xC7, 0x3C, 0x88, 0x81],
};
/// `EFI_GRAPHICS_OUTPUT_PROTOCOL_GUID` (UEFI 2.11 §12.9).
pub const GRAPHICS_OUTPUT_PROTOCOL: Guid = Guid {
    data1: 0x9042_A9DE,
    data2: 0x23DC,
    data3: 0x4A38,
    data4: [0x96, 0xFB, 0x7A, 0xDE, 0xD0, 0x80, 0x51, 0x6A],
};
/// `EFI_CONSOLE_OUT_DEVICE_GUID`: a tag protocol the firmware installs on the handles it uses
/// as console output devices. Its interface is NULL.
pub const CONSOLE_OUT_DEVICE: Guid = Guid {
    data1: 0xD3B3_6F2C,
    data2: 0xD551,
    data3: 0x11D4,
    data4: [0x9A, 0x46, 0x00, 0x90, 0x27, 0x3F, 0xC1, 0x4D],
};

/// `EFI_LOCATE_SEARCH_TYPE` `ByProtocol`.
pub const LOCATE_BY_PROTOCOL: u32 = 2;

/// `EFI_GRAPHICS_PIXEL_FORMAT`.
pub mod pixel_format {
    /// `PixelRedGreenBlueReserved8BitPerColor`: byte 0 is red.
    pub const RGB: u32 = 0;
    /// `PixelBlueGreenRedReserved8BitPerColor`: byte 0 is blue.
    pub const BGR: u32 = 1;
    /// `PixelBitMask`: the masks say which bits are which.
    pub const BIT_MASK: u32 = 2;
    /// `PixelBltOnly`: the mode has no physical framebuffer.
    pub const BLT_ONLY: u32 = 3;
}

/// `EFI_GRAPHICS_OUTPUT_MODE_INFORMATION` (§12.9).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GraphicsModeInformation {
    /// `Version`.
    pub version: u32,
    /// `HorizontalResolution`: visible pixels per row.
    pub horizontal_resolution: u32,
    /// `VerticalResolution`: visible rows.
    pub vertical_resolution: u32,
    /// `PixelFormat`: a [`pixel_format`] value.
    pub pixel_format: u32,
    /// `PixelInformation.RedMask`, valid only for [`pixel_format::BIT_MASK`].
    pub red_mask: u32,
    /// `PixelInformation.GreenMask`.
    pub green_mask: u32,
    /// `PixelInformation.BlueMask`.
    pub blue_mask: u32,
    /// `PixelInformation.ReservedMask`.
    pub reserved_mask: u32,
    /// `PixelsPerScanLine`, which can exceed `horizontal_resolution`.
    pub pixels_per_scan_line: u32,
}

/// `EFI_GRAPHICS_OUTPUT_PROTOCOL_MODE` (§12.9).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct GraphicsOutputMode {
    /// `MaxMode`.
    pub max_mode: u32,
    /// `Mode`: the current mode's number.
    pub mode: u32,
    /// `Info`: the current mode.
    pub info: *const GraphicsModeInformation,
    /// `SizeOfInfo`.
    pub size_of_info: usize,
    /// `FrameBufferBase`: physical address of the first pixel, or 0 in a Blt-only mode.
    pub frame_buffer_base: u64,
    /// `FrameBufferSize`.
    pub frame_buffer_size: usize,
}

/// `EFI_GRAPHICS_OUTPUT_PROTOCOL` (§12.9). The loader changes no mode and calls no member.
#[repr(C)]
#[derive(Debug)]
pub struct GraphicsOutputProtocol {
    /// `QueryMode`.
    pub query_mode: *const c_void,
    /// `SetMode`.
    pub set_mode: *const c_void,
    /// `Blt`.
    pub blt: *const c_void,
    /// `Mode`: the current mode and its framebuffer.
    pub mode: *const GraphicsOutputMode,
}

/// `EFI_BOOT_SERVICES_SIGNATURE`: "BOOTSERV".
pub const BOOT_SERVICES_SIGNATURE: u64 = 0x5652_4553_544F_4F42;

/// `EFI_ALLOCATE_TYPE` `AllocateAnyPages`.
pub const ALLOCATE_ANY_PAGES: u32 = 0;

/// `EFI_MEMORY_TYPE` values (§7.2), including the OS-defined range the loader uses for the
/// boot info and the kernel stack, so the firmware's memory map marks them (ADR-0002 B3).
pub mod memory_type {
    /// `EfiLoaderCode`: the kernel image. Firmware keeps this type executable, unlike
    /// `EfiLoaderData` and the OS-defined range, which OVMF marks no-execute.
    pub const LOADER_CODE: u32 = 1;
    /// `EfiLoaderData`.
    pub const LOADER_DATA: u32 = 2;
    /// OS-defined: the boot-info structure and its region array.
    pub const OS_BOOT_INFO: u32 = 0x8000_0002;
    /// OS-defined: the kernel's initial stack.
    pub const OS_KERNEL_STACK: u32 = 0x8000_0003;
}

/// `EFI_ALLOCATE_PAGES`.
pub type AllocatePagesFn = unsafe extern "efiapi" fn(
    alloc_type: u32,
    memory_type: u32,
    pages: usize,
    memory: *mut u64,
) -> Status;
/// `EFI_FREE_PAGES`.
pub type FreePagesFn = unsafe extern "efiapi" fn(memory: u64, pages: usize) -> Status;
/// `EFI_GET_MEMORY_MAP`.
pub type GetMemoryMapFn = unsafe extern "efiapi" fn(
    map_size: *mut usize,
    map: *mut u8,
    map_key: *mut usize,
    descriptor_size: *mut usize,
    descriptor_version: *mut u32,
) -> Status;
/// `EFI_HANDLE_PROTOCOL`.
pub type HandleProtocolFn = unsafe extern "efiapi" fn(
    handle: Handle,
    protocol: *const Guid,
    interface: *mut *mut c_void,
) -> Status;
/// `EFI_LOCATE_HANDLE`.
pub type LocateHandleFn = unsafe extern "efiapi" fn(
    search_type: u32,
    protocol: *const Guid,
    search_key: *mut c_void,
    buffer_size: *mut usize,
    buffer: *mut Handle,
) -> Status;
/// `EFI_EXIT_BOOT_SERVICES`.
pub type ExitBootServicesFn = unsafe extern "efiapi" fn(image: Handle, map_key: usize) -> Status;

/// `EFI_BOOT_SERVICES` up to `SetWatchdogTimer` (§4.4). The table is longer; the loader
/// checks that `HeaderSize` covers at least these members.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct BootServices {
    /// Table header; signature [`BOOT_SERVICES_SIGNATURE`].
    pub hdr: TableHeader,
    /// `RaiseTPL`.
    pub raise_tpl: *const c_void,
    /// `RestoreTPL`.
    pub restore_tpl: *const c_void,
    /// `AllocatePages`.
    pub allocate_pages: Option<AllocatePagesFn>,
    /// `FreePages`.
    pub free_pages: Option<FreePagesFn>,
    /// `GetMemoryMap`.
    pub get_memory_map: Option<GetMemoryMapFn>,
    /// `AllocatePool`.
    pub allocate_pool: *const c_void,
    /// `FreePool`.
    pub free_pool: *const c_void,
    /// `CreateEvent` through `CheckEvent` (six members).
    pub events: [*const c_void; 6],
    /// `InstallProtocolInterface`, `ReinstallProtocolInterface`, `UninstallProtocolInterface`.
    pub protocol_interfaces: [*const c_void; 3],
    /// `HandleProtocol`.
    pub handle_protocol: Option<HandleProtocolFn>,
    /// `Reserved`.
    pub reserved: *const c_void,
    /// `RegisterProtocolNotify`.
    pub register_protocol_notify: *const c_void,
    /// `LocateHandle`.
    pub locate_handle: Option<LocateHandleFn>,
    /// `LocateDevicePath`, `InstallConfigurationTable`.
    pub device_paths: [*const c_void; 2],
    /// `LoadImage`, `StartImage`, `Exit`, `UnloadImage`.
    pub images: [*const c_void; 4],
    /// `ExitBootServices`.
    pub exit_boot_services: Option<ExitBootServicesFn>,
    /// `GetNextMonotonicCount`, `Stall`, `SetWatchdogTimer`.
    pub misc: [*const c_void; 3],
}

/// `EFI_LOADED_IMAGE_PROTOCOL` up to `DeviceHandle` (§9.1).
#[repr(C)]
#[derive(Debug)]
pub struct LoadedImageProtocol {
    /// `Revision`.
    pub revision: u32,
    /// `ParentHandle`.
    pub parent_handle: Handle,
    /// `SystemTable`.
    pub system_table: *mut c_void,
    /// `DeviceHandle`: the device the image was loaded from.
    pub device_handle: Handle,
}

/// `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL_OPEN_VOLUME`.
pub type OpenVolumeFn = unsafe extern "efiapi" fn(
    this: *mut SimpleFileSystemProtocol,
    root: *mut *mut FileProtocol,
) -> Status;

/// `EFI_SIMPLE_FILE_SYSTEM_PROTOCOL` (§13.4).
#[repr(C)]
#[derive(Debug)]
pub struct SimpleFileSystemProtocol {
    /// `Revision`.
    pub revision: u64,
    /// `OpenVolume`.
    pub open_volume: Option<OpenVolumeFn>,
}

/// `EFI_FILE_OPEN`.
pub type FileOpenFn = unsafe extern "efiapi" fn(
    this: *mut FileProtocol,
    new_handle: *mut *mut FileProtocol,
    file_name: *const u16,
    open_mode: u64,
    attributes: u64,
) -> Status;
/// `EFI_FILE_CLOSE`.
pub type FileCloseFn = unsafe extern "efiapi" fn(this: *mut FileProtocol) -> Status;
/// `EFI_FILE_READ`.
pub type FileReadFn = unsafe extern "efiapi" fn(
    this: *mut FileProtocol,
    buffer_size: *mut usize,
    buffer: *mut c_void,
) -> Status;
/// `EFI_FILE_GET_INFO`.
pub type FileGetInfoFn = unsafe extern "efiapi" fn(
    this: *mut FileProtocol,
    information_type: *const Guid,
    buffer_size: *mut usize,
    buffer: *mut c_void,
) -> Status;

/// `EFI_FILE_PROTOCOL` up to `GetInfo` (§13.5).
#[repr(C)]
#[derive(Debug)]
pub struct FileProtocol {
    /// `Revision`.
    pub revision: u64,
    /// `Open`.
    pub open: Option<FileOpenFn>,
    /// `Close`.
    pub close: Option<FileCloseFn>,
    /// `Delete`.
    pub delete: *const c_void,
    /// `Read`.
    pub read: Option<FileReadFn>,
    /// `Write`.
    pub write: *const c_void,
    /// `GetPosition`.
    pub get_position: *const c_void,
    /// `SetPosition`.
    pub set_position: *const c_void,
    /// `GetInfo`.
    pub get_info: Option<FileGetInfoFn>,
}

/// `EFI_FILE_MODE_READ`.
pub const FILE_MODE_READ: u64 = 1;
/// Offset of `FileSize` in `EFI_FILE_INFO` (after the `Size` field).
pub const FILE_INFO_FILE_SIZE_OFFSET: usize = 8;

/// `EFI_CONFIGURATION_TABLE` (§4.6).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ConfigurationTable {
    /// Identifies the table.
    pub vendor_guid: Guid,
    /// The table.
    pub vendor_table: *mut c_void,
}

#[cfg(target_pointer_width = "64")]
const _: () = {
    use core::mem::{offset_of, size_of};
    assert!(size_of::<Guid>() == 16);
    assert!(offset_of!(BootServices, allocate_pages) == 40);
    assert!(offset_of!(BootServices, free_pages) == 48);
    assert!(offset_of!(BootServices, get_memory_map) == 56);
    assert!(offset_of!(BootServices, handle_protocol) == 152);
    assert!(offset_of!(BootServices, locate_handle) == 176);
    assert!(offset_of!(BootServices, exit_boot_services) == 232);
    assert!(size_of::<GraphicsModeInformation>() == 36);
    assert!(offset_of!(GraphicsModeInformation, pixel_format) == 12);
    assert!(offset_of!(GraphicsModeInformation, pixels_per_scan_line) == 32);
    assert!(offset_of!(GraphicsOutputMode, info) == 8);
    assert!(offset_of!(GraphicsOutputMode, size_of_info) == 16);
    assert!(offset_of!(GraphicsOutputMode, frame_buffer_base) == 24);
    assert!(offset_of!(GraphicsOutputMode, frame_buffer_size) == 32);
    assert!(size_of::<GraphicsOutputMode>() == 40);
    assert!(offset_of!(GraphicsOutputProtocol, mode) == 24);
    assert!(size_of::<BootServices>() == 264);
    assert!(offset_of!(LoadedImageProtocol, device_handle) == 24);
    assert!(offset_of!(SimpleFileSystemProtocol, open_volume) == 8);
    assert!(offset_of!(FileProtocol, read) == 32);
    assert!(offset_of!(FileProtocol, get_info) == 64);
    assert!(size_of::<ConfigurationTable>() == 24);
};
