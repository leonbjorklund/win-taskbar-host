use std::{
    cell::{Cell, RefCell},
    ptr::null_mut,
    rc::Rc,
    time::{Duration, Instant},
};
use win_taskbar_host::{Hwnd, State, Surface, TaskbarHost};
use windows_sys::Win32::{
    Foundation::RECT,
    UI::{HiDpi::*, Input::KeyboardAndMouse::GetCapture, WindowsAndMessaging::*},
};

thread_local! {
    /// The container and view that `label` created last on this test's thread.
    static CREATED: Cell<(Hwnd, Hwnd)> = const { Cell::new((null_mut(), null_mut())) };
    /// The owner window of the menu that `choose_from_menu` typed into.
    static MENU_OWNER: Cell<Hwnd> = const { Cell::new(null_mut()) };
}

fn label(surface: &Surface) -> Result<Hwnd, String> {
    let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            WS_CHILD,
            0,
            0,
            0,
            0,
            surface.parent,
            null_mut(),
            null_mut(),
            null_mut(),
        )
    };
    if hwnd.is_null() {
        return Err("CreateWindowExW failed".into());
    }
    CREATED.set((surface.parent, hwnd));
    Ok(hwnd)
}

/// Runs this thread's message loop until `done` or a timeout.
fn pump_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        unsafe {
            let mut message = std::mem::zeroed();
            while PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        if done() {
            return true;
        }
        if start.elapsed() > timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn builder() -> win_taskbar_host::Builder {
    // The test executable has no manifest. Measure in physical pixels like a PMv2 app.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    TaskbarHost::builder(40.0)
}

fn class_of(hwnd: Hwnd) -> String {
    let mut buffer = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), 64) };
    String::from_utf16_lossy(&buffer[..len as usize])
}

fn rect(hwnd: Hwnd) -> RECT {
    let mut rect = unsafe { std::mem::zeroed() };
    unsafe { GetWindowRect(hwnd, &mut rect) };
    rect
}

#[test]
#[ignore = "needs an interactive desktop with Explorer"]
fn attaches_content_inside_the_taskbar_and_cleans_up() {
    let host = builder().create(label).unwrap();
    assert_eq!(host.state(), State::WaitingForTaskbar, "attachment waits for the message loop");
    assert!(pump_until(Duration::from_secs(5), || host.state() == State::Attached));

    let (container, view) = CREATED.get();
    let taskbar = unsafe { GetParent(container) };
    assert_eq!(class_of(taskbar), "Shell_TrayWnd");
    let dpi = unsafe { GetDpiForWindow(taskbar) } as i32;
    let (outer, inner) = (rect(container), rect(view));
    assert_eq!(outer.right - outer.left, (40 * dpi + 48) / 96);
    assert_eq!(
        (inner.left, inner.right, inner.bottom),
        (outer.left, outer.right, outer.bottom),
        "the host sizes the content"
    );
    assert_ne!(unsafe { IsWindowVisible(view) }, 0, "the host shows the content");

    drop(host);
    assert_eq!(unsafe { IsWindow(view) }, 0);
    assert_eq!(unsafe { IsWindow(container) }, 0);
}

#[test]
#[ignore = "needs an interactive desktop with Explorer"]
fn content_resources_outlive_their_window() {
    struct Owned(Hwnd, Rc<Cell<Option<bool>>>);
    impl win_taskbar_host::Content for Owned {
        fn hwnd(&self) -> Hwnd {
            self.0
        }
    }
    impl Drop for Owned {
        fn drop(&mut self) {
            self.1.set(Some(unsafe { IsWindow(self.0) } == 0));
        }
    }
    for no_space in [false, true] {
        let destroyed_first = Rc::new(Cell::new(None));
        let observed = destroyed_first.clone();
        let host = builder()
            .create(move |surface: &Surface| Ok(Owned(label(surface)?, observed.clone())))
            .unwrap();
        assert!(pump_until(Duration::from_secs(5), || host.state() == State::Attached));
        if no_space {
            host.set_width_dip(10_000.0).unwrap();
            assert!(pump_until(Duration::from_secs(5), || host.state() == State::NoSpace));
        }
        drop(host);
        assert_eq!(
            destroyed_first.get(),
            Some(true),
            "the window must be gone before Content drops"
        );
    }
}

