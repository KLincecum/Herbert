//! Build archives from verified, run-local files without guest binary copies.
use crate::storage::Storage;
use herbert_contract::{Diagnostic, FileRef};
use std::{collections::BTreeSet, io::Cursor};

pub(crate) fn build(
    storage: &mut Storage,
    entries: Vec<(String, FileRef)>,
) -> Result<FileRef, Diagnostic> {
    let invalid = || {
        Diagnostic::new(
            "ARCHIVE_INVALID",
            "Cannot build the ZIP from retained files.",
        )
    };
    let mut names = BTreeSet::new();
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, file) in entries {
        crate::storage::safe_relative(&name)?;
        if !names.insert(name.clone()) {
            return Err(invalid());
        }
        storage.verify(&file)?;
        let mut input =
            std::fs::File::open(storage.confined(&file.path)?).map_err(|_| invalid())?;
        zip.start_file(
            name,
            zip::write::SimpleFileOptions::default()
                // Native layout reads provide no file time. Use a stable value
                // so recovery can reuse an unchanged archive byte for byte.
                .last_modified_time(zip::DateTime::default())
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(1)),
        )
        .map_err(|_| invalid())?;
        std::io::copy(&mut input, &mut zip).map_err(|_| invalid())?;
    }
    let bytes = zip.finish().map_err(|_| invalid())?.into_inner();
    storage.preserve_raw(&bytes, "zip")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn zip_preserves_bodies_and_rejects_changed_sources_or_unsafe_members() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage =
            Storage::open(dir.path(), false, crate::storage::new_journal("zip".into())).unwrap();
        let body = b"<Report>\r\n  exact bytes\r\n</Report>";
        let file = storage.preserve_raw(body, "rdl").unwrap();
        let name = "reports/CustomReports/A/Main.rdl";
        let result = build(&mut storage, vec![(name.into(), file.clone())]).unwrap();
        let mut archive = zip::ZipArchive::new(
            std::fs::File::open(storage.confined(&result.path).unwrap()).unwrap(),
        )
        .unwrap();
        let mut actual = Vec::new();
        assert_eq!(archive.by_name(name).unwrap().last_modified(), Some(zip::DateTime::default()));
        std::io::Read::read_to_end(&mut archive.by_name(name).unwrap(), &mut actual).unwrap();
        assert_eq!(actual, body);
        let repeated = build(&mut storage, vec![(name.into(), file.clone())]).unwrap();
        assert_eq!(result.sha256, repeated.sha256);
        assert!(build(&mut storage, vec![("../escape.rdl".into(), file.clone())]).is_err());
        assert!(
            build(
                &mut storage,
                vec![(name.into(), file.clone()), (name.into(), file.clone())]
            )
            .is_err()
        );
        let mut changed = file;
        changed.sha256 = "0".repeat(64);
        assert!(build(&mut storage, vec![(name.into(), changed)]).is_err());
    }
}
