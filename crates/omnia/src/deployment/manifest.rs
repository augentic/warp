//! # Deployment manifest (`omnia.toml`)
//!
//! Registry population, routing, linking, and transport are *deployment*
//! decisions, not build-time ones. A manifest may be loaded from a startup
//! configuration file or assembled programmatically before the registry is
//! built.
//!
//! The manifest is parsed **generically** — Omnia sees opaque [`GuestId`]s and
//! interface *strings*, never `source:`/`target:`/`mcp`. Consumers write the
//! concrete file; the runtime core stays domain-agnostic.
//!
//! The `[[guest]]` population (file or embedded-bytes sources), each guest's
//! `routes` tables, and the deployment-wide `[link] interfaces` list (which
//! drives host-mediated dynamic linking) are all consumed. Distributed `[transport]` is not yet
//! implemented: only the in-process default is accepted.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use omnia_core::{
    CliRoutes, GuestId, HttpRoutes, Location, PatternRoutes, ResolvedPreopen, Routes,
};
use serde::Deserialize;

use super::source::Source;

/// Host-mediated interfaces the runtime polyfills onto the shared linker.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LinkConfig {
    /// Interface names the host dispatches between guests.
    pub interfaces: Vec<String>,
}

/// Where the plugin loader acquires packages (`[[plugin.location]]`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginConfig {
    /// Named path roots and at most one default registry endpoint.
    #[serde(rename = "location")]
    pub locations: Vec<Location>,
}

/// The deployment manifest: which guests load and how host-mediated calls
/// travel.
///
/// `deny_unknown_fields` turns a stale top-level section (for example the
/// removed `[[route.*]]` tables — routes now live on each `[[guest]]`) into a
/// loud parse error rather than a silent no-op.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(try_from = "ManifestDe")]
pub struct Manifest {
    /// Registry population: each entry maps an identity to a source.
    pub guests: Vec<GuestEntry>,
    /// Working-tree mounts preopened into the guest sandbox.
    pub mounts: Vec<Mount>,
    /// Host-mediated link interfaces polyfilled onto the shared linker.
    pub link: LinkConfig,
    /// Plugin-loader acquisition locations.
    pub plugin: PluginConfig,
    /// Transport configuration for host-mediated calls.
    pub transport: Transport,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ManifestDe {
    #[serde(rename = "guest")]
    guests: Vec<GuestEntry>,
    #[serde(rename = "mount")]
    mounts: Vec<Mount>,
    link: LinkConfig,
    plugin: PluginConfig,
    transport: Transport,
    plugins: Option<toml::Value>,
    #[serde(rename = "location")]
    location: Option<toml::Value>,
}

impl TryFrom<ManifestDe> for Manifest {
    type Error = &'static str;

    fn try_from(de: ManifestDe) -> Result<Self, Self::Error> {
        if de.plugins.is_some() {
            return Err("link interfaces moved to `[link] interfaces`");
        }
        if de.location.is_some() {
            return Err("moved to `[[plugin.location]]`");
        }
        Ok(Self {
            guests: de.guests,
            mounts: de.mounts,
            link: de.link,
            plugin: de.plugin,
            transport: de.transport,
        })
    }
}

impl Manifest {
    /// Start an empty programmatic manifest.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Load a manifest and resolve its relative paths against the config directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the current directory cannot be read, or the file
    /// cannot be read or parsed as TOML.
    pub fn from_config(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let text = fs::read_to_string(&path)
            .with_context(|| format!("reading manifest {}", path.display()))?;
        let mut manifest: Self = toml::from_str(&text)
            .with_context(|| format!("parsing manifest {}", path.display()))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        manifest.resolve_paths(base);
        Ok(manifest)
    }

    /// Create a single-guest manifest from a component path.
    #[must_use]
    pub fn from_wasm(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let id = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("default").to_owned();
        Self::new().guest(GuestEntry::new(id, path))
    }

    /// Append a guest.
    #[must_use]
    pub fn guest(mut self, guest: GuestEntry) -> Self {
        self.guests.push(guest);
        self
    }