#[test]
#[ignore = "needs an interactive desktop with Explorer"]
fn resizing_keeps_the_view_and_tells_the_content() {
    /// Counts resize calls and keeps the last width.
    struct Counted(Hwnd, Rc<Cell<(u32, i32)>>);
    impl win_taskbar_host::Content for Counted {
        fn hwnd(&self) -> Hwnd {
            self.0
        }
        fn resized(&mut self, surface: &Surface) {
            self.1.set((self.1.get().0 + 1, surface.width));
        }
    }
    let resized = Rc::new(Cell::new((0, 0)));
    let counter = resized.clone();
    let host = builder()
        .create(move |surface: &Surface| Ok(Counted(label(surface)?, counter.clone())))
        .unwrap();
    assert!(pump_until(Duration::from_secs(5), || host.state() == State::Attached));
    let view = CREATED.get().1;
    let width = (60 * unsafe { GetDpiForWindow(view) } as i32 + 48) / 96;
    host.set_width_dip(60.0).unwrap();
    assert!(pump_until(Duration::from_secs(2), || rect(view).right - rect(view).left == width));
    assert_eq!(CREATED.get().1, view, "resizing keeps the view");
    assert_eq!(resized.get(), (1, width));
}

#[test]
#[ignore = "needs an interactive desktop with Explorer"]
fn factory_failure_is_final() {
    for panics in [false, true] {
        let calls = Rc::new(Cell::new(0));
        let counter = calls.clone();
        let host = builder()
            .create(move |_: &Surface| {
                counter.set(counter.get() + 1);
                assert!(!panics, "the factory panics");
                Err::<Hwnd, _>("the factory fails".into())
            })
            .unwrap();
        assert!(pump_until(Duration::from_secs(5), || host.state() == State::Failed));
        host.set_width_dip(41.0).unwrap();
        pump_until(Duration::from_millis(500), || false);
        assert_eq!(calls.get(), 1, "a failed host never calls the factory again");
        assert_eq!(host.state(), State::Failed);
    }
}

#[test]
#[ignore = "needs an interactive desktop with Explorer"]
fn placement_moves_the_content_and_is_saved_under_the_key() {
    // Keep the record out of the real %LOCALAPPDATA%. `set_var` is safe on Windows.
    let local = std::env::temp_dir().join(format!("wth-test-{}", std::process::id()));
    unsafe { std::env::set_var("LOCALAPPDATA", &local) };
    let key = "wth-test";
    let host = builder().save_placement_as(key).create(label).unwrap();
    assert!(pump_until(Duration::from_secs(5), || host.state() == State::Attached));
    let view = CREATED.get().1;
    let before = rect(view).left;

    host.set_position(0.5);
    assert!(pump_until(Duration::from_secs(2), || rect(view).left != before));
    let path = local.join("win-taskbar-host").join(format!("{key}.placement"));
    let saved = std::fs::read_to_string(&path).unwrap();
    assert!(saved.contains("\nposition 0.500000\n"), "{saved}");
    drop(host);

    // A new host restores the saved placement.
    let restored = builder().save_placement_as(key).create(label).unwrap();
    assert_eq!(restored.position(), 0.5);
    drop(restored);
    std::fs::remove_dir_all(local).unwrap();
}

#[test]
#[ignore = "needs an interactive desktop with Explorer"]
fn dropping_the_host_inside_its_factory_closes_after_the_factory() {
    CREATED.set((null_mut(), null_mut()));
    let slot: Rc<RefCell<Option<TaskbarHost>>> = Rc::default();
    let inside = slot.clone();
    let host = builder()
        .create(move |surface: &Surface| {
            drop(inside.borrow_mut().take());
            label(surface)
        })
        .unwrap();
    *slot.borrow_mut() = Some(host);
    assert!(pump_until(Duration::from_secs(5), || slot.borrow().is_none()));
    let (container, view) = CREATED.get();
    assert!(!view.is_null(), "the factory created its view after the drop");
    assert_eq!(unsafe { IsWindow(view) }, 0);
    assert_eq!(unsafe { IsWindow(container) }, 0, "the container must leave the taskbar too");
}

