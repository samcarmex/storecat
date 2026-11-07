//! Where a file's data starts on the disk, so a spinning drive can be read in one sweep
//! instead of seeking back and forth. Windows asks the file system for the first cluster
//! of the file; elsewhere there is no cheap equivalent and callers fall back to folder order.

use std::path::Path;

/// First logical cluster of the file's data, or `None` when it has none (small files kept
/// inside the file table, empty or virtual files) or the file system won't say.
#[cfg(windows)]
pub fn first_cluster(path: &Path) -> Option<u64> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_MORE_DATA, HANDLE};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    use windows_sys::Win32::System::Ioctl::{
        FSCTL_GET_RETRIEVAL_POINTERS, RETRIEVAL_POINTERS_BUFFER, STARTING_VCN_INPUT_BUFFER,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    // Attribute-only access: no data is read and antivirus has nothing to scan.
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
        .ok()?;
    let input = STARTING_VCN_INPUT_BUFFER { StartingVcn: 0 };
    // SAFETY: plain-old-data out buffer with room for the header and one extent.
    let mut out: RETRIEVAL_POINTERS_BUFFER = unsafe { std::mem::zeroed() };
    let mut returned = 0u32;
    // SAFETY: valid handle for the lifetime of `file`; buffers are correctly sized.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle() as HANDLE,
            FSCTL_GET_RETRIEVAL_POINTERS,
            &input as *const _ as *const _,
            std::mem::size_of::<STARTING_VCN_INPUT_BUFFER>() as u32,
            &mut out as *mut _ as *mut _,
            std::mem::size_of::<RETRIEVAL_POINTERS_BUFFER>() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    // More extents than fit is fine: the first one is what matters.
    if ok == 0 && unsafe { GetLastError() } != ERROR_MORE_DATA {
        return None;
    }
    if out.ExtentCount == 0 {
        return None;
    }
    let lcn = out.Extents[0].Lcn;
    (lcn >= 0).then_some(lcn as u64)
}

#[cfg(not(windows))]
pub fn first_cluster(_path: &Path) -> Option<u64> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn finds_where_a_file_starts() {
        let dir = tempfile::tempdir().unwrap();
        let big = dir.path().join("big.bin");
        std::fs::write(&big, vec![7u8; 256 * 1024]).unwrap();
        assert!(
            super::first_cluster(&big).is_some(),
            "a 256 KB file has clusters"
        );
        let tiny = dir.path().join("tiny.txt");
        std::fs::write(&tiny, b"hi").unwrap();
        // Tiny files live inside the file table on NTFS: no clusters of their own.
        let _ = super::first_cluster(&tiny);
        assert!(super::first_cluster(&dir.path().join("missing")).is_none());
    }
}
