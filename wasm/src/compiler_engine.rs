//! Shared by the Redis host and a separate System-allocated executable.
use sha2::{Digest, Sha256};
use std::hash::{Hash, Hasher};
use wasmtime::{Config, Engine};

pub fn engine() -> anyhow::Result<Engine> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    // Preserve the host's Redis signal-handler and epoch policies exactly.
    config.signals_based_traps(false);
    config.epoch_interruption(true);
    config.wasm_backtrace(false);
    Engine::new(&config)
}

/// No randomized hash state: identical builds/configurations in two processes
/// must agree. Wasmtime's own compile-environment hash includes ISA settings.
pub fn fingerprint(engine: &Engine) -> [u8; 32] {
    struct Fingerprint(Sha256);
    impl Hasher for Fingerprint {
        fn finish(&self) -> u64 {
            0
        }
        fn write(&mut self, bytes: &[u8]) {
            self.0.update(bytes);
        }
    }
    let mut hash = Fingerprint(Sha256::new());
    env!("WASM_COMPILER_BUILD").hash(&mut hash);
    engine.precompile_compatibility_hash().hash(&mut hash);
    hash.0.finalize().into()
}
