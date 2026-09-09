//! A manifest-driven command deployment run over a backend bundle.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use omnia::{
    DeploymentBuilder, ExitStatus, GuestEntry, Host, Location, Manifest, ManifestSource, Mode,
    Mount, Plugins, Provides, Runtime, Server, SourceSpec, StoreCtx, WasiPlugins, Wiring,
};
use omnia_wasi_otel::WasiOtel;

/// One command-mode deployment: guests, mounts, arguments, the link
/// interfaces the host mediates, plugin locations, and the directory the `.`
/// path location serves.
///
/// Built from nothing, or as an overlay on the manifest a production
/// `runtime!` compiled in (`Deployment::from(runtime::manifest())`): the
/// builder methods add to that base, `command` re-marks its command guest,
/// `path_root` rewrites its `.` path location. Drive it through the
/// generated wiring with [`run_with`](Self::run_with), or link hosts by hand
/// with [`run`](Self::run).
///
/// ```no_run
/// use omnia::ExitStatus;
/// use omnia_test::host::{Backends, Deployment, scratch};
///
/// # async fn example(requester: &'static str, plugin: &'static str) -> anyhow::Result<()> {
/// let scratch = scratch();
/// std::fs::copy(plugin, scratch.path().join("plugin.wasm"))?;
/// let status = Deployment::new()
///     .link(["acme:tools/ops"])
///     .guest("requester", requester)
///     .mount(scratch.mount(false))
///     .path_root(scratch.path())
///     .run(Backends::defaults().await, |_| Ok(()))
///     .await?;
/// assert_eq!(status, ExitStatus::SUCCESS);
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Default)]
pub struct Deployment {
    base: Option<ManifestSource>,
    guests: Vec<GuestEntry>,
    command: Option<String>,
    mounts: Vec<Mount>,
    args: Vec<String>,
    link: Vec<String>,
    locations: Vec<Location>,
    path_root: Option<PathBuf>,
}

impl From<ManifestSource> for Deployment {
    fn from(base: ManifestSource) -> Self {
        Self {
            base: Some(base),
            ..Self::default()
        }
    }
}

impl Deployment {
    /// An empty deployment.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a guest under `id` from a component path or embedded bytes.
    #[must_use]
    pub fn guest(mut self, id: impl Into<String>, source: impl Into<SourceSpec>) -> Self {
        self.guests.push(GuestEntry::new(id, source));
        self
    }

    /// Marks `id` as the `wasi:cli/run` target (unmarking any the base
    /// manifest marked); without it the sole exporter is the catch-all.
    #[must_use]
    pub fn command(mut self, id: impl Into<String>) -> Self {
        self.command = Some(id.into());
        self
    }

    /// Preopens `mount` into the guest sandbox.
    #[must_use]
    pub fn mount(mut self, mount: Mount) -> Self {
        self.mounts.push(mount);
        self
    }

    /// Preopens every mount into the guest sandbox.
    #[must_use]
    pub fn mounts(mut self, mounts: impl IntoIterator<Item = Mount>) -> Self {
        self.mounts.extend(mounts);
        self
    }

