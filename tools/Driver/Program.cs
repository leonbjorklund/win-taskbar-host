// Injects input and reports what it observes as JSON, for acceptance.ps1 and restart-explorer.ps1.
// Coordinates are physical pixels because the process is per-monitor-v2 DPI aware.
using System.Diagnostics;
using System.Globalization;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;

sealed class Fail(string message) : Exception(message);

record Box(int left, int top, int right, int bottom) { public int width => right - left; public int height => bottom - top; }

static class Program
{
    static readonly HashSet<string> Held = [];

    static int Main(string[] args)
    {
        SetProcessDpiAwarenessContext(-4); // per-monitor v2
        try { Console.CancelKeyPress += (_, _) => ReleaseHeld(); } catch { }
        // A literal ';' argument chains commands in one process.
        var chain = new List<List<string>> { new() };
        foreach (var a in args) if (a == ";") chain.Add([]); else chain[^1].Add(a);
        foreach (var c in chain.Where(c => c.Count > 0))
        {
            var cmd = c[0].ToLowerInvariant();
            // One JSON line per command. The default encoder escapes non-ASCII, so any code page reads it intact.
            try
            {
                var o = JsonSerializer.SerializeToNode(Run(cmd, c.Skip(1).ToList()) ?? new { })!.AsObject();
                o["cmd"] = cmd;
                o["ok"] = true;
                Console.WriteLine(o.ToJsonString());
            }
            catch (Exception e)
            {
                ReleaseHeld();
                Console.WriteLine(JsonSerializer.Serialize(new { cmd, ok = false, error = e is Fail ? e.Message : $"{e.GetType().Name}: {e.Message}" }));
                return 1;
            }
        }
        return 0;
    }

    static object? Run(string cmd, List<string> a)
    {
        switch (cmd)
        {
            case "fg": { var h = GetForegroundWindow(); return h == 0 ? new { hwnd = 0L } : Info(h); }
            case "find": return new { windows = Children(Taskbar(), a[0]).Select(Info).ToList() };
            case "taskbar": return Info(Taskbar());
            case "start-open": return StartOpen();
            case "up": Button(a[0], false); return null;
            case "click":
            {
                var b = a.Count > 2 ? a[2] : "left";
                MoveTo(I(a[0]), I(a[1])); Thread.Sleep(30); Button(b, true); Button(b, false);
                return null;
            }
            case "drag": return Drag(a);
            case "key":
            {
                ushort vk = a[0].ToUpperInvariant() switch { "ESCAPE" => 0x1B, "LWIN" => 0x5B, _ => throw new Fail($"unknown key '{a[0]}'") };
                Send(Key(vk, false), Key(vk, true));
                return null;
            }
            case "sleep": Thread.Sleep(I(a[0])); return null;
            case "watch": return Watch(a);
            case "hwnd-at": return Info(WindowFromPoint(new POINT { x = I(a[0]), y = I(a[1]) }));
            case "menu-items": return MenuItems();
            case "sentinel": Sentinel(a[0], int.Parse(a[1], CultureInfo.InvariantCulture)); return null;
            default: throw new Fail($"unknown command '{cmd}'");
        }
    }

    static object StartOpen()
    {
        var fg = GetForegroundWindow(); var fproc = fg == 0 ? null : ProcName(PidOf(fg));
        var vs = VirtualScreen();
        bool shown = Children(0, "Windows.UI.Core.CoreWindow").Any(h =>
        {
            if (!IsStartHost(ProcName(PidOf(h))) || !IsWindowVisible(h) || Cloaked(h) != 0) return false;
            GetWindowRect(h, out var r); var b = ToBox(r);
            return b.width > 0 && b.height > 0 && b.right > vs.left && b.left < vs.right && b.bottom > vs.top && b.top < vs.bottom;
        });
        // On current Windows 11 builds an open Start menu gives the foreground to SearchHost's "Search" window (its search
        // box), so SearchHost counts only while Start's own CoreWindow is uncloaked; Win+S alone leaves it cloaked.
        return new { open = IsStartHost(fproc) || (shown && string.Equals(fproc, "SearchHost", StringComparison.OrdinalIgnoreCase)) };
    }

