//! Instance lock: one live wagglepost serve per managed root. Acquired BEFORE
//! Store::open / recover / startup-code / cli-session writes (WP06).
use sha2::Sha256;

pub struct InstanceGuard {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.handle);
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

/// Try to acquire the instance mutex for this managed root.
/// Ok(guard): we are the single live instance (an abandoned mutex from a crashed
/// owner is retaken). Err: another live instance holds the root.
pub fn acquire(managed_root: &std::path::Path) -> Result<InstanceGuard, String> {
    let key = format!(
        "wagglepost-instance-{}",
        &sha256_hex(managed_root.to_string_lossy().to_lowercase().as_bytes())[..24]
    );
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::CreateMutexW;
        use windows_sys::Win32::Foundation::{GetLastError, WAIT_ABANDONED, WAIT_OBJECT_0};
        let wide: Vec<u16> = key.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
        if handle.is_null() {
            return Err(format!("CreateMutexW failed (code {})", unsafe { GetLastError() }));
        }
        // immediate acquisition check: a live second instance must be REFUSED, not wait
        let wait = unsafe { windows_sys::Win32::System::Threading::WaitForSingleObject(handle, 0) };
        if wait == WAIT_OBJECT_0 {
            return Ok(InstanceGuard { handle });
        }
        if wait == WAIT_ABANDONED {
            // previous owner crashed; the managed root has no live server — retake
            return Ok(InstanceGuard { handle });
        }
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        Err(format!(
            "another wagglepost instance is already running for this managed root (mutex {key})"
        ))
    }
    #[cfg(not(windows))]
    {
        // pid-file heuristic fallback
        let lock = managed_root.join("instance.lock");
        if lock.exists() {
            if let Ok(pid) = std::fs::read_to_string(&lock) {
                if let Ok(pid) = pid.trim().parse::<u32>() {
                    if proc_alive(pid) {
                        return Err(format!(
                            "another wagglepost instance is already running for this managed root (pid {pid})"
                        ));
                    }
                }
            }
        }
        std::fs::write(&lock, std::process::id().to_string()).map_err(|e| e.to_string())?;
        Ok(InstanceGuard {})
    }
}

#[cfg(not(windows))]
impl Drop for InstanceGuard {
    fn drop(&mut self) {}
}

#[cfg(not(windows))]
fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest as _;
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}

#[cfg(not(windows))]
fn proc_alive(_pid: u32) -> bool {
    true // conservative on non-Windows
}

#[cfg(windows)]
fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest as _;
    let mut h = Sha256::new();
    h.update(data);
    hex::encode(h.finalize())
}
