// Dictionary overrides are global. Read each source once and retain native keys.
// Do not return shipped values when only a custom override belongs in the output.
oSuccess = false;
oMessage = "";
oJson = "[]";
try
{
    var tables = new System.Collections.Generic.Dictionary<string, Newtonsoft.Json.Linq.JObject>(
        StringComparer.OrdinalIgnoreCase);
    Action<string, string, Newtonsoft.Json.Linq.JObject, string> add =
        (system, table, item, section) =>
        {
            var key = system + "\u0000" + table;
            Newtonsoft.Json.Linq.JObject parent;
            if (!tables.TryGetValue(key, out parent))
            {
                parent = new Newtonsoft.Json.Linq.JObject();
                parent["SystemCode"] = system;
                parent["DataTableID"] = table;
                parent["ZDataField"] = new Newtonsoft.Json.Linq.JArray();
                parent["ZColumnProperty"] = new Newtonsoft.Json.Linq.JArray();
                tables.Add(key, parent);
            }
            ((Newtonsoft.Json.Linq.JArray)parent[section]).Add(item);
        };

    var fields = Db.ZDataField.Where(row =>
        !String.IsNullOrEmpty(row.UDLikeDataFieldSystemCode)
        || !String.IsNullOrEmpty(row.UDLikeDataFieldTableID)
        || !String.IsNullOrEmpty(row.UDLikeDataFieldName)
        || !String.IsNullOrEmpty(row.UDFieldFormat)
        || !String.IsNullOrEmpty(row.UDFieldLabel)
        || !String.IsNullOrEmpty(row.UDInitialValue)
        || !String.IsNullOrEmpty(row.UDCurrencyCodeColumn)
        || !String.IsNullOrEmpty(row.UDCurrencyType)
        || !String.IsNullOrEmpty(row.UDCurrencySource)
        || !String.IsNullOrEmpty(row.UDPredictiveSearchID)
        || row.UDRequired || row.UDReadOnly)
        .Select(row => new
        {
            row.SystemCode, row.DataTableID, row.FieldName,
            row.UDLikeDataFieldSystemCode, row.UDLikeDataFieldTableID, row.UDLikeDataFieldName,
            row.UDFieldFormat, row.UDFieldLabel, row.UDInitialValue,
            row.UDCurrencyCodeColumn, row.UDCurrencyType, row.UDCurrencySource,
            row.UDPredictiveSearchID, row.UDRequired, row.UDReadOnly
        }).ToList();
    foreach (var row in fields)
    {
        var item = new Newtonsoft.Json.Linq.JObject();
        item["SystemCode"] = row.SystemCode;
        item["DataTableID"] = row.DataTableID;
        item["FieldName"] = row.FieldName;
        if (!String.IsNullOrEmpty(row.UDLikeDataFieldSystemCode))
            item["UDLikeDataFieldSystemCode"] = row.UDLikeDataFieldSystemCode;
        if (!String.IsNullOrEmpty(row.UDLikeDataFieldTableID))
            item["UDLikeDataFieldTableID"] = row.UDLikeDataFieldTableID;
        if (!String.IsNullOrEmpty(row.UDLikeDataFieldName))
            item["UDLikeDataFieldName"] = row.UDLikeDataFieldName;
        if (!String.IsNullOrEmpty(row.UDFieldFormat))
            item["UDFieldFormat"] = row.UDFieldFormat;
        if (!String.IsNullOrEmpty(row.UDFieldLabel))
            item["UDFieldLabel"] = row.UDFieldLabel;
        if (!String.IsNullOrEmpty(row.UDInitialValue))
            item["UDInitialValue"] = row.UDInitialValue;
        if (!String.IsNullOrEmpty(row.UDCurrencyCodeColumn))
            item["UDCurrencyCodeColumn"] = row.UDCurrencyCodeColumn;
        if (!String.IsNullOrEmpty(row.UDCurrencyType))
            item["UDCurrencyType"] = row.UDCurrencyType;
        if (!String.IsNullOrEmpty(row.UDCurrencySource))
            item["UDCurrencySource"] = row.UDCurrencySource;
        if (!String.IsNullOrEmpty(row.UDPredictiveSearchID))
            item["UDPredictiveSearchID"] = row.UDPredictiveSearchID;
        if (row.UDRequired) item["UDRequired"] = true;
        if (row.UDReadOnly) item["UDReadOnly"] = true;
        add(row.SystemCode, row.DataTableID, item, "ZDataField");
    }

    var properties = Db.ZColumnProperty
        .Where(row => !row.SystemFlag || row.OverrideValue != null)
        .Select(row => new
        {
            row.SystemCode, row.DataTableID, row.FieldName, row.PropertyID,
            row.SystemFlag, row.PropertyValue, row.OverrideValue
        }).ToList();
    foreach (var row in properties)
    {
        var item = new Newtonsoft.Json.Linq.JObject();
        item["SystemCode"] = row.SystemCode;
        item["DataTableID"] = row.DataTableID;
        item["FieldName"] = row.FieldName;
        item["PropertyID"] = row.PropertyID;
        if (!row.SystemFlag) item["PropertyValue"] = row.PropertyValue;
        // Empty is an explicit override. Only null means no override.
        if (row.OverrideValue != null) item["OverrideValue"] = row.OverrideValue;
        add(row.SystemCode, row.DataTableID, item, "ZColumnProperty");
    }

    oJson = new Newtonsoft.Json.Linq.JArray(tables.Values
        .OrderBy(row => (string)row["SystemCode"])
        .ThenBy(row => (string)row["DataTableID"]))
        .ToString(Newtonsoft.Json.Formatting.None);
    oSuccess = true;
}
catch (Exception error)
{
    oMessage = error.Message;
}