    static bool IsStartHost(string? process) => string.Equals(process, "StartMenuExperienceHost", StringComparison.OrdinalIgnoreCase);

    // drag <x1> <y1> <x2> <y2> <steps> [--no-release], 10 ms per step.
    static object? Drag(List<string> a)
    {
        bool noRelease = a.Remove("--no-release");
        int x1 = I(a[0]), y1 = I(a[1]), x2 = I(a[2]), y2 = I(a[3]), steps = I(a[4]);
        MoveTo(x1, y1); Thread.Sleep(10); Button("left", true);
        for (int i = 1; i <= steps; i++)
        {
            Thread.Sleep(10);
            MoveTo((int)Math.Round(x1 + (x2 - x1) * (double)i / steps), (int)Math.Round(y1 + (y2 - y1) * (double)i / steps));
        }
        Thread.Sleep(10);
        if (!noRelease) Button("left", false);
        return null;
    }

    // watch <ms> <interval-ms> <x> <y> [x y ...]: counts the colors each point shows.
    static object Watch(List<string> a)
    {
        int duration = I(a[0]), interval = Math.Max(1, I(a[1]));
        var pts = Enumerable.Range(0, (a.Count - 2) / 2).Select(i => (X: I(a[2 + 2 * i]), Y: I(a[3 + 2 * i]))).ToList();
        var counts = pts.Select(_ => new Dictionary<string, int>()).ToArray();
        var clock = Stopwatch.StartNew();
        int samples = 0;
        for (; (long)samples * interval <= duration; samples++)
        {
            var wait = samples * interval - (int)clock.ElapsedMilliseconds;
            if (wait > 0) Thread.Sleep(wait);
            for (int k = 0; k < pts.Count; k++)
            {
                var color = Pixel(pts[k].X, pts[k].Y);
                counts[k][color] = counts[k].GetValueOrDefault(color) + 1;
            }
        }
        return new
        {
            samples,
            points = pts.Select((p, k) => new { x = p.X, y = p.Y, distinct = counts[k].Count, colors = counts[k] }).ToList(),
        };
    }

    // Reads the items of open Win32 popup menus (#32768) straight from their HMENU.
    static object MenuItems()
    {
        var menus = Children(0, "#32768").Where(IsWindowVisible).Select(h =>
        {
            var menu = SendMessage(h, MN_GETHMENU, 0, 0);
            var items = Enumerable.Range(0, GetMenuItemCount(menu)).Select(i =>
            {
                var name = new StringBuilder(256);
                GetMenuString(menu, (uint)i, name, name.Capacity, MF_BYPOSITION);
                GetMenuItemRect(0, menu, (uint)i, out var r);
                return new { name = name.ToString(), rect = ToBox(r) };
            }).ToList();
            return new { items };
        }).ToList();
        return new { menus };
    }

    // An ordinary topmost window that logs each key it receives, one line per key, so the checks can prove the
    // foreground app never saw a stray Escape. Runs until process `owner` exits. A WinForms form lost its topmost style
    // here, so this is a plain Win32 window.
    static void Sentinel(string log, int owner)
    {
        File.WriteAllText(log, "");
        var parent = Process.GetProcessById(owner);
        new Thread(() => { parent.WaitForExit(); Environment.Exit(0); }) { IsBackground = true }.Start();
        WndProc proc = (h, m, w, l) =>
        {
            if (m is WM_KEYDOWN or WM_SYSKEYDOWN) File.AppendAllText(log, $"{w}\n");
            return DefWindowProc(h, m, w, l);
        };
        var cls = new WNDCLASS { lpfnWndProc = Marshal.GetFunctionPointerForDelegate(proc), hbrBackground = COLOR_WINDOW + 1, lpszClassName = "FocusSentinel" };
        RegisterClass(ref cls);
        // acceptance.ps1 clicks 600,312 on the title bar and 600,412 inside the window.
        CreateWindowEx(WS_EX_TOPMOST, "FocusSentinel", "Focus sentinel", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 300, 300, 600, 300, 0, 0, 0, 0);
        while (GetMessage(out var msg, 0, 0, 0) > 0) DispatchMessage(ref msg);
        GC.KeepAlive(proc);
    }

