//! Review run problems without moving the download controls.
use super::*;

#[derive(Clone, Debug)]
struct Issue {
    key: String,
    target: String,
    message: String,
    code: String,
    outcome: String,
    next: String,
    files: Vec<String>,
    technical: String,
}

impl Issue {
    fn copy_text(&self) -> String {
        format!(
            "{}\n{}\n\nSaved output: {}\nNext step: {}\n\nFiles:\n{}\n\nTechnical details:\n{}",
            self.target,
            self.message,
            self.outcome,
            self.next,
            self.files.join("\n"),
            self.technical
        )
    }
}

fn target_label(journal: &Value, identity: &Value) -> String {
    let category = identity["category"]
        .as_str()
        .and_then(|id| journal["categoryNames"][id].as_str())
        .unwrap_or("Collection");
    for key in ["ViewId", "QueryID", "LibraryID", "ID", "Name"] {
        if let Some(name) = identity["keys"][key].as_str() {
            return format!("{name} · {category}");
        }
    }
    identity_label(journal, identity)
}

fn issues(journal: &Value) -> Vec<Issue> {
    let artifacts = journal["artifacts"].as_array().cloned().unwrap_or_default();
    let mut diagnostics = journal["diagnostics"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for artifact in &artifacts {
        let details = artifact["diagnostics"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if details.is_empty() && incomplete(artifact) {
            diagnostics.push(json!({"identity":artifact["identity"],"code":"ITEM_INCOMPLETE",
                "message":"This item did not finish successfully. Review its saved output and technical details."}));
        }
        for mut detail in details {
            if detail["identity"].is_null() {
                detail["identity"] = artifact["identity"].clone();
            }
            diagnostics.push(detail);
        }
    }
    if journal["diagnosticLog"]["writeFailed"] == true {
        diagnostics.push(json!({"code":"DIAGNOSTIC_LOG_WRITE_FAILED",
            "message":"The diagnostic log could not be written. Preserve the run journal."}));
    }
    let mut seen = std::collections::BTreeSet::new();
    diagnostics.into_iter().filter_map(|diagnostic| {
        let id = &diagnostic["identity"];
        let code = diagnostic["code"].as_str().unwrap_or("UNSPECIFIED").to_owned();
        let message = diagnostic["message"].as_str().unwrap_or("No explanation was recorded.").to_owned();
        let key=format!("{id}|{code}|{message}");
        if !seen.insert(key.clone()) { return None; }
        let artifact = artifacts.iter().find(|a| !id.is_null() && a["identity"] == *id);
        let files: Vec<String> = artifact.into_iter().flat_map(|a| a["files"].as_array().into_iter().flatten())
            .filter_map(|f| f["path"].as_str().map(str::to_owned)).collect();
        let packages = files.iter().filter(|p| p.ends_with(".zip")).count();
        let outcome = if artifact.is_none() {
            "The diagnostic does not identify a saved item. Its output impact cannot be determined from this message.".into()
        } else if files.is_empty() {
            "No output files are recorded for this item.".into()
        } else {
            format!("{} files recorded{}{}. This does not prove that all dependencies were saved.", files.len(),
                if packages > 0 { ", including " } else { "" },
                if packages > 0 { format!("{packages} ZIP package(s)") } else { String::new() })
        };
        let next = match code.as_str() {
            "KINETIC_PARENT_NOT_FOUND" => "Check the named parent layer before restoring this package. Retrying only helps if the parent becomes available.",
            "KINETIC_RESOURCE_PATH_UNVERIFIED" if id.is_null() => "The source identity was not recorded. Copy the run details for investigation. This message alone does not establish that an application is broken.",
            "KINETIC_RESOURCE_PATH_UNVERIFIED" => "Review the named source file and reference. Check the saved output for the original definition. The referenced resource was not verified.",
            "KINETIC_PACKAGE_INVALID" => "Review the saved recovery files and the package explanation. Do not treat recovery JSON as an importable ZIP.",
            "KINETIC_PACKAGE_RECOVERY_ONLY" => "Keep the saved custom recovery files. This collector did not produce a standalone application package for this group; it does not mean the server read failed.",
            "DIAGNOSTIC_LOG_WRITE_FAILED" => "Check destination access and preserve the run journal for diagnosis.",
            _ if diagnostic["retryable"] == true => "This problem is marked retryable. Resolve the reported cause before starting another download.",
            _ => "Review the explanation and saved files. Copy these details when reporting the problem.",
        }.to_owned();
        let technical = serde_json::to_string_pretty(&json!({"runId":journal["runId"],
            "diagnostic":diagnostic,"acquisition":artifact.map(|a|&a["acquisition"]),
            "processing":artifact.map(|a|&a["processing"])})).unwrap_or_default();
        Some(Issue { key, target: if id.is_null() { diagnostic["context"]["category"].as_str().and_then(|id|journal["categoryNames"][id].as_str()).map(|name|format!("{name} · collection issue")).unwrap_or_else(||"Run-level issue · item not identified".into()) }
            else { target_label(journal,id) }, message, code, outcome, next, files, technical })
    }).collect()
}

pub(super) fn show(ui: &mut egui::Ui, journal: &Value) {
    let items = issues(journal);
    let state_id = ui.id().with((
        "issue-review",
        journal["runId"].as_str().unwrap_or("current"),
    ));
    let mut open = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(state_id))
        .unwrap_or(false);
    #[cfg(test)]
    if std::env::var_os("HERBERT_STATUS_ISSUES").is_some() {
        open = true;
    }
    // This summary has the same height with zero or many issues.
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("Issues · {}", items.len())).strong());
        if items.is_empty() {
            ui.label("No issues reported.");
        } else {
            ui.label("Review affected items and saved output.");
        }
        if ui
            .add_enabled(
                !items.is_empty(),
                egui::Button::new("Review issues").min_size(egui::vec2(130.0, 32.0)),
            )
            .clicked()
        {
            open = true;
        }
    });
    let screen = ui.ctx().content_rect().size();
    let size = egui::vec2(
        (screen.x - 48.0).min(1100.0).max(240.0),
        (screen.y - 64.0).min(680.0).max(200.0),
    );
    egui::Window::new("Download issues")
        .id(state_id.with("window"))
        .open(&mut open)
        .resizable(true)
        .collapsible(false)
        .default_size(size)
        .max_size(screen - egui::vec2(24.0, 24.0))
        .show(ui.ctx(), |ui| {
            // Use a quiet selection fill with readable text in both themes.
            ui.visuals_mut().selection.bg_fill = if ui.visuals().dark_mode {
                egui::Color32::from_rgb(46, 76, 101)
            } else {
                egui::Color32::from_rgb(209, 225, 237)
            };
            ui.visuals_mut().selection.stroke.color = ui.visuals().text_color();
            let mut filter = ui
                .ctx()
                .data(|d| d.get_temp::<String>(state_id.with("filter")))
                .unwrap_or_default();
            ui.horizontal(|ui| {
                ui.label(format!("{} issues", items.len()));
                ui.add(
                    egui::TextEdit::singleline(&mut filter)
                        .hint_text("Filter by item, message or code")
                        .desired_width(320.0),
                );
                if ui.button("Copy private details").clicked() {
                    ui.ctx().copy_text(
                        items
                            .iter()
                            .map(Issue::copy_text)
                            .collect::<Vec<_>>()
                            .join("\n\n---\n\n"),
                    );
                }
            });
            ui.ctx()
                .data_mut(|d| d.insert_temp(state_id.with("filter"), filter.clone()));
            ui.label("Names appear here. Use Save diagnostics to share sanitized evidence.");
            ui.separator();
            let filter = filter.to_lowercase();
            let visible: Vec<_> = items
                .iter()
                .enumerate()
                .filter(|(_, i)| {
                    format!("{} {} {}", i.target, i.message, i.code)
                        .to_lowercase()
                        .contains(&filter)
                })
                .collect();
            let mut selected = ui
                .ctx()
                .data(|d| d.get_temp::<String>(state_id.with("selected")))
                .and_then(|key| items.iter().position(|issue| issue.key == key))
                .unwrap_or(0);
            if !visible.iter().any(|(index, _)| *index == selected) {
                selected = visible.first().map_or(0, |(index, _)| *index);
            }
            let height = ui.available_height().max(160.0);
            ui.columns(2, |columns| {
                egui::ScrollArea::vertical()
                    .id_salt("issue-list")
                    .max_height(height)
                    .auto_shrink([false, false])
                    .show(&mut columns[0], |ui| {
                        for (index, issue) in &visible {
                            let title = format!("{}\n{}", issue.target, issue.message);
                            if ui
                                .add(
                                    egui::Button::new(title)
                                        .selected(selected == *index)
                                        .wrap()
                                        .min_size(egui::vec2(ui.available_width(), 64.0)),
                                )
                                .clicked()
                            {
                                selected = *index;
                            }
                            ui.add_space(6.0);
                        }
                        if visible.is_empty() {
                            ui.label("No issues match this filter.");
                        }
                    });
                egui::ScrollArea::vertical()
                    .id_salt("issue-detail")
                    .max_height(height)
                    .auto_shrink([false, false])
                    .show(&mut columns[1], |ui| {
                        if let Some(issue) = items.get(selected).filter(|_| !visible.is_empty()) {
                            ui.add(egui::Label::new(RichText::new(&issue.target).heading()).wrap());
                            ui.add_space(8.0);
                            ui.add(egui::Label::new(&issue.message).wrap().selectable(true));
                            ui.add_space(12.0);
                            ui.strong("Saved output");
                            ui.label(&issue.outcome);
                            ui.add_space(12.0);
                            ui.strong("Next step");
                            ui.label(&issue.next);
                            ui.add_space(12.0);
                            if ui.button("Copy this issue").clicked() {
                                ui.ctx().copy_text(issue.copy_text());
                            }
                            ui.collapsing(format!("Saved files ({})", issue.files.len()), |ui| {
                                for file in &issue.files {
                                    ui.add(egui::Label::new(file).wrap().selectable(true));
                                }
                            });
                            ui.collapsing("Technical details", |ui| {
                                ui.add(
                                    egui::Label::new(RichText::new(&issue.technical).monospace())
                                        .wrap()
                                        .selectable(true),
                                );
                            });
                        }
                    });
            });
            ui.ctx().data_mut(|d| {
                d.insert_temp(
                    state_id.with("selected"),
                    items
                        .get(selected)
                        .map(|issue| issue.key.clone())
                        .unwrap_or_default(),
                )
            });
        });
    ui.ctx().data_mut(|d| d.insert_temp(state_id, open));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selecting_an_issue_and_copying_uses_its_own_file_evidence() {
        let ctx=egui::Context::default();
        let artifacts:Vec<_>=["Alpha","Beta"].into_iter().map(|name|json!({
            "identity":{"category":"C12","kind":"application","keys":{"ViewId":name}},
            "acquisition":"collected","processing":"derived","files":[{"path":format!("{name}.zip")}],
            "diagnostics":[{"code":"MISSING","message":format!("Check {name}")}]})).collect();
        let journal=json!({"runId":"interaction","artifacts":artifacts});
        let frame=|events|ctx.run_ui(egui::RawInput {
            screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1180.0,820.0))),
            events,..Default::default()
        },|ui|{
            let id=ui.id().with(("issue-review","interaction"));
            ui.ctx().data_mut(|d|d.insert_temp(id,true));
            show(ui,&journal);
        });
        let find=|output:&egui::FullOutput,needle:&str|output.shapes.iter().find_map(|shape| {
            if let egui::Shape::Text(text)=&shape.shape {
                if text.galley.text().starts_with(needle) {return Some(text.pos+text.galley.size()*0.5);}
            }
            None
        }).expect("Expected visible control");
        let click=|pos|vec![egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {pos,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()},
            egui::Event::PointerButton {pos,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()}];
        let mut output=frame(vec![]);
        output.textures_delta.clear();
        for _ in 0..3 {output=frame(vec![]);output.textures_delta.clear();}
        let beta=find(&output,"Beta");
        output=frame(click(beta));output.textures_delta.clear();
        output=frame(vec![]);output.textures_delta.clear();
        let copy=find(&output,"Copy this issue");
        output=frame(click(copy));output.textures_delta.clear();
        assert!(output.platform_output.commands.iter().any(|command|
            matches!(command,egui::OutputCommand::CopyText(text) if text.contains("Beta.zip") && !text.contains("Alpha.zip"))));
    }
    #[test]
    fn global_diagnostic_resolves_saved_files_and_deduplicates_artifact_copy() {
        let id =
            json!({"category":"C12","kind":"KineticApplicationBundle","keys":{"ViewId":"App"}});
        let diagnostic =
            json!({"identity":id,"code":"KINETIC_PARENT_NOT_FOUND","message":"Missing Parent"});
        let journal = json!({"diagnostics":[diagnostic],"artifacts":[{"identity":id,"acquisition":"collected","processing":"derived","files":[{"path":"App.zip"}],"diagnostics":[diagnostic]}]});
        let result = issues(&journal);
        assert_eq!(result.len(), 1);
        assert!(result[0].target.starts_with("App ·"));
        assert!(result[0].outcome.contains("1 ZIP"));
        assert!(result[0].next.contains("before restoring"));
    }
    #[test]
    fn unidentified_issue_does_not_claim_unrelated_output_was_saved() {
        let result = issues(
            &json!({"diagnostics":[{"message":"Read failed"}],"artifacts":[{"identity":{"keys":{"ID":"Other"}},"acquisition":"collected","processing":"derived","files":[{"path":"Other.zip"}]}]}),
        );
        assert_eq!(result.len(), 1);
        assert!(result[0].files.is_empty());
        assert!(result[0].outcome.contains("cannot be determined"));
    }
    #[test]
    fn issue_review_has_visible_search_copy_and_detail_at_narrow_and_default_sizes() {
        for width in [720.0, 1180.0] {
            let ctx = egui::Context::default();
            let journal = json!({"runId":"fixture","diagnostics":[{"code":"KINETIC_PARENT_NOT_FOUND",
                "identity":{"category":"C12","kind":"application","keys":{"ViewId":"App"}},"message":"Missing parent. Child ZIP saved."}]});
            let mut shapes = Vec::new();
            for _ in 0..4 {
                let mut output = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(width, 820.0),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        let id = ui.id().with(("issue-review", "fixture"));
                        ui.ctx().data_mut(|d| d.insert_temp(id, true));
                        show(ui, &journal);
                    },
                );
                output.textures_delta.clear();
                shapes = output.shapes;
            }
            for label in [
                "Copy private details",
                "Saved output",
                "Next step",
                "Copy this issue",
            ] {
                assert!(shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text)
                    if text.galley.text()==label && shape.clip_rect.expand(1.0).contains_rect(egui::Rect::from_min_size(text.pos,text.galley.size())))),"{label} clipped at {width}");
            }
        }
    }
}
