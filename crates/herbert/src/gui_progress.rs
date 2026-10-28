//! Current-run progress uses journal facts, not elapsed-time estimates.
use super::*;
#[path = "gui_plugin_activity.rs"]
mod plugin_activity;
#[path = "gui_run_issues.rs"]
mod run_issues;

#[cfg(windows)]
fn download_folder(output_root: &str) -> Option<std::path::PathBuf> {
    if output_root.is_empty() {
        return None;
    }
    let path = std::path::PathBuf::from(output_root);
    path.is_dir().then_some(path)
}

#[cfg(windows)]
fn folder_command(output_root: &str) -> Result<std::process::Command, String> {
    let folder = download_folder(output_root)
        .ok_or_else(|| "The download folder is not available yet.".to_owned())?;
    // Explorer expects ordinary drive/UNC paths. Preserve the exact run path,
    // including spaces, as one argument. Never interpolate it into a shell.
    let text = folder.to_string_lossy();
    let explorer_path = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
    };
    let windows = std::env::var_os("SystemRoot")
        .ok_or_else(|| "Windows Explorer could not be located.".to_owned())?;
    let mut command =
        std::process::Command::new(std::path::PathBuf::from(windows).join("explorer.exe"));
    command.arg(explorer_path);
    Ok(command)
}

pub(super) fn terminal(phase: &str) -> bool {
    matches!(
        phase,
        "partial" | "canceled" | "completed" | "failed" | "destinationReleaseUncertain"
    )
}
pub(super) fn execution_active(journal: &Value) -> bool {
    journal["executionActive"]
        .as_bool()
        .unwrap_or_else(|| !terminal(journal["phase"].as_str().unwrap_or("")))
}
fn phase_label(phase: &str, active: bool) -> &str {
    if active && terminal(phase) {
        return "Finishing cleanup";
    }
    match phase {
        "completed" => "Download complete",
        "partial" => "Finished with incomplete items",
        "failed" => "Download failed",
        "canceled" => "Download canceled",
        "paused" => "Paused",
        "running" => "Downloading",
        "discovering" => "Discovering items",
        "acquiring" => "Downloading",
        "installingArtifacts" => "Installing temporary helper in Epicor",
        "cleaningArtifacts" => "Removing temporary helper from Epicor",
        "destinationReleaseUncertain" => "Output cleanup needs attention",
        other => other,
    }
}
#[derive(Default, Debug, PartialEq)]
struct Counts {
    categories: usize,
    discovered_categories: usize,
    known: usize,
    handled: usize,
    files: usize,
    bytes: u64,
    incomplete: usize,
}
impl Counts {
    fn from_journal(journal: &Value) -> Self {
        let artifacts = journal["artifacts"].as_array();
        Self {
            categories: journal["categories"].as_array().map_or(0, Vec::len),
            discovered_categories: journal["discoveryFinished"]
                .as_object()
                .map_or(0, |groups| {
                    groups
                        .values()
                        .filter(|value| value.as_bool() == Some(true))
                        .count()
                }),
            known: journal["discovered"].as_object().map_or(0, |groups| {
                groups
                    .values()
                    .filter_map(Value::as_array)
                    .map(Vec::len)
                    .sum()
            }),
            handled: artifacts.map_or(0, Vec::len),
            files: journal["fileCount"]
                .as_u64()
                .map(|n| n as usize)
                .unwrap_or_else(|| journal["acquisitions"].as_array().map_or(0, Vec::len)),
            bytes: journal["fileBytes"].as_u64().unwrap_or_else(|| {
                journal["acquisitions"].as_array().map_or(0, |files| {
                    files.iter().filter_map(|file| file["bytes"].as_u64()).sum()
                })
            }),
            incomplete: artifacts.map_or(0, |items| {
                items.iter().filter(|item| incomplete(item)).count()
            }),
        }
    }
}

#[derive(Default, Debug, PartialEq)]
struct ItemResults {
    downloaded: usize,
    partial: usize,
    failed: usize,
    absent: usize,
    excluded: usize,
}
impl ItemResults {
    fn add(&mut self, item: &Value) {
        match (
            item["acquisition"].as_str().unwrap_or(""),
            item["processing"].as_str(),
        ) {
            ("collected", Some("derived")) => self.downloaded += 1,
            ("partial", _) | ("collected", _) => self.partial += 1,
            ("verifiedAbsent" | "verifiedNotApplicable", Some("derived")) => self.absent += 1,
            ("excluded", Some("derived")) => self.excluded += 1,
            _ => self.failed += 1,
        }
    }
    fn handled(&self) -> usize {
        self.downloaded + self.partial + self.failed + self.absent + self.excluded
    }
}

fn pending_items(journal: &Value, category: &str) -> usize {
    // Match the scheduler's identity deduplication, including scope and keys.
    fn key(value: &Value) -> String {
        serde_json::from_value::<herbert_contract::Identity>(value.clone())
            .map(|identity| crate::storage::identity_key(&identity))
            .unwrap_or_else(|_| value.to_string())
    }
    let expected = if journal["scope"].as_str() == Some("targeted") {
        journal["targets"].as_array()
    } else {
        journal["discovered"][category].as_array()
    };
    let expected: std::collections::BTreeSet<_> = expected
        .into_iter()
        .flatten()
        .filter(|identity| identity["category"].as_str() == Some(category))
        .map(key)
        .collect();
    let handled: std::collections::BTreeSet<_> = journal["artifacts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["identity"]["category"].as_str() == Some(category))
        .map(|item| key(&item["identity"]))
        .collect();
    expected.difference(&handled).count()
}

fn fraction(done: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        (done as f32 / total as f32).clamp(0.0, 1.0)
    }
}

