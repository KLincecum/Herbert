// Read original current packages in one page. Do not create a second ZIP.
// Keep the existing caller, tenant and native-root checks from scope.cs.
if (iPageSize <= 0) { oMessage = "POSITIVE_PAGE_SIZE_REQUIRED"; return; }
var query = Db.CustomReportStore.Where(row => row.PrintProgram.StartsWith(rootPrefix));
if (hasTenant) query = query.Where(row => !row.PrintProgram.StartsWith(customPrefix) || row.PrintProgram.StartsWith(tenantPrefix));
else query = query.Where(row => !row.PrintProgram.StartsWith(allTenantsPrefix));
if (iHasCursor) query = query.Where(row => String.Compare(row.PrintProgram, iAfter) > 0);
var rows = query.OrderBy(row => row.PrintProgram).Take(iPageSize)
    .Select(row => new { row.PrintProgram, row.Content, row.ContentHash,
        row.CreatedBy, row.CreatedOn, row.ModifiedBy, row.ModifiedOn,
        row.SysRowID, row.SysRevID }).ToList();
var table = new DataTable("Store");
foreach (var name in new [] { "PrintProgram", "Content", "ContentHash", "CreatedBy", "CreatedOn", "ModifiedBy", "ModifiedOn", "SysRowID", "SysRevID", "SHA256" })
    table.Columns.Add(name, typeof(String));
table.Columns.Add("Length", typeof(Int64));
foreach (var row in rows)
{
    if (!String.Equals(row.PrintProgram, Ice.Lib.Reporting.SsrsPath.CleanFullPath(rootFolder, tenant, row.PrintProgram), StringComparison.OrdinalIgnoreCase))
        throw new InvalidOperationException("CURRENT_PACKAGE_SCOPE_MISMATCH");
    if (row.Content == null) throw new InvalidOperationException("CURRENT_PACKAGE_CONTENT_NULL");
    string digest;
    using (var hash = System.Security.Cryptography.SHA256.Create())
        digest = BitConverter.ToString(hash.ComputeHash(row.Content)).Replace("-", "").ToLowerInvariant();
    Object revision = row.SysRevID;
    DateTime? createdOn = row.CreatedOn, modifiedOn = row.ModifiedOn;
    table.Rows.Add(row.PrintProgram, Convert.ToBase64String(row.Content), row.ContentHash, row.CreatedBy,
        createdOn.HasValue ? (Object)createdOn.Value.ToString("O", System.Globalization.CultureInfo.InvariantCulture) : DBNull.Value,
        row.ModifiedBy, modifiedOn.HasValue ? (Object)modifiedOn.Value.ToString("O", System.Globalization.CultureInfo.InvariantCulture) : DBNull.Value,
        row.SysRowID.ToString(), revision is Byte[] ? Convert.ToBase64String((Byte[])revision) : Convert.ToString(revision, System.Globalization.CultureInfo.InvariantCulture), digest, row.Content.LongLength);
}
string after = rows.Count == 0 ? "" : rows[rows.Count - 1].PrintProgram;
bool more = rows.Count == iPageSize && query.Any(row => String.Compare(row.PrintProgram, after) > 0);
var cursor = new DataTable("Cursor");
cursor.Columns.Add("More", typeof(Boolean)); cursor.Columns.Add("After", typeof(String));
cursor.Rows.Add(more, after); oDS.Tables.Add(table); oDS.Tables.Add(cursor);
oSuccess = true; oMessage = "OK";
