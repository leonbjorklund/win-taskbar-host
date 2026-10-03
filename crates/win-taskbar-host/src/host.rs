//! Window procedures, lifecycle, Move mode and recovery.
//!
//! Window procedures and consumer callbacks can run inside any Win32 call that
//! dispatches messages. Lifecycle state is never borrowed across such calls.
//! Borrowing a callback blocks its recursive invocation. Consumer callbacks run
//! only through [`Inner::call`], which tracks nesting so a `close` requested from
//! a callback finishes after it returns. The view's window procedure also runs
//! inside `SetWindowPos` and `DestroyWindow`, outside that tracking.

use std::{
    cell::{Cell, RefCell},
    mem::{ManuallyDrop, zeroed},
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    ptr::{null, null_mut},
    rc::{Rc, Weak},
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::PtInRect,
    System::{
        Diagnostics::Debug::OutputDebugStringW,
        LibraryLoader::{
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            GetModuleHandleExW,
        },
    },
    UI::{
        Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
        HiDpi::*,
        Input::KeyboardAndMouse::*,
        WindowsAndMessaging::*,
    },
};

use crate::{
    Builder, Content, State, Surface, placement,
    shell::{Geometry, Layout, Taskbar},
    wide,
};

pub(crate) type Factory = Box<dyn FnMut(&Surface) -> Result<Box<dyn Content>, String>>;
pub(crate) type MenuAction = Box<dyn FnMut()>;
pub(crate) type ContextMenu = Box<dyn FnMut(HWND, i32, i32)>;

const CONTROLLER_CLASS: &str = "WinTaskbarHost.Controller";
const CONTAINER_CLASS: &str = "WinTaskbarHost.Container";
const WM_HOST_WORK: u32 = WM_APP + 0x3A1;
const TIMER_RETRY: usize = 1;
const RETRY_MS: u32 = 500;
/// Container opacity while Move mode is active, as a visual cue.
const MOVE_ALPHA: u8 = 170;
const DEFAULT_POSITION: f64 = 0.25;

#[derive(Clone, Copy, PartialEq)]
enum Move {
    Idle,
    Armed,
    /// Holds the cursor x where the drag started and the container's current x.
    Dragging(i32, i32),
    /// Cancelled while a button was down. Waits for its release.
    Draining,
}

pub(crate) struct Inner {
    me: Weak<Inner>,
    width_dip: Cell<f64>,
    height_dip: Option<f64>,
    store: Option<PathBuf>,
    /// The creating thread's DPI awareness, restored for menu actions.
    app_dpi: DPI_AWARENESS_CONTEXT,
    /// Builder menu items. Their command ids are index + 1, and Move follows them.
    menu: Vec<(Vec<u16>, RefCell<MenuAction>)>,
    /// Shown instead of the built-in menu.
    context_menu: Option<RefCell<ContextMenu>>,
    taskbar_created: u32,
    controller: Cell<HWND>,
    /// Transient foreground owner while a menu or Move mode runs.
    owner: Cell<HWND>,
    container: Cell<HWND>,
    taskbar: Cell<Option<Taskbar>>,
    hooks: Cell<[HWINEVENTHOOK; 2]>,
    layout: Cell<Option<Layout>>,
    pub state: Cell<State>,
    pub position: Cell<f64>,
    depth: Cell<u32>,
    closed: Cell<bool>,
    close_requested: Cell<bool>,
    work_posted: Cell<bool>,
    need_layout: Cell<bool>,
    need_raise: Cell<bool>,
    need_save: Cell<bool>,
    moving: Cell<Move>,
    factory: RefCell<Factory>,
    /// The content and its window.
    content: RefCell<Option<(Box<dyn Content>, HWND)>>,
}

thread_local! {
    static HOOK_OWNERS: RefCell<Vec<(HWINEVENTHOOK, Weak<Inner>)>> = const { RefCell::new(Vec::new()) };
}

