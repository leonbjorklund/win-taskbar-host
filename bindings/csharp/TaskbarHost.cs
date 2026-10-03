using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Threading;

namespace WinTaskbarHost;

public enum HostState { WaitingForTaskbar = 1, Attached, NoSpace, Failed }

/// <summary>Shows a WPF element inside the Windows 11 x64 primary horizontal taskbar.</summary>
/// <remarks>Create, use and dispose the host on one WPF dispatcher thread. Other threads get
/// <see cref="InvalidOperationException"/>. Give the element an opaque background, declare <c>PerMonitorV2</c> in the
/// app manifest, and set <c>Focusable = false</c> on controls that should not take keyboard focus when clicked. Menu
/// actions run on that thread after the menu closes. Exceptions from them, or from showing the view, are rethrown
/// through the dispatcher. Failing to show the view also leaves the host <see cref="HostState.Failed"/>.</remarks>
public sealed unsafe partial class TaskbarHost : IDisposable
{
    const int WS_CHILD = 0x40000000, WM_CONTEXTMENU = 0x007B;

    readonly UIElement _view;
    readonly Action[] _menuActions;
    // The window showing _view in the current container, and that container.
    HwndSource? _source;
    nint _parent;
    nint _host;
    // Context pointer for the native callbacks. It also keeps this object alive until Dispose.
    GCHandle _self;

    TaskbarHost(UIElement view, Action[] menuActions) => (_view, _menuActions) = (view, menuActions);

    /// <summary>Creates a host on the current dispatcher thread. It shows <paramref name="view"/> in a new child
    /// window each time it attaches, including after Explorer restarts. A <c>ContextMenu</c> on the view replaces the
    /// built-in menu; call <see cref="BeginMove"/> from its item.</summary>
    /// <param name="heightDip">0 fills the taskbar height minus 4 DIPs at the top and bottom.</param>
    /// <param name="savePlacementAs">Keeps placement in %LOCALAPPDATA%\win-taskbar-host\&lt;key&gt;.placement.</param>
    /// <param name="menuItems">Items above Move in the built-in right-click menu. <c>&amp;</c> marks the access key.</param>
    public static TaskbarHost Create(UIElement view, double widthDip, double heightDip = 0,
        string? savePlacementAs = null, IReadOnlyList<(string Label, Action Invoked)>? menuItems = null)
    {
        ArgumentNullException.ThrowIfNull(view);
        view.VerifyAccess();
        menuItems ??= [];
        if (savePlacementAs?.Contains('\0') == true)
            throw new ArgumentException("A placement key cannot contain NUL.", nameof(savePlacementAs));
        if (menuItems.Any(item => item.Label?.Contains('\0') == true))
            throw new ArgumentException("A menu label cannot contain NUL.", nameof(menuItems));
        var host = new TaskbarHost(view,
            [.. menuItems.Select(item => item.Invoked ?? throw new ArgumentNullException(nameof(menuItems)))]);
        var labels = new nint[menuItems.Count];
        nint key = 0;
        GCHandle self = default;
        try
        {
            for (var i = 0; i < labels.Length; i++)
                labels[i] = Marshal.StringToCoTaskMemUTF8(menuItems[i].Label);
            key = Marshal.StringToCoTaskMemUTF8(savePlacementAs);
            self = GCHandle.Alloc(host);
            fixed (nint* menu = labels)
            {
                var options = new Options
                {
                    WidthDip = widthDip,
                    HeightDip = heightDip,
                    PlacementKey = key,
                    MenuLabels = menu,
                    MenuCount = (nuint)labels.Length,
                    Context = GCHandle.ToIntPtr(self),
                    CreateContent = &OnCreateContent,
                    DestroyContent = &OnDestroyContent,
                    OnMenu = &OnMenu,
                    OnContextMenu = ContextMenuService.GetContextMenu(view) is null ? null : &OnContextMenu,
                };
                nint handle;
                if (wth_create(&options, &handle) != 0)
                    throw Error();
                host._host = handle;
                host._self = self;
            }
        }
        finally
        {
            if (host._host == 0 && self.IsAllocated) self.Free();
            foreach (var label in labels) Marshal.FreeCoTaskMem(label);
            Marshal.FreeCoTaskMem(key);
        }
        view.MouseRightButtonUp += host.OnRightButtonUp;
        return host;
    }

    public HostState State
    {
        get { uint state; Check(wth_get_state(Live, &state)); return (HostState)state; }
    }

    /// <summary>Position along the taskbar, from 0 at the leading edge to 1 at the trailing edge. Setting it moves the
    /// content and saves it under the placement key. Use it for an accessible alternative to dragging.</summary>
    public double Position
    {
        get { double position; Check(wth_get_position(Live, &position)); return position; }
        set => Check(wth_set_position(Live, value));
    }

    /// <summary>Starts Move as if chosen from the built-in menu. Call it from a click handler, so the host may take the
    /// foreground for the drag.</summary>
    public void BeginMove() => Check(wth_begin_move(Live));

    public void SetWidth(double widthDip) => Check(wth_set_width(Live, widthDip));

