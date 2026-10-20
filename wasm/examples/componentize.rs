//! Turns a core wasm module that a guest built with `wit-bindgen` into a
//! component that `WASM.LOAD` accepts, as `wasm-tools component new` does:
//!
//! ```text
//! cargo run -p redis-module-wasm --example componentize -- <module.wasm> <component.wasm>
//! ```

fn main() -> anyhow::Result<()> {
    let [input, output] = <[String; 2]>::try_from(std::env::args().skip(1).collect::<Vec<_>>())
        .map_err(|_| anyhow::anyhow!("usage: componentize <module.wasm> <component.wasm>"))?;
    let module = std::fs::read(&input)?;
    let component = wit_component::ComponentEncoder::default()
        .module(&module)?
        .validate(true)
        .encode()?;
    std::fs::write(&output, component)?;
    Ok(())
}
