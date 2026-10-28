// Read a page of deployed layouts through Epicor's native collector.
// ExportSsrsReports is not used: its validation step can deploy stored reports.
var requested = Newtonsoft.Json.JsonConvert.DeserializeObject<String[]>(iPaths);
if (requested == null) { oMessage = "LAYOUT_PATHS_REQUIRED"; return; }
var failures = new System.Collections.Generic.Dictionary<String, String>(StringComparer.OrdinalIgnoreCase);
var layouts = new DataTable("Layouts");
layouts.Columns.Add("Path", typeof(String));
layouts.Columns.Add("Content", typeof(Byte[]));
layouts.Columns.Add("IsSubReport", typeof(Boolean));
var errors = new DataTable("Errors");
errors.Columns.Add("Path", typeof(String));
errors.Columns.Add("Code", typeof(String));

// Use native tenant conversion for roots and every discovered dependency.
// The helper accepts report display paths, not URLs or filesystem paths.
Func<String, String> fullPath = path =>
{
    if (String.IsNullOrWhiteSpace(path)) throw new InvalidOperationException("LAYOUT_PATH_REQUIRED");
    string full = Ice.Lib.Reporting.SsrsPath.DisplayPathToFullPath(rootFolder, tenant, path);
    if (full.Split('/').Any(part => part == ".."))
        throw new InvalidOperationException("LAYOUT_PATH_NOT_RESOLVED");
    string clean = Ice.Lib.Reporting.SsrsPath.CleanFullPath(rootFolder, tenant, full);
    if (!String.Equals(full, clean, StringComparison.OrdinalIgnoreCase) ||
        !full.StartsWith(rootPrefix, StringComparison.OrdinalIgnoreCase))
        throw new InvalidOperationException("LAYOUT_OUTSIDE_CALLER_SCOPE");
    return full;
};
this.CallService<Ice.Contracts.ReportSvcContract>(service =>
{
    var collector = new Ice.Lib.Reporting.SsrsReportCollector(path =>
    {
        if (failures.ContainsKey(path)) throw new InvalidOperationException("LAYOUT_READ_FAILED");
        try
        {
            // CleanFullPath rejects foreign tenant paths before the native read.
            string clean = Ice.Lib.Reporting.SsrsPath.CleanFullPath(rootFolder, tenant, path);
            if (!String.Equals(path, clean, StringComparison.OrdinalIgnoreCase) ||
                path.Split('/').Any(part => part == "..") ||
                !path.StartsWith(rootPrefix, StringComparison.OrdinalIgnoreCase))
                throw new InvalidOperationException("LAYOUT_OUTSIDE_CALLER_SCOPE");
            return service.LoadSsrsRdl(path);
        }
        catch (Exception)
        {
            failures[path] = "LAYOUT_READ_FAILED";
            throw;
        }
    });
    foreach (string path in requested.Distinct(StringComparer.OrdinalIgnoreCase))
    {
        try
        {
            // A missing dependency must not discard the remaining page.
            collector.CollectReport(fullPath(path), false, false);
        }
        catch (Exception)
        {
            failures[path ?? ""] = "LAYOUT_COLLECTION_FAILED";
        }
    }
    foreach (var pair in collector.CollectedReports)
        layouts.Rows.Add(Ice.Lib.Reporting.SsrsPath.FullPathToDisplayPath(pair.Key),
            pair.Value.ReportContents, pair.Value.IsSubReport);
});
foreach (var failure in failures)
{
    // Invalid caller paths also need a failure row. Formatting that row must
    // not discard layouts that the same page read successfully.
    string path = failure.Key;
    try { path = Ice.Lib.Reporting.SsrsPath.FullPathToDisplayPath(path); }
    catch (Exception) { }
    errors.Rows.Add(path, failure.Value);
}
oDS.Tables.Add(layouts);
oDS.Tables.Add(errors);
oSuccess = true;
oMessage = errors.Rows.Count == 0 ? "OK" : "PARTIAL_LAYOUT_PAGE";
