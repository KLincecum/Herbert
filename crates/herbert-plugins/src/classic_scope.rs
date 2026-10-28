//! Native Classic definition types, separate from the general XXXDef store.
// GenXDataSvc.whereClauseProgramPortion identifies Customization, Productization
// and BaseExtension. Its import AdjustCompany and verifyTypeCode paths also
// retain Localization and Verticalization. This is a source-backed type contract,
// not a naming convention or a count limit. Unknown types require classification.
// Launch eligibility (SysCheckBox01) must not discard retained backup variants.
pub const TYPES: &[&str] = &[
    "Customization", "BaseExtension", "Productization", "Verticalization", "Localization",
];

pub fn owns(code: &str) -> bool {
    TYPES.iter().any(|expected| expected.eq_ignore_ascii_case(code))
}
