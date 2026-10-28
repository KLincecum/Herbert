//! Shared memory accounting for fetched pages. Disk files are temporary queue entries.
use herbert_contract::{Diagnostic, FileRef, GuestReply};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Serialize, Deserialize)]
pub(crate) struct Page {
    pub cursor: Option<String>,
    pub reply: GuestReply,
    pub files: Vec<FileRef>,
}

#[derive(Default)]
struct Accounting {
    retained: u64,
    working: std::collections::BTreeMap<String, u64>,
}

/// Account for every plugin in this host. A worker reserves its largest observed page.
#[derive(Default)]
pub(crate) struct BufferMemory(Mutex<Accounting>);

pub(crate) struct Producer {
    memory: Arc<BufferMemory>,
    id: String,
}
impl Drop for Producer {
    fn drop(&mut self) {
        self.memory.0.lock().unwrap().working.remove(&self.id);
    }
}

struct Charge {
    memory: Arc<BufferMemory>,
    bytes: u64,
}
impl Drop for Charge {
    fn drop(&mut self) {
        self.memory.0.lock().unwrap().retained -= self.bytes;
    }
}

enum Payload {
    Memory(Page),
    Disk(File, [u8; 32]),
}
pub(crate) struct BufferedPage {
    payload: Payload,
    charge: Option<Charge>,
    pub bytes: u64,
    pub spilled: bool,
}
pub(crate) struct LoadedPage {
    pub page: Page,
    // Keep the reservation until materialization finishes, not just until dequeue.
    _charge: Option<Charge>,
}

impl BufferMemory {
    pub fn producer(self: &Arc<Self>) -> Producer {
        let id = uuid::Uuid::new_v4().to_string();
        self.0.lock().unwrap().working.insert(id.clone(), 0);
        Producer {
            memory: self.clone(),
            id,
        }
    }
}
impl Producer {
    pub fn store(&self, page: Page, directory: &Path) -> Result<BufferedPage, Diagnostic> {
        self.store_with_memory(page, directory, available_memory())
    }

    fn store_with_memory(
        &self,
        page: Page,
        directory: &Path,
        available: Option<u64>,
    ) -> Result<BufferedPage, Diagnostic> {
        // Retain the original tree in memory. Do not serialize and parse every
        // ordinary page merely to pass it from one stage to the next.
        let size = page_memory(&page)?;
        let charge = {
            let mut state = self.memory.0.lock().unwrap();
            let largest = state.working.entry(self.id.clone()).or_default();
            *largest = (*largest).max(size);
            let working = state
                .working
                .values()
                .copied()
                .sum::<u64>()
                .saturating_mul(2);
            // Available memory already excludes this page and retained buffers.
            // Two observed page footprints reserve the next fetch and materialization.
            if available.is_some_and(|free| free > working.saturating_add(size)) {
                state.retained += size;
                Some(Charge {
                    memory: self.memory.clone(),
                    bytes: size,
                })
            } else {
                None
            }
        };
        let spilled = charge.is_none();
        let payload = if spilled {
            std::fs::create_dir_all(directory).map_err(|_| io_error())?;
            // Anonymous files are removed when their last handle closes, including
            // process termination. No saved cursor depends on this temporary file.
            let file = tempfile::tempfile_in(directory).map_err(|_| io_error())?;
            let mut writer = HashWriter {
                inner: BufWriter::new(file),
                hash: Sha256::new(),
            };
            serde_json::to_writer(&mut writer, &page).map_err(|_| io_error())?;
            writer.flush().map_err(|_| io_error())?;
            let hash: [u8; 32] = writer.hash.finalize().into();
            let mut file = writer.inner.into_inner().map_err(|_| io_error())?;
            file.seek(SeekFrom::Start(0)).map_err(|_| io_error())?;
            Payload::Disk(file, hash)
        } else {
            Payload::Memory(page)
        };
        Ok(BufferedPage {
            payload,
            charge,
            bytes: size,
            spilled,
        })
    }
}
impl BufferedPage {
    pub fn load(self) -> Result<LoadedPage, Diagnostic> {
        let page = match self.payload {
            Payload::Memory(page) => page,
            Payload::Disk(mut file, expected) => {
                file.seek(SeekFrom::Start(0)).map_err(|_| io_error())?;
                let mut reader = BufReader::new(HashReader {
                    inner: file,
                    hash: Sha256::new(),
                });
                let page = serde_json::from_reader(&mut reader).map_err(|_| invalid())?;
                let actual: [u8; 32] = reader.into_inner().hash.finalize().into();
                if actual != expected {
                    return Err(invalid());
                }
                page
            }
        };
        Ok(LoadedPage {
            page,
            _charge: self.charge,
        })
    }
}
struct HashWriter<W> {
    inner: W,
    hash: Sha256,
}
impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let count = self.inner.write(bytes)?;
        self.hash.update(&bytes[..count]);
        Ok(count)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