/// Count current page definitions and actual decompiler jobs as equal work units.
/// This is a cycling work counter, not elapsed-time weighting. Pending capture
/// pages are explicit unsettled work before the server reveals assembly counts.
fn combined_work_progress(definitions: (usize,usize), statistics:&Value)->f32 {
    let jobs=&statistics["decompilerJobs"];
    let count=|name:&str|jobs[name].as_u64().unwrap_or(0) as f64;
    let settled=count("finished")+count("failed");
    let pending=count("queued")+count("running")+count("committing")
        +statistics["pendingCompiledPages"].as_u64().unwrap_or(0) as f64;
    let done=definitions.0.min(definitions.1) as f64+settled;
    let total=definitions.1 as f64+settled+pending;
    if total==0.0{return 0.0;}
    let progress=(done/total) as f32;
    // A very large settled count must not round outstanding work to complete.
    if pending>0.0 {progress.min(f32::from_bits(1.0f32.to_bits()-1))}else{progress.clamp(0.0,1.0)}
}

#[test]
fn combined_progress_tracks_concurrent_pages_jobs_and_pending_reads(){
    let mut stats=json!({"decompilerJobs":{"finished":3,"failed":1,"queued":2,"running":1,"committing":1}});
    assert_eq!(combined_work_progress((10,10),&stats),14.0/18.0);
    for state in ["queued","running","committing"]{assert!(combined_work_progress((100,100),&json!({"decompilerJobs":{(state):1}}))<1.0);}
    stats["decompilerJobs"]=json!({"finished":7,"failed":1});
    assert_eq!(combined_work_progress((10,10),&stats),1.0);
    assert!(combined_work_progress((0,10),&stats)<1.0); // a new page cycles back
    stats["pendingCompiledPages"]=json!(1);
    assert!(combined_work_progress((10,10),&stats)<1.0);
    assert_eq!(combined_work_progress((0,0),&json!({})),0.0);
    assert_eq!(combined_work_progress((0,0),&json!({"decompilerJobs":{"running":1}})),0.0);
    assert!(combined_work_progress((usize::MAX,usize::MAX),&json!({"decompilerJobs":{"queued":1}}))<1.0);
    assert_eq!(combined_work_progress((5,10),&json!({})),0.5);
}

