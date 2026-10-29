"""Publish the pinned companion without PowerShell or in-tree obj/bin output."""
from pathlib import Path
import argparse,os,shutil,subprocess,sys,winreg

def main():
 parser=argparse.ArgumentParser();parser.add_argument('--rid',default='win-x64');args=parser.parse_args()
 source=Path(__file__).resolve().parent
 dotnet=os.environ.get('HERBERT_DOTNET') or shutil.which('dotnet')
 if not dotnet:raise SystemExit('Set HERBERT_DOTNET to the .NET SDK dotnet executable or put dotnet on PATH.')
 work=Path(os.environ.get('HERBERT_BUILD_HOME') or Path(os.environ.get('XDG_CACHE_HOME',str(Path.home()/'.cache')))/'herbert')/'managed-decompilation'
 work.mkdir(parents=True,exist_ok=True);env=os.environ.copy()
 for key,name in [('ProgramFiles','ProgramFilesDir'),('ProgramFiles(x86)','ProgramFilesDir (x86)')]:
  if not env.get(key):
   with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE,r'SOFTWARE\Microsoft\Windows\CurrentVersion')as k:env[key]=winreg.QueryValueEx(k,name)[0]
 if not env.get('APPDATA'):
  with winreg.OpenKey(winreg.HKEY_CURRENT_USER,r'Software\Microsoft\Windows\CurrentVersion\Explorer\User Shell Folders')as k:env['APPDATA']=os.path.expandvars(winreg.QueryValueEx(k,'AppData')[0])
 env.setdefault('OS','Windows_NT');env['NUGET_PACKAGES']=str(work/'nuget');env['DOTNET_CLI_TELEMETRY_OPTOUT']='1';env['DOTNET_NOLOGO']='1'
 command=[dotnet,'publish',str(source/'Herbert.ManagedDecompiler.csproj'),'-c','Release','-r',args.rid,'--nologo','-p:BaseIntermediateOutputPath='+str(work/'obj')+'/', '-p:BaseOutputPath='+str(work/'bin')+'/', '-p:TrimmerSingleWarn=false','-o',str(work/'publish'/args.rid)]
 with (work/'build.log').open('w',encoding='utf-8')as log:result=subprocess.run(command,cwd=source,env=env,stdout=log,stderr=subprocess.STDOUT)
 print('AOT publish exit',result.returncode,'log',work/'build.log');return result.returncode
if __name__=='__main__':raise SystemExit(main())