#[test]
#[ignore = "needs an interactive desktop with Explorer"]
fn dropping_the_host_from_content_drop_stops_recovery() {
    struct Closing(Hwnd, Rc<RefCell<Option<TaskbarHost>>>);
    impl win_taskbar_host::Content for Closing {
        fn hwnd(&self) -> Hwnd {
            self.0
        }
    }
    impl Drop for Closing {
        fn drop(&mut self) {
            drop(self.1.borrow_mut().take());
        }
    }
    let slot: Rc<RefCell<Option<TaskbarHost>>> = Rc::default();
    let inside = slot.clone();
    let calls = Rc::new(Cell::new(0));
    let counter = calls.clone();
    let host = builder()
        .create(move |surface: &Surface| {
            counter.set(counter.get() + 1);
            Ok(Closing(label(surface)?, inside.clone()))
        })
        .unwrap();
    *slot.borrow_mut() = Some(host);
    assert!(pump_until(Duration::from_secs(5), || slot
        .borrow()
        .as_ref()
        .is_some_and(|h| h.state() == State::Attached)));
    // The next attach attempt drops the lost content, which drops the host.
    unsafe { DestroyWindow(CREATED.get().0) };
    assert!(pump_until(Duration::from_secs(5), || slot.borrow().is_none()));
    pump_until(Duration::from_millis(500), || false);
    assert_eq!(calls.get(), 1, "a closed host never calls the factory again");
}

#[test]
#[ignore = "needs an interactive desktop with Explorer"]
fn factory_resize_waits_for_nested_message_loop_to_return() {
    let slot: Rc<RefCell<Option<TaskbarHost>>> = Rc::default();
    let inside = Rc::downgrade(&slot);
    let calls = Rc::new(Cell::new(0));
    let seen = calls.clone();
    let stable = Rc::new(Cell::new(false));
    let observed = stable.clone();
    *slot.borrow_mut() = Some(
        builder()
            .create(move |surface: &Surface| {
                seen.set(seen.get() + 1);
                if seen.get() == 1 {
                    let slot = inside.upgrade().unwrap();
                    let slot = slot.borrow();
                    let host = slot.as_ref().unwrap();
                    host.set_width_dip(10_000.0).unwrap();
                    pump_until(Duration::from_millis(30), || false);
                    observed.set(host.state() == State::WaitingForTaskbar);
                }
                label(surface)
            })
            .unwrap(),
    );
    let state = || slot.borrow().as_ref().unwrap().state();
    assert!(pump_until(Duration::from_secs(5), || state() == State::NoSpace));
    assert!(stable.get(), "resize must wait for the factory");
    slot.borrow().as_ref().unwrap().set_width_dip(40.0).unwrap();
    assert!(pump_until(Duration::from_secs(5), || state() == State::Attached));
    assert_eq!(calls.get(), 2);
}

#[test]
#[ignore = "needs an interactive desktop with Explorer"]
fn begin_move_arms_a_drag_that_a_right_click_cancels() {
    let host = builder().create(label).unwrap();
    assert!(pump_until(Duration::from_secs(5), || host.state() == State::Attached));
    let (container, _) = CREATED.get();
    host.begin_move();
    assert_eq!(unsafe { GetCapture() }, container, "the container captures the mouse");
    unsafe { SendMessageW(container, WM_RBUTTONDOWN, 0, 0) };
    assert!(unsafe { GetCapture() }.is_null(), "a right-click ends the drag");
}

#[test]
#[ignore = "needs an interactive desktop with Explorer"]
fn context_menu_runs_with_an_owner_at_the_click() {
    let shown = Rc::new(Cell::new(None));
    let seen = shown.clone();
    let host = builder()
        .context_menu(move |owner, x, y| {
            seen.set(Some((owner, x, y, unsafe { IsWindow(owner) } != 0)))
        })
        .create(label)
        .unwrap();
    assert!(pump_until(Duration::from_secs(5), || host.state() == State::Attached));
    let view = CREATED.get().1;
    let bounds = rect(view);
    let at = (bounds.left & 0xFFFF) as isize | ((bounds.top & 0xFFFF) as isize) << 16;
    unsafe { SendMessageW(view, WM_CONTEXTMENU, view as usize, at) };
    let (owner, x, y, alive) = shown.get().expect("the menu callback ran");
    assert!(alive, "a hidden owner exists while the menu shows");
    assert_eq!((x, y), (bounds.left, bounds.top));
    assert_eq!(unsafe { IsWindow(owner) }, 0, "the owner goes once the menu returns");
}

