//! Read custom C# from the persisted BPM workflow. The native body remains the
//! authority. These files are readable copies, never reconstructed BPM packages.
use herbert_contract::Diagnostic;
use quick_xml::{Reader, XmlVersion, events::Event};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub fn attach_method_export_owner(definition: &mut Value, method: &Value, category: &str, company: &str) {
    let (fields,kind,key) = match method["Source"].as_str() {
        Some("BO") => (vec!["SystemCode", "ObjectNS", "BusinessObject"],"MethodDirectiveService","Service"),
        Some("DB") => (vec!["SystemCode", "BusinessObject"],"DataDirectiveTable","Table"),
        _ => return,
    };
    let parts = fields.into_iter().map(|key| method[key].as_str().filter(|value| !value.is_empty())).collect::<Option<Vec<_>>>();
    let Some(parts) = parts else {return;};
    definition["herbertExportIdentity"] = json!(herbert_contract::Identity {
        category:category.into(), kind:kind.into(),
        scope:herbert_contract::Scope::Company {company:company.into()},
        keys:BTreeMap::from([(key.into(),parts.join("."))]),
    });
}

pub(crate) fn label(text: &str) -> String {
    let mut result=String::new();
    let length=text.chars().count();
    for (index,ch) in text.chars().enumerate() {
        if ch.is_control() || "<>:\"/\\|?*%".contains(ch) || (ch=='.' && (index==0 || index+1==length)) || (ch==' ' && index+1==length) {
            for byte in ch.to_string().bytes() {result.push_str(&format!("%{byte:02X}"));}
        } else {result.push(ch);}
    }
    if result.is_empty() {return "Unnamed".into();}
    let stem=result.split('.').next().unwrap_or("").to_ascii_uppercase();
    if ["CON","PRN","AUX","NUL"].contains(&stem.as_str()) || (stem.len()==4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && matches!(stem.as_bytes()[3],b'1'..=b'9')) {result.insert_str(0,"%5F");}
    result
}

pub(crate) fn code_files(body: &str) -> Result<Vec<(String, String)>, ()> {
    let mut reader=Reader::from_str(body);
    let mut usings=String::new();
    let mut step=String::new();
    let mut code=Vec::new();
    let mut names=std::collections::BTreeSet::<String>::new();
    let mut depth=0usize;
    let mut root_seen=false;
    loop {
        let event=reader.read_event().map_err(|_|())?;
        match &event {
            Event::Start(_) => {if depth==0 && root_seen {return Err(());} root_seen=true;depth+=1;},
            Event::Empty(_) if depth==0 => {if root_seen {return Err(());} root_seen=true;},
            Event::End(_) => depth=depth.checked_sub(1).ok_or(())?,
            _=>{},
        }
        match event {
            Event::Start(element) | Event::Empty(element) => {
                let attributes=element.attributes().map(|attribute| {
                    let attribute=attribute.map_err(|_|())?;
                    let name=String::from_utf8(attribute.key.local_name().as_ref().to_vec()).map_err(|_|())?;
                    let value=attribute.decoded_and_normalized_value(XmlVersion::Implicit1_0,reader.decoder()).map_err(|_|())?.into_owned();
                    Ok((name,value))
                }).collect::<Result<BTreeMap<_,_>,()>>()?;
                match element.local_name().as_ref() {
                    b"DirectiveDefinition2" => {
                        usings=attributes.get("AdditionalUsings").cloned().unwrap_or_default();
                        // Native XAML represents null using a namespace-qualified
                        // markup extension, not a C# using directive.
                        if let Some(prefix)=usings.strip_prefix('{').and_then(|v|v.strip_suffix(":Null}")) {
                            if attributes.get(prefix).is_some_and(|uri|uri=="http://schemas.microsoft.com/winfx/2006/xaml") {usings.clear();}
                        }
                    },
                    b"DirectiveStep" => step=attributes.get("DisplayName").or_else(||attributes.get("Id")).cloned().unwrap_or_default(),
                    b"CustomCodeAction" => {
                        if let Some(text)=attributes.get("Code").filter(|text| !text.is_empty()) {
                            let name=label(if step.is_empty() {"Custom code"} else {&step});
                            let base=name;let mut name=base.clone();let mut count=1usize;
                            while !names.insert(name.to_lowercase()) {count+=1;name=format!("{base} - {count}");}
                            let text=if usings.is_empty() {text.clone()} else {format!("{usings}\n\n{text}")};
                            code.push((format!("{name}.cs"),text));
                        }
                    },
                    _=>{},
                }
            },
            Event::DocType(_) => return Err(()),
            Event::Eof => return if root_seen && depth==0 {Ok(code)} else {Err(())},
            _=>{},
        }
    }
}

