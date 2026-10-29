//! One table for concurrent work. Counts come from received pages and the journal.
use super::*;

#[derive(Debug)]
pub(super) struct PluginRow {
    pub name: String,
    pub state: &'static str,
    pub running: bool,
    pub attention: bool,
    pub work: String,
    pub batch: String,
    pub totals: String,
}

/// Put running work first so all eight admitted plugins fit in the default view.
/// Preserve selection order within each state; do not sort by changing counts.
pub(super) fn rows(journal: &Value, operations: &[Value], batches: &[Value]) -> Vec<PluginRow> {
    let active = execution_active(journal);
    let mut rows = journal["categories"].as_array().into_iter().flatten().filter_map(|id| {
        let id = id.as_str()?;
        let name = journal["categoryNames"][id].as_str().unwrap_or(id);
        let ops = operations.iter().filter(|op| op["plugin"] == name || op["identity"]["category"] == id).collect::<Vec<_>>();
        let pages = batches.iter().filter(|b| b["category"] == id || b["plugin"] == name).collect::<Vec<_>>();
        let latest = pages.iter().rev().find(|b| b["files"].as_u64().unwrap_or(0) > 0);
        let committed = pages.iter().filter_map(|b| b["completedItems"].as_u64()).max().unwrap_or(0);
        let mut results = ItemResults::default();
        for item in journal["artifacts"].as_array().into_iter().flatten().filter(|a| a["identity"]["category"] == id) {
            results.add(item);
        }
        let lifecycle = journal["pluginStates"][id].as_str().unwrap_or("");
        let outcome = journal["categoryOutcomes"][id].as_str().unwrap_or("");
        let attention = results.failed + results.partial > 0 || matches!(lifecycle, "failed" | "incomplete")
            || matches!(outcome, "discoveryFailed" | "discoveryIncomplete");
        let running = active && (lifecycle == "running" || !ops.is_empty());
        let finished = lifecycle == "completed" || (!active
            && journal["discoveryFinished"][id] == true && pending_items(journal, id) == 0);
        let state = if running {
            if journal["cancelRequested"] == true { "Stopping" }
            else if journal["pauseRequested"] == true { "Pausing" }
            else { "Running" }
        } else if lifecycle == "failed" || outcome == "discoveryFailed" { "Failed" }
        else if lifecycle == "canceled" || (!active && !finished && journal["phase"] == "canceled") { "Canceled" }
        else if attention { "Incomplete" }
        else if finished { "Finished" }
        else if journal["pauseRequested"] == true { "Paused" }
        else if active { "Queued" }
        else { "Ended early" };
        let mut steps = Vec::new();
        for op in &ops {
            if let Some(step) = op["step"].as_str() {
                if step == "Downloaded pages waiting in memory: 0" { continue; }
                let seconds = op["elapsedSeconds"].as_u64().unwrap_or(0);
                // A buffer label is a queue gauge. Its guard lives for the
                // whole plugin and is not the age of the queued pages.
                let timed = if step.starts_with("Downloaded pages waiting") { step.to_owned() }
                    else { format!("{step} ({}s)", seconds) };
                if !steps.contains(&timed) { steps.push(timed); }
            }
        }
        let work = if !steps.is_empty() { steps.join(" · ") }
        else if running { "Processing received data / preparing the next request".into() }
        else if attention { format!("{state}. See Issues below for what was not saved.") }
        else if finished && outcome == "verifiedAbsent" { "No eligible items matched the run selection".into() }
        else if finished { "Collection complete within the run selection".into() }
        else if state == "Queued" { "Waiting for a plugin slot or prerequisite".into() }
        else { state.to_owned() };
        let work = match journal["statistics"]["pluginElapsedMs"][id].as_u64() {
            Some(ms) => format!("{} | {work}", duration(ms)),
            None => work,
        };
        let batch = if let Some(page) = latest {
            let total = page["files"].as_u64().unwrap_or(0);
            let processed = page.get("processed").or_else(|| page.get("verified")).and_then(Value::as_u64).unwrap_or(0);
            let page_number = pages.iter().filter(|b| b["files"].as_u64().unwrap_or(0) > 0).count();
            let pending = pages.last().is_some_and(|b| b["countKnown"] == false && b["phase"] != "Interrupted");
            format!("Page {page_number}: {processed} / {total} processed{}", if pending { " · next page requested" } else { "" })
        } else if running && (results.downloaded > 0 || committed > 0) { "Processing collected items".into() }
        else if running { "Waiting for the first page".into() }
        else { String::new() };
        let completed = if pages.iter().any(|b| b.get("completedItems").is_some()) { committed as usize }
            else { results.downloaded };
        Some(PluginRow { name:name.to_owned(), state, running, attention, work, batch,
            totals:format!("{completed} complete · {} partial · {} failed", results.partial, results.failed) })
    }).collect::<Vec<_>>();
    rows.sort_by_key(|row| if row.running { 0 } else if matches!(row.state, "Queued" | "Paused") { 1 } else { 2 });
    rows
}

fn line(ui: &mut egui::Ui, rect: egui::Rect, text: &str, size: f32, color: egui::Color32, strong: bool) {
    let style = RichText::new(text).size(size).color(color);
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Min)), |ui| {
        ui.add(egui::Label::new(if strong { style.strong() } else { style }).truncate())
            .on_hover_text(text);
    });
}

