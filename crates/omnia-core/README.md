# omnia-core

The live-runtime SDK of [omnia](https://github.com/augentic/omnia): the
multi-guest registry, host→guest dispatch, the [`LinkSeam`] trait
guest→guest linking implements against, per-store contexts, and
telemetry.

The registry always holds a `LinkSeam`. This crate owns the trait and
the [`NoLinks`] no-op; the in-process implementation (`InProcessLinks`)
lives in `omnia-link` and reaches embedders only through the `omnia`
composition root's `link` feature.

Embedders depend on the `omnia` composition root, which owns the
deployment pipeline and process lifecycle and re-exports this crate's
surface together with the capability crates built on it. Depend on
`omnia-core` directly only when building a capability crate — one that
links a host into the runtime and installs its state through the
[`Wiring::extend`] hook and the runtime's [`Extensions`]. `omnia-plugin`
(the `omnia:plugins/loader` capability) is the exemplar.

[`LinkSeam`]: https://docs.rs/omnia/latest/omnia/trait.LinkSeam.html
[`NoLinks`]: https://docs.rs/omnia/latest/omnia/struct.NoLinks.html
[`Wiring::extend`]: https://docs.rs/omnia/latest/omnia/trait.Wiring.html
[`Extensions`]: https://docs.rs/omnia-core/latest/omnia_core/struct.Extensions.html