/// Replace each streamed category's live item count with its committed batch
/// count. Other collectors retain their normal progress, including on resume.
fn completed_batch_progress(journal: &Value, handled: usize, batches: &[Value]) -> usize {
    let mut completed = std::collections::BTreeMap::new();
    for batch in batches {
        if let (Some(category), Some(count)) =
            (batch["category"].as_str(), batch["completedItems"].as_u64())
        {
            completed.insert(category, count as usize);
        }
    }
    let mut progress = handled;
    for (category, committed) in completed {
        let live = journal["artifacts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|a| a["identity"]["category"] == category)
            .count();
        progress = progress.saturating_sub(live) + committed;
    }
    progress
}

/// Use the latest received batch while another page is in transit. A new
/// request must not erase the work that was just completed on this machine.
fn received_batch_progress(batches: &[Value]) -> Option<(usize, usize, usize)> {
    let mut latest = std::collections::BTreeMap::new();
    let mut last_index = 0;
    for (index, batch) in batches.iter().enumerate() {
        let total = batch["files"].as_u64().unwrap_or(0) as usize;
        if total == 0 { continue; }
        let written = batch.get("processed").or_else(|| batch.get("verified"))
            .and_then(Value::as_u64).unwrap_or(0) as usize;
        let key = batch["category"].as_str().or_else(|| batch["plugin"].as_str()).unwrap_or("");
        latest.insert(key, (written, total));
        last_index = index + 1;
    }
    (!latest.is_empty()).then(|| (last_index,
        latest.values().map(|(written,_)| written).sum(),
        latest.values().map(|(_,total)| total).sum()))
}

#[test]
fn received_batch_advances_on_writes_and_survives_the_next_request() {
    let mut batches = vec![json!({"files":24,"processed":0})];
    for written in [0, 1, 4, 23, 24] {
        batches[0]["processed"] = json!(written);
        assert_eq!(received_batch_progress(&batches), Some((1, written, 24)));
    }
    batches.push(json!({"files":0,"phase":"Downloading"}));
    assert_eq!(received_batch_progress(&batches), Some((1, 24, 24)));
    batches[1] = json!({"files":7,"processed":1});
    assert_eq!(received_batch_progress(&batches), Some((2, 1, 7)));
}

#[test]
fn vacuum_counts_current_pages_from_all_plugins_not_only_the_last_response() {
    let batches=vec![json!({"category":"A","files":100,"processed":100}),
        json!({"category":"A","files":100,"processed":25}),
        json!({"category":"B","files":24,"processed":12}),
        json!({"category":"A","files":0,"phase":"Downloading"})];
    assert_eq!(received_batch_progress(&batches),Some((3,37,124)));
}

#[test]
fn main_progress_waits_for_batch_commit_and_preserves_other_collections() {
    let journal = |count: usize| {
        json!({"artifacts": (0..count).map(|_| json!({"identity":{"category":"C04"}}))
        .chain(std::iter::once(json!({"identity":{"category":"Other"}}))).collect::<Vec<_>>()})
    };
    for live in [0, 1, 4, 23, 24] {
        assert_eq!(
            completed_batch_progress(
                &journal(live),
                live + 1,
                &[json!({"category":"C04","completedItems":0,"phase":"Processing"})]
            ),
            1
        );
    }
    for (live, committed) in [(24, 24), (28, 24), (47, 24), (48, 48), (52, 48), (53, 53)] {
        assert_eq!(
            completed_batch_progress(
                &journal(live),
                live + 1,
                &[json!({"category":"C04","completedItems":committed})]
            ),
            committed + 1
        );
    }
    // A resumed run keeps its previous completed items rather than resetting.
    assert_eq!(
        completed_batch_progress(
            &journal(75),
            76,
            &[json!({"category":"C04","completedItems":72})]
        ),
        73
    );
}

fn identity_label(journal: &Value, identity: &Value) -> String {
    let category = identity["category"]
        .as_str()
        .and_then(|id| journal["categoryNames"].get(id))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Collection");
    let kind = identity["kind"].as_str().unwrap_or("item");
    let keys = identity["keys"]
        .as_object()
        .map(|keys| {
            keys.iter()
                .map(|(key, value)| format!("{key}={}", value.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    if keys.is_empty() {
        format!("{category} / {kind}")
    } else {
        format!("{category} / {kind}: {keys}")
    }
}
fn incomplete(artifact: &Value) -> bool {
    !matches!(
        artifact["acquisition"].as_str(),
        Some("collected" | "verifiedAbsent" | "verifiedNotApplicable" | "excluded")
    ) || artifact["processing"].as_str() != Some("derived")
}
#[cfg(test)]
fn issue_lines(journal: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(diagnostics) = journal["diagnostics"].as_array() {
        for diagnostic in diagnostics {
            if let Some(message) = diagnostic["message"].as_str() {
                lines.push(message.into());
            }
        }
    }
    if let Some(artifacts) = journal["artifacts"].as_array() {
        for artifact in artifacts {
            let diagnostics = artifact["diagnostics"].as_array();
            if incomplete(artifact) || diagnostics.is_some_and(|items| !items.is_empty()) {
                let name = identity_label(journal, &artifact["identity"]);
                let state = format!(
                    "{}; processing {}",
                    artifact["acquisition"].as_str().unwrap_or("unknown"),
                    artifact["processing"].as_str().unwrap_or("unknown")
                );
                let messages = diagnostics
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item["message"].as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                lines.push(if messages.is_empty() {
                    format!("{name} — {state}")
                } else {
                    format!("{name} — {state}. {messages}")
                });
            }
        }
    }
    lines
}
#[cfg(test)]
fn recent_activity(journal: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(artifact) = journal["artifacts"]
        .as_array()
        .and_then(|items| items.last())
    {
        lines.push(format!(
            "Latest item: {} — {}; processing {}",
            identity_label(journal, &artifact["identity"]),
            artifact["acquisition"].as_str().unwrap_or("unknown"),
            artifact["processing"].as_str().unwrap_or("unknown")
        ));
    }
    if let Some(path) = journal["acquisitions"]
        .as_array()
        .and_then(|files| files.last())
        .and_then(|file| file["path"].as_str())
    {
        lines.push(format!("Latest saved file: {path}"));
    }
    lines
}
pub(super) fn duration(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
}

fn statistics_line(journal: &Value, files: usize, active: bool) -> String {
    let s = &journal["statistics"];
    let Some(ms) = s["elapsedMs"].as_u64() else { return "Timing: unavailable for this execution".into(); };
    let n = |key: &str| s[key].as_u64().unwrap_or(0);
    let rate = if ms > 0 { files as f64 * 1000.0 / ms as f64 } else { 0.0 };
    format!("{} {} | {:.1} files/s avg | {} requests / {:.1}s | {:.1} MiB received | {} pages written / {:.1}s",
        if active { "Elapsed" } else { "Finished in" }, duration(ms), rate, n("requests"), n("requestMs") as f64 / 1000.0,
        n("receivedBytes") as f64 / 1_048_576.0, n("pages"), n("pageWriteMs") as f64 / 1000.0)
}

impl Gui {
    pub(super) fn progress(&mut self, ui: &mut egui::Ui) {
        let waiting = json!({"phase":"starting", "executionActive":true});
        let journal = self.journal.as_ref().unwrap_or(&waiting).clone();
        let operations = journal["activeOperations"].as_array().cloned().unwrap_or_default();
        let batches = journal["transferBatches"].as_array().cloned().unwrap_or_default();
        let phase = journal["phase"].as_str().unwrap_or("starting");
        let active = execution_active(&journal);
        let paused = journal["pauseRequested"] == true || phase == "paused";
        let canceling = journal["cancelRequested"] == true;
        let counts = Counts::from_journal(&journal);
        let completed = completed_batch_progress(&journal, counts.handled, &batches);
        let rows = plugin_activity::rows(&journal, &operations, &batches);

        let diagnostics_inline=ui.available_width()>=900.0;
        // The control row has the same allocation in every run state.
        ui.horizontal(|ui| {
            let enabled = active && !terminal(phase) && !canceling;
            let (label, action) = if paused { ("Resume", "run.resume") } else { ("Pause", "run.pause") };
            if ui.add_enabled(enabled, egui::Button::new(label).min_size(egui::vec2(88.0,32.0))).clicked() {
                self.send(action, json!({"id":self.run_id}), ui.ctx());
            }
            if ui.add_enabled(enabled, egui::Button::new("Cancel").min_size(egui::vec2(88.0,32.0))).clicked() {
                self.send("run.cancel", json!({"id":self.run_id}), ui.ctx());
            }
            self.open_download_button(ui);
            if diagnostics_inline {
            if ui.add_enabled(!self.diagnostic_export_pending,egui::Button::new("Save diagnostics…")).on_hover_text("Save a sanitized ZIP for support. Excludes source, business values, credentials, names and paths.").clicked(){
                self.save_diagnostics(ui.ctx());
            }
            }

            if ui.add_enabled(!active,egui::Button::new("New download").min_size(egui::vec2(112.0,32.0))).clicked() {
                self.run_id.clear(); self.journal=None; self.output_root.clear(); self.notice.clear();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let heading = if canceling && active { "Stopping and cleaning up" }
                    else if paused && active { "Paused / finishing active requests" }
                    else { phase_label(phase, active) };
                ui.add(egui::Label::new(RichText::new(heading).strong()).truncate());
            });
        });
        if !diagnostics_inline && ui.add_enabled(!self.diagnostic_export_pending,egui::Button::new("Save diagnostics…")).clicked(){self.save_diagnostics(ui.ctx());}
        ui.add_space(2.0);
        ui.collapsing("Run scope and frozen options", |ui| {
            ui.label(if journal["scope"] == "targeted" {
                "Exact native identities select the roots. Processing options still apply."
            } else {
                "Discovery within the frozen plugin options. Completeness applies to this selection."
            });
            for (category, values) in journal["pluginSettings"].as_object().into_iter().flatten() {
                let name = journal["categoryNames"][category].as_str().unwrap_or(category);
                ui.label(RichText::new(name).strong());
                // Values are private local run evidence. JSON quoting preserves
                // spaces and makes an explicitly blank filter visible as "".
                ui.add(egui::Label::new(values.to_string()).wrap());
            }
        });
        ui.add(egui::Label::new(format!("All plugins: {completed} items in completed batches   ·   {} files saved   ·   {:.1} MiB   ·   {} incomplete",
            counts.files, counts.bytes as f64 / 1_048_576.0, counts.incomplete)).truncate());

        let stats = statistics_line(&journal, counts.files, active);
        ui.add(egui::Label::new(&stats).truncate()).on_hover_text(format!("{stats}
Request time and completed-page write time are sums; concurrent work can overlap. Run time includes setup and cleanup. Timing covers the latest execution."));

        // Host setup and cleanup stay visible above the plugin table. The line
        // retains its height when there is no helper work.
        let host_work = operations.iter().filter(|operation| {
            let plugin=operation["plugin"].as_str().unwrap_or("Download");
            !rows.iter().any(|row| row.name == plugin)
        }).map(|operation| format!("{}: {}",operation["plugin"].as_str().unwrap_or("Host"),
            operation["step"].as_str().unwrap_or("Working"))).collect::<Vec<_>>().join(" · ");
        let host_work = if !host_work.is_empty() { host_work }
            else if active { "Host: collecting and processing plugin data".into() }
            else { "Host: run finished".into() };
        ui.add(egui::Label::new(RichText::new(&host_work).size(14.0)).truncate()).on_hover_text(&host_work);
        ui.add_space(2.0);
        plugin_activity::table(ui, &rows);

        // The work bar combines each plugin's latest definitions with decompiler work.
        // It never advances solely because time elapsed or a request started.
        let received = received_batch_progress(&batches);
        let total_known = counts.categories > 0 && counts.discovered_categories == counts.categories;
        let definitions = received.map_or_else(|| if total_known {(completed,counts.known)}else{(0,0)}, |(_,written,total)|(written,total));
        let progress = combined_work_progress(definitions,&journal["statistics"]);
            let time = ui.input(|input| input.time) as f32;
            let moving = active && !paused && !canceling;
            // Actual work drives travel; the definition bar remains quantitative.
            // Compress bursts for visible motion instead of skipping whole laps
            // when several files arrive in one UI poll.
            let work = journal["statistics"]["meaningfulWork"].as_u64().unwrap_or(0);
            let motion_id = ui.id().with(("vacuum-work", &self.run_id));
            let odometer_id=motion_id.with("odometer");
            let target=ui.ctx().data_mut(|d|{
                let state=d.get_temp_mut_or_default::<(u64,f32)>(odometer_id);
                // Recovery can restart execution counters for the same run ID.
                if moving && work<state.0 {state.0=0;}
                if moving && work>state.0 {state.1+=((work-state.0) as f64).ln_1p() as f32;state.0=work;}
                state.1
            });
            let held_id=motion_id.with("held");
            let held=ui.ctx().data(|d|d.get_temp::<f32>(held_id)).unwrap_or(0.0);
            let distance=if moving {ui.ctx().animate_value_with_time(motion_id,target,0.35)}else{held};
            ui.ctx().data_mut(|d|d.insert_temp(held_id,distance));
            let position=1.0-((distance/12.0).rem_euclid(2.0)-1.0).abs();
            // Celebrate finished processing, even when some items are incomplete.
            // Keep the actual result and diagnostics visible.
            let run_finished = finish_celebration(phase, active);
            let celebration_id = ui.id().with(("vacuum-celebration", &self.run_id, run_finished));
            let celebration_started = if run_finished {
                Some(ui.ctx().data_mut(|data| {
                    *data.get_temp_mut_or_insert_with::<f32>(celebration_id, || time)
                }))
            } else { None };
            // Short visual flourishes do not delay completion or the next page.
            let celebration_age = celebration_started.map(|start| time - start);
            let celebrating = celebration_age.is_some_and(|age| age < 8.0);
            let mascot_position = position;
            let revving = moving && mascot_position <= f32::EPSILON;
            let victory = celebrating && run_finished;
            let victory_age = celebration_age.unwrap_or(0.0);
            // Four two-second laps start and end at the finish line.
            let mascot_position = if victory {
                (1.0 + (victory_age * std::f32::consts::PI).cos()) * 0.5
            } else { mascot_position };
            // Use travel along the carpet, not the small revving shake, to
            // choose direction. Keep the last direction when stationary.
            let facing_id = ui.id().with(("vacuum-facing", &self.run_id));
            let facing_right = ui.ctx().data_mut(|data| {
                let state = data.get_temp_mut_or_insert_with::<(f32, bool)>(
                    facing_id, || (mascot_position, true));
                state.1 = if revving { true } else {
                    vacuum_facing_right(state.0, mascot_position, state.1)
                };
                state.0 = mascot_position;
                state.1
            });
            let (lane, _) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 44.0), egui::Sense::hover());
            // A carpet track grounds the vacuum. Its sweep follows observed activity.
            let carpet = egui::Rect::from_min_max(
                egui::pos2(lane.left(), lane.bottom() - 18.0),
                egui::pos2(lane.right(), lane.bottom() - 2.0),
            );
            let dark = ui.visuals().dark_mode;
            let pile = if dark {
                egui::Color32::from_rgb(111, 88, 65)
            } else {
                egui::Color32::from_rgb(180, 151, 114)
            };
            let clean = if dark {
                egui::Color32::from_rgb(155, 127, 91)
            } else {
                egui::Color32::from_rgb(217, 190, 148)
            };
            ui.painter().rect_filled(carpet, 4.0, pile);
            let cleaned = egui::Rect::from_min_max(
                carpet.min,
                egui::pos2(carpet.left() + carpet.width() * position, carpet.bottom()),
            );
            if cleaned.width() > 0.0 {
                ui.painter().rect_filled(cleaned, 4.0, clean);
            }
            let fiber = egui::Stroke::new(
                1.0,
                if dark {
                    egui::Color32::from_rgb(195, 165, 125)
                } else {
                    egui::Color32::from_rgb(130, 103, 73)
                },
            );
            let mut x = carpet.left() + 5.0;
            while x < carpet.right() - 3.0 {
                ui.painter().line_segment(
                    [
                        egui::pos2(x, carpet.top() + 4.0),
                        egui::pos2(x + 2.0, carpet.bottom() - 4.0),
                    ],
                    fiber,
                );
                x += 7.0;
            }
            // Decorative crumbs sit ahead of the vacuum. Their positions
            // are fixed so a status update does not make the track jump.
            let mut crumb = carpet.left() + 12.0;
            while crumb < carpet.right() - 8.0 {
                if crumb > carpet.left() + carpet.width() * position {
                    ui.painter().circle_filled(
                        egui::pos2(crumb, carpet.center().y - 1.0), 2.0,
                        ui.visuals().text_color(),
                    );
                }
                crumb += 31.0;
            }
            let side = if celebrating { 36.0 } else if revving { 40.0 } else { 42.0 };
            let center = egui::pos2(
                lane.left() + side * 0.5 + (lane.width() - side).max(0.0) * mascot_position,
                lane.center().y,
            );
            if revving {
                // Stagger soft puffs inside the existing lane. No status update
                // or animation frame changes the control positions.
                let painter = ui.painter().with_clip_rect(lane.intersect(ui.clip_rect()));
                for puff in 0..5 {
                    let age = (time * 0.85 + puff as f32 / 5.0).fract();
                    let fade = (std::f32::consts::PI * age).sin();
                    let color = if dark {
                        egui::Color32::from_rgba_unmultiplied(199, 185, 160, (fade * 110.0) as u8)
                    } else {
                        egui::Color32::from_rgba_unmultiplied(125, 113, 96, (fade * 95.0) as u8)
                    };
                    let puff_center = egui::pos2(
                        center.x + 10.0 + age * 34.0,
                        lane.top() + 23.0 - age * 14.0 + (age * 8.0 + puff as f32).sin() * 1.5,
                    );
                    painter.circle_filled(puff_center, 2.0 + age * 4.5, color);
                    painter.circle_filled(puff_center + egui::vec2(3.0, -1.5), 1.5 + age * 3.0, color);
                }
            }
            if victory {
                // A fading exhaust trail follows the actual victory-lap path.
                // Paint only: particles do not allocate space or capture input.
                let painter = ui.painter().with_clip_rect(ui.clip_rect());
                for puff in 0..28 {
                    let lag = puff as f32 * 0.025;
                    if victory_age < lag { continue; }
                    let past = victory_age - lag;
                    let x = lane.left() + side * 0.5
                        + (lane.width() - side).max(0.0)
                            * (1.0 + (past * std::f32::consts::PI).cos()) * 0.5;
                    let fade = 1.0 - puff as f32 / 28.0;
                    let smoke = egui::Color32::from_rgba_unmultiplied(
                        174, 170, 161, (fade * 95.0) as u8);
                    painter.circle_filled(
                        egui::pos2(x, lane.center().y - lag * 55.0),
                        3.0 + lag * 17.0, smoke);
                }
            }
            if celebrating {
                let shower = if victory {
                    egui::Rect::from_min_max(
                        egui::pos2(ui.clip_rect().left(), (lane.top() - 260.0).max(ui.clip_rect().top())),
                        egui::pos2(ui.clip_rect().right(), lane.bottom()))
                } else { lane };
                let painter = ui.painter().with_clip_rect(shower.intersect(ui.clip_rect()));
                let age = celebration_age.unwrap_or(0.0);
                let pieces = if run_finished { 180 } else { 9 };
                for piece in 0..pieces {
                    let seed = piece as f32;
                    let flight = (age * 0.8 + seed * 0.137).fract();
                    let spread = if run_finished { shower.width() } else { 64.0 };
                    let x = if run_finished { shower.left() } else { (center.x - spread).max(lane.left()) }
                        + (seed * 0.618).fract() * spread;
                    let y = shower.top() + 4.0 + flight * (shower.height() - 8.0);
                    let colors = [egui::Color32::from_rgb(88, 209, 195), egui::Color32::from_rgb(246, 194, 88), egui::Color32::from_rgb(176, 147, 237)];
                    let color = colors[piece % colors.len()].gamma_multiply((1.0 - flight) * 0.85);
                    let offset = egui::vec2((age * 9.0 + seed).sin() * 2.0, 2.5);
                    painter.line_segment([egui::pos2(x, y), egui::pos2(x, y) + offset], egui::Stroke::new(2.0, color));
                }
            }
            let center = if celebrating {
                center + egui::vec2(0.0, -(time * if run_finished { 12.0 } else { 9.0 }).sin().abs() * 3.0)
            } else if revving {
                center + egui::vec2(0.8 + (time * 27.0).sin() * 0.8, -(time * 19.0).sin().abs() * 0.7)
            } else { center };
            ui.put(
                egui::Rect::from_center_size(center, egui::vec2(side, side)),
                egui::Image::new(&self.brand_icon)
                    .fit_to_exact_size(egui::vec2(side, side))
                    .uv(egui::Rect::from_min_max(
                        egui::pos2(if facing_right { 1.0 } else { 0.0 }, 0.0),
                        egui::pos2(if facing_right { 0.0 } else { 1.0 }, 1.0),
                    ))
                    .rotate(
                        if celebrating {
                            (time * 14.0).sin() * 0.12
                        } else if revving {
                            (time * 22.0).sin() * (0.025 + 0.025 * (time * 3.0).sin().abs())
                        } else if moving {
                            (time * 8.0).sin() * 0.035
                        } else {
                            0.0
                        },
                        egui::vec2(0.5, 0.8),
                    )
                    .alt_text("Vacuum responds to received data, saved records and decompiler work"),
            );

        let progress_text = if let Some((_,written,total)) = received {
            format!("Work: {written} / {total} current-page definitions saved · decompilation included")
        } else if active { "Waiting for the first page. Each plugin reports its work above.".into() }
        else { format!("Run finished · {completed} items processed") };
        ui.add(egui::Label::new(&progress_text).truncate()).on_hover_text(&progress_text);
        let pending_reads=journal["statistics"]["pendingCompiledPages"].as_u64().unwrap_or(0);
        if pending_reads>0 {ui.label(format!("Compiled capture pages pending: {pending_reads}"));}
        if let Some(jobs)=journal["statistics"]["decompilerJobs"].as_object() {
            let count=|key:&str|jobs.get(key).and_then(Value::as_u64).unwrap_or(0);
            let finished=count("finished");let failed=count("failed");let running=count("running");let committing=count("committing");let queued=count("queued");
            if finished+failed+running+committing+queued>0 {
                ui.label(format!("Decompilation: {finished} finished · {running} working · {committing} saving · {queued} queued · {failed} failed"));
            }
        }
        ui.add(egui::ProgressBar::new(progress).desired_width(ui.available_width()).desired_height(6.0));
        if moving || celebrating { ui.ctx().request_repaint(); }
        ui.add_space(2.0);

        run_issues::show(ui, &journal);
        ui.separator();
        let output = format!("Saving to: {}", if self.output_root.is_empty() { "Preparing destination" } else { &self.output_root });
        ui.add(egui::Label::new(&output).truncate()).on_hover_text(&output);
    }

    fn open_download_button(&mut self, ui: &mut egui::Ui) {
        #[cfg(windows)]
        {
            if ui
                .add_enabled(
                    download_folder(&self.output_root).is_some(),
                    egui::Button::new(RichText::new("Open download folder").strong())
                        .min_size(egui::vec2(180.0, 32.0)),
                )
                .clicked()
            {
                self.folder_open_error = folder_command(&self.output_root)
                    .and_then(|mut command| {
                        command
                            .spawn()
                            .map(|_| ())
                            .map_err(|error| format!("Could not open the download folder: {error}"))
                    })
                    .err();
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_results_deduplicate_targets_without_masking_missing_identities() {
        let first =
            json!({"category":"A","kind":"item","scope":{"type":"global"},"keys":{"ID":"one"}});
        let second =
            json!({"category":"A","kind":"item","scope":{"type":"global"},"keys":{"ID":"two"}});
        let unrelated =
            json!({"category":"A","kind":"item","scope":{"type":"global"},"keys":{"ID":"other"}});
        let mut journal = json!({"scope":"targeted","targets":[first.clone(),first.clone(),second.clone()],
            "artifacts":[{"identity":first.clone()},{"identity":unrelated}]});
        assert_eq!(pending_items(&journal, "A"), 1);
        journal["artifacts"]
            .as_array_mut()
            .unwrap()
            .push(json!({"identity":second}));
        assert_eq!(pending_items(&journal, "A"), 0);
        journal["scope"] = json!("fullBackup");
        journal["discovered"] = json!({"A":[first.clone(),first]});
        assert_eq!(pending_items(&journal, "A"), 0);
    }

    #[test]
    fn item_results_distinguish_saved_partial_failed_and_intentionally_empty_items() {
        let mut results = ItemResults::default();
        for (acquisition, processing) in [
            ("collected", "derived"),
            ("partial", "derived"),
            ("collected", "failed"),
            ("inaccessible", "failed"),
            ("unsupported", "failed"),
            ("verifiedAbsent", "derived"),
            ("verifiedNotApplicable", "derived"),
            ("excluded", "derived"),
            ("verifiedAbsent", "failed"),
        ] {
            results.add(&json!({"acquisition":acquisition,"processing":processing}));
        }
        assert_eq!(
            results,
            ItemResults {
                downloaded: 1,
                partial: 2,
                failed: 3,
                absent: 2,
                excluded: 1
            }
        );
        assert_eq!(results.handled(), 9);
    }

    #[test]
    #[cfg(windows)]
    fn folder_opener_uses_exact_existing_run_root_as_one_argument() {
        let temp = tempfile::tempdir().unwrap();
        let refresh = temp.path().join("Chosen folder with spaces & Ω");
        let separate = refresh.join("Herbert - Company - timestamped child");
        assert!(download_folder("").is_none());
        assert!(download_folder(refresh.to_str().unwrap()).is_none());
        std::fs::create_dir_all(&separate).unwrap();
        for path in [&refresh, &separate] {
            let command = folder_command(path.to_str().unwrap()).unwrap();
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                vec![path.as_os_str()]
            );
            assert_eq!(
                std::path::Path::new(command.get_program())
                    .file_name()
                    .unwrap(),
                "explorer.exe"
            );
            let extended = std::fs::canonicalize(path).unwrap();
            let extended_command = folder_command(extended.to_str().unwrap()).unwrap();
            let args = extended_command.get_args().collect::<Vec<_>>();
            assert_eq!(args.len(), 1);
            // Canonicalization can expand Windows 8.3 aliases in TEMP.
            assert_eq!(std::fs::canonicalize(args[0]).unwrap(), extended);
        }
        let file = refresh.join("file.txt");
        std::fs::write(&file, "fixture").unwrap();
        assert!(folder_command(file.to_str().unwrap()).is_err());
    }
    #[test]
    fn progress_uses_known_work_and_never_invents_completion_for_an_empty_inventory() {
        let journal = json!({"categories":["A","B"],"discoveryFinished":{"A":true,"B":false},"discovered":{"A":[{},{}],"B":[{}]},"artifacts":[{"acquisition":"collected","processing":"derived"},{"acquisition":"unavailable","processing":"pending"}],"acquisitions":[{"bytes":12},{"bytes":8}]});
        assert_eq!(
            Counts::from_journal(&journal),
            Counts {
                categories: 2,
                discovered_categories: 1,
                known: 3,
                handled: 2,
                files: 2,
                bytes: 20,
                incomplete: 1
            }
        );
        assert_eq!(fraction(0, 0), 0.0);
        assert_eq!(fraction(2, 1), 1.0);
        assert!(!terminal("paused"));
        assert!(!terminal("running"));
        assert!(terminal("partial"));
        assert!(terminal("destinationReleaseUncertain"));
    }
    #[test]
    fn terminal_journal_keeps_polling_until_execution_releases_the_destination() {
        let mut journal = json!({"phase":"completed", "executionActive":true});
        assert!(execution_active(&journal));
        assert_eq!(
            phase_label("completed", execution_active(&journal)),
            "Finishing cleanup"
        );
        journal["executionActive"] = json!(false);
        journal["phase"] = json!("destinationReleaseUncertain");
        assert!(!execution_active(&journal));
        assert_eq!(
            phase_label("destinationReleaseUncertain", false),
            "Output cleanup needs attention"
        );
        assert!(execution_active(
            &json!({"phase":"paused","executionActive":true})
        ));
        assert!(!execution_active(
            &json!({"phase":"running","executionActive":false})
        ));
        assert!(!execution_active(&json!({"phase":"completed"})));
    }
    #[test]
    fn progress_labels_resolve_journal_names_in_activity_and_issues() {
        let mut journal = json!({
            "categoryNames":{"vendor.collection.v2":"Custom collection Ω"},
            "artifacts":[{
                "identity":{"category":"vendor.collection.v2","kind":"definition","keys":{"ID":"Owned"}},
                "acquisition":"partial","processing":"derived",
                "diagnostics":[{"message":"Original diagnostic."}]
            }],
            "acquisitions":[{"path":"native/last.bin"}]
        });
        assert_eq!(
            recent_activity(&journal)[0],
            "Latest item: Custom collection Ω / definition: ID=Owned — partial; processing derived"
        );
        assert_eq!(
            issue_lines(&journal),
            vec![
                "Custom collection Ω / definition: ID=Owned — partial; processing derived. Original diagnostic."
            ]
        );
        assert_eq!(
            recent_activity(&journal)[1],
            "Latest saved file: native/last.bin"
        );
        journal["categoryNames"]["vendor.collection.v2"] = json!("Renamed collection");
        assert!(recent_activity(&journal)[0].contains("Renamed collection / definition"));
        for names in [
            Value::Null,
            json!({}),
            json!({"vendor.collection.v2":17}),
            json!({"vendor.collection.v2":"  "}),
        ] {
            journal["categoryNames"] = names;
            let label = identity_label(&journal, &journal["artifacts"][0]["identity"]);
            assert_eq!(label, "Collection / definition: ID=Owned");
            assert!(!recent_activity(&journal)[0].contains("vendor.collection.v2"));
            assert!(!issue_lines(&journal)[0].contains("vendor.collection.v2"));
        }
        assert_eq!(identity_label(&json!({}), &json!({})), "Collection / item");
    }
    #[test]
    fn artifact_failures_are_visible_even_when_the_journal_has_no_global_diagnostic() {
        let journal = json!({"diagnostics":[],"artifacts":[
            {"identity":{"category":"C02","kind":"functionLibrary","keys":{"LibraryID":"Example"}},"acquisition":"inaccessible","processing":"failed","diagnostics":[{"message":"Native export was refused."}]},
            {"identity":{"category":"C01","kind":"query","keys":{"QueryID":"Good"}},"acquisition":"collected","processing":"derived","diagnostics":[]},
            {"identity":{"category":"C03","kind":"layout","keys":{"ID":"Partial"}},"acquisition":"partial","processing":"derived","diagnostics":[]}
        ],"acquisitions":[{"path":"native/last.bin"}]});
        let issues = issue_lines(&journal);
        assert_eq!(issues.len(), 2);
        assert!(issues[0].contains("LibraryID=Example"));
        assert!(issues[0].contains("Native export was refused."));
        assert!(issues[1].contains("partial; processing derived"));
        assert_eq!(recent_activity(&journal).len(), 2);
        assert!(recent_activity(&journal)[0].contains("ID=Partial"));
        assert_eq!(
            recent_activity(&journal)[1],
            "Latest saved file: native/last.bin"
        );
    }
}

// An early terminal journal update must not celebrate before execution stops.
fn finish_celebration(phase: &str, active: bool) -> bool {
    !active && matches!(phase, "completed" | "partial")
}

#[test]
fn progress_celebrates_finished_work_with_or_without_incomplete_items() {
    for phase in ["completed", "partial"] {
        assert!(finish_celebration(phase, false));
        assert!(!finish_celebration(phase, true));
    }
    for phase in ["running", "paused", "canceled", "failed", "destinationReleaseUncertain"] {
        assert!(!finish_celebration(phase, false));
        assert!(!finish_celebration(phase, true));
    }
}

// The source image faces left. The painter mirrors it for rightward travel.
fn vacuum_facing_right(previous: f32, current: f32, facing_right: bool) -> bool {
    if current > previous { true }
    else if current < previous { false }
    else { facing_right }
}

#[test]
fn progress_vacuum_faces_travel_and_keeps_direction_when_stopped() {
    assert!(vacuum_facing_right(0.2, 0.3, false));
    assert!(!vacuum_facing_right(0.3, 0.2, true));
    assert!(vacuum_facing_right(0.3, 0.3, true));
    assert!(!vacuum_facing_right(0.3, 0.3, false));
    // Victory laps reverse at each end of the carpet.
    let positions = [1.0, 0.5, 0.0, 0.5, 1.0, 0.5];
    let mut facing = true;
    for (pair, expected) in positions.windows(2).zip([false, false, true, true, false]) {
        facing = vacuum_facing_right(pair[0], pair[1], facing);
        assert_eq!(facing, expected);
    }
}

#[cfg(test)]
mod timing_tests {
    use super::*;
    #[test]
    fn timing_labels_distinguish_running_finished_and_missing_evidence() {
        let j = json!({"statistics":{"elapsedMs":87000,"requests":16,"requestMs":4882,
            "receivedBytes":19111152,"pages":3,"pageWriteMs":79159}});
        assert!(statistics_line(&j, 5973, true).starts_with("Elapsed 0:01:27"));
        assert!(statistics_line(&j, 5973, false).starts_with("Finished in 0:01:27"));
        assert!(statistics_line(&j, 5973, false).contains("68.7 files/s"));
        assert!(statistics_line(&json!({}), 0, false).contains("unavailable"));
        assert_eq!(duration(3_661_000), "1:01:01");
    }
}
