# Windows ICU oracle for known fixture units, deliberately not a copy of the
# product's age-unit selector. UChar is UTF-16, including on PowerShell 7.
if (-not ('ItSidebarAgeOracle' -as [type])) {
    Add-Type @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class ItSidebarAgeOracle {
    [DllImport("icu.dll", CallingConvention=CallingConvention.Cdecl, CharSet=CharSet.Ansi)]
    static extern IntPtr ureldatefmt_open(string locale, IntPtr nf, int style, int context, ref int error);
    [DllImport("icu.dll", CallingConvention=CallingConvention.Cdecl, CharSet=CharSet.Unicode)]
    static extern int ureldatefmt_formatNumeric(IntPtr f, double offset, int unit, StringBuilder result, int capacity, ref int error);
    [DllImport("icu.dll", CallingConvention=CallingConvention.Cdecl)]
    static extern void ureldatefmt_close(IntPtr f);
    [DllImport("icu.dll", CallingConvention=CallingConvention.Cdecl, CharSet=CharSet.Ansi)]
    static extern IntPtr ucal_open(ushort[] zone, int length, string locale, int type, ref int error);
    [DllImport("icu.dll", CallingConvention=CallingConvention.Cdecl)]
    static extern void ucal_setMillis(IntPtr c, double time, ref int error);
    [DllImport("icu.dll", CallingConvention=CallingConvention.Cdecl)]
    static extern int ucal_getFieldDifference(IntPtr c, double time, int field, ref int error);
    [DllImport("icu.dll", CallingConvention=CallingConvention.Cdecl)]
    static extern void ucal_close(IntPtr c);
    static void Check(int error) { if (error > 0) throw new Exception("Windows ICU error: " + error); }
    public static int Count(string unit, DateTimeOffset source, DateTimeOffset capture) {
        if (unit == "month" || unit == "year") {
            int error = 0;
            IntPtr c = ucal_open(new ushort[] { 85, 84, 67 }, 3, "en_US", 1, ref error);
            Check(error);
            if (c == IntPtr.Zero) throw new Exception("No ICU Gregorian calendar");
            try {
                ucal_setMillis(c, source.ToUnixTimeMilliseconds(), ref error);
                int count = ucal_getFieldDifference(c, capture.ToUnixTimeMilliseconds(), unit == "year" ? 1 : 2, ref error);
                Check(error);
                return count;
            } finally { ucal_close(c); }
        }
        double seconds = (capture - source).TotalSeconds;
        double divisor = unit == "minute" ? 60 : unit == "hour" ? 3600 : unit == "day" ? 86400 : unit == "week" ? 604800 : 0;
        if (divisor == 0) throw new ArgumentException("Unknown fixture unit");
        return (int)Math.Floor(seconds / divisor);
    }
    public static string Format(string locale, string unit, int count) {
        int enumUnit = unit == "minute" ? 6 : unit == "hour" ? 5 : unit == "day" ? 4 : unit == "week" ? 3 : unit == "month" ? 2 : unit == "year" ? 0 : -1;
        if (enumUnit < 0) throw new ArgumentException("Unknown fixture unit");
        int error = 0;
        IntPtr f = ureldatefmt_open(locale, IntPtr.Zero, 1, 256, ref error);
        Check(error);
        if (f == IntPtr.Zero) throw new Exception("No ICU relative formatter");
        try {
            var result = new StringBuilder(512);
            int length = ureldatefmt_formatNumeric(f, -count, enumUnit, result, result.Capacity, ref error);
            Check(error);
            if (length <= 0) throw new Exception("Empty ICU relative time");
            return result.ToString();
        } finally { ureldatefmt_close(f); }
    }
}
'@
}
