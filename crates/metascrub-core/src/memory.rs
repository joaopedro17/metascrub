//! Gives memory freed after a large file back to the operating system.
//! glibc's allocator keeps a separate pool for each thread and holds on to
//! what a thread freed, so large files cleaned one after another on
//! different threads could together keep twice the memory budget (four
//! 400 MB PDFs: 4.4 GB instead of 2 GB). `malloc_trim` hands it back. The
//! allocators of the release builds (musl, Windows, macOS) already do, so
//! there this does nothing.
#![allow(unsafe_code)]

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) fn release_freed() {
    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> std::ffi::c_int;
    }
    // SAFETY: `malloc_trim` takes no pointers; it only returns free pages
    // of the allocator's pools to the system, and is safe to call from any
    // thread at any time.
    unsafe {
        malloc_trim(0);
    }
    log::debug!("freed memory handed back to the system after a large file");
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
pub(crate) fn release_freed() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_be_called_from_any_thread() {
        release_freed();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    let block = vec![1u8; 64 * 1024 * 1024];
                    drop(block);
                    release_freed();
                });
            }
        });
    }
}
