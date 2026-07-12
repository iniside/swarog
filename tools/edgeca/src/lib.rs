use std::path::Path;

use edge::DevCA;

/// Mint a development-only edge CA at the requested PEM paths.
pub fn mint_dev_ca(
    cert: &Path,
    key: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ca = DevCA::generate()?;
    let cert = cert.to_str().ok_or("certificate path is not UTF-8")?;
    let key = key.to_str().ok_or("key path is not UTF-8")?;
    ca.write_pem(cert, key)?;
    Ok(())
}
