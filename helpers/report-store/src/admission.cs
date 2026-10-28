// Check the caller and selected company before reading report packages.
if (String.IsNullOrEmpty(iCompany) ||
    !String.Equals(iCompany, Session.CompanyID, StringComparison.Ordinal) ||
    String.IsNullOrEmpty(Session.UserID))
{
    oMessage = "CALLER_COMPANY_MISMATCH";
    return;
}
bool admitted = false;
this.CallService<Ice.Contracts.UserFileSvcContract>(service =>
{
    var ds = service.GetByID(Session.UserID);
    var user = ds.UserFile.SingleOrDefault(row =>
        String.Equals(row.UserID, Session.UserID, StringComparison.Ordinal));
    admitted = user != null && user.SecurityMgr && !user.UserDisabled &&
        ds.UserComp.Any(row =>
            String.Equals(row.Company, iCompany, StringComparison.Ordinal) &&
            String.Equals(row.UserID, Session.UserID, StringComparison.Ordinal));
});
if (!admitted)
{
    oMessage = "SECURITY_MANAGER_REQUIRED";
    return;
}
// The BO is disposed and its rows materialized before opening a Db reader.