impl Inner {
    pub fn create(builder: Builder, factory: Factory) -> Result<Rc<Self>, String> {
        check_dip(builder.width_dip)?;
        builder.height_dip.map_or(Ok(()), check_dip)?;
        if builder.context_menu.is_some() && !builder.menu.is_empty() {
            return Err("menu items and a context menu exclude each other".to_owned());
        }
        let store = builder.key.as_deref().map(placement::store_path).transpose()?;
        let app_dpi = unsafe { GetThreadDpiAwarenessContext() };
        let _dpi = DpiScope::per_monitor_v2()
            .ok_or_else(|| last_error("Enter per-monitor-v2 DPI awareness"))?;
        register_classes()?;
        let taskbar_created = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
        if taskbar_created == 0 {
            return Err(last_error("Register TaskbarCreated"));
        }
        let position = match store.as_ref().map(placement::load) {
            Some(Ok(Some(position))) => position,
            Some(Err(message)) => {
                log(&format!("ignored saved placement: {message}"));
                DEFAULT_POSITION
            }
            _ => DEFAULT_POSITION,
        };
        let inner = Rc::new_cyclic(|me| Self {
            me: me.clone(),
            width_dip: Cell::new(builder.width_dip),
            height_dip: builder.height_dip,
            store,
            app_dpi,
            menu: builder
                .menu
                .into_iter()
                .map(|(label, action)| (wide(&label), RefCell::new(action)))
                .collect(),
            context_menu: builder.context_menu.map(RefCell::new),
            taskbar_created,
            controller: Cell::new(null_mut()),
            owner: Cell::new(null_mut()),
            container: Cell::new(null_mut()),
            taskbar: Cell::new(None),
            hooks: Cell::new([null_mut(); 2]),
            layout: Cell::new(None),
            state: Cell::new(State::WaitingForTaskbar),
            position: Cell::new(position),
            depth: Cell::new(0),
            closed: Cell::new(false),
            close_requested: Cell::new(false),
            work_posted: Cell::new(false),
            need_layout: Cell::new(false),
            need_raise: Cell::new(false),
            need_save: Cell::new(false),
            moving: Cell::new(Move::Idle),
            factory: RefCell::new(factory),
            content: RefCell::new(None),
        });
        let controller = inner.create_window(CONTROLLER_CLASS, WS_EX_TOOLWINDOW);
        if controller.is_null() {
            return Err(last_error("Create host controller window"));
        }
        inner.controller.set(controller);
        inner.request(&inner.need_layout);
        Ok(inner)
    }

    /// Creates a top-level popup whose procedure finds this host. The
    /// controller and the foreground owner must be top-level, because a
    /// message-only window misses TaskbarCreated and cannot take the foreground.
    fn create_window(&self, class: &str, ex_style: WINDOW_EX_STYLE) -> HWND {
        unsafe {
            CreateWindowExW(
                ex_style,
                wide(class).as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                module(),
                &self.me as *const Weak<Inner> as *const _,
            )
        }
    }

    pub fn set_position(&self, position: f64) {
        if !position.is_finite() {
            return;
        }
        self.position.set(position.clamp(0.0, 1.0));
        // Saved once per message-loop pass, not on every slider tick.
        self.need_save.set(true);
        self.request(&self.need_layout);
    }

    pub fn set_width(&self, width: f64) -> Result<(), String> {
        check_dip(width)?;
        self.width_dip.set(width);
        self.request(&self.need_layout);
        Ok(())
    }

    /// Arms Move like choosing it from the menu. Windows lets a process take
    /// the foreground only right after it received input, so this belongs in
    /// a click handler.
    pub fn begin_move(&self) {
        if self.closed.get() || self.moving.get() != Move::Idle || self.layout.get().is_none() {
            return;
        }
        let _dpi = DpiScope::per_monitor_v2();
        self.take_foreground();
        self.arm_move();
    }

    /// Closes now, or after the running consumer callback returns.
    pub fn close(&self) {
        if self.closed.get() {
            return;
        }
        if self.depth.get() > 0 {
            self.close_requested.set(true);
            return;
        }
        self.closed.set(true);
        if self.need_save.replace(false) {
            self.save_placement();
        }
        self.unhook();
        self.detach();
        self.release_foreground();
        unsafe { DestroyWindow(self.controller.replace(null_mut())) };
    }