struct HashReader<R> {
    inner: R,
    hash: Sha256,
}
impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let count = self.inner.read(bytes)?;
        self.hash.update(&bytes[..count]);
        Ok(count)
    }
}
fn invalid() -> Diagnostic {
    Diagnostic::new(
        "PAGE_BUFFER_INVALID",
        "Herbert could not read the buffered page. Resume the run to fetch this page again.",
    )
}
fn io_error() -> Diagnostic {
    Diagnostic::new(
        "PAGE_BUFFER_IO_FAILED",
        "Herbert could not store a temporary page. Check free space and access to Herbert's data folder.",
    )
}
fn page_memory(page: &Page) -> Result<u64, Diagnostic> {
    struct Count(u64);
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len() as u64);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    // Include identities, file references and cursors without allocating a second
    // copy. JSON trees need additional space for their containers and strings.
    let mut encoded = Count(0);
    serde_json::to_writer(&mut encoded, page).map_err(|_| invalid())?;
    fn value(v: &serde_json::Value) -> u64 {
        use serde_json::Value::*;
        std::mem::size_of_val(v) as u64
            + match v {
                String(s) => s.capacity() as u64,
                Array(a) => a.iter().map(value).sum(),
                Object(o) => o.iter().map(|(k, v)| k.capacity() as u64 + value(v)).sum(),
                _ => 0,
            }
    }
    let tree = std::mem::size_of_val(page) as u64
        + match &page.reply {
            GuestReply::PreparedPage {
                state, next_state, ..
            } => value(state) + value(next_state),
            GuestReply::InventoryData { records, .. } => {
                records.iter().map(|(_, v)| value(v)).sum()
            }
            _ => 0,
        };
    Ok(tree.max(encoded.0))
}

