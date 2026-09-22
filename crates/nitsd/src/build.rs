//! Executable inspection without contacting a daemon or opening its store.
//!
//! The running process caches its own bytes before an installed path can be
//! replaced. A candidate is executed only through the explicitly selected Nits
//! program, and its reported digest is checked against that program's bytes.

use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use nits_protocol::{
    BuildDescriptor, BuildDigest, ControlVersion, ProtocolVersion, ReleaseIdentity, SchemaVersion,
    WorkerVersion,
};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

static RUNNING: OnceLock<Result<BuildDescriptor, String>> = OnceLock::new();
const INSPECTION_LIMIT: u64 = 16 * 1024;
const INSPECTION_TIMEOUT: Duration = Duration::from_secs(5);

/// Exact identity of the executable that is running, including after an atomic
/// installation replaces its path. Call at process startup before serving I/O.
pub fn running() -> io::Result<BuildDescriptor> {
    RUNNING
        .get_or_init(|| describe_running().map_err(|error| error.to_string()))
        .clone()
        .map_err(io::Error::other)
}

fn describe_running() -> io::Result<BuildDescriptor> {
    #[cfg(target_os = "linux")]
    let path = PathBuf::from("/proc/self/exe");
    #[cfg(not(target_os = "linux"))]
    let path = std::env::current_exe()?;
    Ok(BuildDescriptor {
        digest: digest(&path)?,
        release: ReleaseIdentity {
            channel: option_env!("NITS_BUILD_CHANNEL")
                .unwrap_or("stable")
                .parse()
                .map_err(io::Error::other)?,
            version: option_env!("NITS_BUILD_RELEASE")
                .unwrap_or(env!("CARGO_PKG_VERSION"))
                .parse()
                .map_err(io::Error::other)?,
        },
        protocol: ProtocolVersion::CURRENT,
        schema: SchemaVersion::CURRENT,
        control: ControlVersion::CURRENT,
        worker: WorkerVersion::CURRENT,
    })
}

/// SHA-256 over the complete executable, read in bounded chunks.
pub fn digest(path: &Path) -> io::Result<BuildDigest> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Ok(BuildDigest::from_bytes(hash.finalize().into()));
        }
        hash.update(&buffer[..count]);
    }
}

/// Resolve precisely the selected program, including a PATH selection.
pub fn executable(program: &Path) -> io::Result<PathBuf> {
    if program.components().count() > 1 || program.is_absolute() {
        let path = if program.is_absolute() {
            program.to_path_buf()
        } else {
            std::env::current_dir()?.join(program)
        };
        if !std::fs::metadata(&path)?.is_file() {
            return Err(io::Error::other(
                "selected executable is not a regular file",
            ));
        }
        return Ok(path);
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join(program))
        .find(|candidate| {
            candidate.is_file()
                && rustix::fs::accessat(
                    rustix::fs::CWD,
                    candidate,
                    rustix::fs::Access::EXEC_OK,
                    rustix::fs::AtFlags::EACCESS,
                )
                .is_ok()
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "selected Nits executable is missing",
            )
        })
        .and_then(|path| {
            if path.is_absolute() {
                Ok(path)
            } else {
                Ok(std::env::current_dir()?.join(path))
            }
        })
}

#[derive(Debug, Clone)]
pub struct InspectedBuild {
    pub program: PathBuf,
    pub descriptor: BuildDescriptor,
}

/// Inspect an installed binary independently of config, daemon protocol, or
/// store schema. Output size, process lifetime and identity are all bounded.
pub async fn inspect(program: &Path) -> io::Result<InspectedBuild> {
    let program = executable(program)?;
    let mut child = tokio::process::Command::new(&program)
        .args(["daemon", "inspect", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing inspector stdout"))?;
    let result = tokio::time::timeout(INSPECTION_TIMEOUT, async {
        let mut bytes = Vec::new();
        stdout.take(INSPECTION_LIMIT + 1).read_to_end(&mut bytes).await?;
        if bytes.len() as u64 > INSPECTION_LIMIT {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "inspector response exceeds limit"));
        }
        if !child.wait().await?.success() {
            return Err(io::Error::other("installed executable does not support build inspection; install a supported release"));
        }
        let descriptor: BuildDescriptor = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if descriptor.control != ControlVersion::CURRENT || descriptor.worker != WorkerVersion::CURRENT {
            return Err(io::Error::other("installed executable requires a newer maintenance or MCP supervisor protocol"));
        }
        let path = program.clone();
        let digest = tokio::task::spawn_blocking(move || digest(&path)).await.map_err(io::Error::other)??;
        if digest != descriptor.digest {
            return Err(io::Error::other("installed executable changed during inspection; retry without disrupting the running daemon"));
        }
        Ok(descriptor)
    }).await.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "installed executable inspection timed out"))??;
    Ok(InspectedBuild {
        program,
        descriptor: result,
    })
}

impl InspectedBuild {
    /// Freeze verified bytes under the store's private lifecycle directory.
    /// Renaming the installed path during a handoff cannot change what starts.
    pub fn freeze(&self, data_dir: &Path) -> io::Result<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let directory = data_dir
            .join("upgrades")
            .join(self.descriptor.digest.to_string());
        std::fs::create_dir_all(&directory)?;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
        let target = directory.join("nits");
        if target.is_file() && digest(&target)? == self.descriptor.digest {
            return Ok(target);
        }
        let temporary = directory.join(format!(
            ".nits-{}-{}",
            std::process::id(),
            fastrand::u64(..)
        ));
        let result = (|| {
            std::fs::copy(&self.program, &temporary)?;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o700))?;
            if digest(&temporary)? != self.descriptor.digest {
                return Err(io::Error::other(
                    "installed executable changed while preparing replacement",
                ));
            }
            std::fs::rename(&temporary, &target)?;
            Ok(target)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_identity_changes_with_bytes_and_frozen_bytes_remain_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("selected");
        std::fs::write(&program, b"first executable").unwrap();
        let mut descriptor = running().unwrap();
        descriptor.digest = digest(&program).unwrap();
        let candidate = InspectedBuild {
            program: program.clone(),
            descriptor,
        };
        let frozen = candidate.freeze(dir.path()).unwrap();
        std::fs::write(&program, b"second executable").unwrap();
        assert_eq!(digest(&frozen).unwrap(), candidate.descriptor.digest);
        assert_ne!(digest(&program).unwrap(), candidate.descriptor.digest);
        std::fs::remove_file(&frozen).unwrap();
        assert!(candidate.freeze(dir.path()).is_err());
        assert!(!frozen.exists());
    }
}