    fn request(&self, flag: &Cell<bool>) {
        flag.set(true);
        self.post_work();
    }

    fn post_work(&self) {
        if !self.work_posted.get() && !self.closed.get() {
            let posted = unsafe { PostMessageW(self.controller.get(), WM_HOST_WORK, 0, 0) };
            self.work_posted.set(posted != 0);
        }
    }

    fn work(&self) {
        self.work_posted.set(false);
        if self.closed.get() || self.depth.get() > 0 {
            // `call` posts the skipped work again once consumer code returns.
            return;
        }
        let _dpi = DpiScope::per_monitor_v2();
        if self.need_layout.replace(false) {
            self.relayout();
        }
        if self.need_save.replace(false) {
            self.save_placement();
        }
        let container = self.container.get();
        if self.need_raise.replace(false) && !container.is_null() {
            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
            unsafe { SetWindowPos(container, HWND_TOP, 0, 0, 0, 0, flags) };
        }
    }

    /// Replaces any lost or unplaceable container.
    fn try_attach(&self) {
        // Dropping the content runs consumer code, which may close the host.
        self.detach();
        if self.state.get() == State::Failed || self.closed.get() {
            return;
        }
        unsafe { KillTimer(self.controller.get(), TIMER_RETRY) };
        let state = match Taskbar::find_primary() {
            Some(taskbar) => {
                if self.taskbar.get() != Some(taskbar) {
                    self.unhook();
                    self.taskbar.set(Some(taskbar));
                    self.hook(taskbar);
                }
                self.attach(taskbar).err().unwrap_or(State::Attached)
            }
            None => State::WaitingForTaskbar,
        };
        if state == State::WaitingForTaskbar && !self.closed.get() {
            unsafe { SetTimer(self.controller.get(), TIMER_RETRY, RETRY_MS, None) };
        }
        self.state.set(state);
    }

    /// `Err(WaitingForTaskbar)` asks for a retry.
    fn attach(&self, taskbar: Taskbar) -> Result<(), State> {
        let layout = self.layout_for(&taskbar.geometry()).ok_or(State::NoSpace)?;
        // The container is layered so it stays visible in Explorer's composed
        // taskbar. It starts as a popup because creating a layered child directly
        // has different manifest requirements.
        let ex_style = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
        let container = self.create_window(CONTAINER_CLASS, ex_style);
        if container.is_null() {
            return Err(State::WaitingForTaskbar);
        }
        self.container.set(container);
        let attached = unsafe {
            SetWindowLongPtrW(container, GWL_STYLE, (WS_CHILD | WS_CLIPCHILDREN) as isize);
            SetParent(container, taskbar.hwnd);
            GetParent(container) == taskbar.hwnd
        };
        if !attached {
            self.destroy_container();
            return Err(State::WaitingForTaskbar);
        }
        if unsafe { SetLayeredWindowAttributes(container, 0, 255, LWA_ALPHA) } == 0 {
            log(&last_error("Make the container layered"));
            self.destroy_container();
            return Err(State::Failed);
        }
        set_pos(container, layout.x, layout.y, &layout, SWP_FRAMECHANGED);
        self.layout.set(Some(layout));
        let surface = self.surface(&layout);
        let created = self
            .call(|| self.factory.borrow_mut()(&surface))
            .unwrap_or_else(|| Err("content factory panicked".into()));
        if self.container.get() != container {
            // Explorer destroyed the container, or the host was closed, while the factory ran.
            if let Ok(content) = created {
                self.call(|| drop(content));
            }
            return Err(State::WaitingForTaskbar);
        }
        let content = created.map_err(|message| {
            log(&message);
            self.destroy_container();
            State::Failed
        })?;
        let hwnd = self.call(|| content.hwnd()).unwrap_or(null_mut());
        // A child whose DPI awareness differs from the container's breaks scaling and input.
        let valid = unsafe {
            IsWindow(hwnd) != 0
                && GetAncestor(hwnd, GA_PARENT) == container
                && AreDpiAwarenessContextsEqual(
                    GetWindowDpiAwarenessContext(hwnd),
                    GetWindowDpiAwarenessContext(container),
                ) != 0
        };
        *self.content.borrow_mut() = Some((content, hwnd));
        if !valid {
            log("the content window must be a per-monitor-v2 child of Surface::parent");
            self.detach();
            return Err(State::Failed);
        }
        set_pos(hwnd, 0, 0, &layout, SWP_SHOWWINDOW);
        unsafe { ShowWindow(container, SW_SHOWNOACTIVATE) };
        Ok(())
    }