pub(super) fn table(ui: &mut egui::Ui, rows: &[PluginRow]) {
    let running = rows.iter().filter(|row| row.running).count();
    let queued = rows.iter().filter(|row| row.state == "Queued").count();
    let finished = rows.iter().filter(|row| row.state == "Finished").count();
    ui.add(egui::Label::new(RichText::new(format!("Plugins   {running} running   ·   {queued} queued   ·   {finished} finished")).strong()).truncate());
    let (header, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(),20.0),egui::Sense::hover());
    let split = header.left() + (header.width()*0.36).clamp(180.0,350.0);
    let color = ui.visuals().weak_text_color();
    line(ui,egui::Rect::from_min_max(header.min+egui::vec2(6.0,0.0),egui::pos2(split-80.0,header.bottom())),"Plugin / completed items",TEXT_DETAIL,color,true);
    line(ui,egui::Rect::from_min_max(egui::pos2(split-78.0,header.top()),egui::pos2(split-4.0,header.bottom())),"State",TEXT_DETAIL,color,true);
    line(ui,egui::Rect::from_min_max(egui::pos2(split+8.0,header.top()),header.max),"Current work / page progress",TEXT_DETAIL,color,true);
    // Eight rows match the largest user-selectable concurrency. More selected
    // plugins remain available in this table; they are not a collection limit.
    egui::ScrollArea::vertical().id_salt("plugin-run-table")
        .max_height(272.0).min_scrolled_height(272.0).auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if rows.is_empty() { ui.label("Preparing the selected plugins…"); }
            for (index, row) in rows.iter().enumerate() {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::hover());
                if index % 2 == 0 { ui.painter().rect_filled(rect, 3.0, ui.visuals().faint_bg_color); }
                let name_width = (rect.width() * 0.36).clamp(180.0, 350.0);
                let split = rect.left() + name_width;
                let fg = ui.visuals().text_color();
                let muted = ui.visuals().weak_text_color();
                let color = if row.attention { ui.visuals().warn_fg_color }
                    else if row.running { if ui.visuals().dark_mode { egui::Color32::from_rgb(139, 216, 188) } else { egui::Color32::from_rgb(21, 100, 76) } }
                    else { muted };
                let cell = |left, right, top, bottom| egui::Rect::from_min_max(egui::pos2(left,top),egui::pos2(right,bottom));
                line(ui, cell(rect.left()+6.0, split-80.0, rect.top(),rect.top()+17.0), &row.name, TEXT_DENSE, fg, true);
                line(ui, cell(split-78.0,split-4.0,rect.top(),rect.top()+17.0), row.state, TEXT_DETAIL, color, true);
                line(ui, cell(split+8.0,rect.right()-6.0,rect.top(),rect.top()+17.0), &row.work,TEXT_DENSE,fg,false);
                line(ui, cell(rect.left()+6.0,split-4.0,rect.top()+17.0,rect.bottom()), &row.totals,TEXT_DETAIL,muted,false);
                line(ui, cell(split+8.0,rect.right()-6.0,rect.top()+17.0,rect.bottom()), &row.batch,TEXT_DETAIL,muted,false);
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completed_items_do_not_claim_first_page_wait_and_queue_age_is_not_work_age() {
        let journal=json!({"executionActive":true,"categories":["configuration-references"],"categoryNames":{"configuration-references":"References"},"pluginStates":{"configuration-references":"running"}});
        let batches=vec![json!({"category":"configuration-references","completedItems":451,"files":0})];
        let operations=vec![json!({"plugin":"References","step":"Downloaded pages waiting in memory: 0","elapsedSeconds":978}),json!({"plugin":"References","step":"Writing output files","elapsedSeconds":0})];
        let result=rows(&journal,&operations,&batches);
        assert_eq!(result[0].batch,"Processing collected items");
        assert!(!result[0].work.contains("978"));
        assert!(result[0].work.contains("Writing output files"));
        let operations=vec![json!({"plugin":"References","step":"Downloaded pages waiting in memory: 3","elapsedSeconds":978})];
        let result=rows(&journal,&operations,&batches);
        assert!(result[0].work.contains("memory: 3"));
        assert!(!result[0].work.contains("978"));
    }
    #[test]
    fn failed_and_incomplete_outcomes_never_claim_collection_complete() {
        for (outcome, lifecycle, expected) in [
            ("discoveryFailed", "completed", "Failed"),
            ("discoveryIncomplete", "incomplete", "Incomplete"),
        ] {
            let journal = json!({"executionActive":false,"phase":"partial",
                "categories":["report-definitions"],"pluginStates":{"report-definitions":lifecycle},
                "categoryOutcomes":{"report-definitions":outcome},"discoveryFinished":{"report-definitions":true}});
            let result = rows(&journal, &[], &[]);
            assert_eq!(result[0].state, expected);
            assert!(!result[0].work.contains("Collection complete"));
            assert!(result[0].attention);
        }
    }
    #[test]
    fn active_plugins_are_visible_and_discovery_is_not_collection_completion() {
        let journal=json!({"executionActive":true,"categories":["A","B","C"],
            "categoryNames":{"A":"Queued","B":"Working","C":"Done"},
            "pluginStates":{"B":"running","C":"completed"},"discoveryFinished":{"B":true,"C":true}});
        let result=rows(&journal,&[],&[]);
        assert_eq!(result.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),["Working","Queued","Done"]);
        assert_eq!(result[0].state,"Running");
        assert_eq!(result[2].state,"Finished");
    }
    #[test]
    fn current_page_keeps_processing_progress_while_the_next_page_is_requested() {
        let journal=json!({"executionActive":true,"categories":["A"],"categoryNames":{"A":"Example"},"pluginStates":{"A":"running"}});
        let batches=vec![json!({"category":"A","files":100,"processed":35,"completedItems":100}),
            json!({"category":"A","files":0,"countKnown":false,"phase":"Downloading","completedItems":0})];
        let result=rows(&journal,&[],&batches);
        assert!(result[0].batch.contains("35 / 100"));
        assert!(result[0].batch.contains("next page requested"));
        assert!(result[0].totals.starts_with("100 complete"));
    }
}
