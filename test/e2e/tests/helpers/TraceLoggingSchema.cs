using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;

namespace ItE2E
{
    // x64 Windows EVENT_TRACE_LOGFILEW and TRACE_EVENT_INFO offsets from evntrace.h/tdh.h.
    // TDH understands self-describing TraceLogging metadata even when tracerpt -export
    // produces an empty manifest. Event values remain decoded by Windows tracerpt.
    public static class TraceLoggingSchema
    {
        public sealed class Schema
        {
            public string Provider { get; set; }
            public string Name { get; set; }
            public int ProcessId { get; set; }
            public Dictionary<string, string> Types { get; set; }
        }

        [UnmanagedFunctionPointer(CallingConvention.Winapi)]
        private delegate void EventCallback(IntPtr record);
        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, EntryPoint = "OpenTraceW", SetLastError = true)]
        private static extern ulong OpenTrace(IntPtr logfile);
        [DllImport("advapi32.dll")]
        private static extern uint ProcessTrace(ulong[] handles, uint count, IntPtr start, IntPtr end);
        [DllImport("advapi32.dll")]
        private static extern uint CloseTrace(ulong handle);
        [DllImport("tdh.dll")]
        private static extern uint TdhGetEventInformation(IntPtr record, uint contextCount, IntPtr context, IntPtr buffer, ref uint size);

        private static string Text(IntPtr buffer, int offset)
        {
            int position = Marshal.ReadInt32(buffer, offset);
            return position == 0 ? "" : Marshal.PtrToStringUni(IntPtr.Add(buffer, position));
        }

        private static string TypeName(ushort type)
        {
            if (type == 300) return "tdh:CountedUnicodeString";
            if (type == 301) return "tdh:CountedAnsiString";
            string[] types = {
                "Null", "UnicodeString", "AnsiString", "Int8", "UInt8", "Int16", "UInt16",
                "Int32", "UInt32", "Int64", "UInt64", "Float", "Double", "Boolean",
                "Binary", "GUID", "Pointer", "FILETIME", "SYSTEMTIME", "SID", "HexInt32", "HexInt64"
            };
            return type < types.Length ? "win:" + types[type] : "tdh:" + type;
        }

        public static Schema[] Read(string path)
        {
            if (IntPtr.Size != 8) throw new PlatformNotSupportedException("ETW decoding requires x64 PowerShell.");
            var result = new List<Schema>();
            var seen = new HashSet<string>();
            Exception failure = null;
            EventCallback callback = record =>
            {
                if (failure != null) return;
                IntPtr buffer = IntPtr.Zero;
                try
                {
                    uint size = 0;
                    uint status = TdhGetEventInformation(record, 0, IntPtr.Zero, IntPtr.Zero, ref size);
                    if (status != 122) return; // Non-TraceLogging ETL bookkeeping records.
                    buffer = Marshal.AllocHGlobal(checked((int)size));
                    status = TdhGetEventInformation(record, 0, IntPtr.Zero, buffer, ref size);
                    if (status != 0) throw new Win32Exception((int)status);
                    if (Marshal.ReadInt32(buffer, 48) != 3) return; // DecodingSourceTlg.
                    string name = Text(buffer, 92);
                    byte[] guid = new byte[16];
                    Marshal.Copy(IntPtr.Add(buffer, 0), guid, 0, 16);
                    string provider = new Guid(guid).ToString();
                    // Only the inherited interaction event belongs to this funnel;
                    // other Win32Host events can contain structured diagnostic data.
                    if (provider == "56c06166-2e2e-5f4d-7ff3-74f4b78c87d6" && name != "SessionBecameInteractive") return;
                    int processId = Marshal.ReadInt32(record, 12);
                    int count = Marshal.ReadInt32(buffer, 104);
                    var types = new Dictionary<string, string>();
                    for (int i = 0; i < count; i++)
                    {
                        int property = 112 + 24 * i;
                        int flags = Marshal.ReadInt32(buffer, property);
                        if ((flags & 1) != 0) throw new NotSupportedException("Structured telemetry fields require explicit decoding: " + provider + "/" + name);
                        string field = Text(buffer, property + 4);
                        ushort inputType = unchecked((ushort)Marshal.ReadInt16(buffer, property + 8));
                        types.Add(field, TypeName(inputType));
                    }
                    string key = provider + "|" + name + "|" + processId + "|" +
                        string.Join("|", types.Keys) + "|" + string.Join("|", types.Values);
                    if (seen.Add(key)) result.Add(new Schema { Provider = provider, Name = name, ProcessId = processId, Types = types });
                }
                catch (Exception ex) { failure = ex; }
                finally { if (buffer != IntPtr.Zero) Marshal.FreeHGlobal(buffer); }
            };
            IntPtr logfile = Marshal.AllocHGlobal(448);
            IntPtr filename = Marshal.StringToHGlobalUni(path);
            ulong handle = ulong.MaxValue;
            try
            {
                Marshal.Copy(new byte[448], 0, logfile, 448);
                Marshal.WriteIntPtr(logfile, 0, filename);
                Marshal.WriteInt32(logfile, 28, unchecked((int)0x10000000));
                Marshal.WriteIntPtr(logfile, 424, Marshal.GetFunctionPointerForDelegate(callback));
                handle = OpenTrace(logfile);
                if (handle == ulong.MaxValue) throw new Win32Exception(Marshal.GetLastWin32Error());
                uint status = ProcessTrace(new[] { handle }, 1, IntPtr.Zero, IntPtr.Zero);
                if (failure != null) throw new InvalidOperationException("TDH schema decoding failed.", failure);
                if (status != 0) throw new Win32Exception((int)status);
                return result.ToArray();
            }
            finally
            {
                if (handle != ulong.MaxValue) CloseTrace(handle);
                GC.KeepAlive(callback);
                Marshal.FreeHGlobal(filename);
                Marshal.FreeHGlobal(logfile);
            }
        }
    }
}