    fn relayout(&self) {
        let container = self.container.get();
        let alive = |t: &Taskbar| !container.is_null() && t.is_alive();
        let taskbar = self.taskbar.get().filter(alive);
        let Some(layout) = taskbar.and_then(|t| self.layout_for(&t.geometry())) else {
            // Recreates a lost container, or detaches content that no longer fits.
            self.try_attach();
            return;
        };
        let previous = self.layout.get();
        if previous == Some(layout) {
            return;
        }
        self.end_move(false);
        set_pos(container, layout.x, layout.y, &layout, 0);
        self.layout.set(Some(layout));
        let size = |l: Layout| (l.width, l.height, l.dpi);
        let hwnd = self.content.borrow().as_ref().map(|(_, hwnd)| *hwnd);
        let Some(hwnd) = hwnd.filter(|_| previous.is_none_or(|p| size(p) != size(layout))) else {
            return;
        };
        // The view's window procedure runs here and may close the host, which drops the content.
        set_pos(hwnd, 0, 0, &layout, 0);
        let surface = self.surface(&layout);
        let Some((mut content, hwnd)) = self.content.borrow_mut().take() else {
            return;
        };
        self.call(|| content.resized(&surface));
        // The callback may have closed the host or lost the container.
        if self.container.get().is_null() {
            self.call(|| drop(content));
        } else {
            *self.content.borrow_mut() = Some((content, hwnd));
        }
    }

    fn layout_for(&self, geometry: &Geometry) -> Option<Layout> {
        Layout::compute(geometry, self.width_dip.get(), self.height_dip, self.position.get())
    }

    fn surface(&self, layout: &Layout) -> Surface {
        Surface {
            parent: self.container.get(),
            width: layout.width,
            height: layout.height,
            dpi: layout.dpi,
        }
    }

    /// Leaves Move mode, destroys the container, then drops the content.
    fn detach(&self) {
        self.end_move(false);
        // A drag cancelled with the button down waits for its release, which this
        // container never gets. Clear it so the next container receives clicks.
        self.moving.set(Move::Idle);
        let content = self.content.borrow_mut().take();
        // Child window procedures may use resources owned by Content until
        // WM_NCDESTROY returns. Keep it alive through window destruction.
        self.destroy_container();
        if let Some((content, _)) = content {
            self.call(|| drop(content));
        }
    }

    fn destroy_container(&self) {
        let container = self.container.replace(null_mut());
        self.layout.set(None);
        if !container.is_null() {
            unsafe { DestroyWindow(container) };
        }
    }

    fn on_container_destroyed(&self, hwnd: HWND) {
        if self.container.get() == hwnd {
            // Usually Explorer destroyed its taskbar. Recreate the content from the
            // message loop, not during destruction.
            self.container.set(null_mut());
            self.layout.set(None);
            self.request(&self.need_layout);
        }
    }

    fn hook(&self, taskbar: Taskbar) {
        // Create, destroy and show, then location changes, on Explorer's taskbar thread only.
        let ranges = [
            (EVENT_OBJECT_CREATE, EVENT_OBJECT_SHOW),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
        ];
        self.hooks.set(ranges.map(|(first, last)| {
            let hook = unsafe {
                SetWinEventHook(
                    first,
                    last,
                    null_mut(),
                    Some(win_event),
                    taskbar.pid,
                    taskbar.tid,
                    WINEVENT_OUTOFCONTEXT,
                )
            };
            if !hook.is_null() {
                HOOK_OWNERS.with(|owners| owners.borrow_mut().push((hook, self.me.clone())));
            }
            hook
        }));
    }