    /// Append workspace mounts.
    #[must_use]
    pub fn mounts(mut self, mounts: impl IntoIterator<Item = Mount>) -> Self {
        self.mounts.extend(mounts);
        self
    }

    /// Append host-mediated link interfaces (the manifest's `[link] interfaces`).
    #[must_use]
    pub fn link<I, S>(mut self, interfaces: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.link.interfaces.extend(interfaces.into_iter().map(Into::into));
        self
    }

    /// Append plugin acquisition locations (the manifest's `[[plugin.location]]` entries).
    #[must_use]
    pub fn locations(mut self, locations: impl IntoIterator<Item = Location>) -> Self {
        self.plugin.locations.extend(locations);
        self
    }

    /// Validate manifest-level invariants surfaced before the registry is
    /// built. An `allow_empty` (dynamic) deployment may define no `[[guest]]`
    /// entries.
    pub(super) fn validate(&self, allow_empty: bool) -> Result<()> {
        if self.guests.is_empty() && !allow_empty {
            bail!("manifest defines no [[guest]] entries");
        }
        let mut ids = BTreeSet::new();
        for entry in &self.guests {
            if !ids.insert(entry.id.as_str()) {
                bail!("duplicate [[guest]] id `{}`: guest identities must be unique", entry.id);
            }
        }
        let marked: Vec<&str> =
            self.guests.iter().filter(|e| e.command).map(|e| e.id.as_str()).collect();
        if marked.len() > 1 {
            bail!(
                "multiple [[guest]] entries marked `command = true` ({}): at most one guest may \
                 be the command guest",
                marked.join(", ")
            );
        }
        if self.transport.default != TransportKind::InProcess {
            bail!(
                "transport `{:?}` is not yet implemented; only in-process transport is supported",
                self.transport.default
            );
        }
        let registries: Vec<&str> = self
            .plugin
            .locations
            .iter()
            .filter_map(|location| match location {
                Location::Registry { registry } => Some(registry.as_str()),
                Location::Path { .. } => None,
            })
            .collect();
        if registries.len() > 1 {
            bail!(
                "multiple registry [[plugin.location]] entries ({}): a deployment declares one \
                 default registry (a load's own location may still override the endpoint)",
                registries.join(", ")
            );
        }
        // A config file can declare locations the compiled runtime cannot
        // serve; refuse up front rather than silently never installing them.
        #[cfg(not(feature = "plugin"))]
        if !self.plugin.locations.is_empty() {
            bail!(
                "this runtime was built without the `plugin` feature; remove the \
                 [[plugin.location]] entries or enable the feature on the `omnia` dependency \
                 (`features = [\"plugin\"]`)"
            );
        }
        #[cfg(not(feature = "link"))]
        if !self.link.interfaces.is_empty() {
            bail!(
                "this runtime was built without the `link` feature; remove the [link] \
                 interfaces or enable the feature on the `omnia` dependency (`features = \
                 [\"link\"]`)"
            );
        }
        Ok(())
    }

    fn resolve_paths(&mut self, base: &Path) {
        for guest in &mut self.guests {
            if let SourceSpec::Path(path) = &mut guest.source
                && path.is_relative()
            {
                *path = base.join(&*path);
            }
        }
        for mount in &mut self.mounts {
            if mount.path.is_relative() {
                mount.path = base.join(&mount.path);
            }
        }
        for location in &mut self.plugin.locations {
            if let Location::Path { path, .. } = location
                && path.is_relative()
            {
                *path = base.join(&*path);
            }
        }
    }

    /// Telemetry/component name for this deployment.
    ///
    /// The first `[[guest]]` entry doubles as the name for now.
    #[must_use]
    pub fn name(&self) -> &str {
        self.guests.first().map_or("omnia", |entry| entry.id.as_str())
    }

