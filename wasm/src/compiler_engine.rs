//! Shared by the Redis host and a separate System-allocated executable.
use sha2::{Digest, Sha256};
use std::hash::{Hash, Hasher};
use wasmtime::{Config, Engine};

pub fn engine() -> anyhow::Result<Engine> {
    Engine::new(&config())
}

pub(crate) fn config() -> Config {
    let mut config = Config::new();
    config.wasm_component_model(true);
    // Preserve the host's Redis signal-handler and epoch policies exactly.
    config.signals_based_traps(false);
    config.epoch_interruption(true);
    config.wasm_backtrace(false);
    config
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

#[cfg(test)]
mod tests {
    #[test]
    fn shared_memory_is_rejected_rather_than_bypassing_the_resource_limiter() {
        let engine = super::engine().unwrap();
        // Valid core wasm: (module (memory 1 1 shared)). ResourceLimiter does
        // not cover shared memory, so keep the pinned no-threads validation.
        let bytes = b"\0asm\x01\0\0\0\x05\x04\x01\x03\x01\x01";
        let error = wasmtime::Module::new(&engine, bytes).err().unwrap();
        let text = format!("{error:#}");
        assert!(
            text.contains("shared") || text.contains("threads"),
            "{text}"
        );
    }
}
