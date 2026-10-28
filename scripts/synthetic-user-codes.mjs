// Synthetic responses for local CLI and MCP tests. No server export is used.
const company = "Fixture";
const codeType = "EXAMPLE";
export const wire = {
  returnObj: {
    UDCodeType: [{ Company: company, CodeTypeID: codeType, CodeTypeDesc: "Synthetic codes", SystemFlag: false }],
    UDCodes: [{ Company: company, CodeTypeID: codeType, CodeID: "A", CodeDesc: "Synthetic Ω entry", SystemFlag: false }],
  },
  parameters: { morePages: false },
};
export const metadata = ["UDCodeType", "UDCodes"].map((table) => ({
  table,
  wire: {
    returnObj: {
      TableFieldList: Object.entries(wire.returnObj[table][0]).map(([name, value]) => ({
        DBSchemaName: "Ice", DBTableName: table, DBFieldName: name,
        DataType: typeof value === "boolean" ? "bit" : "nvarchar",
        FieldFormat: typeof value === "boolean" ? "yes/no" : "x(100)",
      })),
    },
  },
}));
