//! Build a backup EFXJ from the native Function Designer tableset.
//! Field and reference mappings follow Epicor's LibraryPersister and Mapster configuration.
use herbert_contract::Diagnostic;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn invalid(message: &str) -> Diagnostic {
    Diagnostic::new("FUNCTION_DEFINITION_INVALID", message)
}

fn rows<'a>(data: &'a Value, table: &str, library: &str) -> Result<Vec<&'a Value>, Diagnostic> {
    let rows = data[table]
        .as_array()
        .ok_or_else(|| invalid(&format!("The Function response omitted {table}.")))?;
    if rows.iter().any(|row| row["LibraryID"].as_str().is_none()) {
        return Err(invalid(
            "A Function definition row has no library identity.",
        ));
    }
    Ok(rows
        .iter()
        .filter(|row| row["LibraryID"] == library)
        .collect())
}

fn fields(row: &Value, required: &[&str], nullable: &[&str]) -> Result<Value, Diagnostic> {
    let mut output = json!({});
    for key in required {
        output[*key] = row
            .get(*key)
            .ok_or_else(|| invalid(&format!("The Function definition omitted {key}.")))?
            .clone();
    }
    // The native writer omits null optional properties. Empty strings are data.
    for key in nullable {
        let value = row
            .get(*key)
            .ok_or_else(|| invalid(&format!("The Function definition omitted {key}.")))?;
        if !value.is_null() {
            output[*key] = value.clone();
        }
    }
    Ok(output)
}

