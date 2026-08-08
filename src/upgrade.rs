// Copyright (C) 2026 huangdihd
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Self-update: fetch the latest release asset for this platform and replace
//! the running binary. `programmer upgrade` is the counterpart of
//! `install.sh` / `install.ps1`, which place the binary from the same assets.

use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use color_eyre::eyre::{Context, Result, bail, eyre};
use serde::Deserialize;

use crate::cli::{UninstallArgs, UpgradeArgs};

const API_LATEST: &str = "https://api.github.com/repos/huangdihd/programmer/releases/latest";
const DOWNLOAD_BASE: &str = "https://github.com/huangdihd/programmer/releases/download";
const USER_AGENT: &str = concat!("programmer/", env!("CARGO_PKG_VERSION"), " (self-update)");
const CONFIG_DIR_NAME: &str = "programmer";

/// Human-friendly version comparison: `newer(a, b)` is true when `a` tags a
/// later release than `b`. Used by both the CLI and the startup check.
fn newer(a: &str, b: &str) -> bool {
    parse_version(a) > parse_version(b)
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
}

/// Version triple, e.g. `v0.2.0` -> (0, 2, 0). Non-numeric segments are 0.
fn parse_version(tag: &str) -> (u64, u64, u64) {
    let trimmed = tag.trim_start_matches('v');
    let mut parts = trimmed.split('.');
    let get = |part: Option<&str>| part.and_then(|s| s.parse().ok()).unwrap_or(0);
    (get(parts.next()), get(parts.next()), get(parts.next()))
}

fn asset_target() -> Result<&'static str> {
    let os = env::consts::OS;
    let arch = env::consts::ARCH;
    match (os, arch) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("linux", "arm") => Ok("armv7-unknown-linux-gnueabihf"),
        ("linux", "riscv64") => Ok("riscv64gc-unknown-linux-gnu"),
        ("linux", "x86") => Ok("i686-unknown-linux-gnu"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Ok("aarch64-pc-windows-msvc"),
        ("windows", "x86") => Ok("i686-pc-windows-msvc"),
        _ => bail!("no prebuilt release asset for {os}/{arch}"),
    }
}

async fn fetch_latest_tag() -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(USER_AGENT)
        .build()
        .wrap_err("cannot build HTTP client")?;
    let release: Release = client
        .get(API_LATEST)
        .send()
        .await
        .wrap_err("cannot reach the GitHub releases API")?
        .error_for_status()
        .wrap_err("GitHub releases API returned an error")?
        .json()
        .await
        .wrap_err("cannot parse the GitHub releases API response")?;
    Ok(release.tag_name)
}

async fn download(client: &reqwest::Client, url: &str, dest: &Path) -> Result<()> {
    let mut response = client
        .get(url)
        .send()
        .await?
        .error_for_status()
        .wrap_err_with(|| format!("download failed for {url}"))?;
    let mut file = fs::File::create(dest).wrap_err("cannot create download file")?;
    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk).wrap_err("cannot write download")?;
    }
    Ok(())
}

/// Extract the archive into `dir`. Uses the system `tar`, which handles both
/// `.tar.gz` (unix) and `.zip` (Windows 10 1803+ ships bsdtar).
fn extract(archive: &Path, dir: &Path, target: &str) -> Result<()> {
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive)
        .arg("-C")
        .arg(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .wrap_err_with(|| {
            format!("cannot run `tar` to unpack the {target} release (a system tar is required)")
        })?;
    if !status.success() {
        bail!("`tar` failed to unpack the {target} release");
    }
    Ok(())
}

fn binary_name() -> &'static str {
    if env::consts::OS == "windows" {
        "programmer.exe"
    } else {
        "programmer"
    }
}

/// Verify the freshly unpacked binary runs, then atomically replace the
/// current executable.
fn install_replacement(new_binary: &Path, current_exe: &Path) -> Result<()> {
    // The unpacked archive keeps the original permissions; be explicit anyway.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(new_binary)
            .wrap_err("cannot stat new binary")?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(new_binary, perms).wrap_err("cannot chmod new binary")?;
    }

    let probe = Command::new(new_binary)
        .arg("--version")
        .output()
        .wrap_err("cannot run the downloaded binary")?;
    if !probe.status.success() {
        bail!(
            "downloaded binary failed its version check: {}",
            String::from_utf8_lossy(&probe.stderr).trim()
        );
    }

    if env::consts::OS == "windows" {
        install_replacement_windows(new_binary, current_exe)
    } else {
        fs::rename(new_binary, current_exe).wrap_err_with(|| {
            format!(
                "cannot replace {} (permission denied?)",
                current_exe.display()
            )
        })
    }
}