    fn unhook(&self) {
        for hook in self.hooks.replace([null_mut(); 2]) {
            if !hook.is_null() {
                unsafe { UnhookWinEvent(hook) };
                // `try_with`, because a host kept in a thread-local may be dropped after this registry.
                let _ =
                    HOOK_OWNERS.try_with(|owners| owners.borrow_mut().retain(|(h, _)| *h != hook));
            }
        }
    }

    fn on_win_event(&self, event: u32, hwnd: HWND) {
        let Some(taskbar) = self.taskbar.get() else {
            return;
        };
        if hwnd != taskbar.hwnd {
            // Explorer can create its XAML layer as a sibling over the whole taskbar
            // after we attach, for example while it restarts. Raise the container above it.
            let container = self.container.get();
            if matches!(event, EVENT_OBJECT_CREATE | EVENT_OBJECT_SHOW)
                && !container.is_null()
                && unsafe { GetAncestor(hwnd, GA_PARENT) } == taskbar.hwnd
            {
                self.request(&self.need_raise);
            }
            return;
        }
        // Destruction, moves including auto-hide slides, and resizes. `relayout`
        // does nothing when the layout is unchanged.
        self.request(&self.need_layout);
    }

    // Clicking taskbar content deactivates the user's app before any message
    // reaches us, so there is no previous foreground window to restore. A menu
    // only dismisses on outside clicks when its owner is foreground. Escape only
    // reaches a foreground window, and only a foreground window captures the
    // mouse everywhere. Menus and Move therefore run with a hidden transient
    // owner in the foreground. Destroying it afterwards lets Windows activate
    // the next window in z-order, normally the user's app.

    /// Shows the menu with the builder items and Move, then runs the chosen
    /// item or arms Move. A consumer menu runs instead and arms Move itself.
    fn show_menu(&self, x: i32, y: i32) {
        if self.closed.get() || self.container.get().is_null() || self.moving.get() != Move::Idle {
            return;
        }
        let _dpi = DpiScope::per_monitor_v2();
        let owner = self.take_foreground();
        if let Some(show) = &self.context_menu {
            // A nested right-click while the menu shows must not drop its owner.
            if let Ok(mut show) = show.try_borrow_mut() {
                self.call(|| show(owner, x, y));
                if self.moving.get() == Move::Idle {
                    self.release_foreground();
                }
            }
            return;
        }
        let move_command = self.menu.len() + 1;
        let command = unsafe {
            let menu = CreatePopupMenu();
            for (index, (label, _)) in self.menu.iter().enumerate() {
                AppendMenuW(menu, MF_STRING, index + 1, label.as_ptr());
            }
            if !self.menu.is_empty() {
                AppendMenuW(menu, MF_SEPARATOR, 0, null());
            }
            AppendMenuW(menu, MF_STRING, move_command, wide("Move").as_ptr());
            let command = TrackPopupMenuEx(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
                x,
                y,
                owner,
                null(),
            );
            DestroyMenu(menu);
            command as usize
        };
        if command == move_command && self.layout.get().is_some() {
            self.arm_move();
            return;
        }
        self.release_foreground();
        // Runs after the menu and its foreground owner are gone, like any click
        // handler, unless the app closed the host while the menu was open.
        if !self.closed.get()
            && let Some((_, action)) = command.checked_sub(1).and_then(|i| self.menu.get(i))
            && let Ok(mut action) = action.try_borrow_mut()
        {
            let _app = DpiScope::enter(self.app_dpi);
            self.call(&mut **action);
        }
    }

    /// Dims the container and captures the mouse until a click starts or ends
    /// the drag. Needs a layout, which exists only while the container does.
    fn arm_move(&self) {
        let container = self.container.get();
        self.moving.set(Move::Armed);
        unsafe {
            SetLayeredWindowAttributes(container, 0, MOVE_ALPHA, LWA_ALPHA);
            SetCapture(container);
            SetCursor(LoadCursorW(null_mut(), IDC_SIZEALL));
        }
    }