    /// Resolve every `[[guest]]` source into a loadable source.
    ///
    /// # Errors
    ///
    /// Returns an error if a guest uses a source kind not yet supported.
    pub fn sources(&self) -> Result<Vec<Source>> {
        let mut sources = Vec::with_capacity(self.guests.len());
        for entry in &self.guests {
            let id = GuestId::from(entry.id.as_str());
            match &entry.source {
                SourceSpec::Path(path) => sources.push(Source::with_id(id, path)),
                SourceSpec::Bytes(bytes) => sources.push(Source::embedded(id, bytes.clone())),
                SourceSpec::Oci(reference) => {
                    bail!("guest `{id}`: OCI source `{reference}` is not yet supported")
                }
            }
        }
        Ok(sources)
    }

    /// The host-mediated link interfaces (the `[link] interfaces` list) as an
    /// ordered set.
    #[must_use]
    pub fn link_interfaces(&self) -> BTreeSet<Box<str>> {
        self.link.interfaces.iter().map(|interface| Box::from(interface.as_str())).collect()
    }

    /// Per-trigger route tables aggregated from each guest's `routes` lists,
    /// in guest declaration order.
    #[must_use]
    pub fn routes(&self) -> Routes {
        let pairs = |select: fn(&GuestRoutes) -> &Vec<String>| {
            self.guests.iter().flat_map(move |guest| {
                select(&guest.routes)
                    .iter()
                    .map(|pattern| (pattern.clone(), GuestId::from(guest.id.as_str())))
            })
        };
        let http = HttpRoutes::new(pairs(|routes| &routes.http));
        let messaging = PatternRoutes::new(pairs(|routes| &routes.messaging));
        let websocket = PatternRoutes::new(pairs(|routes| &routes.websocket));
        // CLI routes are not yet parsed; an empty table makes a sole
        // `wasi:cli/run` exporter the catch-all (multi-command routing is
        // deferred).
        Routes::new(http, messaging, websocket, CliRoutes::default())
    }

    /// The identity of the guest marked `command = true`, if any.
    #[must_use]
    pub fn command_guest(&self) -> Option<GuestId> {
        self.guests.iter().find(|e| e.command).map(|e| GuestId::from(e.id.as_str()))
    }

    /// Resolve every `[[mount]]` into a [`ResolvedPreopen`].
    #[must_use]
    pub fn preopens(&self) -> Vec<ResolvedPreopen> {
        self.mounts.iter().map(|entry| entry.resolve(Path::new("."))).collect()
    }
}

/// A single workspace mount: a host directory preopened into the guest
/// sandbox under a guest-visible name.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Mount {
    /// Guest-visible name `preopens.get-directories()` returns (e.g. `.`).
    pub name: String,
    /// Host path. [`Manifest::from_config`] resolves relative paths against the
    /// config file's directory; a relative path set programmatically resolves
    /// against the process working directory.
    pub path: PathBuf,
    /// Read+write when `true`; read-only (the review-flow default) otherwise.
    #[serde(default)]
    pub writable: bool,
}

impl Mount {
    /// Resolve this mount into a [`ResolvedPreopen`], joining a relative host
    /// path against `base` (an absolute path passes through unchanged).
    #[must_use]
    pub fn resolve(&self, base: &Path) -> ResolvedPreopen {
        let host_path =
            if self.path.is_absolute() { self.path.clone() } else { base.join(&self.path) };
        ResolvedPreopen::new(self.name.clone(), host_path, self.writable)
    }
}

/// A single registry population entry.
///
/// `deny_unknown_fields` turns a stale per-guest key (for example the removed
/// `link` list — plugin interfaces are deployment-wide now) into a loud
/// parse error rather than a silent no-op.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestEntry {
    /// Opaque guest identity (the runtime core never parses it).
    pub id: String,
    /// Where the guest's component bytes come from.
    pub source: SourceSpec,
    /// Inbound routes targeting this guest, one list per trigger.
    #[serde(default)]
    pub routes: GuestRoutes,
    /// Marks this guest as the command-mode `wasi:cli/run` target; without a
    /// marked guest the sole static exporter is the catch-all.
    #[serde(default)]
    pub command: bool,
}

