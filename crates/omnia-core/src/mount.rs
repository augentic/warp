//! # Preopen mounts and the mount registry
//!
//! A deployment may mount node-local directories into a guest's WASI sandbox via
//! `[[mount]]` so a guest can read or edit a mounted directory tree. Two
//! host-side artefacts back that capability:
//!
//! - [`ResolvedPreopen`] — a mount resolved to an absolute host path plus
//!   writability, applied per store via [`WasiCtxBuilder::preopened_dir`].
//! - [`MountRegistry`] — the host-side source of truth that maps a lent
//!   `wasi:filesystem` descriptor back to its mount by directory identity. It is
//!   built once at startup (opening each directory and capturing its identity),
//!   then shared read-only across every store. The registry — never the
//!   descriptor — supplies the resolved handle: a cap-std [`Dir`] for bounded
//!   operations and the absolute host path.
//!
//! A `wasi:filesystem` `Descriptor` is path-less by design (cap-std exposes no
//! API to recover a host path from a `Dir`), so the absolute path *must* come
//! from this registry, keyed by descriptor identity. Resolving a guest-lent
//! `borrow<descriptor>` into a usable handle is the job of the host crate that
//! defines that grant (e.g. `wasi-model`'s workspace tools), not of this crate.
//!
//! [`WasiCtxBuilder::preopened_dir`]: wasmtime_wasi::WasiCtxBuilder::preopened_dir

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context as _, Result};
use cap_fs_ext::MetadataExt as _;
use cap_std::ambient_authority;
use cap_std::fs::Dir;

/// A preopen mount resolved to an absolute host path and writability, ready
/// to be opened into the guest sandbox and recorded in the [`MountRegistry`].
///
/// Writability maps to WASI filesystem permissions only at the preopen call
/// site, keeping wasmtime types out of the mount API.
#[derive(Clone, Debug)]
pub struct ResolvedPreopen {
    /// Guest-visible name returned by `preopens.get-directories()`.
    pub name: String,
    /// Absolute host path of the mount root.
    pub host_path: PathBuf,
    /// Whether the mount permits writes; read-only otherwise.
    pub writable: bool,
}

impl ResolvedPreopen {
    /// Build a resolved preopen: read-only unless `writable`.
    #[must_use]
    pub const fn new(name: String, host_path: PathBuf, writable: bool) -> Self {
        Self {
            name,
            host_path,
            writable,
        }
    }
}

/// A single resolved mount: one record in the [`MountRegistry`].
///
/// Holds the cap-std [`Dir`] for bounded `read`/`list`/`write` confined to the
/// mount, plus the absolute `host_path` of the mount root. A lent descriptor is
/// matched back to this entry by directory identity (`identity`).
#[derive(Clone)]
pub struct Mount {
    /// Guest-visible name returned by `preopens.get-directories()`.
    pub name: String,
    /// Absolute host path of the mount root.
    pub host_path: PathBuf,
    /// Host-side capability handle to the mount root.
    pub dir: Arc<Dir>,
    /// Whether the mount permits writes; read-only otherwise.
    pub writable: bool,
    /// Directory identity `(device, inode)`, used to match a lent descriptor
    /// back to this entry.
    pub identity: (u64, u64),
}

/// The host-side registry of authorized mounts.
///
/// Built once at startup from the resolved preopens (opening each directory and
/// capturing its identity); shared read-only across every store via an `Arc`.
/// A consuming host crate matches a lent `borrow<descriptor>` against this
/// registry by directory identity, never trusting an OS path read out of the
/// descriptor.
#[derive(Clone, Default)]
pub struct MountRegistry {
    entries: Vec<Mount>,
}

impl MountRegistry {
    /// Open every resolved preopen, capturing its directory identity, and build
    /// the registry. This is the startup fail-fast gate: a mount whose host
    /// path cannot be opened as a directory (or stat-ed) is a configuration
    /// error surfaced before the registry is built.
    ///
    /// Preopens are first deduplicated by guest-visible name with last-wins
    /// semantics, so a CLI `--mount` layered after a manifest `[[mount]]` of the
    /// same name overrides it rather than being silently shadowed by wasi-libc's
    /// first-match path resolution. The overriding entry keeps the position of
    /// the first occurrence; only its value is replaced, and the discarded
    /// mount's host path is never opened.
    ///
    /// # Errors
    ///
    /// Returns an error if a mount's host path cannot be opened as a directory
    /// or its metadata cannot be read.
    pub fn open(preopens: Vec<ResolvedPreopen>) -> Result<Self> {
        let preopens = dedup_last_wins(preopens);
        let mut entries = Vec::with_capacity(preopens.len());
        for preopen in preopens {
            let dir = Dir::open_ambient_dir(&preopen.host_path, ambient_authority()).with_context(
                || format!("opening mount `{}` at {}", preopen.name, preopen.host_path.display()),
            )?;
            let meta = dir
                .dir_metadata()
                .with_context(|| format!("reading metadata for mount `{}`", preopen.name))?;
            entries.push(Mount {
                name: preopen.name,
                host_path: preopen.host_path,
                dir: Arc::new(dir),
                writable: preopen.writable,
                identity: (meta.dev(), meta.ino()),
            });
        }
        Ok(Self { entries })
    }

