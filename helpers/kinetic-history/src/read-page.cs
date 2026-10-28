// Read one bounded page. Do not save queries, change versions, or use raw SQL.
oSuccess = false;
oMessage = "";
oJson = "{}";
try
{
    if (iOffset < 0 || iPageSize < 1)
        throw new ArgumentException("The history offset must be nonnegative and the page size must be positive.");
    var requests = Newtonsoft.Json.Linq.JArray.Parse(iRequests);
    if (requests.Count == 0)
        throw new ArgumentException("Select at least one application or layer history identity.");
    var keys = requests.Select(r => (string)r["Key1"]).Distinct().ToArray();
    if (keys.Any(String.IsNullOrEmpty))
        throw new ArgumentException("Each history identity requires its application or layer name.");
    var baseApps = requests.Where(r => String.IsNullOrEmpty((string)r["Key2"]))
        .Select(r => (string)r["Key1"]).Distinct().ToArray();
    var company = Session.CompanyID;
    var tenant = String.IsNullOrEmpty(Session.TenantID) ? "" : "T_" + Session.TenantID;
    // Scope comes from the authenticated session, never from a caller's Company.
    // Candidate keys are paged before exact identity matching. The cursor counts
    // candidates so a filtered row cannot cause a page loop or skipped match.
    var page = Db.UXAppVersion.Where(r =>
        (r.Company == company || r.Company == "" || r.Company == tenant)
        && (keys.Contains(r.Key1) || (r.Key1 == "" && baseApps.Contains(r.Key2))))
        .OrderBy(r => r.SysRowID).Skip(iOffset).Take(iPageSize)
        .Select(r => new { r.Company, r.ProductID, r.TypeCode, r.CGCCode,
            r.Key1, r.Key2, r.Key3, r.Version, r.SysRowID, r.CreatedBy,
            r.CreatedOn, r.PublishDate, r.Comment, r.Content }).ToList();
    var rows = new Newtonsoft.Json.Linq.JArray();
    foreach (var row in page)
    {
        var matches = requests.Any(q =>
            (String.IsNullOrEmpty((string)q["ProductID"]) || (string)q["ProductID"] == row.ProductID)
            && (String.IsNullOrEmpty((string)q["CGCCode"]) || (string)q["CGCCode"] == row.CGCCode)
            && (String.IsNullOrEmpty((string)q["Key2"])
                ? (row.Key1 == (string)q["Key1"] || (row.Key1 == "" && row.Key2 == (string)q["Key1"]))
                : (row.Key1 == (string)q["Key1"] && row.Key2 == (string)q["Key2"]
                    && (String.IsNullOrEmpty((string)q["Key3"]) || row.Key3 == (string)q["Key3"]))));
        if (matches) rows.Add(Newtonsoft.Json.Linq.JObject.FromObject(row));
    }
    var result = new Newtonsoft.Json.Linq.JObject();
    result["rows"] = rows;
    result["offset"] = iOffset;
    result["scanned"] = page.Count;
    result["more"] = page.Count == iPageSize;
    result["nextOffset"] = checked(iOffset + page.Count);
    oJson = result.ToString(Newtonsoft.Json.Formatting.None);
    oSuccess = true;
}
catch (Exception ex)
{
    oMessage = ex.Message;
}