    /// The operator's arguments (the runtime supplies `argv[0]`).
    #[must_use]
    pub fn args<S: Into<String>>(mut self, args: impl IntoIterator<Item = S>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Interfaces the host mediates between guests.
    #[must_use]
    pub fn link<S: Into<String>>(mut self, interfaces: impl IntoIterator<Item = S>) -> Self {
        self.link.extend(interfaces.into_iter().map(Into::into));
        self
    }

    /// Plugin acquisition locations (`[[plugin.location]]`).
    #[must_use]
    pub fn locations(mut self, locations: impl IntoIterator<Item = Location>) -> Self {
        self.locations.extend(locations);
        self
    }

    /// Serves path loads (`Location::Path`) from `dir` as the `.` location,
    /// replacing the base manifest's `.` root when it declares one.
    #[must_use]
    pub fn path_root(mut self, dir: impl AsRef<Path>) -> Self {
        self.path_root = Some(dir.as_ref().to_path_buf());
        self
    }

    /// The manifest this overlay describes.
    ///
    /// # Errors
    ///
    /// Returns an error if a `config:` base manifest cannot be loaded.
    pub fn manifest(&self) -> Result<Manifest> {
        let base = self.base.clone().map(ManifestSource::into_manifest).transpose()?;
        let mut manifest = base
            .unwrap_or_default()
            .mounts(self.mounts.iter().cloned())
            .link(self.link.iter().cloned())
            .locations(self.locations.iter().cloned());
        for guest in &self.guests {
            manifest = manifest.guest(guest.clone());
        }
        if let Some(command) = &self.command {
            for guest in &mut manifest.guests {
                guest.command = guest.id == *command;
            }
        }
        if let Some(root) = &self.path_root {
            let dot = manifest.plugin.locations.iter_mut().find_map(|location| match location {
                Location::Path { name, path } if name == "." => Some(path),
                _ => None,
            });
            match dot {
                Some(path) => path.clone_from(root),
                None => manifest.plugin.locations.push(Location::path(".", root.clone())),
            }
        }
        Ok(manifest)
    }

    fn builder(&self) -> Result<DeploymentBuilder> {
        Ok(DeploymentBuilder::new()
            .manifest(self.manifest()?)
            .mode(Mode::Command)
            .args(self.args.clone()))
    }

    /// Assembles the runtime by hand: builds the deployment, links the
    /// plugin host when locations are declared and the caller's hosts
    /// through `link`, installs the declared locations, and wires the
    /// link serve side.
    ///
    /// # Errors
    ///
    /// Returns an error if the deployment cannot be built or linked, a path
    /// location cannot be opened, or the link serve side cannot be wired.
    pub async fn boot<B>(
        &self, backends: B, link: impl FnOnce(&mut omnia::Deployment<StoreCtx<B>>) -> Result<()>,
    ) -> Result<Runtime<B>>
    where
        B: Clone + Send + Sync + 'static,
    {
        let manifest = self.manifest()?;
        let link_loader = !manifest.plugin.locations.is_empty();
        let mut deployment = DeploymentBuilder::new()
            .manifest(manifest)
            .mode(Mode::Command)
            .args(self.args.clone())
            .build::<StoreCtx<B>>()
            .await
            .context("building deployment")?;
        if link_loader {
            deployment.host::<WasiPlugins, B>().context("linking the plugins host")?;
        }
        link(&mut deployment).context("linking hosts")?;
        let runtime = deployment.assemble(backends).await?;
        Plugins::install_declared(&runtime)?;
        Ok(runtime)
    }

    /// Boots by hand, drives the command guest once, and shuts the runtime
    /// down.
    ///
    /// # Errors
    ///
    /// Same as [`Deployment::boot`], or if the guest traps without exiting.
    pub async fn run<B>(
        &self, backends: B, link: impl FnOnce(&mut omnia::Deployment<StoreCtx<B>>) -> Result<()>,
    ) -> Result<ExitStatus>
    where
        B: Clone + Send + Sync + 'static,
    {
        let runtime = self.boot(backends, link).await?;
        let status = runtime.run_command().await;
        runtime.shutdown();
        status
    }

    /// [`Deployment::run`] linking the host under test, `H`, plus the
    /// telemetry host every `omnia_guest::command!` guest imports.
    ///
    /// A suite testing `WasiOtel` itself, or a bundle without an otel
    /// backend, links by hand through [`run`](Self::run).
    ///
    /// # Errors
    ///
    /// Same as [`Deployment::run`].
    pub async fn run_host<H, B>(&self, backends: B) -> Result<ExitStatus>
    where
        H: Host<StoreCtx<B>> + Server<B>,
        B: Provides<WasiOtel> + Clone + Send + Sync + 'static,
    {
        self.run(backends, |deployment| {
            deployment.host::<H, B>()?;
            deployment.host::<WasiOtel, B>()?;
            Ok(())
        })
        .await
    }

    /// Drives the command guest once through a production `runtime!`'s
    /// wiring (`runtime::Hooks`) over `backends` — the same `link`, `extend`
    /// and `serve` the binary runs, connecting nothing.
    ///
    /// # Errors
    ///
    /// Returns an error if the deployment cannot be built or assembled, or
    /// the guest traps without exiting.
    pub async fn run_with<H, B>(&self, backends: B) -> Result<ExitStatus>
    where
        H: Wiring<B>,
        B: Clone + Send + Sync + 'static,
    {
        let deployment = self.builder()?.build::<StoreCtx<B>>().await?;
        omnia::run_with::<B, H>(deployment, backends).await
    }
}
