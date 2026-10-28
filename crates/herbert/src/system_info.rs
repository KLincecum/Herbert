//! Curated host facts for language-independent teaching plugins.
//!
//! Read values from the standard library. Do not read credentials, arbitrary
//! environment variables, business data, network interfaces or user files.
pub(crate) fn snapshot() -> serde_json::Value {
    serde_json::json!({
        "herbertVersion":env!("CARGO_PKG_VERSION"),
        "pluginAbi":herbert_contract::PLUGIN_ABI,
        "platform":{
            "os":std::env::consts::OS,
            "architecture":std::env::consts::ARCH,
            "family":std::env::consts::FAMILY,
            "pointerWidthBits":usize::BITS,
            "endianness":if cfg!(target_endian="little"){"little"}else{"big"}
        },
        "availableParallelism":std::thread::available_parallelism().ok().map(|value|value.get())
    })
}
