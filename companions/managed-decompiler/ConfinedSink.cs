using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;
namespace Herbert.ManagedDecompiler;
// No metadata or PDB path ever reaches this boundary. Files are flat, host-chosen,
// exclusive-create leaves. Root handles prevent directory replacement during writes.
internal sealed partial class ConfinedSink : IDisposable
{
    readonly string root;
    readonly List<SafeFileHandle> pinned = [];
    readonly SafeFileHandle? unixRoot;
    internal readonly List<OutputFile> Files = [];
    public ConfinedSink(string path, bool requireEmpty = true)
    {
        root = System.IO.Path.GetFullPath(path);
        if (!Directory.Exists(root) || (requireEmpty && Directory.EnumerateFileSystemEntries(root).Any()))
            throw new IOException("Output root must be an existing empty host-owned directory.");
        if (OperatingSystem.IsWindows())
        {
            for (var p = root; p != null; p = Directory.GetParent(p)?.FullName)
            {
                var handle = CreateFile(p, 0x80, 3, 0, 3, 0x02200000, 0);
                if (handle.IsInvalid) throw new IOException("Cannot pin output directory", Marshal.GetLastPInvokeError());
                pinned.Add(handle);
                if (!GetFileInformationByHandle(handle, out var info) || (info.Attributes & 0x400) != 0)
                    throw new IOException("Output ancestor is a reparse point or inaccessible.");
            }
        }
        else if (OperatingSystem.IsLinux())
        {
            // Linux O_DIRECTORY|O_NOFOLLOW, followed by root-relative exclusive opens.
            var fd = Open("/", 0x10000 | 0x20000 | 0x80000);
            if (fd < 0) throw new IOException("Cannot pin filesystem root", Marshal.GetLastPInvokeError());
            var current = new SafeFileHandle(fd, true);
            try
            {
                foreach (var component in root.Split('/', StringSplitOptions.RemoveEmptyEntries))
                {
                    var next = OpenAt(current.DangerousGetHandle().ToInt32(), component, 0x10000 | 0x20000 | 0x80000, 0);
                    if (next < 0) throw new IOException("Output ancestor is linked or inaccessible", Marshal.GetLastPInvokeError());
                    current.Dispose(); current = new SafeFileHandle(next, true);
                }
                unixRoot = current;
            }
            catch { current.Dispose(); throw; }
        }
        else throw new PlatformNotSupportedException("Companion filesystem confinement is supported on Windows and Linux only.");
    }
    public void WriteText(string name, string text, string kind, string? logical = null) =>
        Write(name, new MemoryStream(new UTF8Encoding(false).GetBytes(text)), kind, logical);
    public void Write(string name, Stream input, string kind, string? logical = null)
    {
        if (name.Length == 0 || name.Any(c => !(char.IsAsciiLetterOrDigit(c) || c is '.' or '-')) || name.StartsWith('.') || name.EndsWith('.'))
            throw new IOException("Only generated flat filenames are accepted.");
        using var file = OpenLeaf(name);
        input.CopyTo(file);
        file.Flush();
        file.Position = 0;
        Files.Add(new(name, file.Length, Convert.ToHexStringLower(System.Security.Cryptography.SHA256.HashData(file)), kind, logical));
    }
    internal FileStream OpenExistingLeaf(string name) => OpenLeaf(name, false);
    FileStream OpenLeaf(string name, bool create = true)
    {
        SafeFileHandle handle;
        if (OperatingSystem.IsWindows())
            handle = CreateFile(System.IO.Path.Combine(root, name), 0xC0000000, create?0u:3u, 0, create?1u:3u, 0x00200000, 0);
        else
        {
            var fd = OpenAt(unixRoot!.DangerousGetHandle().ToInt32(), name, 2 | (create?(0x40 | 0x80):0) | 0x20000 | 0x80000, 0x180);
            if (fd < 0) throw new IOException("Exclusive output creation failed", Marshal.GetLastPInvokeError());
            handle = new SafeFileHandle(fd, true);
        }
        if (handle.IsInvalid) { handle.Dispose(); throw new IOException("Exclusive output creation failed", Marshal.GetLastPInvokeError()); }
        if (OperatingSystem.IsWindows() && (!GetFileInformationByHandle(handle,out var info) || (info.Attributes & (0x400u|0x10u))!=0)) {handle.Dispose();throw new IOException("Owned file is linked or is a directory.");}
        return new FileStream(handle, FileAccess.ReadWrite);
    }
    public void Dispose() { unixRoot?.Dispose(); foreach (var h in pinned) h.Dispose(); }
    [StructLayout(LayoutKind.Sequential)] struct FileInfo { public uint Attributes; public System.Runtime.InteropServices.ComTypes.FILETIME Creation, Access, Write; public uint Volume, SizeHigh, SizeLow, Links, IndexHigh, IndexLow; }
    [LibraryImport("kernel32.dll", EntryPoint="CreateFileW", StringMarshalling=StringMarshalling.Utf16, SetLastError=true)]
    private static partial SafeFileHandle CreateFile(string path, uint access, uint sharing, nint security, uint disposition, uint flags, nint template);
    [LibraryImport("kernel32.dll", SetLastError=true)] [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool GetFileInformationByHandle(SafeFileHandle handle, out FileInfo info);
    [LibraryImport("libc", EntryPoint="open", StringMarshalling=StringMarshalling.Utf8, SetLastError=true)] private static partial int Open(string path, int flags);
    [LibraryImport("libc", EntryPoint="openat", StringMarshalling=StringMarshalling.Utf8, SetLastError=true)] private static partial int OpenAt(int dir, string name, int flags, int mode);
}