    /// <summary>Closes the host. No menu action runs after it returns. You may call it inside menu actions.</summary>
    public void Dispose()
    {
        _view.VerifyAccess();
        if (_host == 0) return;
        // Clear the handle first, because disposing the view's window runs WPF code that may call Dispose again.
        var host = _host;
        _host = 0;
        if (wth_destroy(host) != 0)
        {
            _host = host;
            throw Error();
        }
        // No callback runs after wth_destroy returns, so the context can be freed.
        _self.Free();
        _view.MouseRightButtonUp -= OnRightButtonUp;
    }

    // WPF consumes mouse messages, so DefWindowProc never turns a right-click into WM_CONTEXTMENU for the host.
    // Posting lets the WPF input event finish before the host's menu loop starts.
    void OnRightButtonUp(object sender, MouseButtonEventArgs e)
    {
        if (_source is null) return;
        e.Handled = true;
        var at = _view.PointToScreen(e.GetPosition(_view));
        PostMessageW(_parent, WM_CONTEXTMENU, _source.Handle, ((int)at.Y << 16) | ((int)at.X & 0xFFFF));
    }

    nint Live
    {
        get
        {
            _view.VerifyAccess();
            return _host != 0 ? _host : throw new ObjectDisposedException(nameof(TaskbarHost));
        }
    }

    static void Check(int status) { if (status != 0) throw Error(); }

    static InvalidOperationException Error() => new(Marshal.PtrToStringUTF8(wth_last_error()));

    // No exception may cross into native code. Rethrow posts it to the dispatcher, so none is silently discarded.

    static TaskbarHost From(nint context) => (TaskbarHost)GCHandle.FromIntPtr(context).Target!;

    static void Rethrow(Exception e) => Dispatcher.CurrentDispatcher.BeginInvoke(() => ExceptionDispatchInfo.Throw(e));

    [UnmanagedCallersOnly]
    static nint OnCreateContent(nint context, Surface* surface)
    {
        try
        {
            var host = From(context);
            var parameters = new HwndSourceParameters("WinTaskbarHost content", surface->Width, surface->Height)
            {
                ParentWindow = surface->Parent,
                WindowStyle = WS_CHILD,
            };
            host._parent = surface->Parent;
            var source = new HwndSource(parameters);
            try
            {
                source.RootVisual = host._view;
                // Assigning the visual can run consumer code that disposes the host.
                // Native code has not accepted the child yet, so we own its cleanup.
                if (host._host == 0) { source.Dispose(); return 0; }
                host._source = source;
                return source.Handle;
            }
            catch { source.Dispose(); throw; }
        }
        catch (Exception e)
        {
            Rethrow(e);
            return 0;
        }
    }

    [UnmanagedCallersOnly]
    static void OnDestroyContent(nint context, nint content)
    {
        try
        {
            var host = From(context);
            var source = host._source;
            host._source = null;
            // If Explorer destroyed the container, the window went with it and the HwndSource disposed itself.
            if (source is { IsDisposed: false }) source.Dispose();
        }
        catch (Exception e) { Rethrow(e); }
    }

    [UnmanagedCallersOnly]
    static void OnMenu(nint context, uint index)
    {
        try { From(context)._menuActions[index](); } catch (Exception e) { Rethrow(e); }
    }

    // The host holds the foreground until this returns, which lets the menu close on outside clicks.
    [UnmanagedCallersOnly]
    static void OnContextMenu(nint context, nint owner, int x, int y)
    {
        try
        {
            var view = From(context)._view;
            var menu = ContextMenuService.GetContextMenu(view);
            menu.PlacementTarget = view;
            var frame = new DispatcherFrame();
            RoutedEventHandler closed = (_, _) => frame.Continue = false;
            menu.Closed += closed;
            try
            {
                menu.IsOpen = true;
                Dispatcher.PushFrame(frame);
            }
            finally { menu.Closed -= closed; }
        }
        catch (Exception e) { Rethrow(e); }
    }

    // win_taskbar_host.h

    [StructLayout(LayoutKind.Sequential)]
    struct Surface
    {
        public nint Parent;
        public int Width, Height;
        public uint Dpi;
    }

    struct Options
    {
        public double WidthDip, HeightDip;
        public nint PlacementKey;
        public nint* MenuLabels;
        public nuint MenuCount;
        public nint Context;
        public delegate* unmanaged<nint, Surface*, nint> CreateContent;
        public delegate* unmanaged<nint, nint, Surface*, void> ContentResized;
        public delegate* unmanaged<nint, nint, void> DestroyContent;
        public delegate* unmanaged<nint, uint, void> OnMenu;
        public delegate* unmanaged<nint, nint, int, int, void> OnContextMenu;
    }

    const string Dll = "win_taskbar_host";
    [LibraryImport(Dll)] private static partial int wth_create(Options* options, nint* host);
    [LibraryImport(Dll)] private static partial int wth_destroy(nint host);
    [LibraryImport(Dll)] private static partial int wth_get_state(nint host, uint* state);
    [LibraryImport(Dll)] private static partial int wth_get_position(nint host, double* position);
    [LibraryImport(Dll)] private static partial int wth_set_position(nint host, double position);
    [LibraryImport(Dll)] private static partial int wth_begin_move(nint host);
    [LibraryImport(Dll)] private static partial int wth_set_width(nint host, double widthDip);
    [LibraryImport(Dll)] private static partial nint wth_last_error();
    [LibraryImport("user32.dll")] private static partial int PostMessageW(nint hwnd, uint message, nint wParam, nint lParam);
}
