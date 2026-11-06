//! Files only their owner may read.

use std::fs;
use std::path::Path;

use crate::error::BootstrapError;

/// Writes `contents` to `path`, readable and writable by its owner only.
pub fn write_private_file(
    path: impl AsRef<Path>,
    contents: impl AsRef<[u8]>,
) -> Result<(), BootstrapError> {
    let path = path.as_ref();
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents.as_ref())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::write(path, contents)?;
        Ok(())
    }
}