    static nint Taskbar() { var h = FindWindow("Shell_TrayWnd", null); return h != 0 ? h : throw new Fail("Shell_TrayWnd not found"); }

    // Direct children of `parent` (0 for top-level windows) in z-order. FindWindowEx sees immersive windows such as
    // Start's CoreWindow, which EnumWindows skips, and taskbar children that EnumChildWindows once skipped.
    static List<nint> Children(nint parent, string cls)
    {
        var l = new List<nint>();
        for (var h = FindWindowEx(parent, 0, cls, null); h != 0; h = FindWindowEx(parent, h, cls, null)) l.Add(h);
        return l;
    }
    static string ClassOf(nint h) { var b = new StringBuilder(256); GetClassName(h, b, b.Capacity); return b.ToString(); }
    static uint PidOf(nint h) { GetWindowThreadProcessId(h, out var pid); return pid; }
    static int Cloaked(nint h) => DwmGetWindowAttribute(h, DWMWA_CLOAKED, out var v, 4) == 0 ? v : 0;

    static string? ProcName(uint pid)
    {
        if (pid == 0) return null;
        try { using var p = Process.GetProcessById((int)pid); return p.ProcessName; } catch { return null; }
    }

    static object Info(nint h)
    {
        GetWindowRect(h, out var r); var pid = PidOf(h);
        return new { hwnd = (long)h, @class = ClassOf(h), pid, process = ProcName(pid), rect = ToBox(r), dpi = GetDpiForWindow(h) };
    }

    static void Send(params INPUT[] inputs)
    {
        var sent = SendInput((uint)inputs.Length, inputs, Marshal.SizeOf<INPUT>());
        if (sent != inputs.Length) throw new Fail($"SendInput inserted {sent} of {inputs.Length} events (error {Marshal.GetLastPInvokeError()})");
    }

