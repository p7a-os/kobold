//! On-demand adapter downloader and installer.
//!
//! Downloads and installs individual adapters (`kobold-adapter-acp`, `kobold-openai`,
//! `kobold-adapter-tmux`) chosen by the user into `~/.local/bin/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Determine target install directory (`~/.local/bin`).
pub fn target_install_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("KOBOLD_INSTALL_DIR") {
        PathBuf::from(dir)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/bin")
    } else {
        PathBuf::from("/usr/local/bin")
    }
}

/// Check if an adapter binary is already available and resolvable.
pub fn is_adapter_installed(adapter: &str) -> bool {
    if let Some(p) = kobold_core::sandbox::resolve(adapter) {
        if p.is_file() {
            return true;
        }
    }

    let target_dir = target_install_dir();
    let dest = target_dir.join(adapter);
    dest.is_file()
}

/// Detect platform OS name: darwin | linux
pub fn platform_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    }
}

/// Detect platform arch name: aarch64 | x86_64
pub fn platform_arch() -> &'static str {
    let arch = std::env::consts::ARCH;
    match arch {
        "x86_64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        _ => "x86_64",
    }
}

/// Install an adapter on demand.
pub async fn install_adapter(adapter: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let install_dir = target_install_dir();
    fs::create_dir_all(&install_dir)?;
    let dest_path = install_dir.join(adapter);

    if is_adapter_installed(adapter) {
        if dest_path.is_file() {
            return Ok(dest_path);
        }
        if let Some(p) = kobold_core::sandbox::resolve(adapter) {
            return Ok(p);
        }
    }

    // 1. Check if running in repo build with target/release or sibling directory
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            let candidate = parent.join(adapter);
            if candidate.is_file() {
                let _ = fs::copy(&candidate, &dest_path);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(&dest_path, fs::Permissions::from_mode(0o755));
                }
                println!(
                    "\x1b[32m✓\x1b[0m Installed {adapter} from local build to {}",
                    dest_path.display()
                );
                return Ok(dest_path);
            }
        }
    }

    for rel in ["target/release", "target/debug"] {
        let candidate = Path::new(rel).join(adapter);
        if candidate.is_file() {
            let _ = fs::copy(&candidate, &dest_path);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&dest_path, fs::Permissions::from_mode(0o755));
            }
            println!(
                "\x1b[32m✓\x1b[0m Installed {adapter} from local build to {}",
                dest_path.display()
            );
            return Ok(dest_path);
        }
    }

    // 2. Download from GitHub Release archive
    let os_name = platform_os();
    let arch_name = platform_arch();
    let archive_url = format!(
        "https://github.com/p7a-os/kobold/releases/latest/download/kobold-{os_name}-{arch_name}.tar.gz"
    );

    println!("\x1b[36minfo:\x1b[0m Downloading {adapter} from {archive_url}...");

    let tmp_dir = tempfile::tempdir()?;
    let tar_path = tmp_dir.path().join("kobold.tar.gz");

    // Download via curl (available on macOS and Linux)
    let status = Command::new("curl")
        .arg("-fsSL")
        .arg("-o")
        .arg(&tar_path)
        .arg(&archive_url)
        .status()?;

    if !status.success() || !tar_path.is_file() {
        // If curl failed (e.g. offline / no release yet), build locally via cargo if available
        if Command::new("cargo").arg("--version").status().is_ok() {
            println!("\x1b[33mwarning:\x1b[0m Prebuilt archive unavailable; building {adapter} from workspace...");
            let cargo_status = Command::new("cargo")
                .arg("build")
                .arg("--release")
                .arg("-p")
                .arg(adapter)
                .status()?;
            if cargo_status.success() {
                let built_bin = Path::new("target/release").join(adapter);
                if built_bin.is_file() {
                    fs::copy(&built_bin, &dest_path)?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = fs::set_permissions(&dest_path, fs::Permissions::from_mode(0o755));
                    }
                    println!("\x1b[32m✓\x1b[0m Compiled and installed {adapter}");
                    return Ok(dest_path);
                }
            }
        }
        return Err(format!("Failed to download {adapter} from {archive_url}").into());
    }

    // Extract archive
    let extract_status = Command::new("tar")
        .arg("-xzf")
        .arg(&tar_path)
        .arg("-C")
        .arg(tmp_dir.path())
        .status()?;

    if !extract_status.success() {
        return Err(format!("Failed to unpack release archive for {adapter}").into());
    }

    let extracted_bin = tmp_dir.path().join(adapter);
    if extracted_bin.is_file() {
        fs::copy(&extracted_bin, &dest_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dest_path, fs::Permissions::from_mode(0o755));
        }
        println!(
            "\x1b[32m✓\x1b[0m Installed {adapter} to {}",
            dest_path.display()
        );
        Ok(dest_path)
    } else {
        Err(format!("Binary '{adapter}' not found in downloaded release archive").into())
    }
}

/// Ensure all adapters required by the selected agents and providers are installed.
pub async fn install_required_adapters(
    selected_agents: &[String],
    selected_providers: &[String],
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut installed = Vec::new();

    if !selected_agents.is_empty() && !is_adapter_installed("kobold-adapter-acp") {
        install_adapter("kobold-adapter-acp").await?;
        installed.push("kobold-adapter-acp".to_string());
    }

    if selected_providers
        .iter()
        .any(|p| p == "openai" || p == "openrouter")
        && !is_adapter_installed("kobold-openai")
    {
        install_adapter("kobold-openai").await?;
        installed.push("kobold-openai".to_string());
    }

    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_strings_are_valid() {
        assert!(["darwin", "linux"].contains(&platform_os()));
        assert!(["aarch64", "x86_64"].contains(&platform_arch()));
    }
}
