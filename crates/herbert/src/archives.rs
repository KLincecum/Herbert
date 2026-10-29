//! Native ZIP entries become portable source views. Original ZIP bytes stay raw.
use crate::storage::{self, Storage};
use herbert_contract::{Diagnostic, FileRef, Identity};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
};

fn extension(name: &str) -> &str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "json" => "json",
        "jsonc" => "jsonc",
        "xml" => "xml",
        "cs" => "cs",
        "txt" => "txt",
        "rdl" => "rdl",
        "efxj" => "efxj",
        "baq" => "baq",
        "bpm" => "bpm",
        "png" => "png",
        "jpg" | "jpeg" => "jpg",
        "svg" => "svg",
        "dll" => "dll",
        "pdb" => "pdb",
        "css" => "css",
        "js" => "js",
        "ts" => "ts",
        "woff" => "woff",
        "woff2" => "woff2",
        _ => "bin",
    }
}
fn traverses(name: &str) -> bool {
    let name = name.replace('\\', "/");
    name.starts_with('/')
        || name.split('/').any(|p| p == ".." || p.contains(':'))
        || name.contains('\0')
}

/// ZIP timestamps and compression can change when the same export is requested
/// twice. Compare every member's exact content and names without rewriting either
/// native package. Unreadable or duplicate entries cannot establish equivalence.
pub(crate) fn same_contents(storage: &Storage, left: &FileRef, right: &FileRef) -> bool {
    use sha2::{Digest, Sha256};
    fn contents(path: std::path::PathBuf) -> Option<(Vec<u8>, BTreeMap<String, (u64, String, Option<u32>, String)>)> {
        let mut archive = zip::ZipArchive::new(File::open(path).ok()?).ok()?;
        let comment = archive.comment().to_vec();
        let mut members = BTreeMap::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).ok()?;
            let name = entry.name().to_owned();
            if traverses(&name) || members.contains_key(&name) { return None; }
            let mut hash = Sha256::new();
            let mut size = 0u64;
            // This is an I/O buffer, not a supported-file-size limit.
            let mut buffer = [0u8; 8192];
            if entry.size() == 0 && entry.compressed_size() == 0 && entry.crc32() != 0 { return None; }
            if !(entry.size() == 0 && entry.compressed_size() == 0) {
                loop {
                    let count = entry.read(&mut buffer).ok()?;
                    if count == 0 { break; }
                    hash.update(&buffer[..count]);
                    size = size.checked_add(count as u64)?;
                }
            }
            if size != entry.size() { return None; }
            members.insert(name, (size, hex::encode(hash.finalize()), entry.unix_mode(), entry.comment().to_owned()));
        }
        Some((comment, members))
    }
    let Some(left) = storage.confined(&left.path).ok().and_then(contents) else { return false; };
    storage.confined(&right.path).ok().and_then(contents).is_some_and(|right| left == right)
}
pub fn views(
    storage: &Storage,
    identity: &Identity,
    native: &[FileRef],
) -> (Vec<FileRef>, Vec<Diagnostic>) {
    views_with(storage, identity, native, |name, mut input| {
        storage.write_derived_stream(identity, name, &mut input)
    })
}

/// Decode readable archive members in memory. The shared writers publish them.
pub(crate) fn prepare_views(storage: &Storage, identity: &Identity, native: &[FileRef])
    -> (Vec<crate::processing::SourceFile>, Vec<Diagnostic>) {
    let mut sources = Vec::new();
    let (_, diagnostics) = views_with(storage, identity, native, |name, input| {
        let mut bytes = Vec::new();
        input.read_to_end(&mut bytes).map_err(|_| Diagnostic::new(
            "SOURCE_STREAM_FAILED", "Cannot read the complete Epicor archive member."))?;
        let file = FileRef { path:name.into(), sha256:storage::sha256(&bytes),
            bytes:bytes.len() as u64, role:"derived".into() };
        sources.push(crate::processing::SourceFile { name:name.into(), bytes,
            pointer:String::new(), provenance:None });
        Ok((file, false))
    });
    (sources, diagnostics)
}

