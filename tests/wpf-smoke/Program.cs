// Drives the WPF package against the real taskbar: attach, placement, resize, the right-click menu and dispose.
// Run through `just test-desktop`.
using System.IO;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Threading;
using WinTaskbarHost;

static class Program
{
    const uint WM_CHAR = 0x0102;
    static int failures;

    static void Check(bool passed, string what)
    {
        Console.WriteLine($"{(passed ? "ok" : "FAILED")} - {what}");
        if (!passed) failures++;
    }

    // Runs the dispatcher, which also pumps Win32 messages, until done() or five seconds pass.
    static bool Pump(Func<bool> done)
    {
        for (var deadline = DateTime.UtcNow.AddSeconds(5); !done() && DateTime.UtcNow < deadline; Thread.Sleep(10))
        {
            var frame = new DispatcherFrame();
            Dispatcher.CurrentDispatcher.BeginInvoke(DispatcherPriority.Background, () => frame.Continue = false);
            Dispatcher.PushFrame(frame);
        }
        return done();
    }

    [STAThread]
    static int Main(string[] args)
    {
        if (args.Contains("--native-load-failure")) return CheckNativeLoadFailure();
        // Keep the placement record out of the real %LOCALAPPDATA%. The native library reads the process environment.
        var local = Path.Combine(Path.GetTempPath(), $"wth-wpf-smoke-{Environment.ProcessId}");
        Environment.SetEnvironmentVariable("LOCALAPPDATA", local);
        var chosen = false;
        var view = new Border { Background = Brushes.DarkSlateBlue, Child = new TextBlock { Text = "WPF smoke", Foreground = Brushes.White } };
        var host = TaskbarHost.Create(view, 60, savePlacementAs: "wpf-smoke", menuItems: [("&Zap", () => chosen = true)]);
        CheckThreadAccess(host);

        Check(Pump(() => host.State == HostState.Attached), "attaches to the taskbar");
        var window = ((HwndSource)PresentationSource.FromVisual(view)).Handle;
        var container = GetParent(window);
        var taskbar = FindWindow("Shell_TrayWnd", null);
        Check(ClassOf(container) == "WinTaskbarHost.Container" && GetParent(container) == taskbar, "the view sits in the taskbar's container");
        GetWindowRect(window, out var rect);
        Check(WindowFromPoint(new POINT { x = (rect.left + rect.right) / 2, y = (rect.top + rect.bottom) / 2 }) == window, "the view is hit-testable above the taskbar");

        var width = (int)Math.Round(90 * GetDpiForWindow(window) / 96.0, MidpointRounding.AwayFromZero);
        host.SetWidth(90);
        Check(Pump(() => GetWindowRect(window, out var r) && r.right - r.left == width), "SetWidth resizes the view");

        var record = Path.Combine(local, "win-taskbar-host", "wpf-smoke.placement");
        host.Position = 0.5;
        Check(Pump(() => File.Exists(record) && File.ReadAllText(record).Contains("\nposition 0.500000\n")) && host.Position == 0.5, "Position moves and saves under the key");

        // A right-click on the view opens the host's menu; another thread types the item's access key.
        var thread = GetCurrentThreadId();
        var typist = new Thread(() =>
        {
            var info = new GUITHREADINFO { cbSize = Marshal.SizeOf<GUITHREADINFO>() };
            for (var deadline = DateTime.UtcNow.AddSeconds(5); DateTime.UtcNow < deadline; Thread.Sleep(50))
            {
                if (GetGUIThreadInfo(thread, ref info) && info.hwndMenuOwner != 0) { PostMessage(info.hwndMenuOwner, WM_CHAR, 'z', 0); return; }
            }
        });
        typist.Start();
        view.RaiseEvent(new MouseButtonEventArgs(Mouse.PrimaryDevice, Environment.TickCount, MouseButton.Right) { RoutedEvent = UIElement.MouseRightButtonUpEvent });
        Check(Pump(() => chosen), "a right-click opens the menu and the chosen item runs");
        typist.Join();

        host.Dispose();
        Check(!IsWindow(container), "Dispose removes the container");
        CheckThreadAccess(host);
        try { _ = host.State; Check(false, "the host rejects calls after Dispose"); }
        catch (ObjectDisposedException) { Check(true, "the host rejects calls after Dispose"); }
        // A view with its own ContextMenu gets that menu, and its item starts a move.
        var item = new MenuItem { Header = "Move" };
        var custom = new Border { Background = Brushes.DarkOliveGreen, ContextMenu = new ContextMenu { Items = { item } } };
        host = TaskbarHost.Create(custom, 60);
        custom.ContextMenu.Opened += (_, _) => item.RaiseEvent(new RoutedEventArgs(MenuItem.ClickEvent));
        item.Click += (_, _) => host.BeginMove();
        Check(Pump(() => host.State == HostState.Attached), "a view with a ContextMenu attaches");
        custom.RaiseEvent(new MouseButtonEventArgs(Mouse.PrimaryDevice, Environment.TickCount, MouseButton.Right) { RoutedEvent = UIElement.MouseRightButtonUpEvent });
        container = GetParent(((HwndSource)PresentationSource.FromVisual(custom)).Handle);
        Check(Pump(() => GetCapture() == container), "the ContextMenu opens and BeginMove arms a drag");
        host.Dispose();
        CheckCreationCleanup();
        Directory.Delete(local, true);

        Console.WriteLine(failures == 0 ? "all checks passed" : $"{failures} check(s) FAILED");
        return failures == 0 ? 0 : 1;
    }

