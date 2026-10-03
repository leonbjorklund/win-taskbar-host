//! C ABI over `win-taskbar-host`. See `include/win_taskbar_host.h` for the contract.
#![cfg(windows)]
#![allow(clippy::missing_safety_doc)]

use std::{
    cell::{Cell, RefCell},
    ffi::{CStr, CString, c_char, c_void},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::null_mut,
    rc::Rc,
    thread::{self, ThreadId},
};
use win_taskbar_host::{Content, Hwnd, Surface, TaskbarHost};
use windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow;

const OK: i32 = 0;
const FAILED: i32 = 1;

type CreateFn = unsafe extern "C" fn(*mut c_void, *const Surface) -> Hwnd;
type ResizedFn = unsafe extern "C" fn(*mut c_void, Hwnd, *const Surface);
type DestroyFn = unsafe extern "C" fn(*mut c_void, Hwnd);
type MenuFn = unsafe extern "C" fn(*mut c_void, u32);
type ContextMenuFn = unsafe extern "C" fn(*mut c_void, Hwnd, i32, i32);

// The layouts in win_taskbar_host.h.
const _: () = assert!(size_of::<Surface>() == 24 && size_of::<Options>() == 88);

#[repr(C)]
pub struct Options {
    width_dip: f64,
    height_dip: f64,
    placement_key: *const c_char,
    menu_labels: *const *const c_char,
    menu_count: usize,
    context: *mut c_void,
    create_content: Option<CreateFn>,
    content_resized: Option<ResizedFn>,
    destroy_content: Option<DestroyFn>,
    on_menu: Option<MenuFn>,
    on_context_menu: Option<ContextMenuFn>,
}

struct Callbacks {
    /// `wth_destroy` clears it before it returns, even when the host finishes
    /// closing later because a callback destroyed it.
    enabled: Cell<bool>,
    /// The live content window, so `wth_destroy` can dispose it synchronously.
    content: Cell<Hwnd>,
    context: *mut c_void,
    resized: Option<ResizedFn>,
    destroy: Option<DestroyFn>,
}

impl Callbacks {
    /// Calls `destroy_content` once for the live content.
    fn dispose(&self) {
        let hwnd = self.content.replace(null_mut());
        if let (false, true, Some(destroy)) = (hwnd.is_null(), self.enabled.get(), self.destroy) {
            unsafe { destroy(self.context, hwnd) };
        }
    }
}

pub struct Host {
    thread: ThreadId,
    /// Taken by `wth_destroy`, so a nested call from `destroy_content` fails.
    host: RefCell<Option<Rc<TaskbarHost>>>,
    callbacks: Rc<Callbacks>,
}

struct CContent {
    hwnd: Hwnd,
    callbacks: Rc<Callbacks>,
}

impl Content for CContent {
    fn hwnd(&self) -> Hwnd {
        self.hwnd
    }

    fn resized(&mut self, surface: &Surface) {
        if let (true, Some(resized)) = (self.callbacks.enabled.get(), self.callbacks.resized) {
            unsafe { resized(self.callbacks.context, self.hwnd, surface) };
        }
    }
}

impl Drop for CContent {
    fn drop(&mut self) {
        self.callbacks.dispose();
    }
}

thread_local!(static LAST_ERROR: RefCell<CString> = RefCell::default());

fn fail(message: impl Into<String>) -> i32 {
    let text = CString::new(message.into().replace('\0', " ")).unwrap_or_default();
    LAST_ERROR.set(text);
    FAILED
}

unsafe fn text(text: *const c_char, name: &str) -> Result<String, i32> {
    if text.is_null() {
        return Err(fail(format!("{name} is NULL")));
    }
    match unsafe { CStr::from_ptr(text) }.to_str() {
        Ok(text) => Ok(text.to_owned()),
        Err(_) => Err(fail(format!("{name} is not valid UTF-8"))),
    }
}

fn guard(body: impl FnOnce() -> Result<(), i32>) -> i32 {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(())) => OK,
        Ok(Err(status)) => status,
        Err(_) => fail("internal panic"),
    }
}

unsafe fn resolve(host: *mut Host) -> Result<Rc<TaskbarHost>, i32> {
    let Some(host) = (unsafe { host.as_ref() }) else {
        return Err(fail("host is NULL"));
    };
    if host.thread != thread::current().id() {
        return Err(fail("use the host on the thread that created it"));
    }
    host.host.borrow().clone().ok_or_else(|| fail("host is closed"))
}

