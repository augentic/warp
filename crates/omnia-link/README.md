# omnia-link

In-process guest→guest linking for the [omnia](https://github.com/augentic/omnia)
runtime: [`InProcessLinks`] implements the `LinkSeam` the registry drives when a
deployment declares link interfaces, polyfilling caller imports, serving callee
exports over an in-memory wRPC carrier, and selecting the target guest per call.

Depend on the `omnia` facade, not on this crate: the whole surface re-exports
there (`omnia::InProcessLinks`, `omnia::GuestSelector`, `omnia::FirstArgSelector`).
A direct dependency on `omnia-link` (or on `omnia-core`) is never needed by a
deployment.

[`InProcessLinks`]: https://docs.rs/omnia/latest/omnia/struct.InProcessLinks.html

## License

MIT OR Apache-2.0
