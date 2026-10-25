using System.ComponentModel;
using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace AvgTranslator.Core.Isolation;

/// <summary>
/// Runs an extension in a Windows less-privileged AppContainer with no capabilities.
/// A job limits memory/lifetime and disallows children. No unrestricted fallback exists.
/// </summary>
public sealed class WindowsAppContainerProcessRunner : IIsolatedProcessRunner
{
    public Task<IsolatedProcessResult> RunAsync(IsolatedProcessRequest request, CancellationToken cancellationToken)
    {
        if (!OperatingSystem.IsWindowsVersionAtLeast(10))
            throw new IsolationUnavailableException("Automatic extensions require Windows 10 or later with AppContainer support.");
        cancellationToken.ThrowIfCancellationRequested();
        return RunOnWindowsAsync(request, cancellationToken);
    }

    [System.Runtime.Versioning.SupportedOSPlatform("windows")]
    private static Task<IsolatedProcessResult> RunOnWindowsAsync(IsolatedProcessRequest request, CancellationToken cancellationToken) =>
        Task.Run(() => Run(request, cancellationToken), cancellationToken);

    [System.Runtime.Versioning.SupportedOSPlatform("windows")]
    private static IsolatedProcessResult Run(IsolatedProcessRequest request, CancellationToken cancellationToken)
    {
        var workspace = Path.GetFullPath(request.WorkspacePath);
        var executable = Path.GetFullPath(request.ExecutablePath);
        if (!Directory.Exists(workspace) || !File.Exists(executable))
            throw new IsolationUnavailableException("The trusted runtime or staged workspace is missing.");
        if (request.MemoryLimitBytes < 64 * 1024 * 1024 || request.MaximumOutputBytes is < 1 or > 16 * 1024 * 1024)
            throw new ArgumentOutOfRangeException(nameof(request));
        WorkspaceFiles.RejectLinkAncestors(workspace);
        var roots = request.ReadOnlyDirectories.Select(Path.GetFullPath).Distinct(StringComparer.OrdinalIgnoreCase).ToArray();
        if (!roots.Any(root => IsWithin(root, executable)))
            throw new IsolationUnavailableException("The executable is outside the explicitly allowed trusted runtime directories.");
        foreach (var root in roots)
        {
            WorkspaceFiles.RejectLinkAncestors(root);
            if (!Directory.Exists(root) || IsWithin(root, workspace) || IsWithin(workspace, root))
                throw new IsolationUnavailableException("The trusted runtime and writable workspace must be separate directories.");
        }

        var profile = "AvgTranslator." + Guid.NewGuid().ToString("N");
        IntPtr sid = IntPtr.Zero;
        IntPtr attributes = IntPtr.Zero;
        var allocations = new List<IntPtr>();
        var grants = new List<(string Path, FileSystemAccessRule Rule)>();
        SafeFileHandle? job = null;
        SafeFileHandle? process = null;
        SafeFileHandle? thread = null;
        var profileCreated = false;
        var stdio = Path.Combine(workspace, ".stdio-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(stdio);
        var inputPath = Path.Combine(stdio, "input.txt");
        var outputPath = Path.Combine(stdio, "output.txt");
        var errorPath = Path.Combine(stdio, "error.txt");
        File.WriteAllText(inputPath, request.StandardInput, new UTF8Encoding(false));
        try
        {
            var hr = Native.CreateAppContainerProfile(profile, profile, "Temporary AVG translator extension", IntPtr.Zero, 0, out sid);
            if (hr < 0) throw new IsolationUnavailableException("Windows could not create the required AppContainer user profile (HRESULT " + hr.ToString("X8")
                + "). Start the application from a normal Windows desktop user session. No generated code was executed.");
            profileCreated = true;
            var principal = new SecurityIdentifier(sid);
            Grant(workspace, principal, FileSystemRights.Modify | FileSystemRights.Synchronize, grants);
            SetLowIntegrity(workspace);
            SetLowIntegrity(stdio);
            SetLowIntegrity(inputPath);
            foreach (var root in roots) Grant(root, principal, FileSystemRights.ReadAndExecute | FileSystemRights.Synchronize, grants);

            using var stdin = OpenInherited(inputPath, 0x80000000, 3);
            using var stdout = OpenInherited(outputPath, 0x40000000, 2);
            using var stderr = OpenInherited(errorPath, 0x40000000, 2);
            job = Native.CreateJobObjectW(IntPtr.Zero, null);
            Check(!job.IsInvalid, "CreateJobObject");
            var limits = new Native.ExtendedLimits
            {
                BasicLimitInformation = new Native.BasicLimits
                {
                    LimitFlags = 0x2000 | 0x8 | 0x100,
                    ActiveProcessLimit = 1
                },
                ProcessMemoryLimit = (UIntPtr)request.MemoryLimitBytes
            };
            var limitPointer = Allocate(limits, allocations);
            Check(Native.SetInformationJobObject(job, 9, limitPointer, (uint)Marshal.SizeOf<Native.ExtendedLimits>()), "SetInformationJobObject limits");
            var uiRestrictions = Allocate(0xFFu, allocations);
            Check(Native.SetInformationJobObject(job, 4, uiRestrictions, sizeof(uint)), "SetInformationJobObject UI restrictions");

            UIntPtr attributeBytes = UIntPtr.Zero;
            Native.InitializeProcThreadAttributeList(IntPtr.Zero, 4, 0, ref attributeBytes);
            attributes = Marshal.AllocHGlobal(checked((int)attributeBytes));
            Check(Native.InitializeProcThreadAttributeList(attributes, 4, 0, ref attributeBytes), "InitializeProcThreadAttributeList");
            var capabilities = Allocate(new Native.SecurityCapabilities { AppContainerSid = sid }, allocations);
            Attribute(attributes, 0x20009, capabilities, Marshal.SizeOf<Native.SecurityCapabilities>());
            var lpac = Allocate(1u, allocations);
            Attribute(attributes, 0x2000F, lpac, sizeof(uint));
            var noChildren = Allocate(1u, allocations);
            Attribute(attributes, 0x2000E, noChildren, sizeof(uint));
            var handles = Marshal.AllocHGlobal(3 * IntPtr.Size);
            allocations.Add(handles);
            Marshal.WriteIntPtr(handles, 0, stdin.DangerousGetHandle());
            Marshal.WriteIntPtr(handles, IntPtr.Size, stdout.DangerousGetHandle());
            Marshal.WriteIntPtr(handles, 2 * IntPtr.Size, stderr.DangerousGetHandle());
            Attribute(attributes, 0x20002, handles, 3 * IntPtr.Size);

            var startup = new Native.StartupInfoEx
            {
                StartupInfo = new Native.StartupInfo
                {
                    cb = Marshal.SizeOf<Native.StartupInfoEx>(),
                    dwFlags = 0x100,
                    hStdInput = stdin.DangerousGetHandle(),
                    hStdOutput = stdout.DangerousGetHandle(),
                    hStdError = stderr.DangerousGetHandle()
                },
                lpAttributeList = attributes
            };
            var environment = ControlledEnvironment(workspace, roots[0]);
            var environmentPointer = Marshal.StringToHGlobalUni(environment);
            allocations.Add(environmentPointer);
            var command = new StringBuilder(Quote(executable));
            foreach (var argument in request.Arguments) command.Append(' ').Append(Quote(argument));
            Check(Native.CreateProcessW(executable, command, IntPtr.Zero, IntPtr.Zero, true,
                0x00080000 | 0x00000400 | 0x00000004 | 0x08000000,
                environmentPointer, workspace, ref startup, out var processInfo), "CreateProcess AppContainer");
            process = new SafeFileHandle(processInfo.hProcess, true);
            thread = new SafeFileHandle(processInfo.hThread, true);
            VerifyAppContainerToken(process);
            Check(Native.AssignProcessToJobObject(job, process), "AssignProcessToJobObject");
            Check(Native.ResumeThread(thread) != uint.MaxValue, "ResumeThread");
            var elapsed = Stopwatch.StartNew();
            var timeout = request.Timeout ?? TimeSpan.FromSeconds(30);
            uint waitResult;
            while ((waitResult = Native.WaitForSingleObject(process, 40)) == 0x102)
            {
                cancellationToken.ThrowIfCancellationRequested();
                if (elapsed.Elapsed >= timeout) throw new IsolatedProcessLimitException("The isolated extension exceeded its execution time limit.");
                if (new FileInfo(outputPath).Length + new FileInfo(errorPath).Length > request.MaximumOutputBytes)
                    throw new IsolatedProcessLimitException("The isolated extension exceeded its diagnostic output limit.");
            }
            Check(waitResult == 0, "WaitForSingleObject");
            cancellationToken.ThrowIfCancellationRequested();
            Check(Native.GetExitCodeProcess(process, out var exitCode), "GetExitCodeProcess");
            stdout.Dispose();
            stderr.Dispose();
            var output = WorkspaceFiles.ReadBounded(workspace, Path.GetRelativePath(workspace, outputPath), request.MaximumOutputBytes);
            var error = WorkspaceFiles.ReadBounded(workspace, Path.GetRelativePath(workspace, errorPath), request.MaximumOutputBytes);
            if ((long)output.Length + error.Length > request.MaximumOutputBytes)
                throw new IsolatedProcessLimitException("The isolated extension exceeded its diagnostic output limit.");
            return new IsolatedProcessResult(unchecked((int)exitCode), Encoding.UTF8.GetString(output), Encoding.UTF8.GetString(error));
        }
        catch (Exception exception) when (exception is Win32Exception or COMException or UnauthorizedAccessException)
        {
            throw new IsolationUnavailableException("Windows could not establish the required extension sandbox. No extension was run without isolation. " + exception.Message, exception);
        }
        finally
        {
            // Also kills a suspended process if assigning it to a job failed.
            if (process is not null && !process.IsInvalid) Native.TerminateProcess(process, 137);
            job?.Dispose();
            thread?.Dispose();
            process?.Dispose();
            if (attributes != IntPtr.Zero)
            {
                Native.DeleteProcThreadAttributeList(attributes);
                Marshal.FreeHGlobal(attributes);
            }
            foreach (var allocation in allocations) Marshal.FreeHGlobal(allocation);
            foreach (var grant in grants.AsEnumerable().Reverse())
            {
                try
                {
                    var directory = new DirectoryInfo(grant.Path);
                    var security = directory.GetAccessControl(AccessControlSections.Access);
                    security.RemoveAccessRuleSpecific(grant.Rule);
                    directory.SetAccessControl(security);
                }
                catch (Exception exception) when (exception is IOException or UnauthorizedAccessException) { /* Unique SID is never reused. */ }
            }
            if (sid != IntPtr.Zero) Native.FreeSid(sid);
            if (profileCreated) Native.DeleteAppContainerProfile(profile);
        }
    }

    private static bool IsWithin(string root, string path) => path.StartsWith(root.TrimEnd(Path.DirectorySeparatorChar) + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase)
        || string.Equals(root.TrimEnd(Path.DirectorySeparatorChar), path.TrimEnd(Path.DirectorySeparatorChar), StringComparison.OrdinalIgnoreCase);

    [System.Runtime.Versioning.SupportedOSPlatform("windows")]
    private static void Grant(string path, SecurityIdentifier sid, FileSystemRights rights, List<(string Path, FileSystemAccessRule Rule)> grants)
    {
        var directory = new DirectoryInfo(path);
        var security = directory.GetAccessControl(AccessControlSections.Access);
        var rule = new FileSystemAccessRule(sid, rights, InheritanceFlags.ContainerInherit | InheritanceFlags.ObjectInherit, PropagationFlags.None, AccessControlType.Allow);
        security.AddAccessRule(rule);
        directory.SetAccessControl(security);
        grants.Add((path, rule));
    }

    private static string ControlledEnvironment(string workspace, string runtimeRoot)
    {
        var windows = Environment.GetFolderPath(Environment.SpecialFolder.Windows);
        return string.Join('\0', new SortedDictionary<string, string>(StringComparer.OrdinalIgnoreCase)
        {
            ["SystemRoot"] = windows, ["WINDIR"] = windows, ["PATH"] = "", ["TEMP"] = workspace, ["TMP"] = workspace,
            ["USERPROFILE"] = workspace, ["LOCALAPPDATA"] = workspace, ["APPDATA"] = workspace,
            ["DOTNET_ROOT"] = runtimeRoot, ["DOTNET_MULTILEVEL_LOOKUP"] = "0", ["DOTNET_EnableDiagnostics"] = "0",
            ["COMPlus_EnableDiagnostics"] = "0", ["DOTNET_CLI_HOME"] = workspace, ["DOTNET_ROLL_FORWARD"] = "Disable",
            ["DOTNET_CLI_TELEMETRY_OPTOUT"] = "1", ["DOTNET_SKIP_FIRST_TIME_EXPERIENCE"] = "1",
            ["NUGET_PACKAGES"] = Path.Combine(workspace, ".packages")
        }.Select(pair => pair.Key + "=" + pair.Value)) + "\0\0";
    }

    private static string Quote(string value)
    {
        if (value.Contains('\0')) throw new ArgumentException("Process arguments cannot contain NUL.");
        var result = new StringBuilder("\"");
        var slashes = 0;
        foreach (var character in value)
        {
            if (character == '\\') { slashes++; continue; }
            if (character == '"') result.Append('\\', slashes * 2 + 1).Append('"');
            else result.Append('\\', slashes).Append(character);
            slashes = 0;
        }
        return result.Append('\\', slashes * 2).Append('"').ToString();
    }

    private static IntPtr Allocate<T>(T value, List<IntPtr> allocations) where T : struct
    {
        var pointer = Marshal.AllocHGlobal(Marshal.SizeOf<T>());
        Marshal.StructureToPtr(value, pointer, false);
        allocations.Add(pointer);
        return pointer;
    }

    private static void Attribute(IntPtr list, nuint attribute, IntPtr value, int size) =>
        Check(Native.UpdateProcThreadAttribute(list, 0, attribute, value, (UIntPtr)size, IntPtr.Zero, IntPtr.Zero), "UpdateProcThreadAttribute");

    private static SafeFileHandle OpenInherited(string path, uint access, uint creation)
    {
        var security = new Native.SecurityAttributes { nLength = Marshal.SizeOf<Native.SecurityAttributes>(), bInheritHandle = true };
        var handle = Native.CreateFileW(path, access, 3, ref security, creation, 0x80, IntPtr.Zero);
        Check(!handle.IsInvalid, "CreateFile stdio");
        return handle;
    }

    private static void SetLowIntegrity(string path)
    {
        if (HasLowIntegrity(path)) return;
        Check(Native.ConvertStringSecurityDescriptorToSecurityDescriptorW("S:(ML;OICI;NW;;;LW)", 1, out var descriptor, out _), "Create low integrity label");
        try
        {
            Check(Native.GetSecurityDescriptorSacl(descriptor, out _, out var sacl, out _), "Read low integrity label");
            var error = Native.SetNamedSecurityInfoW(path, 1, 0x10, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, sacl);
            if (error != 0) throw new Win32Exception((int)error, "Set low integrity label");
        }
        finally { Native.LocalFree(descriptor); }
    }

    internal static void CreateLowIntegrityDirectory(string path)
    {
        if (!OperatingSystem.IsWindows()) throw new IsolationUnavailableException("Windows AppContainer support is required.");
        if (Directory.Exists(path)) return;
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
        Check(Native.ConvertStringSecurityDescriptorToSecurityDescriptorW("S:(ML;OICI;NW;;;LW)", 1, out var descriptor, out _), "Create low integrity descriptor");
        try
        {
            var attributes = new Native.SecurityAttributes { nLength = Marshal.SizeOf<Native.SecurityAttributes>(), lpSecurityDescriptor = descriptor };
            Check(Native.CreateDirectoryW(path, ref attributes), "Create low integrity directory");
        }
        catch (Win32Exception exception) { throw new IsolationUnavailableException("Windows denied creation of a low-integrity extension workspace.", exception); }
        finally { Native.LocalFree(descriptor); }
    }

    private static bool HasLowIntegrity(string path)
    {
        var error = Native.GetNamedSecurityInfoW(path, 1, 0x10, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero, out var sacl, out var descriptor);
        if (error != 0) return false;
        try
        {
            if (sacl == IntPtr.Zero) return false;
            var count = (ushort)Marshal.ReadInt16(sacl, 4);
            for (uint index = 0; index < count; index++)
            {
                if (!Native.GetAce(sacl, index, out var ace) || Marshal.ReadByte(ace) != 0x11) continue;
                var sid = IntPtr.Add(ace, 8);
                var subAuthorities = Marshal.ReadByte(Native.GetSidSubAuthorityCount(sid));
                var rid = (uint)Marshal.ReadInt32(Native.GetSidSubAuthority(sid, (uint)(subAuthorities - 1)));
                if (rid <= 0x1000 && (Marshal.ReadInt32(ace, 4) & 1) != 0) return true;
            }
            return false;
        }
        finally { Native.LocalFree(descriptor); }
    }

    private static void Check(bool result, string operation)
    {
        if (!result) throw new Win32Exception(Marshal.GetLastWin32Error(), operation);
    }

    private static void VerifyAppContainerToken(SafeFileHandle process)
    {
        Check(Native.OpenProcessToken(process, 0x8, out var token), "OpenProcessToken");
        using (token)
        {
            var pointer = Marshal.AllocHGlobal(sizeof(uint));
            try
            {
                Check(Native.GetTokenInformation(token, 29, pointer, sizeof(uint), out _), "Verify AppContainer token");
                if (Marshal.ReadInt32(pointer) != 1)
                    throw new IsolationUnavailableException("Windows returned a process without an AppContainer token.");
                Check(Native.GetTokenInformation(token, 46, pointer, sizeof(uint), out _), "Verify less-privileged AppContainer token");
                if (Marshal.ReadInt32(pointer) != 1)
                    throw new IsolationUnavailableException("Windows returned a process without the required less-privileged AppContainer token.");
            }
            finally { Marshal.FreeHGlobal(pointer); }
        }
    }

    private static class Native
    {
        [StructLayout(LayoutKind.Sequential)] internal struct SecurityCapabilities { public IntPtr AppContainerSid, Capabilities; public uint CapabilityCount, Reserved; }
        [StructLayout(LayoutKind.Sequential)] internal struct SecurityAttributes { public int nLength; public IntPtr lpSecurityDescriptor; [MarshalAs(UnmanagedType.Bool)] public bool bInheritHandle; }
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] internal struct StartupInfo
        {
            public int cb; public string? lpReserved, lpDesktop, lpTitle; public uint dwX, dwY, dwXSize, dwYSize, dwXCountChars, dwYCountChars, dwFillAttribute, dwFlags;
            public ushort wShowWindow, cbReserved2; public IntPtr lpReserved2, hStdInput, hStdOutput, hStdError;
        }
        [StructLayout(LayoutKind.Sequential)] internal struct StartupInfoEx { public StartupInfo StartupInfo; public IntPtr lpAttributeList; }
        [StructLayout(LayoutKind.Sequential)] internal struct ProcessInformation { public IntPtr hProcess, hThread; public uint dwProcessId, dwThreadId; }
        [StructLayout(LayoutKind.Sequential)] internal struct BasicLimits
        {
            public long PerProcessUserTimeLimit, PerJobUserTimeLimit; public uint LimitFlags; public UIntPtr MinimumWorkingSetSize, MaximumWorkingSetSize;
            public uint ActiveProcessLimit; public UIntPtr Affinity; public uint PriorityClass, SchedulingClass;
        }
        [StructLayout(LayoutKind.Sequential)] internal struct IoCounters { public ulong ReadOperationCount, WriteOperationCount, OtherOperationCount, ReadTransferCount, WriteTransferCount, OtherTransferCount; }
        [StructLayout(LayoutKind.Sequential)] internal struct ExtendedLimits
        {
            public BasicLimits BasicLimitInformation; public IoCounters IoInfo; public UIntPtr ProcessMemoryLimit, JobMemoryLimit, PeakProcessMemoryUsed, PeakJobMemoryUsed;
        }
        [DllImport("userenv.dll", CharSet = CharSet.Unicode)] internal static extern int CreateAppContainerProfile(string name, string displayName, string description, IntPtr capabilities, uint capabilityCount, out IntPtr sid);
        [DllImport("userenv.dll", CharSet = CharSet.Unicode)] internal static extern int DeleteAppContainerProfile(string name);
        [DllImport("advapi32.dll")] internal static extern IntPtr FreeSid(IntPtr sid);
        [DllImport("kernel32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool InitializeProcThreadAttributeList(IntPtr list, int count, int flags, ref UIntPtr bytes);
        [DllImport("kernel32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool UpdateProcThreadAttribute(IntPtr list, uint flags, nuint attribute, IntPtr value, UIntPtr size, IntPtr previous, IntPtr returnSize);
        [DllImport("kernel32.dll")] internal static extern void DeleteProcThreadAttributeList(IntPtr list);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool CreateProcessW(string application, StringBuilder command, IntPtr processAttributes, IntPtr threadAttributes, [MarshalAs(UnmanagedType.Bool)] bool inheritHandles, uint flags, IntPtr environment, string directory, ref StartupInfoEx startup, out ProcessInformation process);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] internal static extern SafeFileHandle CreateFileW(string name, uint access, uint share, ref SecurityAttributes security, uint creation, uint flags, IntPtr template);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool CreateDirectoryW(string name, ref SecurityAttributes security);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] internal static extern SafeFileHandle CreateJobObjectW(IntPtr security, string? name);
        [DllImport("kernel32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool SetInformationJobObject(SafeFileHandle job, int informationClass, IntPtr information, uint size);
        [DllImport("kernel32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool AssignProcessToJobObject(SafeFileHandle job, SafeFileHandle process);
        [DllImport("kernel32.dll", SetLastError = true)] internal static extern uint ResumeThread(SafeFileHandle thread);
        [DllImport("kernel32.dll", SetLastError = true)] internal static extern uint WaitForSingleObject(SafeFileHandle handle, uint timeout);
        [DllImport("kernel32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool GetExitCodeProcess(SafeFileHandle process, out uint exitCode);
        [DllImport("kernel32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool TerminateProcess(SafeFileHandle process, uint exitCode);
        [DllImport("advapi32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool OpenProcessToken(SafeFileHandle process, uint access, out SafeFileHandle token);
        [DllImport("advapi32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool GetTokenInformation(SafeFileHandle token, int informationClass, IntPtr information, int size, out int returnSize);
        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool ConvertStringSecurityDescriptorToSecurityDescriptorW(string value, uint revision, out IntPtr descriptor, out uint size);
        [DllImport("advapi32.dll", SetLastError = true)] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool GetSecurityDescriptorSacl(IntPtr descriptor, [MarshalAs(UnmanagedType.Bool)] out bool present, out IntPtr sacl, [MarshalAs(UnmanagedType.Bool)] out bool defaulted);
        [DllImport("advapi32.dll", CharSet = CharSet.Unicode)] internal static extern uint SetNamedSecurityInfoW(string name, int type, uint information, IntPtr owner, IntPtr group, IntPtr dacl, IntPtr sacl);
        [DllImport("advapi32.dll", CharSet = CharSet.Unicode)] internal static extern uint GetNamedSecurityInfoW(string name, int type, uint information, IntPtr owner, IntPtr group, IntPtr dacl, out IntPtr sacl, out IntPtr descriptor);
        [DllImport("advapi32.dll")] [return: MarshalAs(UnmanagedType.Bool)] internal static extern bool GetAce(IntPtr acl, uint index, out IntPtr ace);
        [DllImport("advapi32.dll")] internal static extern IntPtr GetSidSubAuthorityCount(IntPtr sid);
        [DllImport("advapi32.dll")] internal static extern IntPtr GetSidSubAuthority(IntPtr sid, uint index);
        [DllImport("kernel32.dll")] internal static extern IntPtr LocalFree(IntPtr memory);
    }
}
