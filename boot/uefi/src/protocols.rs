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
    /// `RegisterProtocolNotify`, `LocateHandle`, `LocateDevicePath`,
    /// `InstallConfigurationTable`.
    pub locate: [*const c_void; 4],
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
    assert!(offset_of!(BootServices, exit_boot_services) == 232);
    assert!(size_of::<BootServices>() == 264);
    assert!(offset_of!(LoadedImageProtocol, device_handle) == 24);
    assert!(offset_of!(SimpleFileSystemProtocol, open_volume) == 8);
    assert!(offset_of!(FileProtocol, read) == 32);
    assert!(offset_of!(FileProtocol, get_info) == 64);
    assert!(size_of::<ConfigurationTable>() == 24);
};