impl GuestEntry {
    /// Create a guest from a local component path or embedded component bytes.
    #[must_use]
    pub fn new(id: impl Into<String>, source: impl Into<SourceSpec>) -> Self {
        Self {
            id: id.into(),
            source: source.into(),
            routes: GuestRoutes::default(),
            command: false,
        }
    }

    /// Append an HTTP prefix route targeting this guest.
    #[must_use]
    pub fn route_http(mut self, prefix: impl Into<String>) -> Self {
        self.routes.http.push(prefix.into());
        self
    }

    /// Append a messaging topic route targeting this guest.
    #[must_use]
    pub fn route_messaging(mut self, topic: impl Into<String>) -> Self {
        self.routes.messaging.push(topic.into());
        self
    }

    /// Append a WebSocket route targeting this guest.
    #[must_use]
    pub fn route_websocket(mut self, route: impl Into<String>) -> Self {
        self.routes.websocket.push(route.into());
        self
    }

    /// Mark this guest as the command-mode `wasi:cli/run` target.
    #[must_use]
    pub const fn command(mut self) -> Self {
        self.command = true;
        self
    }
}

/// Where a guest's component bytes come from.
///
/// Modelled as an externally tagged enum so TOML's `source.path = "..."` and
/// `source.oci = "..."` each select a variant.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceSpec {
    /// A local `.wasm` / pre-compiled `.bin` path. [`Manifest::from_config`]
    /// resolves relative paths against the config file's directory; a relative
    /// path set programmatically resolves against the process working directory.
    Path(PathBuf),
    /// Component bytes embedded in the host binary (typically an
    /// `include_bytes!` blob). TOML cannot express this variant; it is set
    /// through the `runtime!` macro or the programmatic [`GuestEntry`] API.
    #[serde(skip)]
    Bytes(Cow<'static, [u8]>),
    /// A digest-pinned OCI reference. Accepted by the parser and surfaced in the
    /// "not yet supported" error; the puller that consumes it lands as a
    /// follow-up.
    Oci(String),
}

// Manual: the derived impl would dump the embedded component bytes.
impl std::fmt::Debug for SourceSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(path) => f.debug_tuple("Path").field(path).finish(),
            Self::Bytes(bytes) => write!(f, "Bytes({} bytes)", bytes.len()),
            Self::Oci(reference) => f.debug_tuple("Oci").field(reference).finish(),
        }
    }
}

impl From<&str> for SourceSpec {
    fn from(path: &str) -> Self {
        Self::Path(PathBuf::from(path))
    }
}

impl From<String> for SourceSpec {
    fn from(path: String) -> Self {
        Self::Path(PathBuf::from(path))
    }
}

impl From<&Path> for SourceSpec {
    fn from(path: &Path) -> Self {
        Self::Path(path.to_path_buf())
    }
}

impl From<PathBuf> for SourceSpec {
    fn from(path: PathBuf) -> Self {
        Self::Path(path)
    }
}

impl From<&'static [u8]> for SourceSpec {
    fn from(bytes: &'static [u8]) -> Self {
        Self::Bytes(Cow::Borrowed(bytes))
    }
}

// `include_bytes!` yields `&[u8; N]`, so the array form is the one embedders hit.
impl<const N: usize> From<&'static [u8; N]> for SourceSpec {
    fn from(bytes: &'static [u8; N]) -> Self {
        Self::Bytes(Cow::Borrowed(bytes))
    }
}

impl From<Vec<u8>> for SourceSpec {
    fn from(bytes: Vec<u8>) -> Self {
        Self::Bytes(Cow::Owned(bytes))
    }
}

/// A guest's inbound routes, one pattern list per trigger; the containing
/// guest is the implicit target.
///
/// `deny_unknown_fields` turns a misspelled trigger (`routes.grpc = [...]`)
/// into a loud parse error rather than a silent no-op.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GuestRoutes {
    /// HTTP path prefixes, matched by longest prefix.
    pub http: Vec<String>,
    /// Messaging topic patterns (`.`-tokenised, `*` one token, `>` trailing
    /// tokens).
    pub messaging: Vec<String>,
    /// WebSocket route patterns (same syntax as messaging).
    pub websocket: Vec<String>,
}