pub fn build(
    data: &Value,
    id: &str,
    version: &str,
    system_code: &str,
) -> Result<Value, Diagnostic> {
    if id.is_empty() || version.is_empty() || system_code.is_empty() {
        return Err(invalid(
            "The library identity or server export context is missing.",
        ));
    }
    let libraries = rows(data, "EfxLibrary", id)?;
    if libraries.len() != 1 {
        return Err(invalid(
            "The Function response must contain one selected library.",
        ));
    }
    let root = libraries[0];
    // Build from the data returned by the authorized native reader. Library
    // mode is preserved below; it is not a restriction of the file format.
    let mut library = fields(
        root,
        &[
            "GlobalID",
            "EpicorVersion",
            "Revision",
            "Published",
            "Private",
            "Disabled",
            "Mode",
            "AllowCustomCodeWidgets",
            "AllowCustomCodeFunctions",
            "DirectDBAccess",
            "OwnedByCompany",
            "Owner",
        ],
        &["OriginalID", "Description", "Notes", "SharedWithGroup"],
    )?;
    let mut names = BTreeSet::new();
    let functions = rows(data, "EfxFunction", id)?;
    for function in &functions {
        let name = function["FunctionID"]
            .as_str()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| invalid("A Function has no identity."))?;
        if !names.insert(name) {
            return Err(invalid(
                "The Function response repeats a Function identity.",
            ));
        }
    }
    let signatures = rows(data, "EfxFunctionSignature", id)?;
    if signatures.iter().any(|row| {
        row["FunctionID"]
            .as_str()
            .is_none_or(|name| !names.contains(name))
    }) {
        return Err(invalid(
            "A signature has no Function in the selected library.",
        ));
    }
    let mut output_functions = Vec::new();
    for function in functions {
        let mut output = fields(
            function,
            &[
                "FunctionID",
                "Kind",
                "RequireTransaction",
                "SingleRowMode",
                "Private",
                "Disabled",
                "Invalid",
                "Thumbnail",
            ],
            &["Description", "Body", "Notes"],
        )?;
        let mut seen = BTreeSet::new();
        let mut arguments = Vec::new();
        for argument in signatures
            .iter()
            .filter(|row| row["FunctionID"] == function["FunctionID"])
        {
            let response = argument["Response"]
                .as_bool()
                .ok_or_else(|| invalid("A signature direction is invalid."))?;
            let parameter = argument["ParameterID"]
                .as_i64()
                .ok_or_else(|| invalid("A signature parameter ID is invalid."))?;
            if !seen.insert((response, parameter)) {
                return Err(invalid(
                    "The Function response repeats a signature parameter.",
                ));
            }
            let mut signature=fields(
                argument,
                &[
                    "Response",
                    "ParameterID",
                    "ArgumentName",
                    "Order",
                    "DataType",
                    "Optional",
                ],
                &["DataTypeInfo", "DefaultValue"],
            )?;
            // Native 4.3.200 reader/export omit this signature property. Later
            // readers can supply it. Preserve absence and every supplied value
            // (including explicit null/empty) without relaxing fields() globally.
            if let Some(description)=argument.get("Description") {signature["Description"]=description.clone();}
            arguments.push(signature);
        }
        if !arguments.is_empty() {
            output["FunctionSignature"] = json!(arguments);
        }
        output_functions.push(output);
    }
    if !output_functions.is_empty() {
        library["Functions"] = json!(output_functions);
    }
    let mut references = Vec::new();
    // These values are the native reference enum, not table enumeration order.
    for (table, key, kind) in [
        ("EfxRefAssembly", "Assembly", 0),
        ("EfxRefTable", "TableID", 1),
        ("EfxRefService", "ServiceID", 2),
        ("EfxRefLibrary", "LibraryRef", 3),
    ] {
        for row in rows(data, table, id)? {
            let reference = row[key]
                .as_str()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| invalid("A library reference has no identity."))?;
            let mut output = json!({"ReferenceType":kind,"ReferenceID":reference});
            if kind == 1 {
                output["Updatable"] = json!(
                    row["Updatable"]
                        .as_bool()
                        .ok_or_else(|| invalid("A table reference has no update permission."))?
                );
            }
            references.push(output);
        }
    }
    if !references.is_empty() {
        library["LibraryReferences"] = json!(references);
    }
    let mappings = rows(data, "EfxLibraryMapping", id)?
        .iter()
        .map(|row| fields(row, &["Company", "Allowed"], &[]))
        .collect::<Result<Vec<_>, _>>()?;
    if !mappings.is_empty() {
        library["LibraryMappings"] = json!(mappings);
    }
    Ok(
        json!({"Mode":"Backup","Version":version,"SystemCode":system_code,"LibraryId":id,"Library":library}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signature_description_preserves_absence_null_empty_and_value_without_weakening_other_fields() {
        let base=json!({"EfxLibrary":[{"LibraryID":"L","GlobalID":"00112233-4455-6677-8899-aabbccddeeff","EpicorVersion":"4.3.200","Revision":1,"Published":false,"Private":false,"Disabled":false,"Mode":0,"AllowCustomCodeWidgets":true,"AllowCustomCodeFunctions":true,"DirectDBAccess":0,"OwnedByCompany":"","Owner":"","OriginalID":null,"Description":null,"Notes":null,"SharedWithGroup":null}],"EfxFunction":[{"LibraryID":"L","FunctionID":"F","Kind":2,"RequireTransaction":false,"SingleRowMode":false,"Private":false,"Disabled":false,"Invalid":false,"Thumbnail":null,"Description":null,"Body":"return;","Notes":null}],"EfxFunctionSignature":[{"LibraryID":"L","FunctionID":"F","Response":false,"ParameterID":1,"ArgumentName":"argument","Order":0,"DataType":"System.String","Optional":false,"DataTypeInfo":null,"DefaultValue":null}],"EfxRefAssembly":[],"EfxRefTable":[],"EfxRefService":[],"EfxRefLibrary":[],"EfxLibraryMapping":[]});
        for description in [None,Some(Value::Null),Some(json!("")),Some(json!("Ω original\r\n"))] {let mut data=base.clone();if let Some(value)=&description{data["EfxFunctionSignature"][0]["Description"]=value.clone();}let model=build(&data,"L","4.3.200","ERP").unwrap();assert_eq!(model["Library"]["Functions"][0]["FunctionSignature"][0].get("Description"),description.as_ref());assert!(crate::function_binary::build(&model).is_ok());}
        for field in ["Response","ParameterID","ArgumentName","Order","DataType","Optional","DataTypeInfo","DefaultValue"]{let mut data=base.clone();data["EfxFunctionSignature"][0]["Description"]=json!("");data["EfxFunctionSignature"][0].as_object_mut().unwrap().remove(field);assert!(build(&data,"L","4.3.200","ERP").is_err(),"Missing {field} silently admitted");}
        let mut data=base;data["EfxLibrary"][0].as_object_mut().unwrap().remove("Description");assert!(build(&data,"L","4.3.200","ERP").is_err());
    }
    #[test]
    #[ignore = "Requires the explicitly retained 2024 signature Description reader/export pair."]
    fn retained_2024_signature_absence_matches_complete_native_export() {
        let folder=std::path::PathBuf::from(std::env::var("HERBERT_FUNCTIONS_SIGNATURE_DESCRIPTION_REPLAY").unwrap());let data:Value=serde_json::from_slice(&std::fs::read(folder.join("before.json")).unwrap()).unwrap();let raw=std::fs::read(folder.join("native-reference.efxj")).unwrap();let native:Value=serde_json::from_slice(raw.strip_prefix(&[0xef,0xbb,0xbf]).unwrap_or(&raw)).unwrap();assert_eq!(data["EfxFunctionSignature"].as_array().unwrap().len(),41);assert!(data["EfxFunctionSignature"].as_array().unwrap().iter().all(|row|row.get("Description").is_none()));let local=build(&data,"CallLib",native["Version"].as_str().unwrap(),native["SystemCode"].as_str().unwrap()).unwrap();assert_eq!(local,native);assert!(crate::function_binary::build(&local).is_ok());
    }
    #[test]
    fn backup_preserves_library_mode_without_imposing_editability() {
        let mut data = json!({"EfxLibrary":[{
            "LibraryID":"Packaged", "GlobalID":"00112233-4455-6677-8899-aabbccddeeff",
            "EpicorVersion":"5.2.100", "Revision":1, "Published":true,
            "Private":false, "Disabled":false, "Mode":1, "IsProtected":true,
            "AllowCustomCodeWidgets":true, "AllowCustomCodeFunctions":true,
            "DirectDBAccess":0, "OwnedByCompany":"", "Owner":"",
            "OriginalID":null, "Description":null, "Notes":null, "SharedWithGroup":null
        }], "EfxFunction":[], "EfxFunctionSignature":[], "EfxRefAssembly":[],
            "EfxRefTable":[], "EfxRefService":[], "EfxRefLibrary":[], "EfxLibraryMapping":[]});
        for mode in [0, 1, 2] {
            data["EfxLibrary"][0]["Mode"] = json!(mode);
            let export = build(&data, "Packaged", "5.2.100", "ERP").unwrap();
            assert_eq!(export["Library"]["Mode"], mode);
            assert_eq!(export["Mode"], "Backup");
            assert!(crate::function_binary::build(&export).is_ok());
        }
        data.as_object_mut().unwrap().remove("EfxFunctionSignature");
        assert!(build(&data, "Packaged", "5.2.100", "ERP").is_err());
    }
    #[test]
    #[ignore = "Requires the explicitly retained native visual Function fixture."]
    fn native_widget_preserves_body_and_omits_empty_collections() {
        let folder = std::path::PathBuf::from(std::env::var("HERBERT_FUNCTION_FIXTURE").unwrap());
        let data: Value = serde_json::from_slice(
            &std::fs::read(folder.join("function-native-widget-tables.json")).unwrap(),
        )
        .unwrap();
        let native: Value = serde_json::from_slice(
            &std::fs::read(folder.join("function-native-CaretProbeL1.efxj")).unwrap(),
        )
        .unwrap();
        let local = build(
            &data,
            "CaretProbeL1",
            native["Version"].as_str().unwrap(),
            native["SystemCode"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(local, native);
        assert!(local["Library"].get("LibraryMappings").is_none());
        assert!(local["Library"].get("LibraryReferences").is_none());
        let mut incomplete = data.clone();
        incomplete
            .as_object_mut()
            .unwrap()
            .remove("EfxFunctionSignature");
        assert!(build(&incomplete, "CaretProbeL1", "5.2.100", "ERP").is_err());
        incomplete = data.clone();
        incomplete["EfxLibrary"][0]["IsProtected"] = json!(true);
        assert!(build(&incomplete, "CaretProbeL1", "5.2.100", "ERP").is_ok());
    }
    #[test]
    #[ignore = "Requires the explicitly retained native Function comparison fixture."]
    fn native_bulk_matches_epicor_backup() {
        let folder = std::path::PathBuf::from(std::env::var("HERBERT_FUNCTION_FIXTURE").unwrap());
        let data: Value = serde_json::from_slice(
            &std::fs::read(folder.join("function-native-bulk-2026.json")).unwrap(),
        )
        .unwrap();
        let native: Value = serde_json::from_slice(
            &std::fs::read(folder.join("function-native-CallLib.efxj")).unwrap(),
        )
        .unwrap();
        let local = build(
            &data,
            "CallLib",
            native["Version"].as_str().unwrap(),
            native["SystemCode"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(local, native);
        std::fs::write(
            folder.join("function-local-CallLib.efxj"),
            serde_json::to_vec_pretty(&local).unwrap(),
        )
        .unwrap();
    }
}
