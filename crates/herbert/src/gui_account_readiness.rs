//! Visible account eligibility uses the existing native admission result.
//! Test connection (gui_connection.rs) produces it; this file only displays it.
use super::*;

fn label(reply: Option<&Reply>) -> (&'static str, String) {
    match reply {
        Some(reply) if reply.ok && reply.result.as_ref().is_some_and(|value|value["admission"]=="verifiedSecurityManager") =>
            ("Verified eligible (last check)", "Epicor confirmed a matching, enabled and unlocked Security Manager. Account eligibility does not authorize server changes.".into()),
        Some(reply) if !reply.ok => ("Verification failed", reply.error.as_ref().map(|error|format!("{}: {}",error.code,error.message)).unwrap_or_else(||"Verification did not establish eligibility. Verify the selected connection again.".into())),
        _ => ("Unknown / unverified", "Use Test connection before downloading from Epicor. Herbert runs its Security Manager sign-in checks first. Passing them does not authorize server changes.".into()),
    }
}
fn display(ui:&mut egui::Ui,reply:Option<&Reply>,testing:bool) {
    if testing {
        ui.horizontal(|ui| { ui.spinner(); ui.label(RichText::new("Account readiness: Verifying…").strong()); });
        return;
    }
    let (state,detail)=label(reply);
    ui.label(RichText::new(format!("Account readiness: {state}")).strong());
    ui.label(detail);
}
impl Gui {
    pub(super) fn account_key(&self)->Result<String,Diagnostic> {
        let arguments=self.connection_arguments(&self.settings.value()?)?;
        Ok(super::gui_server_changes::connection_key(&arguments, false))
    }
    pub(super) fn account_readiness_panel(&mut self,ui:&mut egui::Ui) {
        let current=self.account_key().ok();
        let reply=self.account_readiness.as_ref().filter(|(key,_)|Some(key)==current.as_ref()).map(|(_,reply)|reply);
        display(ui,reply,self.testing_connection);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn reply(ok:bool,result:Option<Value>,error:Option<Diagnostic>)->Reply {Reply{schema_version:1,ok,result,error}}
    fn visible(reply:Option<&Reply>,testing:bool)->String {
        let ctx=egui::Context::default();let mut text=String::new();
        for _ in 0..3 {
            let mut output=ctx.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(900.0,600.0))),..Default::default()},|ui|{egui::CentralPanel::default().show(ui,|ui|display(ui,reply,testing));});
            output.textures_delta.clear();
            text=output.shapes.into_iter().filter_map(|shape|if let egui::Shape::Text(text)=shape.shape {let rect=egui::Rect::from_min_size(text.pos,text.galley.size());shape.clip_rect.expand(1.0).contains_rect(rect).then(||text.galley.text().to_owned())} else {None}).collect::<Vec<_>>().join(" ");
        }text
    }
    #[test]
    fn readiness_states_are_visible_and_eligibility_does_not_create_consent() {
        assert!(visible(None,false).contains("Unknown / unverified"));
        assert!(visible(None,true).contains("Verifying…"), "a running check is not shown as unknown");
        let eligible=reply(true,Some(json!({"admission":"verifiedSecurityManager"})),None);
        let text=visible(Some(&eligible),false);assert!(text.contains("Verified eligible"));assert!(text.contains("does not authorize server changes"));
        let refused=reply(false,None,Some(Diagnostic::new("SECURITY_MANAGER_REQUIRED","Ask your Epicor administrator to verify Security Manager access, then test again.")));
        let text=visible(Some(&refused),false);assert!(text.contains("Verification failed"));assert!(text.contains("SECURITY_MANAGER_REQUIRED"));assert!(text.contains("test again"));
        let unverified=reply(true,Some(json!({"admission":"unknown"})),None);assert!(visible(Some(&unverified),false).contains("Unknown / unverified"));
    }
}