    fn take_foreground(&self) -> HWND {
        let mut owner = self.owner.get();
        if owner.is_null() {
            owner = self.create_window(CONTROLLER_CLASS, WS_EX_TOOLWINDOW);
            self.owner.set(owner);
        }
        unsafe { SetForegroundWindow(owner) };
        owner
    }

    fn release_foreground(&self) {
        let owner = self.owner.replace(null_mut());
        if !owner.is_null() {
            unsafe { DestroyWindow(owner) };
        }
    }

    fn move_input(&self, message: u32, lparam: LPARAM) -> bool {
        let state = self.moving.get();
        if state == Move::Idle {
            return false;
        }
        let container = self.container.get();
        let Some(layout) = self.layout.get() else {
            return false;
        };
        // Where the message happened, so a busy thread does not shift the drag.
        let at = unsafe { GetMessagePos() };
        let cursor = POINT { x: at as i16 as i32, y: (at >> 16) as i16 as i32 };
        match (message, state) {
            // Move was cancelled with a button still down. Swallow the releases, so a
            // right-click cancel never reaches the view as a context menu.
            (WM_LBUTTONUP | WM_RBUTTONUP, Move::Draining) if !buttons_down() => {
                self.moving.set(Move::Idle);
                unsafe { ReleaseCapture() };
            }
            (WM_CAPTURECHANGED, Move::Draining) => self.moving.set(Move::Idle),
            // Only swallow mouse input. Other messages still need DefWindowProc,
            // notably WM_PAINT to validate the update region.
            (_, Move::Draining) => return (WM_MOUSEFIRST..=WM_MOUSELAST).contains(&message),
            (WM_SETCURSOR, _) | (WM_MOUSEMOVE, Move::Armed) => unsafe {
                SetCursor(LoadCursorW(null_mut(), IDC_SIZEALL));
            },
            (WM_LBUTTONDOWN, Move::Armed) => {
                let mut rect = unsafe { zeroed() };
                unsafe { GetWindowRect(container, &mut rect) };
                if unsafe { PtInRect(&rect, cursor) } != 0 {
                    self.moving.set(Move::Dragging(cursor.x, layout.x));
                    unsafe { SetCapture(container) };
                } else {
                    self.end_move(false);
                }
            }
            // The layout only changes after end_move, so it still holds the drag's start.
            (WM_MOUSEMOVE, Move::Dragging(start, _)) => {
                unsafe { SetCursor(LoadCursorW(null_mut(), IDC_SIZEALL)) };
                let x = (layout.x + cursor.x - start).clamp(0, layout.travel);
                set_pos(container, x, layout.y, &layout, SWP_NOSIZE);
                self.moving.set(Move::Dragging(start, x));
            }
            (WM_LBUTTONUP, Move::Dragging(..)) => self.end_move(true),
            (WM_RBUTTONDOWN, _) | (WM_CANCELMODE, _) => self.end_move(false),
            (WM_CAPTURECHANGED, _) if lparam as HWND != container => self.end_move(false),
            _ => return false,
        }
        true
    }

    /// Leaves Move mode. `commit` keeps the dragged position, otherwise the previous one returns.
    fn end_move(&self, commit: bool) {
        let state = self.moving.replace(Move::Idle);
        if matches!(state, Move::Idle | Move::Draining) {
            self.moving.set(state);
            return;
        }
        // A layout exists only while the container does.
        let container = self.container.get();
        if let Some(layout) = self.layout.get() {
            match state {
                Move::Dragging(_, x) if commit && x != layout.x => {
                    self.position.set(layout.position_of(x));
                    self.layout.set(Some(Layout { x, ..layout }));
                    self.request(&self.need_save);
                }
                _ => set_pos(container, layout.x, layout.y, &layout, 0),
            }
            unsafe {
                SetLayeredWindowAttributes(container, 0, 255, LWA_ALPHA);
                if !commit && buttons_down() && GetCapture() == container {
                    // Keep capture until the button is released, so the content never sees it.
                    self.moving.set(Move::Draining);
                } else if GetCapture() == container {
                    ReleaseCapture();
                }
            }
        }
        self.release_foreground();
    }

