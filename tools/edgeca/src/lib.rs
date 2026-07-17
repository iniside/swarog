use std::path::{Path, PathBuf};

use edge::DevCA;

/// Mint a development-only edge CA at the requested PEM paths.
pub fn mint_dev_ca(
    cert: &Path,
    key: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let parent = key.parent().ok_or("key path has no parent")?;
    std::fs::create_dir_all(parent)?;
    if let Some(parent) = cert.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let suffix = format!("{}.{}", std::process::id(), unique_suffix());
    let cert_temp = temporary_path(cert, &suffix);
    let key_temp = temporary_path(key, &suffix);
    let mut cleanup = Cleanup(vec![cert_temp.clone(), key_temp.clone()]);

    processctl::write_private_atomic(&key_temp, b"")?;
    let ca = DevCA::generate()?;
    ca.write_pem(
        cert_temp.to_str().ok_or("certificate path is not UTF-8")?,
        key_temp.to_str().ok_or("key path is not UTF-8")?,
    )?;
    processctl::validate_private_path(&key_temp)?;
    atomic_replace(&key_temp, key)?;
    atomic_replace(&cert_temp, cert)?;
    cleanup.0.clear();
    Ok(())
}

fn temporary_path(path: &Path, suffix: &str) -> PathBuf {
    path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        suffix
    ))
}

fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn atomic_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::rename(source, destination)
}

struct Cleanup(Vec<PathBuf>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests;