/// Transport configuration for host-mediated calls.
///
/// Only the in-process default is implemented; manifest validation rejects any
/// other value, and `#[serde(deny_unknown_fields)]` turns a stale distributed
/// `[transport.target.*]` section into a loud parse error rather than a silent
/// no-op.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Transport {
    /// The transport used for host-mediated calls.
    pub default: TransportKind,
}

/// A transport mechanism for host-mediated calls.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TransportKind {
    /// In-process byte pipe — the co-located default (the only implemented kind).
    #[default]
    InProcess,
    /// Unix-domain socket (same node, separate processes).
    Unix,
    /// NATS (cross-node).
    Nats,
    /// QUIC (cross-node).
    Quic,
}

// Unit tests by design: manifest parsing/validation is pure translation.
#[cfg(test)]
mod tests {
    use omnia_core::Resolver as _;

    use super::*;

    #[test]
    fn parse_multi_guest() {
        let toml = r#"
            [link]
            interfaces = ["omnia:shared/log", "augentic:specify/source"]

            [[guest]]
            id = "workflow"
            source.path = "./guests/workflow.wasm"

            [[guest]]
            id = "mcp"
            source.path = "./guests/mcp.wasm"

            [transport]
            default = "in-process"
        "#;

        let manifest: Manifest = toml::from_str(toml).expect("manifest should parse");
        assert_eq!(manifest.guests.len(), 2);
        assert_eq!(manifest.guests[0].id, "workflow");
        assert!(matches!(manifest.guests[1].source, SourceSpec::Path(_)));
        assert_eq!(manifest.transport.default, TransportKind::InProcess);
        assert!(manifest.link_interfaces().contains("omnia:shared/log"));
        assert!(manifest.link_interfaces().contains("augentic:specify/source"));
    }

    #[test]
    fn reject_stale_link_keys() {
        // The removed top-level `link` array must fail loudly now that `link`
        // is a table (`[link] interfaces`).
        let toml = "link = [\"omnia:shared/log\"]\n\n\
             [[guest]]\nid = \"a\"\nsource.path = \"./a.wasm\"\n";
        toml::from_str::<Manifest>(toml).unwrap_err();

        // So must the renamed top-level `dispatch` list.
        let toml = "dispatch = [\"omnia:shared/log\"]\n\n\
             [[guest]]\nid = \"a\"\nsource.path = \"./a.wasm\"\n";
        toml::from_str::<Manifest>(toml).unwrap_err();

        // So must the removed per-guest form (link interfaces are deployment-wide).
        let toml = "[[guest]]\nid = \"a\"\nsource.path = \"./a.wasm\"\n\
             link = [\"omnia:link/echo\"]\n";
        toml::from_str::<Manifest>(toml).unwrap_err();

        // And `plugins` misplaced on a guest entry.
        let toml = "[[guest]]\nid = \"a\"\nsource.path = \"./a.wasm\"\n\
             plugins=[\"omnia:link/echo\"]\n";
        toml::from_str::<Manifest>(toml).unwrap_err();

        // A top-level plugins array names the new `[link] interfaces` table.
        let toml = "plugins=[\"omnia:shared/log\"]\n\n\
             [[guest]]\nid = \"a\"\nsource.path = \"./a.wasm\"\n";
        let error = toml::from_str::<Manifest>(toml).unwrap_err();
        assert!(
            error.to_string().contains("link interfaces moved to `[link] interfaces`"),
            "{error}"
        );

        // Top-level `[[location]]` names the nested table.
        let toml = "[[guest]]\nid = \"a\"\nsource.path = \"./a.wasm\"\n\n\
             [[location]]\nname = \".\"\npath = \"adapters\"\n";
        let error = toml::from_str::<Manifest>(toml).unwrap_err();
        assert!(error.to_string().contains("moved to `[[plugin.location]]`"), "{error}");
    }