    static void MoveTo(int x, int y)
    {
        var vs = VirtualScreen();
        Send(Mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, Normalize(x - vs.left, vs.width), Normalize(y - vs.top, vs.height)));
    }

    // Smallest normalized value that Windows maps back to this pixel (it maps n to n * size / 65536, rounded down).
    static int Normalize(int offset, int size) => (int)Math.Clamp(((long)offset * 65536 + size - 1) / size, 0, 65535);

    static void Button(string name, bool down)
    {
        var b = name.ToLowerInvariant();
        if (b is not ("left" or "right")) throw new Fail($"unknown button '{name}'");
        bool physicalLeft = (b == "left") != (GetSystemMetrics(SM_SWAPBUTTON) != 0);
        Send(Mouse(physicalLeft ? (down ? MOUSEEVENTF_LEFTDOWN : MOUSEEVENTF_LEFTUP) : (down ? MOUSEEVENTF_RIGHTDOWN : MOUSEEVENTF_RIGHTUP)));
        if (down) Held.Add(b); else Held.Remove(b);
    }

    // Releases buttons this process pressed and did not release, so a failed chain never leaves one stuck.
    static void ReleaseHeld() { foreach (var b in Held.ToList()) try { Button(b, false); } catch { } Held.Clear(); }

    static INPUT Mouse(uint flags, int dx = 0, int dy = 0) => new() { type = INPUT_MOUSE, u = new() { mi = new() { dx = dx, dy = dy, dwFlags = flags } } };
    static INPUT Key(ushort vk, bool up) => new() { type = INPUT_KEYBOARD, u = new() { ki = new() { wVk = vk, wScan = (ushort)MapVirtualKey(vk, 0), dwFlags = (up ? KEYEVENTF_KEYUP : 0) | (vk == 0x5B ? KEYEVENTF_EXTENDEDKEY : 0) } } };

    static Box VirtualScreen()
    {
        int x = GetSystemMetrics(SM_XVIRTUALSCREEN), y = GetSystemMetrics(SM_YVIRTUALSCREEN);
        return new Box(x, y, x + GetSystemMetrics(SM_CXVIRTUALSCREEN), y + GetSystemMetrics(SM_CYVIRTUALSCREEN));
    }

    static string Pixel(int x, int y)
    {
        var screen = GetDC(0); var mem = CreateCompatibleDC(screen);
        var header = new BITMAPINFOHEADER { biSize = 40, biWidth = 1, biHeight = -1, biPlanes = 1, biBitCount = 32 };
        var dib = CreateDIBSection(screen, ref header, 0, out var bits, 0, 0);
        var old = SelectObject(mem, dib);
        try
        {
            if (dib == 0 || !BitBlt(mem, 0, 0, 1, 1, screen, x, y, SRCCOPY | CAPTUREBLT)) throw new Fail($"screen capture failed (error {Marshal.GetLastPInvokeError()})");
            GdiFlush();
            return $"{Marshal.ReadByte(bits, 2):X2}{Marshal.ReadByte(bits, 1):X2}{Marshal.ReadByte(bits):X2}";
        }
        finally { SelectObject(mem, old); DeleteObject(dib); DeleteDC(mem); ReleaseDC(0, screen); }
    }

    static int I(string s) => int.TryParse(s, NumberStyles.Integer, CultureInfo.InvariantCulture, out var v) ? v : throw new Fail($"not an integer: '{s}'");

    static Box ToBox(RECT r) => new(r.left, r.top, r.right, r.bottom);

    const uint INPUT_MOUSE = 0, INPUT_KEYBOARD = 1;
    const uint MOUSEEVENTF_MOVE = 0x1, MOUSEEVENTF_LEFTDOWN = 0x2, MOUSEEVENTF_LEFTUP = 0x4, MOUSEEVENTF_RIGHTDOWN = 0x8, MOUSEEVENTF_RIGHTUP = 0x10;
    const uint MOUSEEVENTF_VIRTUALDESK = 0x4000, MOUSEEVENTF_ABSOLUTE = 0x8000;
    const uint KEYEVENTF_EXTENDEDKEY = 0x1, KEYEVENTF_KEYUP = 0x2;
    const int SM_SWAPBUTTON = 23, SM_XVIRTUALSCREEN = 76, SM_YVIRTUALSCREEN = 77, SM_CXVIRTUALSCREEN = 78, SM_CYVIRTUALSCREEN = 79;
    const int DWMWA_CLOAKED = 14;
    const uint SRCCOPY = 0x00CC0020, CAPTUREBLT = 0x40000000;
    const uint MN_GETHMENU = 0x01E1, MF_BYPOSITION = 0x400;
    const uint WM_KEYDOWN = 0x100, WM_SYSKEYDOWN = 0x104, WS_EX_TOPMOST = 0x8, WS_OVERLAPPEDWINDOW = 0xCF0000, WS_VISIBLE = 0x10000000;
    const int COLOR_WINDOW = 5;

    delegate nint WndProc(nint h, uint message, nint wParam, nint lParam);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern nint FindWindow(string cls, string? title);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern nint FindWindowEx(nint parent, nint after, string? cls, string? title);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(nint h, StringBuilder buffer, int size);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(nint h, out uint pid);
    [DllImport("user32.dll")] static extern bool GetWindowRect(nint h, out RECT r);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(nint h);
    [DllImport("user32.dll")] static extern uint GetDpiForWindow(nint h);
    [DllImport("user32.dll")] static extern nint GetForegroundWindow();
    [DllImport("user32.dll")] static extern nint WindowFromPoint(POINT p);
    [DllImport("user32.dll")] static extern nint SendMessage(nint h, uint message, nint wParam, nint lParam);
    [DllImport("user32.dll")] static extern int GetMenuItemCount(nint menu);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetMenuString(nint menu, uint item, StringBuilder text, int size, uint flags);
    [DllImport("user32.dll")] static extern bool GetMenuItemRect(nint h, nint menu, uint item, out RECT r);
    [DllImport("user32.dll", SetLastError = true)] static extern uint SendInput(uint count, INPUT[] inputs, int size);
    [DllImport("user32.dll")] static extern int GetSystemMetrics(int index);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern uint MapVirtualKey(uint code, uint mapType);
    [DllImport("user32.dll")] static extern nint GetDC(nint h);
    [DllImport("user32.dll")] static extern int ReleaseDC(nint h, nint dc);
    [DllImport("dwmapi.dll")] static extern int DwmGetWindowAttribute(nint h, int attribute, out int value, int size);
    [DllImport("gdi32.dll")] static extern nint CreateCompatibleDC(nint dc);
    [DllImport("gdi32.dll")] static extern nint CreateDIBSection(nint dc, ref BITMAPINFOHEADER header, uint usage, out nint bits, nint section, uint offset);
    [DllImport("gdi32.dll")] static extern nint SelectObject(nint dc, nint obj);
    [DllImport("gdi32.dll", SetLastError = true)] static extern bool BitBlt(nint dst, int x, int y, int w, int h, nint src, int sx, int sy, uint rop);
    [DllImport("gdi32.dll")] static extern bool DeleteObject(nint obj);
    [DllImport("gdi32.dll")] static extern bool DeleteDC(nint dc);
    [DllImport("gdi32.dll")] static extern bool GdiFlush();
    [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(nint context);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern ushort RegisterClass(ref WNDCLASS cls);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern nint CreateWindowEx(uint exStyle, string cls, string title, uint style, int x, int y, int w, int h, nint parent, nint menu, nint instance, nint param);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern nint DefWindowProc(nint h, uint message, nint wParam, nint lParam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetMessage(out MSG msg, nint h, uint min, uint max);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern nint DispatchMessage(ref MSG msg);
}

[StructLayout(LayoutKind.Sequential)] struct POINT { public int x, y; }
[StructLayout(LayoutKind.Sequential)] struct RECT { public int left, top, right, bottom; }
[StructLayout(LayoutKind.Sequential)] struct MOUSEINPUT { public int dx, dy; public uint mouseData, dwFlags, time; public nint dwExtraInfo; }
[StructLayout(LayoutKind.Sequential)] struct KEYBDINPUT { public ushort wVk, wScan; public uint dwFlags, time; public nint dwExtraInfo; }
[StructLayout(LayoutKind.Explicit)] struct InputUnion { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
[StructLayout(LayoutKind.Sequential)] struct INPUT { public uint type; public InputUnion u; }
[StructLayout(LayoutKind.Sequential)] struct MSG { public nint hwnd; public uint message; public nint wParam, lParam; public uint time; public POINT pt; }
[StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)] struct WNDCLASS
{
    public uint style; public nint lpfnWndProc; public int cbClsExtra, cbWndExtra; public nint hInstance, hIcon, hCursor, hbrBackground;
    public string? lpszMenuName; public string lpszClassName;
}
[StructLayout(LayoutKind.Sequential)] struct BITMAPINFOHEADER
{
    public uint biSize; public int biWidth, biHeight; public ushort biPlanes, biBitCount;
    public uint biCompression, biSizeImage; public int biXPelsPerMeter, biYPelsPerMeter; public uint biClrUsed, biClrImportant;
}
