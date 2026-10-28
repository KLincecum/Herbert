// ReportSvc.SaveCustomReportToDB constructs its key with this exact native
// root and tenant conversion. GetRootFolder reads SSRSRootFolder only; passing
// null avoids fetching unrelated SysCompany configuration/credential fields.
string rootFolder = new Ice.Core.SsrsReporting.SsrsConfigurationInformation(null).GetRootFolder();
string tenant = Session.TenantID;
string rootPrefix = Ice.Lib.Reporting.SsrsPath.DisplayPathToFullPath(rootFolder, "", "reports/");
string customPrefix = Ice.Lib.Reporting.SsrsPath.DisplayPathToFullPath(rootFolder, "", "reports/CustomReports/");
string tenantPrefix = Ice.Lib.Reporting.SsrsPath.DisplayPathToFullPath(rootFolder, tenant, "reports/CustomReports/");
string allTenantsPrefix = customPrefix + "Tenants/";
bool hasTenant = !String.IsNullOrWhiteSpace(tenant);
