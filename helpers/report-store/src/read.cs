// PrintProgram is the exact native stored key from Inventory, never a guessed
// display path. Dedicated CustomReportStore is the custom provenance boundary.
if (String.IsNullOrEmpty(iPrintProgram)) { oMessage = "EXACT_KEY_REQUIRED"; return; }
if (!String.Equals(iPrintProgram, Ice.Lib.Reporting.SsrsPath.CleanFullPath(rootFolder, tenant, iPrintProgram), StringComparison.OrdinalIgnoreCase))
{ oMessage = "CURRENT_PACKAGE_OUTSIDE_CALLER_SCOPE"; return; }
var currentRecord = Db.CustomReportStore.Where(r => r.PrintProgram == iPrintProgram)
    .Select(r => new { r.PrintProgram, r.Content, r.ContentHash, r.CreatedBy, r.CreatedOn,
        r.ModifiedBy, r.ModifiedOn, r.SysRowID, r.SysRevID }).SingleOrDefault();
if (currentRecord == null) { oMessage = "CURRENT_PACKAGE_NOT_FOUND"; return; }
if (!String.Equals(currentRecord.PrintProgram, iPrintProgram, StringComparison.Ordinal))
{ oMessage = "EXACT_KEY_MISMATCH"; return; }
if (currentRecord.Content == null) { oMessage = "CURRENT_PACKAGE_CONTENT_NULL"; return; }
oContent = currentRecord.Content;
string digest;
using (var hash = System.Security.Cryptography.SHA256.Create())
    digest = BitConverter.ToString(hash.ComputeHash(oContent)).Replace("-", "").ToLowerInvariant();
var metadata = new DataTable("Metadata");
foreach (var name in new [] { "PrintProgram", "ContentHash", "CreatedBy", "CreatedOn", "ModifiedBy", "ModifiedOn", "SysRowID", "SysRevID", "SHA256" })
    metadata.Columns.Add(name, typeof(String));
metadata.Columns.Add("Length", typeof(Int64));
Object revision = currentRecord.SysRevID;
DateTime? createdOn = currentRecord.CreatedOn, modifiedOn = currentRecord.ModifiedOn;
metadata.Rows.Add(currentRecord.PrintProgram, currentRecord.ContentHash, currentRecord.CreatedBy, createdOn.HasValue ? (Object)createdOn.Value.ToString("O", System.Globalization.CultureInfo.InvariantCulture) : DBNull.Value,
    currentRecord.ModifiedBy, modifiedOn.HasValue ? (Object)modifiedOn.Value.ToString("O", System.Globalization.CultureInfo.InvariantCulture) : DBNull.Value, currentRecord.SysRowID.ToString(),
    revision is Byte[] ? Convert.ToBase64String((Byte[])revision) : Convert.ToString(revision, System.Globalization.CultureInfo.InvariantCulture), digest, oContent.LongLength);
oDS.Tables.Add(metadata); oSuccess = true; oMessage = "OK";