fn views_with(
    storage: &Storage,
    _identity: &Identity,
    native: &[FileRef],
    mut write: impl FnMut(&str, &mut dyn std::io::Read) -> Result<(FileRef, bool), Diagnostic>,
) -> (Vec<FileRef>, Vec<Diagnostic>) {
    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    // Different native packages may legitimately contain the same member name.
    let archive_hashes: std::collections::BTreeSet<_> = native.iter().filter(|original| {
        let mut magic = [0; 4];
        storage.confined(&original.path).ok().and_then(|path| File::open(path).ok())
            .and_then(|mut file| file.read_exact(&mut magic).ok()).is_some()
            && matches!(&magic, b"PK\x03\x04" | b"PK\x05\x06" | b"PK\x07\x08")
    }).map(|original| original.sha256.as_str()).collect();
    let multiple = archive_hashes.len() > 1;
    let mut seen = std::collections::BTreeSet::new();
    let mut archive_number = 0;
    for original in native {
        let result = (|| -> Result<(), Diagnostic> {
            storage.verify(original)?;
            if !seen.insert(original.sha256.clone()) { return Ok(()); }
            let mut source = File::open(storage.confined(&original.path)?).map_err(|_| {
                Diagnostic::new("RAW_UNAVAILABLE", "Epicor's export package is unavailable.")
            })?;
            let mut magic = [0; 4];
            if source.read(&mut magic).map_err(|_| {
                Diagnostic::new(
                    "RAW_UNAVAILABLE",
                    "Cannot inspect Epicor's export package.",
                )
            })? != 4
                || !matches!(&magic, b"PK\x03\x04" | b"PK\x05\x06" | b"PK\x07\x08")
            {
                return Ok(());
            }
            source.seek(SeekFrom::Start(0)).map_err(|_| {
                Diagnostic::new("RAW_UNAVAILABLE", "Cannot read Epicor's export package.")
            })?;
            let mut archive = zip::ZipArchive::new(source).map_err(|_| {
                Diagnostic::new(
                    "ARCHIVE_INVALID",
                    "The Epicor ZIP directory is invalid; its original bytes remain preserved.",
                )
            })?;
            archive_number += 1;
            let mut names = BTreeMap::<String, usize>::new();
            for entry in 0..archive.len() {
                let mut file = match archive.by_index(entry) {
                    Ok(file) => file,
                    Err(_) => {
                        diagnostics.push(Diagnostic::new("ARCHIVE_ENTRY_UNAVAILABLE","An Epicor ZIP entry cannot be read without an additional capability or key. Original package bytes remain preserved."));
                        continue;
                    }
                };
                let name = file.name().to_string();
                if traverses(&name) {
                    diagnostics.push(Diagnostic::new("UNSAFE_ARCHIVE_PATH","An Epicor archive entry contains an absolute or traversing path; it was not extracted."));
                    continue;
                }
                let collision = storage::collision_key(&name);
                let count = names.entry(collision).or_default();
                *count += 1;
                let duplicate = *count > 1;
                if duplicate {
                    diagnostics.push(Diagnostic::new("ARCHIVE_NAME_COLLISION","Epicor archive names collide under portable filesystem rules. Numbered folders preserve each readable entry."));
                }
                if file.is_dir() {
                    continue;
                }
                // Native exports preserve all package members. Publish only readable authored code.
                if !matches!(extension(&name), "cs" | "js" | "ts") { continue; }
                let readable = name.replace('\\', "/").split('/').filter(|part| !part.is_empty()).map(crate::output_layout::component).collect::<Vec<_>>().join("/");
                let readable = if duplicate { format!("Duplicate {}/{readable}", *count) } else { readable };
                let path = if multiple { format!("Code/Archive {archive_number}/{readable}") } else { format!("Code/{readable}") };
                let declared = file.size();
                // Epicor's BAQ export represents an empty QueryDependencies member
                // as Deflated with no compressed payload, expanded length or CRC.
                // There is no deflate stream to decode in that exact representation.
                // Never skip decoding/CRC checks when any payload metadata is nonzero.
                let result = if file.compression() == zip::CompressionMethod::Deflated
                    && declared == 0
                    && file.compressed_size() == 0
                    && file.crc32() == 0
                {
                    write(&path, &mut std::io::Cursor::new(&[]))
                } else {
                    write(&path, &mut file)
                };
                match result {
                    Ok((view, conflict)) => {
                        if view.bytes != declared {
                            diagnostics.push(Diagnostic::new("ARCHIVE_LENGTH_MISMATCH","An archive entry's expanded length differs from its directory metadata."));
                        }
                        if conflict {
                            diagnostics.push(Diagnostic::new("USER_EDIT_CONFLICT","The existing archive source view was preserved; the current server view is stored in conflicts."));
                        }
                        files.push(view);
                    }
                    Err(error) => {
                        diagnostics.push(error);
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            diagnostics.push(error);
        }
    }
    (files, diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use herbert_contract::Scope;
    use std::io::Cursor;
    fn native_empty_deflate(crc: u32, expanded: u32) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "Empty.cs",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        let mut bytes = writer.finish().unwrap().into_inner();
        let central = bytes.windows(4).position(|p| p == [80, 75, 1, 2]).unwrap();
        bytes[8..10].copy_from_slice(&8u16.to_le_bytes());
        bytes[14..18].copy_from_slice(&crc.to_le_bytes());
        bytes[22..26].copy_from_slice(&expanded.to_le_bytes());
        bytes[central + 10..central + 12].copy_from_slice(&8u16.to_le_bytes());
        bytes[central + 16..central + 20].copy_from_slice(&crc.to_le_bytes());
        bytes[central + 24..central + 28].copy_from_slice(&expanded.to_le_bytes());
        bytes
    }
    fn extract(bytes: &[u8]) -> (Vec<FileRef>, Vec<Diagnostic>) {
        let temp = tempfile::tempdir().unwrap();
        let mut storage = Storage::open(
            temp.path(),
            false,
            storage::new_journal("empty-entry".into()),
        )
        .unwrap();
        let original = storage.preserve_raw(bytes, "baq").unwrap();
        let id = Identity {
            category: "baqs-and-ubaqs".into(),
            kind: "BAQ".into(),
            scope: Scope::Global,
            keys: BTreeMap::from([
                ("Company".into(), "".into()),
                ("QueryID".into(), "Q".into()),
            ]),
        };
        let result = views(&storage, &id, &[original.clone()]);
        storage.verify(&original).unwrap();
        for file in &result.0 {
            storage.verify(file).unwrap();
        }
        result
    }
    #[test]
    fn independent_archives_keep_same_names_without_false_collisions() {
        use std::io::Write;
        let temp=tempfile::tempdir().unwrap();
        let mut storage=Storage::open(temp.path(),false,storage::new_journal("archive-versions".into())).unwrap();
        let mut originals=Vec::new();
        for body in [b"version one".as_slice(),b"version two".as_slice()] {
            let mut writer=zip::ZipWriter::new(Cursor::new(Vec::new()));
            writer.start_file("app.js",zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(body).unwrap();
            originals.push(storage.preserve_raw(&writer.finish().unwrap().into_inner(),"zip").unwrap());
        }
        originals.push(originals[0].clone());
        let identity=Identity {category:"test".into(),kind:"App".into(),scope:Scope::Global,keys:BTreeMap::from([("ID".into(),"Example".into())])};
        let (files,errors)=views(&storage,&identity,&originals);
        assert!(errors.is_empty(),"{errors:?}");assert_eq!(files.len(),2);
        for (index,expected) in [b"version one".as_slice(),b"version two".as_slice()].iter().enumerate() {
            let file=files.iter().find(|f|f.path.ends_with(&format!("Code/Archive {}/app.js",index+1))).unwrap();
            assert_eq!(std::fs::read(temp.path().join(&file.path)).unwrap(),*expected);
            storage.verify(file).unwrap();
        }
    }
    #[test]
    fn portable_collisions_inside_one_archive_remain_visible() {
        use std::io::Write;
        let mut writer=zip::ZipWriter::new(Cursor::new(Vec::new()));
        for name in ["App.js","app.js"] {
            writer.start_file(name,zip::write::SimpleFileOptions::default()).unwrap();writer.write_all(name.as_bytes()).unwrap();
        }
        let (files,errors)=extract(&writer.finish().unwrap().into_inner());
        assert_eq!(files.len(),2);
        assert_eq!(errors.iter().filter(|e|e.code=="ARCHIVE_NAME_COLLISION").count(),1);
    }
    #[test]
    fn native_zero_payload_deflate_preserves_empty_member() {
        let (files, errors) = extract(&native_empty_deflate(0, 0));
        assert!(errors.is_empty(), "{errors:?}");
        assert!(
            files
                .iter()
                .any(|f| f.bytes == 0 && f.sha256 == storage::sha256(b""))
        );
    }
    #[test]
    fn zero_payload_with_nonzero_crc_or_length_remains_invalid() {
        for (crc, size) in [(1, 0), (0, 1)] {
            let (files, errors) = extract(&native_empty_deflate(crc, size));
            assert!(errors.iter().any(|e| e.code == "SOURCE_STREAM_FAILED"));
            assert!(!files.iter().any(|f| f.bytes == 0));
        }
    }

    #[test]
    fn ordinary_empty_and_nonempty_payloads_still_use_verified_streams() {
        use std::io::Write;
        for method in [
            zip::CompressionMethod::Stored,
            zip::CompressionMethod::Deflated,
        ] {
            let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
            writer
                .start_file(
                    "empty.cs",
                    zip::write::SimpleFileOptions::default().compression_method(method),
                )
                .unwrap();
            writer
                .start_file(
                    "body.cs",
                    zip::write::SimpleFileOptions::default().compression_method(method),
                )
                .unwrap();
            writer.write_all(b"authored source").unwrap();
            let bytes = writer.finish().unwrap().into_inner();
            let (files, errors) = extract(&bytes);
            assert!(errors.is_empty(), "{errors:?}");
            assert!(files.iter().any(|f| f.bytes == 0));
            assert!(
                files
                    .iter()
                    .any(|f| f.sha256 == storage::sha256(b"authored source"))
            );
        }
    }
    #[test]
    fn damaged_nonempty_payload_is_not_published_as_a_complete_view() {
        use std::io::Write;
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "body.cs",
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(b"authored source").unwrap();
        let mut bytes = writer.finish().unwrap().into_inner();
        let offset = zip::ZipArchive::new(Cursor::new(&bytes))
            .unwrap()
            .by_index(0)
            .unwrap()
            .data_start() as usize;
        bytes[offset] ^= 1;
        let (files, errors) = extract(&bytes);
        assert!(errors.iter().any(|e| e.code == "SOURCE_STREAM_FAILED"));
        assert!(files.is_empty());
    }
}