    #[test]
    fn parse_guest_routes() {
        let toml = r#"
            [[guest]]
            id = "mcp"
            source.path = "./guests/mcp.wasm"
            routes.http = ["/mcp"]
            routes.messaging = ["specify.build.>"]
            routes.websocket = ["events.*"]
        "#;

        let manifest: Manifest = toml::from_str(toml).expect("manifest should parse");
        assert_eq!(manifest.guests[0].routes.http, ["/mcp"]);
        assert_eq!(manifest.guests[0].routes.messaging, ["specify.build.>"]);
        assert_eq!(manifest.guests[0].routes.websocket, ["events.*"]);

        let routes = manifest.routes();
        assert_eq!(routes.http().resolve("/mcp/tool"), Some(&GuestId::from("mcp")));
        assert_eq!(routes.messaging().resolve("specify.build.x"), Some(&GuestId::from("mcp")));
        assert_eq!(routes.websocket().resolve("events.tick"), Some(&GuestId::from("mcp")));
    }

    #[test]
    fn routes_aggregate_across_guests() {
        let toml = r#"
            [[guest]]
            id = "a"
            source.path = "./a.wasm"
            routes.http = ["/a"]

            [[guest]]
            id = "b"
            source.path = "./b.wasm"
            routes.http = ["/a/b"]
        "#;

        let manifest: Manifest = toml::from_str(toml).expect("manifest should parse");
        let routes = manifest.routes();
        // Longest-prefix matching is preserved across guest-owned lists.
        assert_eq!(routes.http().resolve("/a/x"), Some(&GuestId::from("a")));
        assert_eq!(routes.http().resolve("/a/b/x"), Some(&GuestId::from("b")));
    }

    #[test]
    fn reject_top_level_route_tables() {
        // The removed `[[route.*]]` schema must fail loudly, not be ignored.
        let toml = r#"
            [[guest]]
            id = "mcp"
            source.path = "./guests/mcp.wasm"

            [[route.http]]
            prefix = "/mcp"
            guest = "mcp"
        "#;
        toml::from_str::<Manifest>(toml).unwrap_err();
    }

    #[test]
    fn reject_unknown_route_trigger() {
        let toml = "[[guest]]\nid = \"a\"\nsource.path = \"./a.wasm\"\nroutes.grpc = [\"/a\"]\n";
        toml::from_str::<Manifest>(toml).unwrap_err();
    }

    #[test]
    fn parse_and_resolve_mounts() {
        let toml = r#"
            [[guest]]
            id = "model"
            source.path = "./model.wasm"

            [[mount]]
            name = "."
            path = "../.."

            [[mount]]
            name = "shared"
            path = "/srv/shared"
            writable = true
        "#;

        let mut manifest: Manifest = toml::from_str(toml).expect("manifest should parse");
        assert_eq!(manifest.mounts.len(), 2);
        assert_eq!(manifest.mounts[0].name, ".");
        assert!(!manifest.mounts[0].writable, "writable defaults to read-only");
        assert!(manifest.mounts[1].writable);

        let base = Path::new("/deploy/app");
        manifest.resolve_paths(base);
        let resolved = manifest.preopens();
        assert_eq!(resolved.len(), 2);
        // A relative path resolves against the manifest's directory; read-only by default.
        assert_eq!(resolved[0].name, ".");
        assert_eq!(resolved[0].host_path, base.join("../.."));
        assert!(!resolved[0].writable);
        // An absolute path passes through unchanged, and `writable` grants mutation.
        assert_eq!(resolved[1].host_path, PathBuf::from("/srv/shared"));
        assert!(resolved[1].writable);
    }

