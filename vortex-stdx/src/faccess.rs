//! Functions for managine file metadata.
//! From <https://github.com/Freaky/faccess>

use std::io;
use std::path::Path;

use bitflags::bitflags;

// Licensed under MIT from faccess
bitflags! {
    /// Access mode flags for `access` function to test for.
    pub struct AccessMode: u8 {
        /// Path exists
        const EXISTS  = 0b0001;
        /// Path can likely be read
        const READ    = 0b0010;
        /// Path can likely be written to
        const WRITE   = 0b0100;
        /// Path can likely be executed
        const EXECUTE = 0b1000;
    }
}

mod imp {
    use super::*;

    use rustix::fs::Access;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    pub fn access(p: &Path, mode: AccessMode) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            // If vortex has ambient CAP_DAC_OVERRIDE, everything is accessible regardless of mode bits
            use rustix::thread::{capability_is_in_ambient_set, CapabilitySet};
            if capability_is_in_ambient_set(CapabilitySet::DAC_OVERRIDE).unwrap_or(false) {
                return Ok(());
            }
        }

        let mut imode = Access::empty();

        if mode.contains(AccessMode::EXISTS) {
            imode |= Access::EXISTS;
        }

        if mode.contains(AccessMode::READ) {
            imode |= Access::READ_OK;
        }

        if mode.contains(AccessMode::WRITE) {
            imode |= Access::WRITE_OK;
        }

        if mode.contains(AccessMode::EXECUTE) {
            imode |= Access::EXEC_OK;
        }

        rustix::fs::access(p, imode)?;
        Ok(())
    }

    fn chown(p: &Path, uid: Option<u32>, gid: Option<u32>) -> io::Result<()> {
        let uid = uid.map(rustix::fs::Uid::from_raw);
        let gid = gid.map(rustix::fs::Gid::from_raw);
        rustix::fs::chown(p, uid, gid)?;
        Ok(())
    }

    pub fn copy_metadata(from: &Path, to: &Path) -> io::Result<()> {
        let from_meta = std::fs::metadata(from)?;
        let to_meta = std::fs::metadata(to)?;
        let from_gid = from_meta.gid();
        let to_gid = to_meta.gid();

        let mut perms = from_meta.permissions();
        perms.set_mode(perms.mode() & 0o0777);
        if from_gid != to_gid && chown(to, None, Some(from_gid)).is_err() {
            let new_perms = (perms.mode() & 0o0707) | ((perms.mode() & 0o07) << 3);
            perms.set_mode(new_perms);
        }

        #[cfg(target_os = "macos")]
        {
            use std::fs::{File, FileTimes};
            use std::os::macos::fs::FileTimesExt;

            let to_file = File::options().write(true).open(to)?;
            let times = FileTimes::new().set_created(from_meta.created()?);
            to_file.set_times(times)?;
        }

        std::fs::set_permissions(to, perms)?;

        Ok(())
    }

    pub fn hardlink_count(p: &Path) -> std::io::Result<u64> {
        let metadata = p.metadata()?;
        Ok(metadata.nlink())
    }
}

pub fn readonly(p: &Path) -> bool {
    match imp::access(p, AccessMode::WRITE) {
        Ok(_) => false,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

pub fn copy_metadata(from: &Path, to: &Path) -> io::Result<()> {
    imp::copy_metadata(from, to)
}

pub fn hardlink_count(p: &Path) -> io::Result<u64> {
    imp::hardlink_count(p)
}
