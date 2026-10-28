// Read-only, caller-visible UBAQ method snapshots. No compilation or target execution.
oSuccess=false;oMessage="";oDS=new System.Data.DataSet("UbaqCompiledCapture");
try {
 if(String.IsNullOrEmpty(iCaller)||!String.Equals(iCaller,Session.UserID,StringComparison.Ordinal)||!String.Equals(iCompany,Session.CompanyID,StringComparison.Ordinal))throw new Exception("CALLER_COMPANY_MISMATCH");
 var requested=Newtonsoft.Json.Linq.JArray.Parse(iQueryIDs);
 var keys=new System.Collections.Generic.HashSet<string>(StringComparer.Ordinal);
 var ownerKey=new Func<string,string,string>((company,query)=>Newtonsoft.Json.JsonConvert.SerializeObject(new[]{company,query}));
 var rowIds=new System.Collections.Generic.List<Guid>();
 foreach(var item in requested){var company=(string)item["company"];var query=(string)item["query"];if(company==null||String.IsNullOrEmpty(query)||(company!=""&&company!=Session.CompanyID)||!keys.Add(ownerKey(company,query)))throw new Exception("Invalid or repeated exact query owner.");}
 foreach(var item in requested){Guid id;if(!Guid.TryParse((string)item["rowId"],out id)||id==Guid.Empty)throw new Exception("Exact native query row identities are required.");rowIds.Add(id);}
 if(keys.Count==0)throw new Exception("Explicit query owners are required.");
 Ice.Tablesets.BAQDesignerTableset visible=null;
 this.CallService<Ice.Contracts.BAQDesignerSvcContract>(svc=>{visible=svc.GetBySysRowIDs(rowIds);});
 if(visible.DynamicQueryDesigner.Count!=keys.Count||visible.DynamicQueryDesigner.Any(row=>!keys.Contains(ownerKey(row.Company,row.QueryID))||!row.Updatable||row.SystemFlag))throw new Exception("Selected queries are unavailable to this caller.");
 Ice.Tablesets.BpMethodTableset methods=null;
 this.CallService<Ice.Contracts.BpMethodSvcContract>(svc=>{bool more;methods=svc.GetRowsEx("DQ","","",0,0,out more);if(more)throw new Exception("Native method visibility was incomplete.");});
 var ownerByMethod=new System.Collections.Generic.Dictionary<string,string>(StringComparer.Ordinal);
 foreach(var argument in methods.BpArgument.Where(row=>row.Source=="DQ"&&!String.IsNullOrEmpty(row.TypeInfo))){
  Newtonsoft.Json.Linq.JObject info;try{info=Newtonsoft.Json.Linq.JObject.Parse(argument.TypeInfo);}catch(Newtonsoft.Json.JsonReaderException){continue;}
  var company=(string)info["company"];var query=(string)info["query"];if(company==null||query==null)continue;
  var key=ownerKey(company,query);
  if(ownerByMethod.ContainsKey(argument.BpMethodCode)&&ownerByMethod[argument.BpMethodCode]!=key)throw new Exception("Ambiguous authored method owner.");
  ownerByMethod[argument.BpMethodCode]=key;
 }
 var selected=methods.BpMethod.Where(row=>row.Source=="DQ"&&ownerByMethod.ContainsKey(row.BpMethodCode)&&keys.Contains(ownerByMethod[row.BpMethodCode])).ToList();
 var codes=selected.Select(row=>row.BpMethodCode).ToList();
 var assemblyByMethod=selected.ToDictionary(row=>row.BpMethodCode,row=>"Ice.Ubaq."+row.BusinessObject.Replace('/','_').Replace('_','.')+"."+row.Name,StringComparer.Ordinal);
 var collisions=methods.BpMethod.Where(row=>row.Source=="DQ").GroupBy(row=>"Ice.Ubaq."+row.BusinessObject.Replace('/','_').Replace('_','.')+"."+row.Name,StringComparer.OrdinalIgnoreCase).Where(group=>group.Select(row=>row.BpMethodCode).Distinct(StringComparer.OrdinalIgnoreCase).Count()>1).Select(group=>group.Key).ToList();
 if(assemblyByMethod.Values.Any(name=>collisions.Contains(name,StringComparer.OrdinalIgnoreCase)))throw new Exception("Ambiguous native assembly owner.");
 var names=assemblyByMethod.Values.Distinct(StringComparer.Ordinal).ToList();
 // Match all persisted directive IDs to the caller's native rows. A hidden
 // directive must not become readable through its method's compiled assembly.
 var storedDirectiveIds=Db.BpDirective.Where(row=>row.Source=="DQ"&&codes.Contains(row.BpMethodCode)).Select(row=>new{row.BpMethodCode,row.DirectiveID}).ToList();
 foreach(var method in selected){
  var nativeIds=methods.BpDirective.Where(row=>row.Source=="DQ"&&row.BpMethodCode==method.BpMethodCode).Select(row=>row.DirectiveID).OrderBy(id=>id).ToArray();
  var storedIds=storedDirectiveIds.Where(row=>row.BpMethodCode==method.BpMethodCode).Select(row=>row.DirectiveID).OrderBy(id=>id).ToArray();
  if(!nativeIds.SequenceEqual(storedIds)||methods.BpDirective.Any(row=>row.Source=="DQ"&&row.BpMethodCode==method.BpMethodCode&&row.IsEnabled&&String.IsNullOrEmpty(row.Body)))throw new Exception("Compiled directives are not fully visible to this caller.");
 }
 var registrations=Db.Customization.Where(row=>row.Type=="BPM.Ubaq"&&(codes.Contains(row.Target)||names.Contains(row.Assembly))).Select(row=>new{row.Type,row.Target,row.Scope,row.Project,row.Disabled,row.Revision,row.Assembly,row.Version,row.TypeName,row.SysRowID,row.SysRevID}).ToList();
 foreach(var registration in registrations){if(!codes.Contains(registration.Target)||(!String.IsNullOrEmpty(registration.Assembly)&&registration.Assembly!=assemblyByMethod[registration.Target])||registration.Scope!="##UBAQ##")throw new Exception("Native runtime association differs or is ambiguous.");}
 var before=Db.CustomizationStore.Where(row=>row.Folder=="Ubaq"&&names.Contains(row.Assembly)).Select(row=>new{row.Folder,row.Assembly,row.Version,row.Info,row.ModifiedOn,row.ForcePdbLoad,row.SysRowID,row.SysRevID}).ToList();
 var payloads=Db.CustomizationStore.Where(row=>row.Folder=="Ubaq"&&names.Contains(row.Assembly)).Select(row=>new{row.Folder,row.Assembly,row.Version,row.SysRowID,row.SysRevID,row.Data,row.Symbols}).ToList();
 var after=Db.CustomizationStore.Where(row=>row.Folder=="Ubaq"&&names.Contains(row.Assembly)).Select(row=>new{row.Folder,row.Assembly,row.Version,row.SysRevID}).ToList();
 var registrationsAfter=Db.Customization.Where(row=>row.Type=="BPM.Ubaq"&&(codes.Contains(row.Target)||names.Contains(row.Assembly))).Select(row=>new{row.Type,row.Target,row.Scope,row.Project,row.Disabled,row.Revision,row.Assembly,row.Version,row.TypeName,row.SysRowID,row.SysRevID}).ToList();
 Ice.Tablesets.BpMethodTableset methodsAfter=null;
 this.CallService<Ice.Contracts.BpMethodSvcContract>(svc=>{bool more;methodsAfter=svc.GetRowsEx("DQ","","",0,0,out more);if(more)throw new Exception("Native method recheck was incomplete.");});
 Ice.Tablesets.BAQDesignerTableset visibleAfter=null;
 this.CallService<Ice.Contracts.BAQDesignerSvcContract>(svc=>{visibleAfter=svc.GetBySysRowIDs(rowIds);});
 var witness=new Func<Ice.Tablesets.BpMethodTableset,string,string>((ds,owner)=>{
  var owned=new System.Collections.Generic.List<string>();
  foreach(var argument in ds.BpArgument.Where(row=>row.Source=="DQ"&&!String.IsNullOrEmpty(row.TypeInfo))){Newtonsoft.Json.Linq.JObject info;try{info=Newtonsoft.Json.Linq.JObject.Parse(argument.TypeInfo);}catch(Newtonsoft.Json.JsonReaderException){continue;}var company=(string)info["company"];var query=(string)info["query"];if(company!=null&&query!=null&&ownerKey(company,query)==owner&&!owned.Contains(argument.BpMethodCode))owned.Add(argument.BpMethodCode); }
  var rows=ds.BpMethod.Where(row=>row.Source=="DQ"&&owned.Contains(row.BpMethodCode)).Select(row=>"M|"+row.BpMethodCode+"|"+row.SysRowID.ToString().ToLowerInvariant()+"|"+row.SysRevID.ToString(System.Globalization.CultureInfo.InvariantCulture))
   .Concat(ds.BpArgument.Where(row=>row.Source=="DQ"&&owned.Contains(row.BpMethodCode)).Select(row=>"A|"+row.BpMethodCode+"|"+row.SysRowID.ToString().ToLowerInvariant()+"|"+row.SysRevID.ToString(System.Globalization.CultureInfo.InvariantCulture)))
   .Concat(ds.BpDirective.Where(row=>row.Source=="DQ"&&owned.Contains(row.BpMethodCode)).Select(row=>"D|"+row.BpMethodCode+"|"+row.SysRowID.ToString().ToLowerInvariant()+"|"+row.SysRevID.ToString(System.Globalization.CultureInfo.InvariantCulture))).OrderBy(row=>row,StringComparer.Ordinal);
  return Newtonsoft.Json.JsonConvert.SerializeObject(rows);
 });
 var owners=new System.Data.DataTable("Owners");foreach(var name in new[]{"Company","QueryID","DefinitionWitness","DefinitionWitnessAfter","QuerySysRevID","QuerySysRevIDAfter"})owners.Columns.Add(name,typeof(string));owners.Columns.Add("Methods",typeof(int));
 foreach(var query in visible.DynamicQueryDesigner){var key=ownerKey(query.Company,query.QueryID);var current=visibleAfter.DynamicQueryDesigner.SingleOrDefault(row=>row.Company==query.Company&&row.QueryID==query.QueryID);owners.Rows.Add(query.Company,query.QueryID,witness(methods,key),witness(methodsAfter,key),query.SysRevID.ToString(System.Globalization.CultureInfo.InvariantCulture),current==null?"":current.SysRevID.ToString(System.Globalization.CultureInfo.InvariantCulture),selected.Count(row=>ownerByMethod[row.BpMethodCode]==key));}oDS.Tables.Add(owners);
 var methodTable=new System.Data.DataTable("Methods");foreach(var name in new[]{"Company","QueryID","BpMethodCode","Method","Folder","Assembly","Registration","RegistrationAfter"})methodTable.Columns.Add(name,typeof(string));methodTable.Columns.Add("BeforeRows",typeof(int));methodTable.Columns.Add("AfterRows",typeof(int));
 foreach(var method in selected){var key=Newtonsoft.Json.JsonConvert.DeserializeObject<string[]>(ownerByMethod[method.BpMethodCode]);var name=assemblyByMethod[method.BpMethodCode];methodTable.Rows.Add(key[0],key[1],method.BpMethodCode,method.Name,"Ubaq",name,Newtonsoft.Json.JsonConvert.SerializeObject(registrations.Where(row=>row.Target==method.BpMethodCode)),Newtonsoft.Json.JsonConvert.SerializeObject(registrationsAfter.Where(row=>row.Target==method.BpMethodCode)),before.Count(row=>row.Folder=="Ubaq"&&row.Assembly==name),after.Count(row=>row.Folder=="Ubaq"&&row.Assembly==name));}oDS.Tables.Add(methodTable);
 var table=new System.Data.DataTable("Assemblies");foreach(var name in new[]{"Company","QueryID","BpMethodCode","Folder","Assembly","Version","Info","ModifiedOn","SysRowID","SysRevID","BinarySysRevID","AfterSysRevID","SymbolDisposition","ModuleVersionId","AssemblyFullName"})table.Columns.Add(name,typeof(string));table.Columns.Add("ForcePdbLoad",typeof(bool));table.Columns.Add("Data",typeof(byte[]));table.Columns.Add("Symbols",typeof(byte[]));
 foreach(var row in before.OrderBy(row=>row.Assembly,StringComparer.Ordinal).ThenBy(row=>row.Version,StringComparer.Ordinal)){
  if(row.Folder!="Ubaq"||!names.Contains(row.Assembly,StringComparer.Ordinal))throw new Exception("Exact compiled selector differs.");
  var method=selected.Single(item=>assemblyByMethod[item.BpMethodCode]==row.Assembly);var key=Newtonsoft.Json.JsonConvert.DeserializeObject<string[]>(ownerByMethod[method.BpMethodCode]);
  var payload=payloads.SingleOrDefault(item=>item.SysRowID==row.SysRowID&&item.Folder==row.Folder&&item.Assembly==row.Assembly&&item.Version==row.Version);var current=after.SingleOrDefault(item=>item.Folder==row.Folder&&item.Assembly==row.Assembly&&item.Version==row.Version);
  byte[] dll=payload==null?null:payload.Data,pdb=payload==null?null:payload.Symbols;
  string disposition=pdb==null?"absent":"unsupported-unused",mvid="",fullName="";
  if(dll!=null){try{
   using(var stream=new System.IO.MemoryStream(dll))using(var pe=new System.Reflection.PortableExecutable.PEReader(stream)){
    var metadata=System.Reflection.Metadata.PEReaderExtensions.GetMetadataReader(pe);mvid=metadata.GetGuid(metadata.GetModuleDefinition().Mvid).ToString();var assembly=metadata.GetAssemblyDefinition();fullName=metadata.GetString(assembly.Name)+", Version="+assembly.Version.ToString();
    if(pdb!=null){try{using(var symbolStream=new System.IO.MemoryStream(pdb))using(var provider=System.Reflection.Metadata.MetadataReaderProvider.FromPortablePdbStream(symbolStream)){
     var symbolMetadata=provider.GetMetadataReader();if(symbolMetadata.DebugMetadataHeader==null)throw new System.BadImageFormatException("The symbol metadata has no portable PDB header.");var identity=new System.Reflection.Metadata.BlobContentId(symbolMetadata.DebugMetadataHeader.Id);
     disposition=pe.ReadDebugDirectory().Any(entry=>entry.Type==System.Reflection.PortableExecutable.DebugDirectoryEntryType.CodeView&&entry.Stamp==identity.Stamp&&pe.ReadCodeViewDebugDirectoryData(entry).Guid==identity.Guid&&pe.ReadCodeViewDebugDirectoryData(entry).Age==1)?"matched-portable":"mismatched-unused";
    }}catch(System.BadImageFormatException){disposition="unsupported-unused";}}
    if(disposition!="matched-portable"&&pe.ReadDebugDirectory().Any(entry=>entry.Type==System.Reflection.PortableExecutable.DebugDirectoryEntryType.EmbeddedPortablePdb))disposition+=";embedded-candidate";
   }
  }catch(System.BadImageFormatException){disposition="binary-invalid;"+disposition;}}
  table.Rows.Add(key[0],key[1],method.BpMethodCode,row.Folder,row.Assembly,row.Version,row.Info,row.ModifiedOn.ToString("o",System.Globalization.CultureInfo.InvariantCulture),row.SysRowID.ToString(),Convert.ToBase64String(row.SysRevID),payload==null?"":Convert.ToBase64String(payload.SysRevID),current==null?"":Convert.ToBase64String(current.SysRevID),disposition,mvid,fullName,row.ForcePdbLoad,(object)dll??DBNull.Value,(object)pdb??DBNull.Value);
 }oDS.Tables.Add(table);
 var scope=new System.Data.DataTable("Scope");scope.Columns.Add("Caller",typeof(string));scope.Columns.Add("Company",typeof(string));scope.Rows.Add(Session.UserID,Session.CompanyID);oDS.Tables.Add(scope);oSuccess=true;
}catch(Exception error){oDS=new System.Data.DataSet("UbaqCompiledCapture");oMessage=error.Message;}
