use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tar::Archive;

const EMBEDDED_RUNTIME_MAGIC: [u8; 8] = *b"TPKGv1\0\0";
const EMBEDDED_RUNTIME_FOOTER_LEN: usize = 8 + 8 + 32;
static EMBEDDED_RUNTIME_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();

#[derive(Debug)]
struct EmbeddedRuntimePackage {
    payload: Vec<u8>,
}

#[derive(Debug)]
struct EmbeddedRuntimeFooter {
    payload_len: usize,
    digest: [u8; 32],
}

pub(crate) fn embedded_runtime_root() -> Option<PathBuf> {
    EMBEDDED_RUNTIME_ROOT
        .get_or_init(|| resolve_embedded_runtime_root().ok().flatten())
        .clone()
}

fn resolve_embedded_runtime_root() -> std::io::Result<Option<PathBuf>> {
    let Some(footer) = read_embedded_runtime_footer()? else {
        return Ok(None);
    };

    let Some(base_dir) = runtime_cache_base() else {
        return Ok(None);
    };

    let runtime_root = base_dir.join(hex_digest(&footer.digest));
    if runtime_is_ready(&runtime_root) {
        return Ok(Some(runtime_root));
    }

    let package = read_embedded_runtime_package(&footer)?;
    extract_embedded_runtime(&package, &runtime_root)?;
    Ok(Some(runtime_root))
}

fn runtime_cache_base() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".local/share/termphonic")
            .join("runtime")
    })
}

fn runtime_is_ready(root: &Path) -> bool {
    root.join("libexec/yt-dlp").is_file() && root.join("libexec/deno").is_file()
}

fn read_embedded_runtime_footer() -> std::io::Result<Option<EmbeddedRuntimeFooter>> {
    let executable = match std::env::current_exe() {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };

    let mut file = match std::fs::File::open(&executable) {
        Ok(file) => file,
        Err(_) => return Ok(None),
    };

    let file_size = file.metadata()?.len() as usize;
    if file_size < EMBEDDED_RUNTIME_FOOTER_LEN {
        return Ok(None);
    }

    file.seek(SeekFrom::End(-(EMBEDDED_RUNTIME_FOOTER_LEN as i64)))?;
    let mut footer = [0u8; EMBEDDED_RUNTIME_FOOTER_LEN];
    file.read_exact(&mut footer)?;

    if footer[..8] != EMBEDDED_RUNTIME_MAGIC {
        return Ok(None);
    }

    let payload_len = u64::from_le_bytes(footer[8..16].try_into().unwrap()) as usize;
    if payload_len > file_size.saturating_sub(EMBEDDED_RUNTIME_FOOTER_LEN) {
        return Ok(None);
    }

    let digest = footer[16..48].try_into().unwrap();
    Ok(Some(EmbeddedRuntimeFooter {
        payload_len,
        digest,
    }))
}

fn read_embedded_runtime_package(
    footer: &EmbeddedRuntimeFooter,
) -> std::io::Result<EmbeddedRuntimePackage> {
    let executable = std::env::current_exe()?;
    let mut file = std::fs::File::open(&executable)?;
    let file_size = file.metadata()?.len() as usize;
    let payload_len = footer.payload_len;
    if payload_len > file_size.saturating_sub(EMBEDDED_RUNTIME_FOOTER_LEN) {
        return Err(std::io::Error::other(
            "embedded runtime payload is truncated",
        ));
    }

    let payload_offset = (file_size - EMBEDDED_RUNTIME_FOOTER_LEN - payload_len) as u64;
    file.seek(SeekFrom::Start(payload_offset))?;
    let mut payload = vec![0u8; payload_len];
    file.read_exact(&mut payload)?;

    let digest = Sha256::digest(&payload);
    let digest_bytes: [u8; 32] = digest.into();
    if digest_bytes != footer.digest {
        return Err(std::io::Error::other(
            "embedded runtime payload hash mismatch",
        ));
    }

    Ok(EmbeddedRuntimePackage { payload })
}

fn extract_embedded_runtime(
    package: &EmbeddedRuntimePackage,
    runtime_root: &Path,
) -> std::io::Result<()> {
    if runtime_is_ready(runtime_root) {
        return Ok(());
    }

    let Some(parent) = runtime_root.parent() else {
        return Ok(());
    };
    std::fs::create_dir_all(parent)?;

    let temp_dir = parent.join(format!(
        ".{}.tmp-{}-{}",
        runtime_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("runtime"),
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir)?;
    }
    std::fs::create_dir_all(&temp_dir)?;

    let cursor = Cursor::new(package.payload.as_slice());
    let decoder = GzDecoder::new(cursor);
    let mut archive = Archive::new(decoder);
    archive.unpack(&temp_dir)?;

    if !runtime_is_ready(&temp_dir) {
        let _ = std::fs::remove_dir_all(&temp_dir);
        return Err(std::io::Error::other(
            "embedded runtime payload is incomplete",
        ));
    }

    match std::fs::rename(&temp_dir, runtime_root) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if runtime_is_ready(runtime_root) {
                let _ = std::fs::remove_dir_all(&temp_dir);
                Ok(())
            } else {
                let _ = std::fs::remove_dir_all(runtime_root);
                std::fs::rename(&temp_dir, runtime_root).or_else(|rename_error| {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    Err(rename_error)
                })
            }
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&temp_dir);
            Err(error)
        }
    }
}

