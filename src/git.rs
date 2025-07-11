use super::errors::*;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

pub fn bundle_create(bundle_path: &Path, ref_name: &str) -> Result<()> {
    let status = Command::new("git")
        .arg("bundle")
        .arg("create")
        .arg(bundle_path)
        .arg(ref_name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .chain_err(|| "failed to execute git bundle create")?;

    if !status.success() {
        bail!(
            "git bundle create failed with status: {}",
            status
        );
    }
    Ok(())
}

pub fn bundle_unbundle(bundle_path: &Path, ref_name: &str) -> Result<()> {
    let status = Command::new("git")
        .arg("bundle")
        .arg("unbundle")
        .arg(bundle_path)
        .arg(ref_name)
        // This is the critical part: redirect the command's stdout and stderr
        // so they don't interfere with the remote helper protocol.
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .chain_err(|| "failed to execute git bundle unbundle")?;

    if !status.success() {
        bail!(
            "git bundle unbundle failed with status: {}",
            status
        );
    }
    Ok(())
}

pub fn is_ancestor(base_ref: &str, remote_ref: &str) -> Result<bool> {
    let result = Command::new("git")
        .arg("merge-base")
        .arg("--is-ancestor")
        .arg(remote_ref)
        .arg(base_ref)
        .output()
        .chain_err(|| "failed to run git")?;
    Ok(result.status.success())
}

pub fn config(setting: &str) -> Result<String> {
    let result = Command::new("git")
        .arg("config")
        .arg(setting)
        .output()
        .chain_err(|| "failed to run git")?;
    if !result.status.success() {
        bail!("git config failed");
    }
    let s = String::from_utf8(result.stdout).chain_err(|| "not utf8")?;
    Ok(s.trim().to_string())
}

pub fn rev_parse(rev: &str) -> Result<String> {
    let result = Command::new("git")
        .arg("rev-parse")
        .arg(rev)
        .output()
        .chain_err(|| "failed to run git")?;
    if !result.status.success() {
        bail!("git rev-parse failed");
    }
    let s = String::from_utf8(result.stdout).chain_err(|| "not utf8")?;
    Ok(s.trim().to_string())
}
