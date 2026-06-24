// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

//! Helpers for change-detection on Kubernetes ConfigMap / Secret mounts.
//!
//! # Why this module exists
//!
//! A projected ConfigMap/Secret mount does **not** lay the keys down as
//! plain files. kubelet builds the directory like this:
//!
//! ```text
//!   <mount>/inference-policy.json -> ..data/inference-policy.json   (symlink)
//!   <mount>/..data                -> ..2026_06_25_00_00_00.123      (symlink)
//!   <mount>/..2026_06_25_00_00_00.123/inference-policy.json         (real file)
//! ```
//!
//! On every update kubelet writes a brand-new timestamped directory and
//! then **atomically swaps the `..data` symlink** to point at it. Crucially,
//! the per-key symlink (`inference-policy.json`) is created **once** and is
//! never recreated on subsequent updates — only its target changes.
//!
//! This breaks the obvious mtime-poll. [`std::fs::DirEntry::metadata`] is an
//! `lstat`: it returns the metadata of the **symlink itself**, whose mtime is
//! frozen at pod start. So a poll built on `DirEntry::metadata().modified()`
//! returns the same value forever and **never detects a ConfigMap update** —
//! the router silently keeps enforcing the boot-time policy until the pod is
//! restarted. (This was a real production bug: live `kars policy` / prompt-
//! shields / egress / memory edits required a pod bump to take effect.)
//!
//! The fix is to **follow the symlink** with [`std::fs::metadata`] (a `stat`),
//! which resolves through `..data` to the real file whose mtime advances on
//! every kubelet swap. Every router watcher MUST use this helper instead of a
//! hand-rolled `DirEntry::metadata()` loop so the bug cannot reappear in one
//! loader while being fixed in another.

use std::path::Path;
use std::time::SystemTime;

/// Maximum mtime across files in `dir` whose extension is one of `exts`,
/// **following symlinks** so Kubernetes ConfigMap/Secret `..data` swaps are
/// detected.
///
/// Returns `None` when `dir` is not a directory or contains no matching,
/// resolvable file. A dangling symlink (target temporarily absent mid-swap)
/// is skipped rather than treated as a change.
#[must_use]
pub fn dir_max_mtime(dir: &str, exts: &[&str]) -> Option<SystemTime> {
    let path = Path::new(dir);
    if !path.is_dir() {
        return None;
    }
    std::fs::read_dir(path)
        .ok()?
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| exts.iter().any(|want| x == *want))
        })
        // `std::fs::metadata` (stat) FOLLOWS symlinks — unlike
        // `DirEntry::metadata` (lstat). This is the whole point: a ConfigMap
        // `..data` swap changes the symlink *target's* mtime, not the
        // user-facing symlink's own mtime, so we must stat through it.
        .filter_map(|e| std::fs::metadata(e.path()).ok()?.modified().ok())
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn none_for_missing_dir() {
        assert!(dir_max_mtime("/nonexistent/definitely/not/here", &["json"]).is_none());
    }

    #[test]
    fn respects_extension_filter() {
        let d = tempdir().unwrap();
        std::fs::write(d.path().join("note.txt"), b"x").unwrap();
        assert!(
            dir_max_mtime(d.path().to_str().unwrap(), &["json"]).is_none(),
            "non-matching extension must be ignored"
        );
        std::fs::write(d.path().join("p.json"), b"{}").unwrap();
        assert!(dir_max_mtime(d.path().to_str().unwrap(), &["json"]).is_some());
    }

    #[test]
    fn matches_multiple_extensions() {
        let d = tempdir().unwrap();
        std::fs::write(d.path().join("agt-profile.yaml"), b"a: 1").unwrap();
        assert!(dir_max_mtime(d.path().to_str().unwrap(), &["yaml", "yml"]).is_some());
    }

    #[test]
    fn detects_plain_file_change() {
        let d = tempdir().unwrap();
        let f = d.path().join("p.json");
        std::fs::write(&f, b"A").unwrap();
        let before = dir_max_mtime(d.path().to_str().unwrap(), &["json"]).unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(&f, b"BB").unwrap();
        let after = dir_max_mtime(d.path().to_str().unwrap(), &["json"]).unwrap();
        assert!(after > before, "a plain file rewrite must bump max-mtime");
    }

    /// The regression test for the production bug. Replicates the kubelet
    /// ConfigMap layout (per-key symlink -> `..data` -> timestamped dir) and
    /// performs the **atomic `..data` symlink swap** kubelet does on update.
    ///
    /// The old `DirEntry::metadata()` (lstat) implementation returns the
    /// frozen mtime of the unchanged per-key symlink, so `after == before`
    /// and the assertion FAILS — exactly the silent "no hot-reload" bug.
    /// Following the symlink (this helper) detects the swap and the
    /// assertion PASSES.
    #[cfg(unix)]
    #[test]
    fn detects_configmap_data_symlink_swap() {
        use std::os::unix::fs::symlink;
        let root = tempdir().unwrap();
        let rp = root.path();

        // Generation A: the dir + policy file kubelet first writes.
        let gen_a = rp.join("gen_a");
        std::fs::create_dir(&gen_a).unwrap();
        std::fs::write(gen_a.join("policy.json"), b"A").unwrap();
        // `..data` -> gen_a (the symlink kubelet atomically re-points).
        symlink("gen_a", rp.join("..data")).unwrap();
        // User-facing key -> ..data/policy.json. Created ONCE; never
        // recreated on update — which is why lstat-on-it is frozen.
        symlink("..data/policy.json", rp.join("policy.json")).unwrap();

        let before = dir_max_mtime(rp.to_str().unwrap(), &["json"])
            .expect("policy.json must resolve through the symlink chain");

        // Guarantee a strictly-later mtime regardless of filesystem
        // timestamp granularity (ext4/HFS+ can be 1s).
        std::thread::sleep(Duration::from_millis(1100));

        // kubelet update: write generation B, then atomically swap ..data.
        let gen_b = rp.join("gen_b");
        std::fs::create_dir(&gen_b).unwrap();
        std::fs::write(gen_b.join("policy.json"), b"BB").unwrap();
        let tmp = rp.join("..data_tmp");
        symlink("gen_b", &tmp).unwrap();
        std::fs::rename(&tmp, rp.join("..data")).unwrap(); // atomic swap

        let after = dir_max_mtime(rp.to_str().unwrap(), &["json"])
            .expect("policy.json must still resolve after the swap");

        assert!(
            after > before,
            "dir_max_mtime must detect a ConfigMap `..data` symlink swap; \
             this is the regression guard for the lstat-on-symlink hot-reload bug"
        );
    }
}
