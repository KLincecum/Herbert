//! Creator details shared by the GUI, CLI and MCP. No network read is needed.
use serde::Serialize;

#[derive(Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct About {
    pub name: &'static str,
    pub version: &'static str,
    pub copyright: &'static str,
    pub dedication: &'static str,
    pub profiles: [ProfileLink; 0],
}

#[derive(Serialize, schemars::JsonSchema)]
pub(crate) struct ProfileLink {
    pub label: &'static str,
    pub url: &'static str,
}

pub(crate) fn information() -> About {
    About {
        name: "Herbert",
        version: env!("CARGO_PKG_VERSION"),
        copyright: "Copyright 2026 Kevin Lincecum",
        dedication: "Programmed with love for my fellow EpiUsers.",
        profiles: [],
    }
}