    /// Runs consumer code. Tracks nesting and contains panics.
    fn call<R>(&self, f: impl FnOnce() -> R) -> Option<R> {
        self.depth.set(self.depth.get() + 1);
        let result = catch_unwind(AssertUnwindSafe(f)).ok();
        self.depth.set(self.depth.get() - 1);
        if self.depth.get() == 0 {
            if self.close_requested.get() {
                self.close();
            } else if self.need_layout.get() || self.need_raise.get() || self.need_save.get() {
                self.post_work();
            }
        }
        result
    }

    fn save_placement(&self) {
        if let Some(path) = &self.store
            && let Err(message) = placement::save(path, self.position.get())
        {
            log(&message);
        }
    }
}

fn set_pos(hwnd: HWND, x: i32, y: i32, layout: &Layout, flags: SET_WINDOW_POS_FLAGS) {
    let flags = flags | SWP_NOZORDER | SWP_NOACTIVATE;
    unsafe { SetWindowPos(hwnd, null_mut(), x, y, layout.width, layout.height, flags) };
}

/// Whether the left or right button is down, as of the message being processed.
fn buttons_down() -> bool {
    [VK_LBUTTON, VK_RBUTTON].map(|key| unsafe { GetKeyState(key as i32) } < 0) != [false; 2]
}

fn check_dip(value: f64) -> Result<(), String> {
    if value.is_finite() && value > 0.0 && value <= 10_000.0 {
        Ok(())
    } else {
        Err("content width and height must be positive, finite DIP values up to 10000".into())
    }
}

/// Failure details have no caller to return to, so they go to the debugger.
fn log(message: &str) {
    let text = wide(&format!("win-taskbar-host: {message}\n"));
    unsafe { OutputDebugStringW(text.as_ptr()) };
}

struct DpiScope(DPI_AWARENESS_CONTEXT);

impl DpiScope {
    fn per_monitor_v2() -> Option<Self> {
        Self::enter(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
    }

    fn enter(context: DPI_AWARENESS_CONTEXT) -> Option<Self> {
        let previous = unsafe { SetThreadDpiAwarenessContext(context) };
        (!previous.is_null()).then_some(Self(previous))
    }
}

impl Drop for DpiScope {
    fn drop(&mut self) {
        unsafe { SetThreadDpiAwarenessContext(self.0) };
    }
}

fn module() -> HINSTANCE {
    let mut module = null_mut();
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            controller_proc as *const u16,
            &mut module,
        )
    };
    module
}

fn register_classes() -> Result<(), String> {
    for (name, procedure) in [
        (CONTROLLER_CLASS, controller_proc as unsafe extern "system" fn(_, _, _, _) -> _),
        (CONTAINER_CLASS, container_proc),
    ] {
        let name = wide(name);
        let class = WNDCLASSW {
            lpfnWndProc: Some(procedure),
            hInstance: module(),
            lpszClassName: name.as_ptr(),
            hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
            ..unsafe { zeroed() }
        };
        if unsafe { RegisterClassW(&class) } == 0
            && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS
        {
            return Err(last_error("Register window class"));
        }
    }
    Ok(())
}

fn last_error(action: &str) -> String {
    format!("{action}: {}", std::io::Error::last_os_error())
}

/// Finds the host behind a window. On `WM_NCDESTROY` it releases the window's reference.
unsafe fn host_of(hwnd: HWND, message: u32, lparam: LPARAM) -> Option<Rc<Inner>> {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam as *const CREATESTRUCTW);
            // Creation can fail before this message. Only the window that
            // reaches WM_NCCREATE owns a reference to release at WM_NCDESTROY.
            let weak = &*(create.lpCreateParams as *const Weak<Inner>);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, Weak::into_raw(weak.clone()) as isize);
        }
        let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Inner;
        if raw.is_null() {
            return None;
        }
        if message == WM_NCDESTROY {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            return Weak::from_raw(raw).upgrade();
        }
        ManuallyDrop::new(Weak::from_raw(raw)).upgrade()
    }
}