fn hex_digest(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn get_yt_dlp_path() -> PathBuf {
    if let Some(path) = std::env::var_os("TERMPHONIC_YT_DLP") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return path;
        }
    }

    if let Some(runtime_root) = embedded_runtime_root() {
        let path = runtime_root.join("libexec/yt-dlp");
        if path.is_file() {
            return path;
        }
    }

    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            let portable_path = directory.join("libexec/yt-dlp");
            if portable_path.is_file() {
                return portable_path;
            }

            let installed_path = directory.join("../lib/termphonic/libexec/yt-dlp");
            if installed_path.is_file() {
                return installed_path;
            }
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        let user_path = PathBuf::from(home).join(".local/lib/termphonic/libexec/yt-dlp");
        if user_path.is_file() {
            return user_path;
        }
    }

    PathBuf::from("yt-dlp")
}

pub(crate) fn find_javascript_runtime() -> Option<(String, String)> {
    if let Some(path) = std::env::var_os("TERMPHONIC_DENO") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(("deno".to_string(), path.to_string_lossy().into_owned()));
        }
    }

    if let Some(runtime_root) = embedded_runtime_root() {
        let path = runtime_root.join("libexec/deno");
        if path.is_file() {
            return Some(("deno".to_string(), path.to_string_lossy().into_owned()));
        }
    }

    if let Ok(executable) = std::env::current_exe() {
        if let Some(directory) = executable.parent() {
            for path in [
                directory.join("libexec/deno"),
                directory.join("../lib/termphonic/libexec/deno"),
            ] {
                if path.is_file() {
                    return Some(("deno".to_string(), path.to_string_lossy().into_owned()));
                }
            }
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        let path = PathBuf::from(home).join(".local/lib/termphonic/libexec/deno");
        if path.is_file() {
            return Some(("deno".to_string(), path.to_string_lossy().into_owned()));
        }
    }

    for (runtime, binaries) in [
        ("deno", &["deno"][..]),
        ("node", &["node", "nodejs"][..]),
        ("quickjs", &["qjs", "quickjs"][..]),
        ("bun", &["bun"][..]),
    ] {
        for binary in binaries {
            if let Ok(output) = Command::new("which").arg(binary).output() {
                if output.status.success() {
                    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !path.is_empty() {
                        return Some((runtime.to_string(), path));
                    }
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::fs;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    #[test]
    fn extracts_embedded_runtime_payload() {
        let payload_root = unique_temp_dir("termphonic-payload");
        let extract_root = unique_temp_dir("termphonic-runtime");
        let _ = fs::remove_dir_all(&payload_root);
        let _ = fs::remove_dir_all(&extract_root);

        fs::create_dir_all(payload_root.join("libexec")).unwrap();
        fs::write(payload_root.join("libexec/yt-dlp"), b"yt-dlp").unwrap();
        fs::write(payload_root.join("libexec/deno"), b"deno").unwrap();

        let archive = {
            let encoder = GzEncoder::new(Vec::new(), Compression::default());
            let mut builder = tar::Builder::new(encoder);
            builder
                .append_path_with_name(payload_root.join("libexec/yt-dlp"), "libexec/yt-dlp")
                .unwrap();
            builder
                .append_path_with_name(payload_root.join("libexec/deno"), "libexec/deno")
                .unwrap();
            let encoder = builder.into_inner().unwrap();
            encoder.finish().unwrap()
        };

        let package = EmbeddedRuntimePackage { payload: archive };

        extract_embedded_runtime(&package, &extract_root).unwrap();
        assert!(extract_root.join("libexec/yt-dlp").is_file());
        assert!(extract_root.join("libexec/deno").is_file());

        let _ = fs::remove_dir_all(&payload_root);
        let _ = fs::remove_dir_all(&extract_root);
    }

    #[test]
    fn resolves_search_page_size_constant() {
        assert_eq!(crate::models::SEARCH_PAGE_SIZE, 20);
        let _ = crate::models::YtSearchResult {
            id: "id".to_string(),
            title: "title".to_string(),
            duration: None,
            channel: None,
        };
    }
}