    #[test]
    fn parse_and_resolve_locations() {
        let toml = r#"
            [[guest]]
            id = "engine"
            source.path = "./engine.wasm"

            [[plugin.location]]
            name = "."
            path = "adapters"

            [[plugin.location]]
            registry = "ghcr.io"
        "#;

        let mut manifest: Manifest = toml::from_str(toml).expect("manifest should parse");
        manifest.resolve_paths(Path::new("/deploy/app"));
        assert_eq!(
            manifest.plugin.locations,
            [Location::path(".", "/deploy/app/adapters"), Location::registry("ghcr.io"),]
        );
        #[cfg(feature = "plugin")]
        manifest.validate(false).expect("one registry is allowed");
        #[cfg(not(feature = "plugin"))]
        {
            let error = manifest.validate(false).expect_err("locations need the plugin feature");
            assert!(error.to_string().contains("without the `plugin` feature"), "{error}");
        }
    }

    #[test]
    fn interfaces_without_link_feature() {
        let manifest =
            Manifest::new().guest(GuestEntry::new("a", "./a.wasm")).link(["omnia:link/echo"]);
        #[cfg(feature = "link")]
        manifest.validate(false).expect("interfaces are allowed with the link feature");
        #[cfg(not(feature = "link"))]
        {
            let error = manifest.validate(false).expect_err("interfaces need the link feature");
            assert!(error.to_string().contains("without the `link` feature"), "{error}");
        }
    }

    #[test]
    fn reject_mixed_location_keys() {
        let toml = "[[guest]]\nid = \"a\"\nsource.path = \"./a.wasm\"\n\n\
             [[plugin.location]]\nname = \".\"\npath = \"adapters\"\nregistry = \"ghcr.io\"\n";
        toml::from_str::<Manifest>(toml).unwrap_err();
    }

    #[test]
    fn reject_two_registry_locations() {
        let manifest = Manifest::new()
            .guest(GuestEntry::new("a", "./a.wasm"))
            .locations([Location::registry("ghcr.io"), Location::registry("docker.io")]);
        let error = manifest.validate(false).expect_err("two registries must be refused");
        assert!(error.to_string().contains("multiple registry"), "{error}");
    }

    #[test]
    fn cli_mount_resolves_relative_to_base() {
        let entry = Mount {
            name: ".".to_owned(),
            path: PathBuf::from("workspace"),
            writable: true,
        };
        // CLI mounts resolve against the process working directory, unlike
        // manifest mounts which resolve against the manifest's directory.
        let resolved = entry.resolve(Path::new("/cwd"));
        assert_eq!(resolved.host_path, PathBuf::from("/cwd/workspace"));
        assert!(resolved.writable);
    }

    #[test]
    fn defaults_to_in_process() {
        let toml = r#"
            [[guest]]
            id = "only"
            source.path = "./only.wasm"
        "#;

        let manifest: Manifest = toml::from_str(toml).expect("manifest should parse");
        assert_eq!(manifest.transport.default, TransportKind::InProcess);
    }

    #[test]
    fn reject_non_default_transport() {
        let toml = "[[guest]]\nid = \"only\"\nsource.path = \"./only.wasm\"\n\n\
             [transport]\ndefault = \"unix\"\n";
        let manifest: Manifest = toml::from_str(toml).expect("manifest should parse");
        assert!(manifest.validate(false).is_err(), "distributed transport is not yet implemented");
    }

    #[test]
    fn reject_stale_target_section() {
        // A leftover distributed-transport target must fail loudly, not be ignored.
        let toml = "[[guest]]\nid = \"only\"\nsource.path = \"./only.wasm\"\n\n\
             [transport.target.remote]\nkind = \"unix\"\n";
        toml::from_str::<Manifest>(toml).unwrap_err();
    }

    #[test]
    fn parse_file() {
        let path =
            std::env::temp_dir().join(format!("omnia_manifest_ok_{}.toml", std::process::id()));
        std::fs::write(&path, "[[guest]]\nid = \"only\"\nsource.path = \"./only.wasm\"\n")
            .expect("temp manifest should write");

        let manifest = Manifest::from_config(&path).expect("manifest should load");
        let _ = std::fs::remove_file(&path);

        assert_eq!(manifest.guests.len(), 1);
        assert_eq!(manifest.guests[0].id, "only");
        let SourceSpec::Path(source) = &manifest.guests[0].source else {
            panic!("expected path source");
        };
        assert!(source.is_absolute());
    }

