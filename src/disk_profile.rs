//! Conservative source-volume classification; queried only by a background worker.
use std::path::Path;
pub fn ordinary_workers(path: &Path) -> usize {
    let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
    (if is_ssd(path) { 4 } else { 2 }).min(cores.saturating_sub(1).max(1))
}
#[cfg(windows)]
fn is_ssd(path: &Path) -> bool {
    use windows::{
        Win32::{
            Foundation::CloseHandle,
            Storage::FileSystem::{
                CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
                OPEN_EXISTING,
            },
            System::{
                IO::DeviceIoControl,
                Ioctl::{
                    DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY,
                    PropertyStandardQuery, STORAGE_PROPERTY_QUERY,
                    StorageDeviceSeekPenaltyProperty,
                },
            },
        },
        core::HSTRING,
    };
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' {
        return false;
    }
    let volume = format!("\\\\.\\{}:", bytes[0] as char);
    unsafe {
        let Ok(handle) = CreateFileW(
            &HSTRING::from(volume),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        ) else {
            return false;
        };
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceSeekPenaltyProperty,
            QueryType: PropertyStandardQuery,
            ..Default::default()
        };
        let mut descriptor = DEVICE_SEEK_PENALTY_DESCRIPTOR::default();
        let mut returned = 0;
        let ok = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&query as *const STORAGE_PROPERTY_QUERY).cast()),
            std::mem::size_of_val(&query) as u32,
            Some((&mut descriptor as *mut DEVICE_SEEK_PENALTY_DESCRIPTOR).cast()),
            std::mem::size_of_val(&descriptor) as u32,
            Some(&mut returned),
            None,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        ok && returned >= std::mem::size_of_val(&descriptor) as u32 && !descriptor.IncursSeekPenalty
    }
}
#[cfg(not(windows))]
fn is_ssd(_: &Path) -> bool {
    false
}