unsafe extern "system" fn controller_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(inner) = (unsafe { host_of(hwnd, message, lparam) }) else {
            return;
        };
        match message {
            m if m == inner.taskbar_created => inner.request(&inner.need_layout),
            // The retry timer is the only timer.
            WM_TIMER | WM_SETTINGCHANGE | WM_DISPLAYCHANGE => inner.request(&inner.need_layout),
            WM_HOST_WORK => inner.work(),
            WM_KEYDOWN if wparam == VK_ESCAPE as usize => inner.end_move(false),
            WM_ACTIVATE if wparam & 0xFFFF == WA_INACTIVE as usize => inner.end_move(false),
            WM_CANCELMODE => inner.end_move(false),
            _ => {}
        }
    }));
    // DefWindowProc does nothing for the messages handled above.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

unsafe extern "system" fn container_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let handled = catch_unwind(AssertUnwindSafe(|| {
        let inner = unsafe { host_of(hwnd, message, lparam) }?;
        match message {
            // A click activates the taskbar, as clicks on native taskbar controls do.
            // MA_NOACTIVATE would instead activate whichever window the input queue
            // shared with Explorer last had active, possibly one of the app's own.
            // Move keeps its foreground owner active instead.
            WM_MOUSEACTIVATE => {
                let idle = inner.moving.get() == Move::Idle;
                Some(if idle { MA_ACTIVATE } else { MA_NOACTIVATE } as LRESULT)
            }
            WM_NCDESTROY => {
                inner.on_container_destroyed(hwnd);
                None
            }
            WM_CONTEXTMENU => {
                let (mut x, mut y) = (lparam as i16 as i32, (lparam >> 16) as i16 as i32);
                if x == -1 && y == -1 {
                    let mut rect = unsafe { zeroed() };
                    unsafe { GetWindowRect(hwnd, &mut rect) };
                    (x, y) = (rect.left, rect.top);
                }
                inner.show_menu(x, y);
                Some(0)
            }
            WM_DPICHANGED_AFTERPARENT => {
                inner.request(&inner.need_layout);
                Some(0)
            }
            _ => inner.move_input(message, lparam).then_some(if message == WM_SETCURSOR {
                1
            } else {
                0
            }),
        }
    }));
    match handled {
        Ok(Some(result)) => result,
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

unsafe extern "system" fn win_event(
    hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    if object != OBJID_WINDOW || child != CHILDID_SELF as i32 {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let owner = HOOK_OWNERS.with(|owners| {
            owners.borrow().iter().find(|(h, _)| *h == hook).and_then(|(_, weak)| weak.upgrade())
        });
        if let Some(inner) = owner {
            inner.on_win_event(event, hwnd);
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "creates native windows"]
    fn failed_window_creation_does_not_retain_a_host_reference() {
        let inner = Inner::create(
            crate::TaskbarHost::builder(40.0),
            Box::new(|_| unreachable!("this test does not pump attachment work")),
        )
        .unwrap();
        let references = Rc::weak_count(&inner);
        assert!(inner.create_window("WinTaskbarHost.Unregistered", 0).is_null());
        assert_eq!(Rc::weak_count(&inner), references);
        inner.close();
    }

    #[test]
    #[ignore = "creates native windows"]
    fn drag_commit_saves_and_cancel_swallows_only_mouse_input() {
        let inner = Inner::create(
            crate::TaskbarHost::builder(40.0),
            Box::new(|_| unreachable!("this test does not pump attachment work")),
        )
        .unwrap();
        inner.layout.set(Some(Layout { x: 0, y: 0, width: 40, height: 40, travel: 100, dpi: 96 }));
        inner.moving.set(Move::Dragging(0, 50));
        inner.end_move(true);
        assert_eq!((inner.position.get(), inner.need_save.get()), (0.5, true));
        inner.moving.set(Move::Draining);
        assert!(!inner.move_input(WM_PAINT, 0));
        assert!(!inner.move_input(WM_GETTEXT, 0));
        assert!(inner.move_input(WM_MOUSEMOVE, 0));
        // A replaced container never sees the release, so detaching ends the wait.
        inner.detach();
        assert!(inner.moving.get() == Move::Idle);
        inner.close();
    }
}