    /// The registered mounts — the WASI preopens to apply to each store.
    #[must_use]
    pub fn entries(&self) -> &[Mount] {
        &self.entries
    }

    /// The registered mount whose directory identity matches `(dev, ino)`, if
    /// any — how a lent descriptor selects its registry entry.
    #[must_use]
    pub fn match_identity(&self, dev: u64, ino: u64) -> Option<&Mount> {
        self.entries.iter().find(|entry| entry.identity == (dev, ino))
    }
}

/// Collapse preopens sharing a guest-visible name to a single entry, last-wins.
///
/// The winning entry keeps the position of the first occurrence of its name so
/// the overall order is stable; only its value is taken from the last
/// occurrence.
fn dedup_last_wins(preopens: Vec<ResolvedPreopen>) -> Vec<ResolvedPreopen> {
    let mut deduped: Vec<ResolvedPreopen> = Vec::with_capacity(preopens.len());
    let mut index: HashMap<String, usize> = HashMap::new();
    for preopen in preopens {
        if let Some(&position) = index.get(&preopen.name) {
            deduped[position] = preopen;
        } else {
            index.insert(preopen.name.clone(), deduped.len());
            deduped.push(preopen);
        }
    }
    deduped
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use cap_fs_ext::MetadataExt as _;
    use cap_std::ambient_authority;
    use cap_std::fs::Dir;

    use super::{MountRegistry, ResolvedPreopen};

    /// A fresh, empty temp directory unique to this process and `label`.
    fn temp_root(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omnia-reg-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("creating temp mount root");
        dir
    }

    /// `(dev, ino)` of `path`, computed independently of the registry under test.
    fn identity_of(path: &Path) -> (u64, u64) {
        let dir = Dir::open_ambient_dir(path, ambient_authority()).expect("opening dir");
        let meta = dir.dir_metadata().expect("reading metadata");
        (meta.dev(), meta.ino())
    }

    fn registry(name: &str, path: &Path, writable: bool) -> MountRegistry {
        MountRegistry::open(vec![ResolvedPreopen::new(
            name.to_owned(),
            path.to_path_buf(),
            writable,
        )])
        .expect("opening registry")
    }

    #[test]
    fn open_identity() {
        let root = temp_root("identity");
        let registry = registry(".", &root, false);

        let entry = &registry.entries()[0];
        assert_eq!(entry.name, ".");
        assert_eq!(entry.host_path, root);
        assert!(!entry.writable, "a mount defaults to read-only");
        assert_eq!(entry.identity, identity_of(&root), "the entry records the dir's (dev, ino)");
    }

    #[test]
    fn match_identity_by_dev_ino() {
        let root = temp_root("select");
        let registry = registry(".", &root, false);
        let (dev, ino) = identity_of(&root);

        let hit = registry.match_identity(dev, ino).expect("the mount's identity matches");
        assert_eq!(hit.host_path, root);
        // A foreign identity matches nothing — the out-of-scope rejection a
        // consuming host relies on.
        assert!(registry.match_identity(dev ^ 0xFFFF, ino ^ 0xFFFF).is_none());
    }

    #[test]
    fn open_records_writability() {
        let root = temp_root("writable");
        let registry = registry("tree", &root, true);

        let entry = &registry.entries()[0];
        assert!(entry.writable, "a writable mount permits mutation");
    }

    #[test]
    fn duplicate_name_last_wins() {
        let manifest_root = temp_root("dup-manifest");
        let cli_root = temp_root("dup-cli");
        // Same guest-visible name, distinct host paths: the CLI entry is layered
        // last and must override the manifest entry rather than be shadowed.
        let registry = MountRegistry::open(vec![
            ResolvedPreopen::new(".".to_owned(), manifest_root, false),
            ResolvedPreopen::new(".".to_owned(), cli_root.clone(), true),
        ])
        .expect("opening registry");

        assert_eq!(registry.entries().len(), 1, "duplicate names collapse to one entry");
        let entry = &registry.entries()[0];
        assert_eq!(entry.host_path, cli_root, "the last entry wins");
        assert!(entry.writable, "the winning entry's permissions apply");
    }

    #[test]
    fn overridden_mount_path() {
        let cli_root = temp_root("override-cli");
        // The shadowed manifest entry names a nonexistent path; last-wins dedup
        // discards it before opening, so a stale override does not fail startup.
        let registry = MountRegistry::open(vec![
            ResolvedPreopen::new(".".to_owned(), PathBuf::from("/no/such/mount"), false),
            ResolvedPreopen::new(".".to_owned(), cli_root.clone(), true),
        ])
        .expect("the discarded entry's bad path is never opened");

        assert_eq!(registry.entries()[0].host_path, cli_root);
    }
}
