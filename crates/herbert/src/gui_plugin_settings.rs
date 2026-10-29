//! Generic presentation only. Keys, choices and filtering semantics belong to guests.
use eframe::egui;
use herbert_contract::{PluginSetting, resolve_plugin_settings};
use serde_json::{Value, json};

pub(super) fn helper_label(ui: &mut egui::Ui, category: &Value, method: herbert_contract::CollectionMethod) {
    let helpers: Vec<_> = ["helpers", "artifacts"].iter()
        .flat_map(|key| category[*key].as_array().into_iter().flatten()).collect();
    let required: Vec<_> = helpers.iter().filter(|h| h["purpose"].as_str().unwrap_or("requiredContent") == "requiredContent").collect();
    let all = required.iter().any(|h| h["coverage"] == "allContent");
    let text = if all { "Requires a helper" }
        else if !required.is_empty() { "Requires helpers for some content" }
        else if !helpers.is_empty() { "Optional server-side helpers" }
        else { "No helper required" };
    ui.label(egui::RichText::new(text).strong());
    if method == herbert_contract::CollectionMethod::DirectOnly && !required.is_empty() {
        ui.colored_label(ui.visuals().warn_fg_color, if all {
            "Not available with Direct only. Choose Server-assisted to collect this content."
        } else {
            "Direct only collects what Epicor's standard services return. Content that needs a server helper will be listed as not collected."
        });
    }
}

/// Helper effect descriptions for the collapsed plugin details. Native artifact
/// descriptions are shown with their lifecycle there, so each appears once.
pub(super) fn helper_details(category: &Value) -> Vec<&str> {
    category["helpers"].as_array().into_iter().flatten()
        .filter_map(|helper| helper.pointer("/effects/description").and_then(Value::as_str))
        .collect()
}

pub(super) fn show(ui: &mut egui::Ui, category: &Value, overrides: &mut Value) {
    let Ok(declarations) =
        serde_json::from_value::<Vec<PluginSetting>>(category["settings"].clone())
    else {
        return;
    };
    if declarations.is_empty() {
        return;
    }
    let Ok(mut values) = resolve_plugin_settings(&declarations, overrides) else {
        return;
    };
    ui.push_id(category["id"].as_str().unwrap_or(""), |ui| {
        let category_id=category["id"].as_str().unwrap_or("");
        for declaration in declarations.iter().filter(|setting|!crate::application::is_tuning_setting(category_id,setting)) {
            setting(ui,declaration,&mut values);
        }
        if declarations.iter().any(|setting|crate::application::is_tuning_setting(category_id,setting)) {
            egui::CollapsingHeader::new("Advanced").default_open(false).show(ui,|ui| {
                for declaration in declarations.iter().filter(|setting|crate::application::is_tuning_setting(category_id,setting)) {
                    setting(ui,declaration,&mut values);
                }
            });
        }
    });
    *overrides = values;
}

fn setting(ui:&mut egui::Ui,declaration:&PluginSetting,values:&mut Value) {
            ui.add_space(8.0);
            egui::Frame::group(ui.style()).inner_margin(12).corner_radius(6).show(ui, |ui| {
            ui.set_width(ui.available_width());
            match declaration {
                PluginSetting::Integer {key, label, description, minimum, maximum, default, ..} => {
                    let mut number = values[key].as_u64().unwrap_or(*default as u64);
                    ui.horizontal(|ui| {
                        let label = ui.label(egui::RichText::new(label).strong());
                        ui.add(egui::DragValue::new(&mut number).range(*minimum as u64..=*maximum as u64))
                            .labelled_by(label.id);
                    });
                    ui.label(egui::RichText::new(description).small().weak());
                    values[key] = json!(number);
                }
                PluginSetting::Checkbox {
                    key,
                    label,
                    description,
                    ..
                } => {
                    let mut selected = values[key].as_bool().unwrap_or(false);
                    let label = if declaration.demo_only() {
                        format!("{label} · Example only")
                    } else {
                        label.clone()
                    };
                    ui.checkbox(&mut selected, egui::RichText::new(label).strong()).on_hover_text(description);
                    ui.indent(key, |ui| { ui.label(egui::RichText::new(description).small().weak()); });
                    values[key] = json!(selected);
                }
                PluginSetting::Filter {
                    key,
                    label,
                    description,
                    ..
                } => {
                    let mut text = values[key].as_str().unwrap_or("").to_string();
                    ui.vertical(|ui| {
                        let label = if declaration.demo_only() {
                            format!("{label} · Example only")
                        } else {
                            label.clone()
                        };
                        let label = ui.label(egui::RichText::new(label).strong()).on_hover_text(description);
                        ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY))
                            .labelled_by(label.id);
                        if ui.button("Clear filter").clicked() {
                            text.clear();
                        }
                    });
                    ui.label(egui::RichText::new(description).small().weak());
                    values[key] = json!(text);
                }
                PluginSetting::Choice {
                    key,
                    label,
                    description,
                    options,
                    ..
                } => {
                    ui.vertical(|ui| {
                        let label = if declaration.demo_only() {
                            format!("{label} · Example only")
                        } else {
                            label.clone()
                        };
                        ui.label(egui::RichText::new(label).strong()).on_hover_text(description);
                        ui.horizontal_wrapped(|ui| {
                        for option in options {
                            ui.radio_value(&mut values[key], option.value.clone(), &option.label);
                        }
                        });
                    });
                    ui.label(egui::RichText::new(description).small().weak());
                }
            }
            });
}

