//! Process memory sampler — `task_info(TASK_VM_INFO)` `phys_footprint` +
//! `resident_size` (the RSS sampler), ported from the phase-0 spike's
//! `harness::mem`.
//!
//! `mach2` 0.4 does not ship `struct task_vm_info`, so it is hand-declared
//! below with the SAME layout as the SDK header so every field's offset is
//! correct.
//!
//! The SDK's `struct task_vm_info` sits inside `#pragma pack(push, 4)`
//! (`<mach/task_info.h>`), so it is a 4-byte-packed C struct: after the
//! `decompressions: i32` (rev5) field the following `i64`s are 4-aligned, not
//! 8-aligned. Plain `#[repr(C)]` would 8-align those `i64`s and insert 4 bytes
//! of padding, shifting `ledger_swapins` and every rev6/rev7 tail field by 4 —
//! and making `size_of / 4` == 94 instead of the kernel's `TASK_VM_INFO_COUNT`
//! (93). `#[repr(C, packed(4))]` reproduces the header's `pack(4)` exactly, so
//! all offsets (and the element count) match the kernel's `task_vm_info`.

use mach2::task::task_info;
use mach2::task_info::TASK_VM_INFO;
use mach2::traps::mach_task_self;

/// `struct task_vm_info` from `<mach/task_info.h>` (rev7). `mach_vm_size_t` /
/// `mach_vm_address_t` = u64, `integer_t` = i32, ledger fields = i64.
///
/// `packed(4)` mirrors the header's `#pragma pack(push, 4)` — see the module
/// doc. Do not remove it: without it the rev6/rev7 tail fields land at the
/// wrong offsets and the element count no longer matches `TASK_VM_INFO_COUNT`.
#[repr(C, packed(4))]
#[derive(Clone, Copy)]
struct TaskVmInfo {
    virtual_size: u64,
    region_count: i32,
    page_size: i32,
    resident_size: u64,
    resident_size_peak: u64,
    device: u64,
    device_peak: u64,
    internal: u64,
    internal_peak: u64,
    external: u64,
    external_peak: u64,
    reusable: u64,
    reusable_peak: u64,
    purgeable_volatile_pmap: u64,
    purgeable_volatile_resident: u64,
    purgeable_volatile_virtual: u64,
    compressed: u64,
    compressed_peak: u64,
    compressed_lifetime: u64,
    phys_footprint: u64, // rev1
    min_address: u64,    // rev2
    max_address: u64,
    ledger_phys_footprint_peak: i64, // rev3
    ledger_purgeable_nonvolatile: i64,
    ledger_purgeable_novolatile_compressed: i64,
    ledger_purgeable_volatile: i64,
    ledger_purgeable_volatile_compressed: i64,
    ledger_tag_network_nonvolatile: i64,
    ledger_tag_network_nonvolatile_compressed: i64,
    ledger_tag_network_volatile: i64,
    ledger_tag_network_volatile_compressed: i64,
    ledger_tag_media_footprint: i64,
    ledger_tag_media_footprint_compressed: i64,
    ledger_tag_media_nofootprint: i64,
    ledger_tag_media_nofootprint_compressed: i64,
    ledger_tag_graphics_footprint: i64,
    ledger_tag_graphics_footprint_compressed: i64,
    ledger_tag_graphics_nofootprint: i64,
    ledger_tag_graphics_nofootprint_compressed: i64,
    ledger_tag_neural_footprint: i64,
    ledger_tag_neural_footprint_compressed: i64,
    ledger_tag_neural_nofootprint: i64,
    ledger_tag_neural_nofootprint_compressed: i64,
    limit_bytes_remaining: u64, // rev4
    decompressions: i32,        // rev5
    ledger_swapins: i64,        // rev6
    ledger_tag_neural_nofootprint_total: i64, // rev7
    ledger_tag_neural_nofootprint_peak: i64,
}

/// `(phys_footprint, resident_size)` in bytes for THIS process. `(0, 0)` if the
/// kernel call fails.
pub fn sample() -> (u64, u64) {
    let mut info = unsafe { std::mem::zeroed::<TaskVmInfo>() };
    let mut count = (std::mem::size_of::<TaskVmInfo>() / std::mem::size_of::<u32>()) as u32;
    let kr = unsafe {
        task_info(
            mach_task_self(),
            TASK_VM_INFO,
            &mut info as *mut _ as *mut i32,
            &mut count,
        )
    };
    if kr != 0 {
        return (0, 0);
    }
    (info.phys_footprint, info.resident_size)
}

/// Bytes → mebibytes.
#[inline]
pub fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    #[test]
    fn mib_converts_bytes_to_mebibytes() {
        assert_eq!(mib(0), 0.0);
        assert_eq!(mib(1024 * 1024), 1.0);
        assert_eq!(mib(3 * 512 * 1024), 1.5); // 1.5 MiB
        assert_eq!(mib(2 * 1024 * 1024), 2.0);
    }

    /// Guards the `packed(4)` layout against the SDK header
    /// (`<mach/task_info.h>`, `#pragma pack(push, 4)`). If any of these drift,
    /// `sample()` and every later cycle that reads a tail field silently reads
    /// the wrong bytes. Offsets are the header's C-struct offsets under
    /// `pack(4)`.
    #[test]
    fn task_vm_info_layout_matches_sdk_header() {
        // Fields sample() reads live before the pack divergence and must be
        // exactly here.
        assert_eq!(offset_of!(TaskVmInfo, resident_size), 16);
        assert_eq!(offset_of!(TaskVmInfo, phys_footprint), 144);
        // The divergence point: with plain repr(C) the i64 after
        // `decompressions` (offset 344) would be padded to 352; pack(4) keeps
        // it at 348.
        assert_eq!(offset_of!(TaskVmInfo, limit_bytes_remaining), 336);
        assert_eq!(offset_of!(TaskVmInfo, decompressions), 344);
        assert_eq!(offset_of!(TaskVmInfo, ledger_swapins), 348);
        // size / sizeof(natural_t) must equal the kernel's TASK_VM_INFO_COUNT
        // (93). Plain repr(C) would make this 94.
        assert_eq!(size_of::<TaskVmInfo>(), 372);
        assert_eq!(size_of::<TaskVmInfo>() / size_of::<u32>(), 93);
    }

    /// `sample()` hits the live kernel; on this platform it must return a
    /// nonzero `phys_footprint` for the running test process.
    #[test]
    fn sample_returns_live_footprint() {
        let (phys, rss) = sample();
        assert!(phys > 0, "phys_footprint should be nonzero for this process");
        assert!(rss > 0, "resident_size should be nonzero for this process");
    }
}
