//! Native AccessScope JSON ZIP, constructed from the complete received graph.
use herbert_contract::Diagnostic;
use serde_json::Value;
use std::io::{Cursor, Write};
use zip::{ZipWriter, write::SimpleFileOptions};

pub fn build(data: &Value, version: &str) -> Result<Vec<u8>, Diagnostic> {
    let fail = || {
        Diagnostic::new(
            "ACCESS_ARCHIVE_INVALID",
            "The native access scope archive could not be constructed.",
        )
    };
    if version.split('.').count() != 4
        || version.split('.').any(|n| n.parse::<u32>().is_err())
        || data["AccessScope"].as_array().is_none_or(|a| a.len() != 1)
        || ["AccessScopeEntity", "AccessScopeBOMethod"]
            .iter()
            .any(|t| !data[*t].is_array())
    {
        return Err(fail());
    }
    // ExportImportHandler writes the complete native tableset and the assembly
    // version. Whitespace/ZIP timestamps are transport details, not source data.
    let bytes = serde_json::to_vec_pretty(data).map_err(|_| fail())?;
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in [
        ("AccessScopeTableset", bytes.as_slice()),
        ("Version", version.as_bytes()),
    ] {
        zip.start_file(name, options).map_err(|_| fail())?;
        zip.write_all(body).map_err(|_| fail())?;
    }
    zip.finish().map(|c| c.into_inner()).map_err(|_| fail())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Read;
    #[test]
    fn native_members_preserve_unknown_fields_unicode_and_version() {
        let ds = json!({"AccessScope":[{"Company":"C","AccessScopeID":"A","Future":{"text":"ü & <text>"},"SysRevID":9007199254740993u64}],"AccessScopeEntity":[],"AccessScopeBOMethod":[],"ExtensionTables":[]});
        let bytes = build(&ds, "5.2.100.0").unwrap();
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        assert_eq!(zip.len(), 2);
        let mut text = String::new();
        zip.by_name("AccessScopeTableset")
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), ds);
        let mut v = String::new();
        zip.by_name("Version")
            .unwrap()
            .read_to_string(&mut v)
            .unwrap();
        assert_eq!(v, "5.2.100.0");
        assert!(build(&ds, "invented").is_err());
        assert!(build(&json!({"AccessScope":[]}), "5.2.100.0").is_err());
    }
}