    #[test]
    fn parse_command_flag() {
        let toml = "[[guest]]\nid = \"helper\"\nsource.path = \"./helper.wasm\"\n\n\
             [[guest]]\nid = \"app\"\nsource.path = \"./app.wasm\"\ncommand = true\n";
        let manifest: Manifest = toml::from_str(toml).expect("manifest should parse");
        manifest.validate(false).expect("one marked guest validates");
        assert!(!manifest.guests[0].command, "the flag defaults to false");
        assert_eq!(manifest.command_guest(), Some(GuestId::from("app")));
    }

    #[test]
    fn reject_multiple_command_guests() {
        let manifest = Manifest::new()
            .guest(GuestEntry::new("a", "./a.wasm").command())
            .guest(GuestEntry::new("b", "./b.wasm").command());
        let error = manifest.validate(false).expect_err("two marked guests must be rejected");
        assert!(error.to_string().contains("at most one guest may be the command guest"));
    }

    #[test]
    fn reject_duplicate_guest_ids() {
        let toml = "[[guest]]\nid = \"same\"\nsource.path = \"./a.wasm\"\n\n\
             [[guest]]\nid = \"same\"\nsource.path = \"./b.wasm\"\n";
        let manifest: Manifest = toml::from_str(toml).expect("manifest should parse");
        let error = manifest.validate(false).expect_err("duplicate guest ids must be rejected");
        assert!(error.to_string().contains("duplicate [[guest]] id `same`"), "{error}");
    }

    #[test]
    fn reject_without_guests() {
        let manifest: Manifest =
            toml::from_str("[transport]\ndefault = \"unix\"\n").expect("manifest should parse");
        assert!(
            manifest.validate(false).is_err(),
            "a static manifest with no guests must be rejected"
        );
        assert!(
            Manifest::new().validate(true).is_ok(),
            "a dynamic deployment may start with no guests"
        );
    }

    #[test]
    fn bytes_source_maps_to_embedded() {
        // `b"..."` is `&'static [u8; N]` — the `include_bytes!` shape.
        let manifest = Manifest::new()
            .guest(GuestEntry::new("baked", b"\0asm"))
            .guest(GuestEntry::new("read", Vec::from(*b"\0asm")));

        assert!(matches!(manifest.guests[0].source, SourceSpec::Bytes(_)));
        assert_eq!(format!("{:?}", manifest.guests[0].source), "Bytes(4 bytes)");

        let sources = manifest.sources().expect("bytes sources resolve");
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].id(), &GuestId::from("baked"));
        assert_eq!(sources[1].id(), &GuestId::from("read"));
    }

    #[test]
    fn build_programmatically() {
        let manifest = Manifest::new()
            .guest(
                GuestEntry::new("router", "router.wasm")
                    .route_http("/router")
                    .route_messaging("jobs.>"),
            )
            .guest(GuestEntry::new("responder", "responder.wasm").route_websocket("events.*"))
            .mounts([Mount {
                name: ".".to_owned(),
                path: PathBuf::from("workspace"),
                writable: true,
            }])
            .link(["omnia:link/echo"])
            .link(["omnia:shared/log"]);

        #[cfg(feature = "link")]
        manifest.validate(false).expect("manifest should validate");
        assert_eq!(manifest.guests.len(), 2);
        assert_eq!(manifest.mounts.len(), 1);
        assert_eq!(manifest.guests[0].routes.http, ["/router"]);
        assert_eq!(manifest.guests[0].routes.messaging, ["jobs.>"]);
        assert_eq!(manifest.guests[1].routes.websocket, ["events.*"]);
        assert!(manifest.link_interfaces().contains("omnia:link/echo"));
        assert!(manifest.link_interfaces().contains("omnia:shared/log"));
    }
}
