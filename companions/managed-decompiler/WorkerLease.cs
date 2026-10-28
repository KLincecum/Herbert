using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
namespace Herbert.ManagedDecompiler;
// The lease is run-owned host state, never derived from assembly/PDB metadata.
// Windows LockFileEx and Linux flock match fs2's locks in the Rust supervisor.
internal sealed partial class WorkerLease : IDisposable
{
    readonly ConfinedSink root;
    readonly FileStream lease;
    public WorkerLease(LeaseInput input)
    {
        root=new ConfinedSink(input.Root, false);
        try {
            lease=root.OpenExistingLeaf("worker.lock");
            if(OperatingSystem.IsWindows()) {
                var overlapped=new Overlapped();
                if(!LockFileEx(lease.SafeFileHandle,2,0,1,0,ref overlapped)) throw new IOException("Cannot acquire owned worker lease",Marshal.GetLastPInvokeError());
            } else if(Flock(lease.SafeFileHandle.DangerousGetHandle().ToInt32(),2)!=0) throw new IOException("Cannot acquire owned worker lease",Marshal.GetLastPInvokeError());
            using var generation=root.OpenExistingLeaf("generation.txt");
            using var text=new StreamReader(generation);
            if(text.ReadToEnd()!=input.Generation) throw new IOException("This worker generation was superseded before admission.");
        } catch {lease?.Dispose();root.Dispose();throw;}
    }
    public void Dispose(){lease.Dispose();root.Dispose();}
    [StructLayout(LayoutKind.Sequential)] struct Overlapped {public nuint Internal,InternalHigh;public uint Offset,OffsetHigh;public nint Event;}
    [LibraryImport("kernel32.dll",SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)]
    private static partial bool LockFileEx(SafeFileHandle file,uint flags,uint reserved,uint low,uint high,ref Overlapped overlapped);
    [LibraryImport("libc",EntryPoint="flock",SetLastError=true)] private static partial int Flock(int file,int operation);
}