unsafe fn write<T>(out: *mut T, value: T, name: &str) -> Result<(), i32> {
    *unsafe { out.as_mut() }.ok_or_else(|| fail(format!("{name} is NULL")))? = value;
    Ok(())
}

unsafe fn with_host(host: *mut Host, body: impl FnOnce(&TaskbarHost) -> Result<(), i32>) -> i32 {
    guard(|| body(&*unsafe { resolve(host) }?))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn wth_create(options: *const Options, out: *mut *mut Host) -> i32 {
    guard(|| {
        unsafe { write(out, null_mut(), "host output pointer") }?;
        let options = unsafe { options.as_ref() }.ok_or_else(|| fail("options is NULL"))?;
        let create = options.create_content.ok_or_else(|| fail("create_content is required"))?;
        let callbacks = Rc::new(Callbacks {
            enabled: Cell::new(true),
            content: Cell::new(null_mut()),
            context: options.context,
            resized: options.content_resized,
            destroy: options.destroy_content,
        });
        let mut builder = TaskbarHost::builder(options.width_dip);
        if options.height_dip != 0.0 {
            builder = builder.height_dip(options.height_dip);
        }
        if !options.placement_key.is_null() {
            builder = builder
                .save_placement_as(&unsafe { text(options.placement_key, "placement_key") }?);
        }
        if options.menu_count > 0 {
            let (false, Some(menu)) = (options.menu_labels.is_null(), options.on_menu) else {
                return Err(fail("menu items need menu_labels and on_menu"));
            };
            let labels =
                unsafe { std::slice::from_raw_parts(options.menu_labels, options.menu_count) };
            for (index, &label) in labels.iter().enumerate() {
                let callbacks = callbacks.clone();
                builder = builder.menu_item(&unsafe { text(label, "menu label") }?, move || {
                    if callbacks.enabled.get() {
                        unsafe { menu(callbacks.context, index as u32) };
                    }
                });
            }
        }
        if let Some(show) = options.on_context_menu {
            let callbacks = callbacks.clone();
            builder = builder.context_menu(move |owner, x, y| {
                if callbacks.enabled.get() {
                    unsafe { show(callbacks.context, owner, x, y) };
                }
            });
        }
        let factory = callbacks.clone();
        let host = builder
            .create(move |surface| {
                if !factory.enabled.get() {
                    return Err("host is closing".to_owned());
                }
                let hwnd = unsafe { create(factory.context, surface) };
                if hwnd.is_null() {
                    return Err("create_content returned NULL".to_owned());
                }
                factory.content.set(hwnd);
                Ok(CContent { hwnd, callbacks: factory.clone() })
            })
            .map_err(fail)?;
        let host = Host {
            thread: thread::current().id(),
            host: RefCell::new(Some(Rc::new(host))),
            callbacks,
        };
        unsafe { *out = Box::into_raw(Box::new(host)) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn wth_destroy(host: *mut Host) -> i32 {
    guard(|| {
        unsafe { resolve(host) }?;
        let handle = unsafe { &*host };
        drop(handle.host.take());
        // Inside a callback the host finishes closing after it returns. Dispose
        // the content now so no callback runs after this function returns.
        let callbacks = &handle.callbacks;
        // Its window procedure may still use consumer resources during destruction.
        // The normal close already destroyed it; a callback-delayed close has not.
        let content = callbacks.content.get();
        if !content.is_null() {
            unsafe { DestroyWindow(content) };
        }
        callbacks.dispose();
        callbacks.enabled.set(false);
        drop(unsafe { Box::from_raw(host) });
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn wth_get_state(host: *mut Host, state: *mut u32) -> i32 {
    unsafe { with_host(host, |h| write(state, h.state() as u32, "state")) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn wth_get_position(host: *mut Host, position: *mut f64) -> i32 {
    unsafe { with_host(host, |h| write(position, h.position(), "position")) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn wth_set_position(host: *mut Host, position: f64) -> i32 {
    unsafe {
        with_host(host, |h| {
            h.set_position(position);
            Ok(())
        })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn wth_begin_move(host: *mut Host) -> i32 {
    unsafe {
        with_host(host, |h| {
            h.begin_move();
            Ok(())
        })
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn wth_set_width(host: *mut Host, width_dip: f64) -> i32 {
    unsafe { with_host(host, |h| h.set_width_dip(width_dip).map_err(fail)) }
}

#[unsafe(no_mangle)]
pub extern "C" fn wth_last_error() -> *const c_char {
    LAST_ERROR.with_borrow(|e| e.as_ptr())
}