#[cfg(windows)]
fn available_memory() -> Option<u64> {
    #[repr(C)]
    struct Memory {
        length: u32,
        load: u32,
        total: u64,
        available: u64,
        total_commit: u64,
        available_commit: u64,
        total_virtual: u64,
        available_virtual: u64,
        extended: u64,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut Memory) -> i32;
        fn CreateMemoryResourceNotification(kind: i32) -> *mut std::ffi::c_void;
        fn QueryMemoryResourceNotification(handle: *mut std::ffi::c_void, state: *mut i32) -> i32;
        fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    }
    // Use the operating system's low-memory signal instead of inventing a RAM
    // percentage. Also reserve the observed working pages below that boundary.
    let handle = unsafe { CreateMemoryResourceNotification(0) };
    if handle.is_null() {
        return None;
    }
    let mut low = 0;
    let queried = unsafe { QueryMemoryResourceNotification(handle, &mut low) };
    unsafe {
        CloseHandle(handle);
    }
    if queried == 0 {
        return None;
    }
    if low != 0 {
        return Some(0);
    }
    let mut status: Memory = unsafe { std::mem::zeroed() };
    status.length = std::mem::size_of::<Memory>() as u32;
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(
        status
            .available
            .min(status.available_commit)
            .min(status.available_virtual),
    )
}
#[cfg(target_os = "linux")]
fn available_memory() -> Option<u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("MemAvailable:")?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
                .map(|n| n.saturating_mul(1024))
        })
}
#[cfg(not(any(windows, target_os = "linux")))]
fn available_memory() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    fn page(cursor: &str) -> Page {
        Page {
            cursor: Some(cursor.into()),
            files: vec![],
            reply: GuestReply::PreparedPage {
                state: serde_json::json!({"payload":"test content"}),
                next_cursor: None,
                next_state: serde_json::Value::Null,
            },
        }
    }
    #[test]
    fn memory_path_does_not_create_disk_files_and_releases_charge() {
        let memory = Arc::new(BufferMemory::default());
        let producer = memory.producer();
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("not-created");
        let entry = producer
            .store_with_memory(page("1"), &absent, Some(u64::MAX))
            .unwrap();
        assert!(!entry.spilled);
        let loaded = entry.load().unwrap();
        assert_eq!(loaded.page.cursor.as_deref(), Some("1"));
        assert!(memory.0.lock().unwrap().retained > 0);
        drop(loaded);
        assert_eq!(memory.0.lock().unwrap().retained, 0);
        assert!(!absent.exists());
    }
    #[test]
    fn pressure_spills_roundtrips_and_cleans_up_on_consume_or_cancel() {
        let memory = Arc::new(BufferMemory::default());
        let producer = memory.producer();
        let dir = tempfile::tempdir().unwrap();
        let first = producer
            .store_with_memory(page("1"), dir.path(), Some(0))
            .unwrap();
        let canceled = producer
            .store_with_memory(page("2"), dir.path(), Some(0))
            .unwrap();
        assert!(first.spilled);
        assert_eq!(first.load().unwrap().page.cursor.as_deref(), Some("1"));
        drop(canceled);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert_eq!(memory.0.lock().unwrap().retained, 0);
    }
    #[test]
    fn corrupted_spill_is_rejected_and_its_file_is_removed() {
        let memory = Arc::new(BufferMemory::default());
        let producer = memory.producer();
        let dir = tempfile::tempdir().unwrap();
        let mut entry = producer
            .store_with_memory(page("1"), dir.path(), Some(0))
            .unwrap();
        if let Payload::Disk(file, _) = &mut entry.payload {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            let index = bytes
                .windows(12)
                .position(|s| s == b"test content")
                .unwrap();
            bytes[index] = b'b'; // Valid JSON with different data must also fail.
            file.seek(SeekFrom::Start(0)).unwrap();
            file.write_all(&bytes).unwrap();
        }
        assert!(entry.load().is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    #[test]
    fn all_producers_share_the_observed_working_memory_reservation() {
        let memory = Arc::new(BufferMemory::default());
        let first = memory.producer();
        let second = memory.producer();
        let dir = tempfile::tempdir().unwrap();
        let size = page_memory(&page("1")).unwrap();
        let one = first
            .store_with_memory(page("1"), dir.path(), Some(size * 4))
            .unwrap();
        assert!(!one.spilled);
        let two = second
            .store_with_memory(page("2"), dir.path(), Some(size * 4))
            .unwrap();
        assert!(two.spilled);
        drop(first);
        let three = second
            .store_with_memory(page("3"), dir.path(), Some(size * 4))
            .unwrap();
        assert!(!three.spilled);
    }
    #[tokio::test]
    async fn mixed_memory_and_disk_pages_keep_order_and_cancel_releases_the_queue() {
        let memory = Arc::new(BufferMemory::default());
        let producer = memory.producer();
        let dir = tempfile::tempdir().unwrap();
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        // Fetching can queue all pages before the first consumer finishes.
        for i in 0..12 {
            let free = if i % 2 == 0 { 0 } else { u64::MAX };
            send.send(
                producer
                    .store_with_memory(page(&i.to_string()), dir.path(), Some(free))
                    .unwrap(),
            )
            .ok()
            .unwrap();
        }
        for i in 0..5 {
            let loaded = receive.recv().await.unwrap().load().unwrap();
            assert_eq!(loaded.page.cursor, Some(i.to_string()));
        }
        drop(receive);
        assert_eq!(memory.0.lock().unwrap().retained, 0);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