#[cfg(test)]
mod advanced_tests {
    use super::*;
    fn category(id:&str,checkbox:PluginSetting)->Value {json!({"id":id,"settings":[PluginSetting::filter("userIdPrefix","User ID starts with","").description("Select content owners."),checkbox,PluginSetting::integer("itemsPerPage","Rows per request",100,1,i32::MAX as u32)]})}
    fn known()->PluginSetting {PluginSetting::checkbox("combinedRead","Combine source reads in one BAQ",false).description("Read personalization source groups together with an unsaved BAQ. File dependencies still use their native readers. Your choice is saved.")}
    fn draw(ctx:&egui::Context,category:&Value,values:&mut Value,events:Vec<egui::Event>)->Vec<(String,egui::Rect)> {
        let mut output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(760.0,900.0))),events,..Default::default()},|ui|{egui::CentralPanel::default().show(ui,|ui|show(ui,category,values));});
        output.textures_delta.clear();
        output.shapes.into_iter().filter_map(|shape|if let egui::Shape::Text(text)=shape.shape {{let rect=egui::Rect::from_min_size(text.pos,text.galley.size());shape.clip_rect.expand(1.0).contains_rect(rect).then(||(text.galley.text().into(),rect))}}else{None}).collect()
    }
    #[test]
    fn known_kinetic_personalizations_transport_control_is_collapsed_and_content_remains_visible_without_changing_values() {
        let ctx=egui::Context::default();let category=category("kinetic-personalizations",known());let mut values=json!({"combinedRead":true,"userIdPrefix":" Ab ","itemsPerPage":700});
        let mut labels=Vec::new();for _ in 0..3{labels=draw(&ctx,&category,&mut values,vec![]);}
        assert!(labels.iter().any(|(text,_)|text=="User ID starts with"));assert!(!labels.iter().any(|(text,_)|text=="Combine source reads in one BAQ"||text=="Rows per request"));
        let center=labels.iter().find(|(text,_)|text=="Advanced").unwrap().1.center();
        draw(&ctx,&category,&mut values,vec![egui::Event::PointerMoved(center),egui::Event::PointerButton{pos:center,button:egui::PointerButton::Primary,pressed:true,modifiers:Default::default()}]);
        draw(&ctx,&category,&mut values,vec![egui::Event::PointerButton{pos:center,button:egui::PointerButton::Primary,pressed:false,modifiers:Default::default()}]);
        for _ in 0..3{labels=draw(&ctx,&category,&mut values,vec![]);}
        assert!(labels.iter().any(|(text,_)|text=="Combine source reads in one BAQ"));assert_eq!(values,json!({"combinedRead":true,"userIdPrefix":" Ab ","itemsPerPage":700}));
    }
    #[test]
    fn same_name_unknown_category_or_description_stays_visible_as_content() {
        for (id,checkbox,label) in [("kinetic-applications-and-layers",known(),"Combine source reads in one BAQ"),("kinetic-personalizations",PluginSetting::checkbox("combinedRead","Unreviewed content option",false).description("Different meaning"),"Unreviewed content option")] {
            let ctx=egui::Context::default();let category=category(id,checkbox);let mut values=json!({"combinedRead":false,"userIdPrefix":""});let mut labels=Vec::new();for _ in 0..3{labels=draw(&ctx,&category,&mut values,vec![]);}
            assert!(labels.iter().any(|(text,_)|text==label),"{id}: {label}");
        }
    }
}