/// A running Windows executable cannot be overwritten while it is loaded.
/// Stage the new binary next to it and let a short-lived `.cmd` script finish
/// the swap after this process has exited.
#[cfg(windows)]
fn install_replacement_windows(new_binary: &Path, current_exe: &Path) -> Result<()> {
    let dir = current_exe
        .parent()
        .ok_or_else(|| eyre!("cannot determine the install directory"))?;
    let staged = dir.join(format!("programmer.upgrade.{}.exe", std::process::id()));
    fs::copy(new_binary, &staged).wrap_err("cannot stage the new binary")?;

    let script = dir.join("programmer-upgrade.cmd");
    let mut file = fs::File::create(&script).wrap_err("cannot create the upgrade script")?;
    writeln!(file, "@echo off")?;
    writeln!(file, "ping -n 3 127.0.0.1 >nul")?; // wait for this process to exit
    writeln!(
        file,
        "move /y \"{}\" \"{}\" >nul 2>&1",
        staged.display(),
        current_exe.display()
    )?;
    writeln!(file, "del \"{}\" >nul 2>&1", script.display())?;
    drop(file);

    // Detach the script; the parent exits right after this call returns.
    Command::new("cmd")
        .args(["/c", "start", "/b", ""])
        .arg(&script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .wrap_err("cannot launch the upgrade script")?;

    println!(
        "The new version is staged. It will replace programmer on the next command — restart the terminal to finish."
    );
    Ok(())
}

#[cfg(not(windows))]
fn install_replacement_windows(_: &Path, _: &Path) -> Result<()> {
    unreachable!("windows-only helper compiled on a non-windows target")
}

pub(crate) async fn upgrade(args: UpgradeArgs) -> Result<bool> {
    let current = env!("CARGO_PKG_VERSION");
    let target = asset_target()?;

    let tag = match &args.tag {
        Some(tag) => tag.clone(),
        None => fetch_latest_tag().await?,
    };
    if !newer(&tag, current) {
        println!("programmer is up to date ({current})");
        return Ok(true);
    }

    if args.check {
        println!("a newer version is available: {tag} (current: {current})");
        return Ok(true);
    }

    let current_exe = env::current_exe().wrap_err("cannot locate the running binary")?;
    let current_exe = fs::canonicalize(&current_exe).unwrap_or(current_exe);
    let install_dir = current_exe
        .parent()
        .ok_or_else(|| eyre!("cannot determine the install directory"))?;
    if !install_dir.is_dir() {
        bail!(
            "install directory does not exist: {}",
            install_dir.display()
        );
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent(USER_AGENT)
        .build()
        .wrap_err("cannot build HTTP client")?;

    let ext = if env::consts::OS == "windows" {
        "zip"
    } else {
        "tar.gz"
    };
    let url = format!("{DOWNLOAD_BASE}/{tag}/programmer-{target}.{ext}");

    let work = env::temp_dir().join(format!("programmer-upgrade-{}", std::process::id()));
    fs::create_dir_all(&work).wrap_err("cannot create a temporary directory")?;
    let archive = work.join(format!("programmer-{target}.{ext}"));
    let unpacked = work.join("unpacked");
    fs::create_dir_all(&unpacked).wrap_err("cannot create a temporary directory")?;

    println!("Downloading {url}");
    let result = async {
        download(&client, &url, &archive).await?;
        extract(&archive, &unpacked, target)?;
        let new_binary = unpacked.join(binary_name());
        if !new_binary.exists() {
            bail!("the release archive does not contain {}", binary_name());
        }
        install_replacement(&new_binary, &current_exe)?;
        println!("Updated programmer to {tag}");
        Ok(())
    }
    .await;

    let _ = fs::remove_dir_all(&work);
    result?;
    Ok(true)
}

/// Check the latest GitHub release without installing anything.
///
/// Returns the newest tag when a newer release exists, `None` when already up
/// to date. Errors are swallowed — the startup check must never block or
/// interrupt the TUI.
pub(crate) async fn check_for_update() -> Option<String> {
    let latest = fetch_latest_tag().await.ok()?;
    newer(&latest, env!("CARGO_PKG_VERSION")).then_some(latest)
}

/// Remove programmer from this machine.
///
/// Deletes the running executable. With `--purge`, also removes the
/// `~/.config/programmer` directory (config, sessions, skills, …).
pub(crate) async fn uninstall(args: UninstallArgs) -> Result<bool> {
    let current_exe = env::current_exe().wrap_err("cannot locate the running binary")?;
    let current_exe = fs::canonicalize(&current_exe).unwrap_or(current_exe);

    println!("Removing {}", current_exe.display());
    if env::consts::OS == "windows" {
        uninstall_windows(&current_exe)?;
    } else {
        fs::remove_file(&current_exe)
            .wrap_err_with(|| format!("cannot remove {}", current_exe.display()))?;
    }

    if args.purge {
        if let Some(config_dir) = dirs::config_dir() {
            let dir = config_dir.join(CONFIG_DIR_NAME);
            if dir.exists() {
                println!("Removing {}", dir.display());
                fs::remove_dir_all(&dir)
                    .wrap_err_with(|| format!("cannot remove {}", dir.display()))?;
            }
        }
    } else {
        println!("Config and sessions were kept. Re-run with --purge to remove them as well.");
    }

    println!("programmer has been uninstalled.");
    Ok(true)
}

/// A running Windows executable cannot delete itself. Queue the removal via a
/// short-lived `.cmd` script that runs after this process exits.
#[cfg(windows)]
fn uninstall_windows(current_exe: &Path) -> Result<()> {
    let dir = current_exe
        .parent()
        .ok_or_else(|| eyre!("cannot determine the install directory"))?;
    let script = dir.join("programmer-uninstall.cmd");
    let mut file = fs::File::create(&script).wrap_err("cannot create the uninstall script")?;
    writeln!(file, "@echo off")?;
    writeln!(file, "ping -n 3 127.0.0.1 >nul")?; // wait for this process to exit
    writeln!(file, "del /f /q \"{}\" >nul 2>&1", current_exe.display())?;
    writeln!(file, "del \"{}\" >nul 2>&1", script.display())?;
    drop(file);

    Command::new("cmd")
        .args(["/c", "start", "/b", ""])
        .arg(&script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .wrap_err("cannot launch the uninstall script")?;
    println!(
        "programmer.exe will be removed after this process exits — close the terminal to finish."
    );
    Ok(())
}

#[cfg(not(windows))]
fn uninstall_windows(_: &Path) -> Result<()> {
    unreachable!("windows-only helper compiled on a non-windows target")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parsing_and_comparison() {
        assert_eq!(parse_version("v0.2.0"), (0, 2, 0));
        assert_eq!(parse_version("0.2.0"), (0, 2, 0));
        assert_eq!(parse_version("v1.10.0"), (1, 10, 0));
        // Missing / non-numeric segments count as zero, so a version with a
        // suffix still parses deterministically.
        assert_eq!(parse_version("v0.2"), (0, 2, 0));
        assert_eq!(parse_version("v0.2.0-beta.1"), (0, 2, 0));

        assert!(newer("v0.3.0", "v0.2.0"));
        assert!(newer("v1.0.0", "v0.9.9"));
        assert!(newer("v0.2.10", "v0.2.9"));
        assert!(!newer("v0.2.0", "v0.2.0"));
        assert!(!newer("v0.2.0", "v0.3.0"));
    }

    #[test]
    fn current_platform_has_a_release_target() {
        // The current build platform must map to one of the release matrix
        // targets, otherwise `upgrade` is unusable here.
        let target = asset_target().expect("current platform should map to a release target");
        assert!(
            target.ends_with("-apple-darwin")
                || target.ends_with("-linux-gnu")
                || target.contains("pc-windows-msvc")
        );
    }

    #[test]
    fn binary_name_matches_platform() {
        let name = binary_name();
        if cfg!(windows) {
            assert_eq!(name, "programmer.exe");
        } else {
            assert_eq!(name, "programmer");
        }
    }
}
