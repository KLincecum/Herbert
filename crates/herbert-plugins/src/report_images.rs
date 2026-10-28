//! Resolve each authored style mapping through its exact native Erp.Image row.
//! Image has no SystemFlag. Native ImageSvc joins ImageSysRowID to FileStore.
//! Use that same persisted identity and the authenticated read-only byte route.
use herbert_contract::*;
use serde_json::json;
use std::collections::BTreeMap;

fn error(r: &mut GuestRequest, code: &str, message: &str) {
    r.state["errors"]
        .as_array_mut()
        .unwrap()
        .push(json!(Diagnostic::new(code, message)));
}
pub fn start(r: &mut GuestRequest) -> Option<HostAction> {
    r.state["styleImageQueue"] = r.state["definition"]["ReportStyleImage"].clone();
    r.state["styleImageIndex"] = json!(0);
    next(r)
}
fn next(r: &mut GuestRequest) -> Option<HostAction> {
    loop {
        let index = r.state["styleImageIndex"].as_u64().unwrap() as usize;
        let Some(row) = r.state["styleImageQueue"].get(index).cloned() else {
            r.state["styleImagesComplete"] = json!(true);
            return None;
        };
        let (Some(company), Some(image)) = (
            row["ImageCompany"].as_str(),
            row["ImageID"].as_str().filter(|s| !s.is_empty()),
        ) else {
            error(
                r,
                "REPORT_IMAGE_IDENTITY_UNVERIFIED",
                "An image mapping lacks its exact company and image identity.",
            );
            r.state["styleImageIndex"] = json!(index + 1);
            continue;
        };
        if r.state["imageCatalog"][company]["complete"] == true {
            let images = r.state["imageCatalog"][company]["images"][image].clone();
            if let Some(action) =
                image_bytes(r, images.as_array().map(Vec::as_slice).unwrap_or(&[]))
            {
                return Some(action);
            }
            continue;
        }
        r.state["imageCatalogCompany"] = json!(company);
        r.state["imageCatalogAfter"] = serde_json::Value::Null;
        return Some(metadata_page(r));
    }
}
fn metadata_page(r: &mut GuestRequest) -> HostAction {
    r.state["phase"] = json!("styleImageMetadata");
    let mut criteria = vec![QueryCriterion {
        table: "ImageSource".into(),
        field: "Company".into(),
        operator: "=".into(),
        parameter: "company".into(),
    }];
    let mut parameters =
        BTreeMap::from([("company".into(), r.state["imageCatalogCompany"].clone())]);
    if let Some(after) = r.state["imageCatalogAfter"].as_str() {
        criteria.push(QueryCriterion {
            table: "ImageSource".into(),
            field: "SysRowID".into(),
            operator: ">".into(),
            parameter: "after".into(),
        });
        parameters.insert("after".into(), json!(after));
    }
    HostAction::SharedQuery {
        query: QuerySpec {
            tables: vec![QueryTable {
                schema: "Erp".into(),
                table: "Image".into(),
                alias: "ImageSource".into(),
                company_field: None,
                metadata: vec![],
            }],
            fields: [
                "Company",
                "ImageID",
                "SysRowID",
                "ImageSysRowID",
                "FileSize",
                "FileType",
                "GlobalImage",
            ]
            .iter()
            .map(|f| QueryField {
                table: "ImageSource".into(),
                field: (*f).into(),
                alias: (*f).into(),
                expression: None,
            })
            .collect(),
            joins: vec![],
            criteria,
            parameters,
            order: vec![QueryOrder {
                field: "SysRowID".into(),
                descending: false,
            }],
            page_size: r.page_size,
            page: 1,
        },
    }
}
fn image_bytes(r: &mut GuestRequest, rows: &[serde_json::Value]) -> Option<HostAction> {
    let index = r.state["styleImageIndex"].as_u64().unwrap() as usize;
    let mapping = r.state["styleImageQueue"][index].clone();
    if rows.len() == 1 {
        let image = &rows[0];
        let id = image["ImageSysRowID"].as_str().filter(|s| {
            s.len() == 36
                && *s != "00000000-0000-0000-0000-000000000000"
                && s.bytes().enumerate().all(|(i, b)| {
                    if [8, 13, 18, 23].contains(&i) {
                        b == b'-'
                    } else {
                        b.is_ascii_hexdigit()
                    }
                })
        });
        if image["Company"] == mapping["ImageCompany"]
            && image["ImageID"] == mapping["ImageID"]
            && (image["Company"] == r.company || image["GlobalImage"] == true)
            && !crate::system_artifacts::marked(image)
            && image["FileSize"].as_u64().is_some()
            && id.is_some()
        {
            r.state["styleImageMetadata"] = image.clone();
            r.state["phase"] = json!("styleImageBytes");
            return Some(HostAction::SharedNativeRead {
                service: "Ice.Lib.FileStoreSvc".into(),
                method: "ReadAllBytes".into(),
                arguments: json!({"id":image["ImageSysRowID"]}),
                export: true,
            });
        }
    }
    error(
        r,
        "REPORT_IMAGE_OWNERSHIP_UNVERIFIED",
        "The image mapping did not resolve to one accessible native image and file identity.",
    );
    r.state["styleImageIndex"] = json!(index + 1);
    None
}
pub fn complete(r: &mut GuestRequest) -> Option<HostAction> {
    let index = r.state["styleImageIndex"].as_u64().unwrap() as usize;
    let mapping = r.state["styleImageQueue"][index].clone();
    if let Some(e) = r.error.take() {
        r.state["errors"].as_array_mut().unwrap().push(json!(e));
    } else if let Some(response) = r.response.take() {
        if r.state["phase"] == "styleImageMetadata" {
            let company = r.state["imageCatalogCompany"].as_str().unwrap().to_owned();
            if let (Some(rows), Some(size)) =
                (response["rows"].as_array(), r.page_size.filter(|s| *s > 0))
            {
                for image in rows {
                    if image["Company"] != company {
                        continue;
                    }
                    if let Some(id) = image["ImageID"].as_str() {
                        let entry = &mut r.state["imageCatalog"][&company]["images"][id];
                        if !entry.is_array() {
                            *entry = json!([]);
                        }
                        entry.as_array_mut().unwrap().push(image.clone());
                    }
                }
                if rows.len() == size as usize {
                    if let Some(after) = rows
                        .last()
                        .and_then(|v| v["SysRowID"].as_str())
                        .filter(|s| !s.is_empty() && json!(s) != r.state["imageCatalogAfter"])
                    {
                        r.state["imageCatalogAfter"] = json!(after);
                        return Some(metadata_page(r));
                    }
                } else if rows.len() < size as usize {
                    r.state["imageCatalog"][&company]["complete"] = json!(true);
                    return next(r);
                }
            }
            error(
                r,
                "REPORT_IMAGE_METADATA_INCOMPLETE",
                "The image list is missing rows or its page cursor did not advance.",
            );
        } else {
            let file = response
                .pointer("/returnObj/nativeFile")
                .cloned()
                .and_then(|v| serde_json::from_value::<FileRef>(v).ok());
            if file.as_ref().is_none_or(|f| {
                f.bytes == 0 || Some(f.bytes) != r.state["styleImageMetadata"]["FileSize"].as_u64()
            }) {
                error(
                    r,
                    "REPORT_IMAGE_BYTES_UNVERIFIED",
                    "The original image bytes are absent or differ from the native Image file length.",
                );
            }
            r.state["supplements"][format!("ReportImage/{index}")] = json!({"mapping":mapping,"metadata":r.state["styleImageMetadata"],"bytes":response});
        }
    } else {
        error(
            r,
            "REPORT_IMAGE_RESPONSE_MISSING",
            "The native image operation returned no response.",
        );
    }
    r.state["styleImageIndex"] = json!(index + 1);
    next(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> GuestRequest {
        GuestRequest {
            abi: PLUGIN_ABI,
            operation: "acquire".into(),
            category: "C17".into(),
            company: "C".into(),
            identity: None,
            cursor: None,
            page_size: Some(2),
            state: json!({"errors":[],"supplements":{},"definition":{"ReportStyleImage":[{"ImageCompany":"C","ImageID":"Owned"}]}}),
            response: None,
            error: None,
        }
    }
    #[test]
    fn two_images_share_paged_metadata_then_download_each_file_once() {
        let mut r = request();
        r.state["definition"]["ReportStyleImage"]
            .as_array_mut()
            .unwrap()
            .push(json!({"ImageCompany":"C","ImageID":"Second"}));
        let Some(HostAction::SharedQuery { query }) = start(&mut r) else {
            panic!()
        };
        assert_eq!(query.page_size, Some(2));
        assert!(!query.criteria.iter().any(|c| c.field == "ImageID"));
        r.response = Some(json!({"rows":[
            {"Company":"C","ImageID":"Owned","SysRowID":"a","ImageSysRowID":"11111111-1111-1111-1111-111111111111","FileSize":3},
            {"Company":"C","ImageID":"Second","SysRowID":"b","ImageSysRowID":"22222222-2222-2222-2222-222222222222","FileSize":3}
        ]}));
        let Some(HostAction::SharedQuery { query }) = complete(&mut r) else {
            panic!()
        };
        assert_eq!(query.parameters["after"], "b");
        r.response = Some(json!({"rows":[]}));
        for id in [
            "11111111-1111-1111-1111-111111111111",
            "22222222-2222-2222-2222-222222222222",
        ] {
            let Some(HostAction::SharedNativeRead { arguments, .. }) = complete(&mut r) else {
                panic!("No extra metadata query is needed")
            };
            assert_eq!(arguments["id"], id);
            r.response = Some(
                json!({"returnObj":{"nativeFile":FileRef{path:format!("{id}.bin"),sha256:"a".repeat(64),bytes:3,role:"nativeExport".into()}}}),
            );
        }
        assert!(complete(&mut r).is_none());
        assert!(r.state["errors"].as_array().unwrap().is_empty());
        assert_eq!(r.state["supplements"].as_object().unwrap().len(), 2);
    }
    #[test]
    fn exact_native_owner_precedes_bytes_and_length_is_verified() {
        let mut r = request();
        assert!(matches!(
            start(&mut r),
            Some(HostAction::SharedQuery { .. })
        ));
        r.response = Some(
            json!({"rows":[{"Company":"C","ImageID":"Owned","ImageSysRowID":"11111111-1111-1111-1111-111111111111","FileSize":3,"GlobalImage":false}]}),
        );
        assert!(matches!(
            complete(&mut r),
            Some(HostAction::SharedNativeRead { .. })
        ));
        r.response = Some(
            json!({"returnObj":{"nativeFile":FileRef{path:"image.bin".into(),sha256:"a".repeat(64),bytes:3,role:"nativeExport".into()}}}),
        );
        assert!(complete(&mut r).is_none());
        assert!(r.state["errors"].as_array().unwrap().is_empty());
    }
    #[test]
    fn foreign_or_marked_image_never_requests_bytes() {
        for marked in [false, true] {
            let mut r = request();
            start(&mut r);
            r.response = Some(
                json!({"rows":[{"Company":if marked{"C"}else{"FOREIGN"},"ImageID":"Owned","ImageSysRowID":"11111111-1111-1111-1111-111111111111","FileSize":3,"GlobalImage":false,"SystemFlag":marked}]}),
            );
            assert!(complete(&mut r).is_none());
            assert_eq!(
                r.state["errors"][0]["code"],
                "REPORT_IMAGE_OWNERSHIP_UNVERIFIED"
            );
        }
    }
}
