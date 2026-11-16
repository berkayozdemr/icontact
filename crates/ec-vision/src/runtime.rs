//! Starting ONNX Runtime and loading a model, with the settings every model here needs.

use std::sync::OnceLock;

use ort::environment::Environment;
use ort::session::builder::AutoDevicePolicy;
use ort::session::Session;

use crate::VisionError;

/// ONNX Runtime allows one environment per process, so it is made once and kept.
fn environment() -> Result<&'static Environment, VisionError> {
    static ENV: OnceLock<Result<Environment, String>> = OnceLock::new();
    ENV.get_or_init(|| {
        // G3: nothing leaves the machine, so the runtime's own telemetry is off.
        ort::init()
            .with_name("icontact")
            .with_telemetry(false)
            .build()
            .map_err(|e| e.to_string())
    })
    .as_ref()
    .map_err(|e| VisionError::Runtime(e.clone()))
}

/// Load a model from bytes, to run on the CPU with `threads` threads.
pub(crate) fn load(model: &[u8], threads: usize) -> Result<Session, VisionError> {
    let session = Session::builder(environment()?)?
        // R1 is "any CPU". Left alone, this version of `ort` picks CoreML on a Mac
        //, which no Windows laptop has.
        .with_auto_device(AutoDevicePolicy::PreferCPU)?
        .with_intra_threads(threads.max(1))?
        .commit_from_memory(model)?;
    Ok(session)
}