pub fn attach(definition: &mut Value, dataset: &Value) -> Vec<Diagnostic> {
    let mut files=Vec::new();
    let mut candidates=Vec::<(String,String,String)>::new();
    let mut errors=Vec::new();
    for (index,row) in dataset["BpDirective"].as_array().into_iter().flatten().enumerate() {
        let Some(body)=row["Body"].as_str().filter(|body|!body.is_empty()) else {continue;};
        let method=row["BpMethodCode"].as_str().unwrap_or("Method").rsplit('.').next().unwrap_or("Method");
        let name=row["Name"].as_str().filter(|name|!name.is_empty()).or_else(||row["DirectiveID"].as_str()).unwrap_or("Directive");
        match code_files(body) {
            Ok(code)=> {
                let owner=row["DirectiveID"].as_str().filter(|v|!v.is_empty())
                    .or_else(||row["SysRowID"].as_str().filter(|v|!v.is_empty()))
                    .map(label).unwrap_or_else(||format!("row-{}",index+1));
                candidates.extend(code.into_iter().map(|(file,text)|(format!("Code/{} - {} - {file}",label(method),label(name)),text,owner.clone())));
            },
            Err(())=>errors.push(Diagnostic::new("BPM_CODE_UNREADABLE",format!("The custom code in directive {name} could not be read. Its Epicor workflow remains preserved."))),
        }
    }
    let mut counts=BTreeMap::<String,usize>::new();
    for (name,_,_) in &candidates {*counts.entry(name.to_lowercase()).or_default()+=1;}
    let mut used=std::collections::BTreeSet::new();
    for (name,text,owner) in candidates {
        let stem=name.strip_suffix(".cs").unwrap_or(&name);
        let base=if counts[&name.to_lowercase()]>1 {format!("{stem} - {owner}")} else {stem.into()};
        let mut name=format!("{base}.cs");let mut occurrence=1usize;
        while !used.insert(name.to_lowercase()) {occurrence+=1;name=format!("{base} - {occurrence}.cs");}
        files.push(json!({"name":name,"text":text}));
    }
    if !files.is_empty() {
        if !definition["herbertSources"].is_object() {definition["herbertSources"]=json!({});}
        definition["herbertSources"]["files"]=json!(files);
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workflow_code_decodes_entities_preserves_unicode_and_uses_readable_names() {
        let body=r#"<DirectiveDefinition2 AdditionalUsings="using System;&#xA;"><DirectiveStep DisplayName="Execute custom code"><CustomCodeAction Code="if (x &lt; 2)&#xA;  Print(&quot;Ω&quot;);"/></DirectiveStep></DirectiveDefinition2>"#;
        let files=code_files(body).unwrap();assert_eq!(files,vec![("Execute custom code.cs".into(),"using System;\n\n\nif (x < 2)\n  Print(\"Ω\");".into())]);
    }
    #[test]
    fn malformed_workflows_do_not_emit_guessed_code() {
        let mut definition=json!({});let data=json!({"BpDirective":[{"Name":"Protected","IsProtected":true,"Body":"invalid"},{"Name":"Broken","Body":"<DirectiveDefinition2><"}]});
        let errors=attach(&mut definition,&data);assert_eq!(errors.len(),2);assert!(definition["herbertSources"]["files"].is_null());
        assert!(code_files("<DirectiveDefinition2><CustomCodeAction Code=\"partial\"/>").is_err());
    }
    #[test]
    fn data_directive_owner_is_table_scoped_and_code_is_readable() {
        let mut definition=json!({});
        attach_method_export_owner(&mut definition,&json!({"Source":"DB","SystemCode":"ICE","BusinessObject":"UD39"}),"data-directives","C");
        assert_eq!(definition["herbertExportIdentity"]["kind"],"DataDirectiveTable");
        assert_eq!(definition["herbertExportIdentity"]["keys"]["Table"],"ICE.UD39");
        let data=json!({"BpDirective":[{"Source":"DB","Name":"Code","BpMethodCode":"Ice.UD39","IsProtected":false,"Body":r#"<DirectiveDefinition2><DirectiveStep DisplayName="Code"><CustomCodeAction Code="var value = 1;"/></DirectiveStep></DirectiveDefinition2>"#}]});
        assert!(attach(&mut definition,&data).is_empty());
        assert_eq!(definition["herbertSources"]["files"][0]["text"],"var value = 1;");
    }

    #[test]
    fn authorized_protected_workflow_code_is_preserved_exactly() {
        let mut definition=json!({});
        let data=json!({"BpDirective":[{"Name":"Protected", "IsProtected":true, "Body":r#"<DirectiveDefinition2><DirectiveStep DisplayName="Exact"><CustomCodeAction Code="var text = &quot;Ω&quot;;&#xA;"/></DirectiveStep></DirectiveDefinition2>"#}]});
        assert!(attach(&mut definition,&data).is_empty());
        assert_eq!(definition["herbertSources"]["files"][0]["text"],"var text = \"Ω\";\n");
    }
    #[test]
    fn native_xaml_null_usings_do_not_become_csharp_text() {
        for prefix in ["x","markup"] {
            let body=format!(r#"<DirectiveDefinition2 xmlns:{prefix}="http://schemas.microsoft.com/winfx/2006/xaml" AdditionalUsings="{{{prefix}:Null}}"><DirectiveStep DisplayName="Code"><CustomCodeAction Code="var text = &quot;Ω&quot;;"/></DirectiveStep></DirectiveDefinition2>"#);
            assert_eq!(code_files(&body).unwrap()[0].1,"var text = \"Ω\";");
        }
    }

    #[test]
    fn distinct_directives_with_same_readable_names_never_share_a_file() {
        let body=|code| format!(r#"<DirectiveDefinition2><DirectiveStep DisplayName="Step"><CustomCodeAction Code="{code}"/></DirectiveStep></DirectiveDefinition2>"#);
        let rows=json!({"BpDirective":[
            {"Name":"Same","DirectiveID":"first","BpMethodCode":"A.Update","Body":body("return 1;")},
            {"Name":"same","DirectiveID":"second","BpMethodCode":"A.Update","Body":body("return 2;")}
        ]});
        let mut d=json!({});assert!(attach(&mut d,&rows).is_empty());
        let files=d["herbertSources"]["files"].as_array().unwrap();
        assert_eq!(files.len(),2);assert_ne!(files[0]["name"].as_str().unwrap().to_lowercase(),files[1]["name"].as_str().unwrap().to_lowercase());
        assert_eq!(files[0]["text"],"return 1;");assert_eq!(files[1]["text"],"return 2;");
        let body=r#"<DirectiveDefinition2><DirectiveStep DisplayName="Step"><CustomCodeAction Code="one"/></DirectiveStep><DirectiveStep DisplayName="step"><CustomCodeAction Code="two"/></DirectiveStep><DirectiveStep DisplayName="Step - 2"><CustomCodeAction Code="three"/></DirectiveStep></DirectiveDefinition2>"#;
        let code=code_files(body).unwrap();let names:std::collections::BTreeSet<_>=code.iter().map(|(n,_)|n.to_lowercase()).collect();assert_eq!(names.len(),3);
    }

}
