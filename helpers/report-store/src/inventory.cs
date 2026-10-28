if (iPageSize <= 0) { oMessage = "POSITIVE_PAGE_SIZE_REQUIRED"; return; }
// Native Int32 page contract only; no total row limit. Never load Content here.
var query = Db.CustomReportStore.Where(row => row.PrintProgram.StartsWith(rootPrefix));
// This is caller scope, not a custom-content classifier. Stock-named entries
// in the dedicated custom store remain eligible within the same native root.
if (hasTenant) query = query.Where(row => !row.PrintProgram.StartsWith(customPrefix) || row.PrintProgram.StartsWith(tenantPrefix));
else query = query.Where(row => !row.PrintProgram.StartsWith(allTenantsPrefix));
if (iHasCursor) query = query.Where(row => String.Compare(row.PrintProgram, iAfter) > 0);
var rows = query.OrderBy(row => row.PrintProgram).Take(iPageSize)
    .Select(row => new { row.PrintProgram, row.ContentHash, row.SysRowID }).ToList();
var table = new DataTable("Store");
table.Columns.Add("PrintProgram", typeof(String));
table.Columns.Add("ContentHash", typeof(String));
table.Columns.Add("SysRowID", typeof(String));
foreach (var row in rows)
{
    if (!String.Equals(row.PrintProgram, Ice.Lib.Reporting.SsrsPath.CleanFullPath(rootFolder, tenant, row.PrintProgram), StringComparison.OrdinalIgnoreCase))
        throw new InvalidOperationException("CURRENT_PACKAGE_INVENTORY_SCOPE_MISMATCH");
    table.Rows.Add(row.PrintProgram, row.ContentHash, row.SysRowID.ToString());
}
var after = rows.Count == 0 ? "" : rows[rows.Count - 1].PrintProgram;
bool more = rows.Count == iPageSize && query.Any(row => String.Compare(row.PrintProgram, after) > 0);
var cursor = new DataTable("Cursor");
cursor.Columns.Add("More", typeof(Boolean)); cursor.Columns.Add("After", typeof(String));
cursor.Rows.Add(more, after); oDS.Tables.Add(table); oDS.Tables.Add(cursor);
oSuccess = true; oMessage = "OK";
