//! Model example runtime.
//!
//! Registers the `WasiModel` host backed by an example-local backend serving
//! a fixed schema answer, so the run is deterministic with no live model, no
//! network, and no configuration. Command mode drives the `create` guest's
//! `wasi:cli/run` export once. See `README.md`.

cfg_if::cfg_if! {
    if #[cfg(not(target_arch = "wasm32"))] {
        use std::sync::Arc;

        use omnia_wasi_model::{Answer, FutureResult, Request, ToolHost, WasiModel, WasiModelCtx};
        use omnia_wasi_otel::{WasiOtel, OtelDefault};

        #[derive(Clone, Copy, Debug)]
        struct CannedVerdict;

        impl omnia::Backend for CannedVerdict {
            type ConnectOptions = omnia::NoOptions;

            fn connect_with(
                _options: omnia::NoOptions,
            ) -> impl std::future::Future<Output = anyhow::Result<Self>> {
                std::future::ready(Ok(Self))
            }
        }

        impl WasiModelCtx for CannedVerdict {
            fn complete(
                &self, _request: Request, _tool_host: Arc<dyn ToolHost>,
            ) -> FutureResult<Answer> {
                let answer =
                    Answer::from(r#"{"verdict":"pass","reason":"the bounds check is correct"}"#);
                Box::pin(async move { Ok(answer) })
            }
        }

        omnia::runtime!({
            mode: command,
            config: concat!(env!("CARGO_MANIFEST_DIR"), "/model/omnia.toml"),
            hosts: {
                WasiOtel: OtelDefault,
                WasiModel: CannedVerdict,
            }
        });
    } else {
        fn main() {}
    }
}