    // Exercises a failed native load without creating windows or requiring the DLL.
    static int CheckNativeLoadFailure()
    {
        NativeLibrary.SetDllImportResolver(typeof(TaskbarHost).Assembly, (_, _, _) =>
            throw new DllNotFoundException("native-load regression test"));
        foreach (var placement in new[] { false, true })
        {
            try
            {
                TaskbarHost.Create(new Border(), 60,
                    savePlacementAs: placement ? "first\0second" : null,
                    menuItems: placement ? null : [("first\0second", () => { })]);
                Check(false, "embedded NUL is rejected before native loading");
            }
            catch (Exception e)
            {
                Check(e is ArgumentException, placement ? "a placement key cannot contain NUL"
                                                        : "a menu label cannot contain NUL");
            }
        }
        var view = CreateWithoutNativeLibrary();
        GC.Collect();
        GC.WaitForPendingFinalizers();
        GC.Collect();
        Check(!view.IsAlive, "a native-loading exception releases the view");

        var owned = new Border();
        Exception? failure = null;
        var thread = new Thread(() =>
        {
            try { TaskbarHost.Create(owned, 60); }
            catch (Exception e) { failure = e; }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        thread.Join();
        Check(failure is InvalidOperationException, "Create rejects another dispatcher before loading native code");
        return failures == 0 ? 0 : 1;
    }

    [MethodImpl(MethodImplOptions.NoInlining)]
    static WeakReference CreateWithoutNativeLibrary()
    {
        var view = new Border();
        try { TaskbarHost.Create(view, 60); Check(false, "native load throws"); }
        catch (DllNotFoundException) { Check(true, "native load throws"); }
        return new WeakReference(view);
    }

    static void CheckThreadAccess(TaskbarHost host)
    {
        Action[] calls = [() => _ = host.State, () => _ = host.Position,
            () => host.Position = 0.5, () => host.SetWidth(60), host.BeginMove, host.Dispose];
        var rejected = 0;
        var thread = new Thread(() =>
        {
            foreach (var call in calls)
            {
                try { call(); }
                catch (InvalidOperationException e) when (e.GetType() == typeof(InvalidOperationException)) { rejected++; }
                catch (Exception) { }
            }
        });
        thread.Start();
        thread.Join();
        Check(rejected == calls.Length, "every operation rejects another dispatcher before accessing the native handle");
    }

    static void CheckCreationCleanup()
    {
        foreach (var fail in new[] { false, true })
        {
            var view = new Border();
            TaskbarHost? host = null;
            HwndSource? source = null;
            var error = new InvalidOperationException("content attachment regression test");
            var rethrown = false;
            SourceChangedEventHandler attaching = (_, e) =>
            {
                if (e.NewSource is not HwndSource added) return;
                source = added;
                if (fail) throw error;
                host!.Dispose();
            };
            DispatcherUnhandledExceptionEventHandler unhandled = (_, e) =>
            {
                if (!ReferenceEquals(e.Exception, error)) return;
                rethrown = true;
                e.Handled = true;
            };
            PresentationSource.AddSourceChangedHandler(view, attaching);
            Dispatcher.CurrentDispatcher.UnhandledException += unhandled;
            try
            {
                host = TaskbarHost.Create(view, 60);
                Check(Pump(() => source is { IsDisposed: true } && (!fail || rethrown)),
                    fail ? "failed content attachment disposes its HwndSource and reports the exception"
                         : "disposing during content attachment disposes its HwndSource");
                if (fail) Check(host.State == HostState.Failed, "failed content attachment leaves the host Failed");
            }
            finally
            {
                host?.Dispose();
                PresentationSource.RemoveSourceChangedHandler(view, attaching);
                Dispatcher.CurrentDispatcher.UnhandledException -= unhandled;
            }
        }
    }

    static string ClassOf(nint h) { var b = new StringBuilder(256); GetClassName(h, b, b.Capacity); return b.ToString(); }

    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern nint FindWindow(string cls, string? title);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(nint h, StringBuilder buffer, int size);
    [DllImport("user32.dll")] static extern nint GetParent(nint h);
    [DllImport("user32.dll")] static extern bool GetWindowRect(nint h, out RECT r);
    [DllImport("user32.dll")] static extern nint WindowFromPoint(POINT p);
    [DllImport("user32.dll")] static extern uint GetDpiForWindow(nint h);
    [DllImport("user32.dll")] static extern bool IsWindow(nint h);
    [DllImport("user32.dll")] static extern nint GetCapture();
    [DllImport("user32.dll")] static extern bool GetGUIThreadInfo(uint thread, ref GUITHREADINFO info);
    [DllImport("user32.dll")] static extern bool PostMessage(nint h, uint message, nint wParam, nint lParam);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
}

[StructLayout(LayoutKind.Sequential)] struct POINT { public int x, y; }
[StructLayout(LayoutKind.Sequential)] struct RECT { public int left, top, right, bottom; }
[StructLayout(LayoutKind.Sequential)] struct GUITHREADINFO
{
    public int cbSize, flags;
    public nint hwndActive, hwndFocus, hwndCapture, hwndMenuOwner, hwndMoveSize, hwndCaret;
    public RECT rcCaret;
}
