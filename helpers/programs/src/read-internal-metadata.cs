// Read only hidden registration metadata; ordinary definitions and binaries remain direct.
oSuccess=false;oMessage="";oJson="{}";
try {
 var allowed=new System.Collections.Generic.List<string>();
 this.CallService<Ice.Contracts.ExtensionSvcContract>(svc=>{bool more;var ds=svc.GetList("SystemFlag = false",0,1,out more);if(more)throw new Exception("The complete extension inventory reported more pages.");allowed.AddRange(ds.ExtensionList.Select(row=>row.ExtensionSetID));});
 var roots=Db.ExtensionSet.Where(row=>!row.SystemFlag&&allowed.Contains(row.ExtensionSetID)).Select(row=>new{row.ExtensionSetID,row.SysRowID}).ToList();
 var ids=roots.Select(row=>row.SysRowID).ToList();
 var names=roots.Select(row=>row.ExtensionSetID).ToList();
 var runtime=Db.Extension.Where(row=>ids.Contains(row.ExtensionSetRowID)).Select(row=>new{row.ExtensionSetRowID, row.InterceptType, row.ImplementationType, row.CreatedBy, row.CreatedOn, row.ChangedBy, row.ChangedOn, row.SysRevID, row.SysRowID}).ToList();
 var links=Db.ExtensionDataSetTable.Where(row=>names.Contains(row.ExtensionSetID)&&!row.SystemFlag).Select(row=>new{row.ExtensionSetID, row.SystemCode, row.ParentDataSetID, row.DataTableSystemCode, row.DataTableID, row.IsPrimaryTable, row.SystemFlag, row.CreatedBy, row.CreatedOn, row.ChangedBy, row.ChangedOn, row.SysRevID, row.SysRowID, row.GenIMTable}).ToList();
 var systems=links.Select(row=>row.DataTableSystemCode).Distinct().ToList();
 var namespaces=Db.ZSystem.Where(row=>systems.Contains(row.SystemCode)&&!row.SystemFlag).Select(row=>new{row.SystemCode, row.Description, row.DBSchemaName, row.DataContextType, row.SystemFlag, row.SysRevID, row.SysRowID}).ToList();
 var serviceDatasetFields=Db.ZBODataSets.Where(row=>!row.SystemFlag&&Db.ZBODef.Any(parent=>!parent.SystemFlag&&parent.SystemCode==row.SystemCode&&parent.ClassName==row.ClassName)).Select(row=>new{row.SystemCode,row.ClassName,row.DataSetSystemCode,row.DataSetID,row.SysRowID,row.ObjectNS}).ToList();
 var datasetStoredFields=Db.ZDataSet.Where(row=>!row.SystemFlag).Select(row=>new{row.SystemCode,row.DataSetID,row.SysRowID,row.DataSetName,row.TargetNamespace,row.ProIncDir,row.ProIncName,row.Version,row.XsdTypeName,row.AutoGen,row.VerIncr,row.Generated}).ToList();
 oJson=Newtonsoft.Json.JsonConvert.SerializeObject(new{RuntimeExtension=runtime,ExtensionDataSetTable=links,ZSystem=namespaces,ServiceDatasetFields=serviceDatasetFields,DatasetStoredFields=datasetStoredFields});oSuccess=true;
}catch(Exception error){oMessage=error.Message;}