#[test]
#[ignore = "needs an interactive desktop with Explorer"]
fn recreates_content_after_losing_its_container() {
    let host = builder().create(label).unwrap();
    host.set_position(0.5);
    assert!(pump_until(Duration::from_secs(5), || host.state() == State::Attached));
    let (first, view) = CREATED.get();
    let left = rect(first).left;

    // Destroying only this host's container recreates the content the way
    // taskbar loss does, without disturbing Explorer.
    unsafe { DestroyWindow(first) };
    assert_eq!(unsafe { IsWindow(view) }, 0);
    assert!(pump_until(Duration::from_secs(5), || CREATED.get().0 != first
        && host.state() == State::Attached));
    assert_eq!(rect(CREATED.get().0).left, left, "the new container keeps its place");
}

/// Opens the host's menu as a right-click on `view` does and types `key`, the
/// access key of the item to choose. Dismisses the menu after two seconds so a
/// failure cannot hang the test.
fn choose_from_menu(view: Hwnd, key: char) {
    unsafe extern "system" {
        fn GetCurrentThreadId() -> u32;
    }
    // Ticks so far, the key, and whether it was typed.
    thread_local!(static STATE: Cell<(u32, usize, bool)> = const { Cell::new((0, 0, false)) });
    unsafe extern "system" fn tick(_: Hwnd, _: u32, _: usize, _: u32) {
        let (ticks, key, typed) = STATE.get();
        let mut info: GUITHREADINFO = unsafe { std::mem::zeroed() };
        info.cbSize = size_of::<GUITHREADINFO>() as u32;
        let open = unsafe { GetGUIThreadInfo(GetCurrentThreadId(), &mut info) } != 0
            && !info.hwndMenuOwner.is_null();
        if open && !typed {
            MENU_OWNER.set(info.hwndMenuOwner);
            unsafe { PostMessageW(info.hwndMenuOwner, WM_CHAR, key, 0) };
        } else if ticks > 40 {
            unsafe { EndMenu() };
        }
        STATE.set((ticks + 1, key, typed || open));
    }
    STATE.set((0, key as usize, false));
    let bounds = rect(view);
    unsafe {
        let timer = SetTimer(null_mut(), 0, 50, Some(tick));
        // What DefWindowProc sends the parent when the content is right-clicked.
        let at = (bounds.left & 0xFFFF) as isize | ((bounds.top & 0xFFFF) as isize) << 16;
        SendMessageW(view, WM_CONTEXTMENU, view as usize, at);
        KillTimer(null_mut(), timer);
    }
}

#[test]
#[ignore = "needs an interactive desktop with Explorer and briefly opens the content menu"]
fn menu_items_run_after_the_menu_closes_with_app_dpi_and_may_close_the_host() {
    let is = |context| unsafe {
        AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), context) != 0
    };
    // Like an app without a DPI manifest. Only the view runs per-monitor-v2.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_UNAWARE) };
    let slot: Rc<RefCell<Option<TaskbarHost>>> = Rc::default();
    let inside = slot.clone();
    let checks = Rc::new(RefCell::new(Vec::new()));
    let (factory, action) = (checks.clone(), checks.clone());
    let host = TaskbarHost::builder(40.0)
        .menu_item("&Zap", move || {
            let unaware = is(DPI_AWARENESS_CONTEXT_UNAWARE);
            action.borrow_mut().push(("menu action", unaware));
            let owner_gone = unsafe { IsWindow(MENU_OWNER.get()) } == 0;
            action.borrow_mut().push(("menu closed first", owner_gone));
            drop(inside.borrow_mut().take());
        })
        .create(move |surface: &Surface| {
            let v2 = is(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            factory.borrow_mut().push(("factory", v2));
            label(surface)
        })
        .unwrap();
    *slot.borrow_mut() = Some(host);
    assert!(pump_until(Duration::from_secs(5), || slot
        .borrow()
        .as_ref()
        .is_some_and(|h| h.state() == State::Attached)));
    let (container, view) = CREATED.get();
    choose_from_menu(view, 'z');
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    assert!(slot.borrow().is_none(), "the chosen item's action closed the host");
    assert_eq!(unsafe { IsWindow(container) }, 0, "closing from the action removes the container");
    let checks = checks.borrow();
    assert_eq!(checks.len(), 3, "{checks:?}");
    assert!(checks.iter().all(|(_, ok)| *ok), "{checks:?}");
}

#[test]
fn invalid_options_fail_synchronously() {
    assert!(TaskbarHost::builder(0.0).create(label).is_err());
    assert!(builder().menu_item("A", || {}).context_menu(|_, _, _| {}).create(label).is_err());
    let bad_key = builder().save_placement_as("../escape").create(label);
    assert!(bad_key.is_err());
}
