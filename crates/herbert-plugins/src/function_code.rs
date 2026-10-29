//! Readable function sources owned by the Functions plugin. Native bodies remain authoritative.
use herbert_contract::Diagnostic;
use serde_json::{Value, json};
use crate::bpm_code::{code_files, label};

pub fn attach(definition: &mut Value, dataset: &Value) -> Vec<Diagnostic> {
    let mut files = Vec::new();
    let mut errors = Vec::new();
    for row in dataset["EfxFunction"].as_array().into_iter().flatten() {
        let name = row["FunctionID"].as_str().unwrap_or("Function");
        // Native reads can return no body. There is no source to extract in
        // that case. Keep the definition and do not report a parser failure.
        if row["Body"].is_null() || row["Body"].as_str().is_some_and(str::is_empty) {
            continue;
        }
        let body = row["Body"].as_str().unwrap_or("");
        let sources = match row["Kind"].as_u64() {
            Some(2) => serde_json::from_str::<Value>(body).ok().and_then(|value| {
                let code = value["Code"].as_str()?;
                let usings = value["Usings"].as_str()?;
                Some(vec![(format!("{}.cs",label(name)), if usings.is_empty() {code.to_owned()} else {format!("{usings}\n\n{code}")})])
            }),
            Some(0 | 1) => code_files(body).ok().map(|code| code.into_iter().map(|(file,text)| (format!("{} - {file}",label(name)),text)).collect()),
            _ => None,
        };
        match sources {
            Some(sources) => files.extend(sources.into_iter().map(|(name,text)|json!({"name":format!("Code/{name}"),"text":text}))),
            None => errors.push(Diagnostic::new("FUNCTION_CODE_UNREADABLE",format!("Function {name} has an unreadable or unsupported body. Its Epicor definition remains preserved."))),
        }
    }
    if !files.is_empty() {definition["herbertSources"] = json!({"files":files});}
    errors
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn absent_bodies_do_not_report_parser_failures_or_create_sources() {
        let data=json!({"EfxFunction":[
            {"FunctionID":"function-1","Kind":2,"Body":null},
            {"FunctionID":"function-3","Kind":2},
            {"FunctionID":"MyUserCodesCombo","Kind":0,"Body":null},
            {"FunctionID":"Empty","Kind":1,"Body":""}
        ]});
        let mut output=data.clone();
        assert!(attach(&mut output,&data).is_empty());
        assert_eq!(output,data);
    }
    #[test] fn code_and_widget_sources_use_function_names_and_preserve_text() {
        let data=json!({"EfxFunction":[
            {"FunctionID":"Code Ω","Kind":2,"Body":json!({"Code":"var text = \"Ω\";","Usings":"using System;"}).to_string()},
            {"FunctionID":"Widget","Kind":1,"Body":r#"<DirectiveDefinition2><DirectiveStep DisplayName="Execute"><CustomCodeAction Code="if (a &lt; 2)&#xA;  return;"/></DirectiveStep></DirectiveDefinition2>"#},
            {"FunctionID":"Visual","Kind":0,"Body":"<DirectiveDefinition2/>"}
        ]});
        let mut output=json!({}); assert!(attach(&mut output,&data).is_empty());
        assert_eq!(output["herbertSources"]["files"][0]["name"],"Code/Code Ω.cs");
        assert_eq!(output["herbertSources"]["files"][0]["text"],"using System;\n\nvar text = \"Ω\";");
        assert_eq!(output["herbertSources"]["files"][1]["text"],"if (a < 2)\n  return;");
        assert_eq!(output["herbertSources"]["files"].as_array().unwrap().len(),2);
    }
    #[test] fn malformed_and_unknown_bodies_are_explicit_omissions() {
        let data=json!({"EfxFunction":[{"FunctionID":"Broken","Kind":1,"Body":"<DirectiveDefinition2>"},{"FunctionID":"Unknown","Kind":9,"Body":"abc"},{"FunctionID":"MissingCode","Kind":2,"Body":"{}"}]});
        let mut output=json!({}); assert_eq!(attach(&mut output,&data).len(),3); assert!(output["herbertSources"].is_null());
    }
}
