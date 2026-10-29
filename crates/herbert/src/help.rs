//! Offline application guides. Plugin help stays in each module's descriptor.
use herbert_contract::{Diagnostic, HelpSection};
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct HelpDocument {
    pub id: String,
    pub title: String,
    pub audience: String,
    pub status: String,
    pub sections: Vec<HelpSection>,
}

pub(crate) fn documents() -> Result<Vec<HelpDocument>, Diagnostic> {
    // Keep the text in ordinary source files so documentation edits do not
    // require changes to the presentation code or the command dispatcher.
    let mut documents = [
        include_str!("../../../docs/app/user-guide.json"),
        include_str!("../../../docs/app/technical-guide.json"),
    ]
    .into_iter()
    .map(|source| {
        serde_json::from_str(source).map_err(|_| {
            Diagnostic::new(
                "HELP_DOCUMENT_INVALID",
                "An embedded help document does not match the document format.",
            )
        })
    })
    .collect::<Result<Vec<_>, _>>()?;
    documents.push(HelpDocument {
        id: "mcp-guide".into(),
        title: "MCP guide".into(),
        audience: "MCP clients and integration authors".into(),
        status: "Complete local protocol and tool reference.".into(),
        sections: vec![HelpSection {
            title: "Reference".into(),
            paragraphs: vec![include_str!("../../../docs/MCP-GUIDE.md").into()],
        }],
    });
    documents.push(HelpDocument {
        id: "agent-instructions".into(),
        title: "Installed Herbert instructions for agents".into(),
        audience: "Agents operating an installed package".into(),
        status: "Release-user instructions. Load explicitly when the client does not read AGENTS.md.".into(),
        sections: vec![HelpSection {
            title: "Instructions".into(),
            paragraphs: vec![include_str!("../../../docs/RELEASE-AGENTS.md").into()],
        }],
    });
    Ok(documents)
}
