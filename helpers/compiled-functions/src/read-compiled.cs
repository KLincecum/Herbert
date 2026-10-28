// Read-only Function snapshots. Caller visibility precedes every exact-key read.
// Metadata probes permit same-run retained-byte reuse without another DLL/PDB read.
oSuccess=false;oMessage="";oDS=new System.Data.DataSet("FunctionCompiledCapture");
try{
 if(String.IsNullOrEmpty(iCaller)||!String.Equals(iCaller,Session.UserID,StringComparison.Ordinal)||!String.Equals(iCompany,Session.CompanyID,StringComparison.Ordinal))throw new Exception("CALLER_COMPANY_MISMATCH");
 var requested=Newtonsoft.Json.JsonConvert.DeserializeObject<string[]>(iLibraryIDs);
 if(requested==null||requested.Any(id=>String.IsNullOrWhiteSpace(id)))throw new Exception("Explicit nonempty library identities are required.");
 var ids=requested.Distinct(StringComparer.Ordinal).ToArray();
 var known=Newtonsoft.Json.Linq.JArray.Parse(iKnownCaptures);
 var allowed=new System.Collections.Generic.Dictionary<string,int>(StringComparer.Ordinal);
 this.CallService<Ice.Contracts.EfxLibraryDesignerSvcContract>(svc=>{foreach(var library in svc.GetLibraries(ids).EfxLibrary){if(!ids.Contains(library.LibraryID))throw new Exception("Native owner scope differs.");allowed.Add(library.LibraryID,library.Revision);}});
 if(allowed.Count!=ids.Length)throw new Exception("Selected owners are unavailable to this caller.");
 var ownerByAssembly=new System.Collections.Generic.Dictionary<string,string>(StringComparer.Ordinal);
 foreach(var id in ids){var assembly="efx."+id.ToLowerInvariant();if(ownerByAssembly.ContainsKey(assembly))throw new Exception("Ambiguous native assembly owner.");ownerByAssembly.Add(assembly,id);}
 var names=ownerByAssembly.Keys.ToList();
 var before=Db.CustomizationStore.Where(row=>row.Folder==":Efx"&&names.Contains(row.Assembly)).Select(row=>new{row.Folder,row.Assembly,row.Version,row.Info,row.ModifiedOn,row.ForcePdbLoad,row.SysRowID,row.SysRevID}).ToList();
 var reusable=new System.Collections.Generic.Dictionary<Guid,string>();
 foreach(var row in before){
  if(!String.Equals(row.Folder,":Efx",StringComparison.Ordinal)||!ownerByAssembly.ContainsKey(row.Assembly))continue;
  var owner=ownerByAssembly[row.Assembly];var revision=Convert.ToBase64String(row.SysRevID);
  var match=known.FirstOrDefault(k=>(string)k["libraryId"]==owner&&(string)k["folder"]==row.Folder&&(string)k["assembly"]==row.Assembly&&(string)k["version"]==row.Version&&(string)k["sysRevID"]==revision&&(int?)k["definitionRevision"]==allowed[owner]);
  if(match!=null)reusable.Add(row.SysRowID,(string)match["captureId"]);
 }
 var needed=before.Where(row=>!reusable.ContainsKey(row.SysRowID)).Select(row=>row.SysRowID).ToList();
 var bytes=Db.CustomizationStore.Where(row=>row.Folder==":Efx"&&names.Contains(row.Assembly)&&needed.Contains(row.SysRowID)).Select(row=>new{row.Folder,row.Assembly,row.Version,row.SysRowID,row.SysRevID,row.Data,row.Symbols}).ToList();
 var after=Db.CustomizationStore.Where(row=>row.Folder==":Efx"&&names.Contains(row.Assembly)).Select(row=>new{row.Folder,row.Assembly,row.Version,row.SysRevID}).ToList();
 var revisionsAfter=new System.Collections.Generic.Dictionary<string,int>(StringComparer.Ordinal);
 this.CallService<Ice.Contracts.EfxLibraryDesignerSvcContract>(svc=>{foreach(var library in svc.GetLibraries(ids).EfxLibrary)revisionsAfter.Add(library.LibraryID,library.Revision);});
 var table=new System.Data.DataTable("Assemblies");
 foreach(var name in new[]{"LibraryID","Folder","Assembly","Version","Info","ModifiedOn","SysRowID","SysRevID","BinarySysRevID","AfterSysRevID","SymbolDisposition","ModuleVersionId","AssemblyFullName","ReusedCaptureId"})table.Columns.Add(name,typeof(string));
 table.Columns.Add("DefinitionRevision",typeof(int));table.Columns.Add("DefinitionRevisionAfter",typeof(int));table.Columns.Add("ForcePdbLoad",typeof(bool));table.Columns.Add("Data",typeof(byte[]));table.Columns.Add("Symbols",typeof(byte[]));
 foreach(var row in before.OrderBy(r=>r.Assembly).ThenBy(r=>r.Version)){
  if(!String.Equals(row.Folder,":Efx",StringComparison.Ordinal)||!ownerByAssembly.ContainsKey(row.Assembly))continue;
  var owner=ownerByAssembly[row.Assembly];var reused=reusable.ContainsKey(row.SysRowID);
  var payload=bytes.SingleOrDefault(r=>r.SysRowID==row.SysRowID&&String.Equals(r.Folder,row.Folder,StringComparison.Ordinal)&&String.Equals(r.Assembly,row.Assembly,StringComparison.Ordinal)&&String.Equals(r.Version,row.Version,StringComparison.Ordinal));
  var current=after.SingleOrDefault(r=>String.Equals(r.Folder,row.Folder,StringComparison.Ordinal)&&String.Equals(r.Assembly,row.Assembly,StringComparison.Ordinal)&&String.Equals(r.Version,row.Version,StringComparison.Ordinal));
  byte[] dll=payload==null?null:payload.Data,pdb=payload==null?null:payload.Symbols;
  string disposition=pdb==null?"absent":"unsupported-unused",mvid="",fullName="";
  if(dll!=null){try{
   using(var stream=new System.IO.MemoryStream(dll))using(var pe=new System.Reflection.PortableExecutable.PEReader(stream)){
    var metadata=System.Reflection.Metadata.PEReaderExtensions.GetMetadataReader(pe);mvid=metadata.GetGuid(metadata.GetModuleDefinition().Mvid).ToString();var assembly=metadata.GetAssemblyDefinition();fullName=metadata.GetString(assembly.Name)+", Version="+assembly.Version.ToString();
    if(pdb!=null){try{using(var symbolStream=new System.IO.MemoryStream(pdb))using(var provider=System.Reflection.Metadata.MetadataReaderProvider.FromPortablePdbStream(symbolStream)){
     var symbolMetadata=provider.GetMetadataReader();if(symbolMetadata.DebugMetadataHeader==null)throw new System.BadImageFormatException("The symbol metadata has no portable PDB header.");
     var identity=new System.Reflection.Metadata.BlobContentId(symbolMetadata.DebugMetadataHeader.Id);
     disposition=pe.ReadDebugDirectory().Any(entry=>entry.Type==System.Reflection.PortableExecutable.DebugDirectoryEntryType.CodeView&&entry.Stamp==identity.Stamp&&pe.ReadCodeViewDebugDirectoryData(entry).Guid==identity.Guid&&pe.ReadCodeViewDebugDirectoryData(entry).Age==1)?"matched-portable":"mismatched-unused";
    }}catch(System.BadImageFormatException){disposition="unsupported-unused";}}
    if(disposition!="matched-portable"&&pe.ReadDebugDirectory().Any(entry=>entry.Type==System.Reflection.PortableExecutable.DebugDirectoryEntryType.EmbeddedPortablePdb))disposition+=";embedded-candidate";
   }
  }catch(System.BadImageFormatException){disposition="binary-invalid;"+disposition;}}
  table.Rows.Add(owner,row.Folder,row.Assembly,row.Version,row.Info,row.ModifiedOn.ToString("o",System.Globalization.CultureInfo.InvariantCulture),row.SysRowID.ToString(),Convert.ToBase64String(row.SysRevID),reused?Convert.ToBase64String(row.SysRevID):payload==null?"":Convert.ToBase64String(payload.SysRevID),current==null?"":Convert.ToBase64String(current.SysRevID),disposition,mvid,fullName,reused?reusable[row.SysRowID]:"",allowed[owner],revisionsAfter.ContainsKey(owner)?revisionsAfter[owner]:-1,row.ForcePdbLoad,(object)dll??DBNull.Value,(object)pdb??DBNull.Value);
 }
 oDS.Tables.Add(table);
 var owners=new System.Data.DataTable("Owners");owners.Columns.Add("LibraryID",typeof(string));owners.Columns.Add("DefinitionRevision",typeof(int));owners.Columns.Add("DefinitionRevisionAfter",typeof(int));owners.Columns.Add("BeforeRows",typeof(int));owners.Columns.Add("AfterRows",typeof(int));
 foreach(var item in allowed.OrderBy(x=>x.Key)){var assembly="efx."+item.Key.ToLowerInvariant();owners.Rows.Add(item.Key,item.Value,revisionsAfter.ContainsKey(item.Key)?revisionsAfter[item.Key]:-1,before.Count(row=>row.Folder==":Efx"&&row.Assembly==assembly),after.Count(row=>row.Folder==":Efx"&&row.Assembly==assembly));}oDS.Tables.Add(owners);
 var scope=new System.Data.DataTable("Scope");scope.Columns.Add("Caller",typeof(string));scope.Columns.Add("Company",typeof(string));scope.Columns.Add("DownloadedRows",typeof(int));scope.Columns.Add("ReusedRows",typeof(int));scope.Rows.Add(Session.UserID,Session.CompanyID,bytes.Count,reusable.Count);oDS.Tables.Add(scope);
 oSuccess=true;
}catch(Exception error){oMessage=error.Message;}
