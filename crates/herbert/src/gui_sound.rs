//! GUI-only sound cues. CLI and MCP never create this observer or audio worker.
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
    mpsc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cue {
    Batch,
    Complete,
    Attention,
}

#[derive(Default)]
pub(super) struct CompletionSound {
    run_id: String,
    batches: BTreeSet<usize>,
    finished: bool,
    enabled: bool,
    generation: Arc<AtomicU64>,
    sender: Option<mpsc::Sender<(u64, Cue)>>,
}
impl CompletionSound {
    pub(super) fn set_enabled(&mut self, enabled: bool) {
        if self.enabled != enabled {
            self.enabled = enabled;
            // Invalidate queued cues. Re-enabling sound must not replay old work.
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
    }
    pub(super) fn observe(&mut self, journal: &Value, enabled: bool) -> Vec<Cue> {
        self.set_enabled(enabled);
        let id = journal["runId"].as_str().unwrap_or("");
        if id.is_empty() {
            return Vec::new();
        }
        if self.run_id != id {
            self.run_id = id.into();
            self.batches.clear();
            self.finished = false;
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
        let mut cues = Vec::new();
        // Rows retain creation order, but batches can finish out of order.
        // Track each index, not just the number of completed batches.
        for (index, batch) in journal["transferBatches"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            if batch["phase"] == "Processed"
                && batch["files"].as_u64().is_some_and(|n| n > 0)
                && self.batches.insert(index)
                && enabled
            {
                cues.push(Cue::Batch);
            }
        }
        let phase = journal["phase"].as_str().unwrap_or("");
        if !self.finished
            && super::gui_progress::terminal(phase)
            && !super::gui_progress::execution_active(journal)
        {
            self.finished = true;
            // Completed work must not leave a backlog of batch sounds playing
            // after the run. Keep only the terminal cue from this observation.
            self.generation.fetch_add(1, Ordering::SeqCst);
            cues.clear();
            if enabled && phase != "canceled" {
                let warnings = journal["diagnostics"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty());
                cues.push(if phase == "completed" && !warnings {
                    Cue::Complete
                } else {
                    Cue::Attention
                });
            }
        }
        cues
    }
    pub(super) fn queue(&mut self, cues: Vec<Cue>) {
        if cues.is_empty() {
            return;
        }
        if self.sender.is_none() {
            let (sender, receiver) = mpsc::channel::<(u64, Cue)>();
            let generation = self.generation.clone();
            // One worker preserves each cue without overlap or GUI delays.
            if std::thread::Builder::new()
                .name("herbert-gui-audio".into())
                .spawn(move || {
                    for (expected, cue) in receiver {
                        if generation.load(Ordering::SeqCst) == expected {
                            play(cue);
                        }
                    }
                })
                .is_ok()
            {
                self.sender = Some(sender);
            }
        }
        if let Some(sender) = &self.sender {
            let generation = self.generation.load(Ordering::SeqCst);
            for cue in cues {
                let _ = sender.send((generation, cue));
            }
        }
    }
}
impl Drop for CompletionSound {
    fn drop(&mut self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
}
fn play(cue: Cue) {
    #[cfg(windows)]
    {
        #[link(name = "winmm")]
        unsafe extern "system" {
            fn PlaySoundW(sound: *const u16, module: *mut std::ffi::c_void, flags: u32) -> i32;
        }
        let Some(wave) = std::env::current_exe()
            .ok()
            .and_then(|exe| read_sound(&exe, cue))
        else {
            return;
        };
        const SND_MEMORY: u32 = 0x0004;
        const SND_NODEFAULT: u32 = 0x0002;
        // Read closes the file before playback. Keep the bytes alive until the
        // worker finishes. NODEFAULT prevents an unwanted Windows alert.
        unsafe {
            PlaySoundW(
                wave.as_ptr().cast(),
                std::ptr::null_mut(),
                SND_MEMORY | SND_NODEFAULT,
            );
        }
    }
    #[cfg(not(windows))]
    let _ = cue;
}

fn read_sound(executable: &std::path::Path, cue: Cue) -> Option<Vec<u8>> {
    let name = match cue {
        Cue::Batch => "batch.wav",
        Cue::Complete => "complete.wav",
        Cue::Attention => "attention.wav",
    };
    // Read each cue again so replacements take effect without a restart.
    std::fs::read(executable.parent()?.join("sounds").join(name)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn external_cues_reload_and_release_the_file() {
        let root = tempfile::tempdir().unwrap();
        let exe = root.path().join("herbert.exe");
        let sounds = root.path().join("sounds");
        std::fs::create_dir(&sounds).unwrap();
        assert!(read_sound(&exe, Cue::Batch).is_none());
        for (cue, name) in [
            (Cue::Batch, "batch.wav"),
            (Cue::Complete, "complete.wav"),
            (Cue::Attention, "attention.wav"),
        ] {
            let path = sounds.join(name);
            std::fs::write(&path, b"first").unwrap();
            let previous = read_sound(&exe, cue).unwrap();
            std::fs::write(&path, b"replacement").unwrap();
            assert_eq!(read_sound(&exe, cue).unwrap(), b"replacement");
            std::fs::remove_file(path).unwrap();
            assert_eq!(previous, b"first");
            assert!(read_sound(&exe, cue).is_none());
        }
    }
    #[test]
    fn batches_finish_out_of_order_without_repeated_cues() {
        let mut sound = CompletionSound::default();
        let mut status = json!({"runId":"one","phase":"running","transferBatches":[
            {"phase":"Writing files","files":24},{"phase":"Processed","files":7}]});
        assert_eq!(sound.observe(&status, true), vec![Cue::Batch]);
        assert!(sound.observe(&status, true).is_empty());
        status["transferBatches"][0]["phase"] = json!("Processed");
        assert_eq!(sound.observe(&status, true), vec![Cue::Batch]);
        assert!(sound.observe(&status, true).is_empty());
    }
    #[test]
    fn every_nonempty_batch_finishing_between_polls_gets_a_cue() {
        let mut sound = CompletionSound::default();
        let status = json!({"runId":"one","phase":"running","transferBatches":[
            {"phase":"Processed","files":24},{"phase":"Processed","files":24},
            {"phase":"Processed","files":0},{"phase":"Interrupted","files":5}]});
        assert_eq!(sound.observe(&status, true), vec![Cue::Batch, Cue::Batch]);
    }
    #[test]
    fn completion_waits_for_cleanup_and_does_not_repeat() {
        let mut sound = CompletionSound::default();
        let mut status = json!({"runId":"one","phase":"completed","executionActive":true});
        assert!(sound.observe(&status, true).is_empty());
        status["executionActive"] = json!(false);
        assert_eq!(sound.observe(&status, true), vec![Cue::Complete]);
        assert!(sound.observe(&status, true).is_empty());
        status["runId"] = json!("two");
        status["phase"] = json!("partial");
        assert_eq!(sound.observe(&status, true), vec![Cue::Attention]);
    }
    #[test]
    fn finishing_discards_pending_batch_cues() {
        let mut sound = CompletionSound::default();
        let mut status = json!({"runId":"one","phase":"running","transferBatches":[
            {"phase":"Processed","files":100}]});
        assert_eq!(sound.observe(&status, true), vec![Cue::Batch]);
        let queued_generation = sound.generation.load(Ordering::SeqCst);
        status["transferBatches"].as_array_mut().unwrap().push(json!({"phase":"Processed","files":20}));
        status["phase"] = json!("completed");
        status["executionActive"] = json!(false);
        assert_eq!(sound.observe(&status, true), vec![Cue::Complete]);
        assert_ne!(sound.generation.load(Ordering::SeqCst), queued_generation);
        assert!(sound.observe(&status, true).is_empty());
    }
    #[test]
    fn mute_consumes_events_and_invalidates_queued_cues() {
        let mut sound = CompletionSound::default();
        let mut status = json!({"runId":"one","phase":"running","transferBatches":[
            {"phase":"Processed","files":24}]});
        assert!(sound.observe(&status, false).is_empty());
        assert!(sound.observe(&status, true).is_empty());
        let before = sound.generation.load(Ordering::SeqCst);
        sound.set_enabled(false);
        assert_ne!(sound.generation.load(Ordering::SeqCst), before);
        status["phase"] = json!("completed");
        status["executionActive"] = json!(false);
        assert!(sound.observe(&status, false).is_empty());
        assert!(sound.observe(&status, true).is_empty());
        status["runId"] = json!("two");
        status["transferBatches"] = json!([]);
        status["phase"] = json!("canceled");
        assert!(sound.observe(&status, true).is_empty());
    }
}
