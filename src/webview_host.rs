//! Thin WebView2 host used by the Liquid Glass UI.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::fmt;
use std::mem;
use std::ptr;
use std::rc::Rc;
use std::sync::mpsc;

use serde::Deserialize;
use serde_json::Value;
use webview2_com::{Microsoft::Web::WebView2::Win32::*, *};
use windows_061::Win32::Foundation::{
    E_POINTER, HINSTANCE, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM,
};
use windows_061::Win32::Graphics::{Dwm, Gdi};
use windows_061::Win32::System::{Com::*, LibraryLoader, Threading};
use windows_061::Win32::UI::{
    HiDpi,
    Input::KeyboardAndMouse,
    WindowsAndMessaging,
    WindowsAndMessaging::{MSG, WINDOW_LONG_PTR_INDEX, WNDCLASSW},
};
use windows_061::core::{BOOL, HRESULT, Interface, PWSTR, s, w};

#[derive(Debug)]
pub enum WebViewError {
    WebView2(webview2_com::Error),
    Windows(windows_061::core::Error),
    Json(serde_json::Error),
    ChannelClosed,
}

impl fmt::Display for WebViewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WebView2(error) => write!(f, "{error:?}"),
            Self::Windows(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "{error}"),
            Self::ChannelClosed => write!(f, "WebView message channel closed"),
        }
    }
}

impl std::error::Error for WebViewError {}

impl From<webview2_com::Error> for WebViewError {
    fn from(error: webview2_com::Error) -> Self {
        Self::WebView2(error)
    }
}

impl From<windows_061::core::Error> for WebViewError {
    fn from(error: windows_061::core::Error) -> Self {
        Self::Windows(error)
    }
}

impl From<HRESULT> for WebViewError {
    fn from(error: HRESULT) -> Self {
        Self::Windows(windows_061::core::Error::from(error))
    }
}

impl From<serde_json::Error> for WebViewError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

pub type Result<T> = std::result::Result<T, WebViewError>;
pub type MessageHandler = Rc<RefCell<dyn FnMut(Value, WebView)>>;

type WebViewTask = Box<dyn FnOnce(WebView) + Send>;
type WebViewSender = mpsc::Sender<WebViewTask>;
type WebViewReceiver = mpsc::Receiver<WebViewTask>;
type BindingCallback = Box<dyn FnMut(Vec<Value>) -> Result<Value>>;
type BindingsMap = HashMap<String, BindingCallback>;
const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;
const ACCENT_ENABLE_BLURBEHIND: u32 = 3;
const ACRYLIC_TINT: u32 = 0x8C1E1710;

#[derive(Clone)]
pub struct WebViewProxy {
    tx: WebViewSender,
    thread_id: u32,
}

impl WebViewProxy {
    pub fn dispatch<F>(&self, task: F) -> Result<()>
    where
        F: FnOnce(WebView) + Send + 'static,
    {
        self.tx
            .send(Box::new(task))
            .map_err(|_| WebViewError::ChannelClosed)?;
        unsafe {
            let _ = WindowsAndMessaging::PostThreadMessageW(
                self.thread_id,
                WindowsAndMessaging::WM_APP,
                WPARAM::default(),
                LPARAM::default(),
            );
        }
        Ok(())
    }

    pub fn eval(&self, script: String) -> Result<()> {
        self.dispatch(move |webview| {
            let _ = webview.eval(&script);
        })
    }
}

#[derive(Clone)]
pub struct FrameWindow {
    window: Rc<HWND>,
    size: Rc<RefCell<SIZE>>,
}

impl FrameWindow {
    fn new() -> Self {
        let hwnd = {
            let window_class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                lpszClassName: w!("BoundaryToolboxWebView"),
                ..Default::default()
            };

            unsafe {
                WindowsAndMessaging::RegisterClassW(&window_class);
                WindowsAndMessaging::CreateWindowExW(
                    WindowsAndMessaging::WS_EX_APPWINDOW,
                    w!("BoundaryToolboxWebView"),
                    w!("BoundaryToolbox"),
                    WindowsAndMessaging::WS_POPUP
                        | WindowsAndMessaging::WS_CLIPCHILDREN
                        | WindowsAndMessaging::WS_CLIPSIBLINGS,
                    WindowsAndMessaging::CW_USEDEFAULT,
                    WindowsAndMessaging::CW_USEDEFAULT,
                    WindowsAndMessaging::CW_USEDEFAULT,
                    WindowsAndMessaging::CW_USEDEFAULT,
                    None,
                    None,
                    LibraryLoader::GetModuleHandleW(None)
                        .ok()
                        .map(|module| HINSTANCE(module.0)),
                    None,
                )
            }
        };

        let hwnd = hwnd.unwrap_or_default();
        apply_window_material(hwnd);

        Self {
            window: Rc::new(hwnd),
            size: Rc::new(RefCell::new(SIZE { cx: 0, cy: 0 })),
        }
    }
}

struct WebViewController(ICoreWebView2Controller);

impl Drop for WebViewController {
    fn drop(&mut self) {
        let _ = unsafe { self.0.Close() };
    }
}

#[derive(Debug, Deserialize)]
struct InvokeMessage {
    id: u64,
    method: String,
    params: Vec<Value>,
}

#[derive(Clone)]
pub struct WebView {
    controller: Rc<WebViewController>,
    webview: Rc<ICoreWebView2>,
    tx: WebViewSender,
    rx: Rc<WebViewReceiver>,
    thread_id: u32,
    bindings: Rc<RefCell<BindingsMap>>,
    frame: Option<FrameWindow>,
    parent: Rc<HWND>,
}

impl WebView {
    pub fn create(debug: bool, message_handler: MessageHandler) -> Result<Self> {
        set_process_dpi_awareness()?;
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }

        let frame = FrameWindow::new();
        let parent = *frame.window;

        let environment = {
            let (tx, rx) = mpsc::channel();
            CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
                Box::new(|handler| unsafe {
                    CreateCoreWebView2Environment(&handler)
                        .map_err(webview2_com::Error::WindowsError)
                }),
                Box::new(move |error_code, environment| {
                    error_code?;
                    tx.send(environment.ok_or_else(|| windows_061::core::Error::from(E_POINTER)))
                        .expect("send WebView2 environment");
                    Ok(())
                }),
            )?;

            rx.recv().map_err(|_| WebViewError::ChannelClosed)?
        }?;

        let controller = {
            let (tx, rx) = mpsc::channel();
            CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
                Box::new(move |handler| unsafe {
                    environment
                        .CreateCoreWebView2Controller(parent, &handler)
                        .map_err(webview2_com::Error::WindowsError)
                }),
                Box::new(move |error_code, controller| {
                    error_code?;
                    tx.send(controller.ok_or_else(|| windows_061::core::Error::from(E_POINTER)))
                        .expect("send WebView2 controller");
                    Ok(())
                }),
            )?;

            rx.recv().map_err(|_| WebViewError::ChannelClosed)?
        }?;

        let size = get_window_size(parent);
        unsafe {
            controller.SetBounds(RECT {
                left: 0,
                top: 0,
                right: size.cx,
                bottom: size.cy,
            })?;
            controller.SetIsVisible(true)?;
            if let Ok(controller2) = controller.cast::<ICoreWebView2Controller2>() {
                controller2.SetDefaultBackgroundColor(COREWEBVIEW2_COLOR {
                    A: 0,
                    R: 0,
                    G: 0,
                    B: 0,
                })?;
            }
        }

        let webview = unsafe { controller.CoreWebView2()? };
        unsafe {
            let settings = webview.Settings()?;
            settings.SetAreDefaultContextMenusEnabled(debug)?;
            settings.SetAreDevToolsEnabled(debug)?;
            if let Ok(settings9) = settings.cast::<ICoreWebView2Settings9>() {
                let _ = settings9.SetIsNonClientRegionSupportEnabled(true);
            }
        }

        *frame.size.borrow_mut() = size;
        let (tx, rx) = mpsc::channel();
        let thread_id = unsafe { Threading::GetCurrentThreadId() };

        let host = Self {
            controller: Rc::new(WebViewController(controller)),
            webview: Rc::new(webview),
            tx,
            rx: Rc::new(rx),
            thread_id,
            bindings: Rc::new(RefCell::new(HashMap::new())),
            frame: Some(frame),
            parent: Rc::new(parent),
        };

        host.init(
            r#"window.external = { invoke: value => window.chrome.webview.postMessage(value) };"#,
        )?;
        host.install_message_handler(message_handler)?;
        Self::set_window_webview(parent, Some(Box::new(host.clone())));
        Ok(host)
    }

    pub fn proxy(&self) -> WebViewProxy {
        WebViewProxy {
            tx: self.tx.clone(),
            thread_id: self.thread_id,
        }
    }

    pub fn run(self) -> Result<()> {
        if let Some(frame) = self.frame.as_ref() {
            let hwnd = *frame.window;
            unsafe {
                let _ = WindowsAndMessaging::ShowWindow(hwnd, WindowsAndMessaging::SW_SHOW);
                let _ = Gdi::UpdateWindow(hwnd);
                let _ = KeyboardAndMouse::SetFocus(Some(hwnd));
            }
        }

        let mut msg = MSG::default();
        loop {
            while let Ok(task) = self.rx.try_recv() {
                task(self.clone());
            }

            unsafe {
                let result = WindowsAndMessaging::GetMessageW(&mut msg, None, 0, 0).0;
                match result {
                    -1 => break Err(windows_061::core::Error::from_win32().into()),
                    0 => break Ok(()),
                    _ => match msg.message {
                        WindowsAndMessaging::WM_APP => {}
                        _ => {
                            let _ = WindowsAndMessaging::TranslateMessage(&msg);
                            WindowsAndMessaging::DispatchMessageW(&msg);
                        }
                    },
                }
            }
        }
    }

    pub fn terminate(&self) {
        if self.frame.is_some() {
            let _ = Self::set_window_webview(self.get_window(), None);
        }
        unsafe {
            WindowsAndMessaging::PostQuitMessage(0);
        }
    }

    pub fn close_window(&self) {
        unsafe {
            let _ = WindowsAndMessaging::DestroyWindow(self.get_window());
        }
    }

    pub fn minimize_window(&self) {
        unsafe {
            let _ = WindowsAndMessaging::ShowWindow(
                self.get_window(),
                WindowsAndMessaging::SW_MINIMIZE,
            );
        }
    }

    pub fn set_title(&self, title: &str) -> Result<&Self> {
        if let Some(frame) = self.frame.as_ref() {
            let title = CoTaskMemPWSTR::from(title);
            unsafe {
                let _ =
                    WindowsAndMessaging::SetWindowTextW(*frame.window, *title.as_ref().as_pcwstr());
            }
        }
        Ok(self)
    }

    pub fn set_size(&self, width: i32, height: i32) -> Result<&Self> {
        if let Some(frame) = self.frame.as_ref() {
            *frame.size.borrow_mut() = SIZE {
                cx: width,
                cy: height,
            };
            unsafe {
                self.controller.0.SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                })?;
                let _ = WindowsAndMessaging::SetWindowPos(
                    *frame.window,
                    None,
                    0,
                    0,
                    width,
                    height,
                    WindowsAndMessaging::SWP_NOACTIVATE
                        | WindowsAndMessaging::SWP_NOZORDER
                        | WindowsAndMessaging::SWP_NOMOVE,
                );
            }
        }
        Ok(self)
    }

    pub fn navigate_to_string(&self, html: &str) -> Result<&Self> {
        let html = CoTaskMemPWSTR::from(html);
        unsafe {
            self.webview.NavigateToString(*html.as_ref().as_pcwstr())?;
        }
        Ok(self)
    }

    fn get_window(&self) -> HWND {
        *self.parent
    }

    fn init(&self, js: &str) -> Result<&Self> {
        let webview = self.webview.clone();
        let js = String::from(js);
        AddScriptToExecuteOnDocumentCreatedCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| unsafe {
                let js = CoTaskMemPWSTR::from(js.as_str());
                webview
                    .AddScriptToExecuteOnDocumentCreated(*js.as_ref().as_pcwstr(), &handler)
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(|error_code, _id| error_code),
        )?;
        Ok(self)
    }

    fn eval(&self, js: &str) -> Result<&Self> {
        let webview = self.webview.clone();
        let js = String::from(js);
        ExecuteScriptCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| unsafe {
                let js = CoTaskMemPWSTR::from(js.as_str());
                webview
                    .ExecuteScript(*js.as_ref().as_pcwstr(), &handler)
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(|error_code, _result| error_code),
        )?;
        Ok(self)
    }

    fn install_message_handler(&self, handler: MessageHandler) -> Result<()> {
        let bindings = self.bindings.clone();
        let host = self.clone();
        unsafe {
            let mut token = 0;
            self.webview.add_WebMessageReceived(
                &WebMessageReceivedEventHandler::create(Box::new(move |_sender, args| {
                    if let Some(args) = args {
                        let mut message = PWSTR(ptr::null_mut());
                        if args.WebMessageAsJson(&mut message).is_ok() {
                            let message = CoTaskMemPWSTR::from(message);
                            let raw = message.to_string();
                            if let Ok(invoke) = serde_json::from_str::<InvokeMessage>(&raw) {
                                let mut bindings = bindings.borrow_mut();
                                if let Some(callback) = bindings.get_mut(&invoke.method) {
                                    match callback(invoke.params) {
                                        Ok(result) => host.resolve(invoke.id, 0, result),
                                        Err(error) => host.resolve(
                                            invoke.id,
                                            1,
                                            Value::String(error.to_string()),
                                        ),
                                    }
                                    .ok();
                                }
                            } else if let Ok(value) = serde_json::from_str::<Value>(&raw) {
                                (handler.borrow_mut())(value, host.clone());
                            }
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(())
    }

    fn resolve(&self, id: u64, status: i32, result: Value) -> Result<()> {
        let result = result.to_string();
        self.proxy().eval(format!(
            r#"
            if (window._rpc && window._rpc[{id}]) {{
              window._rpc[{id}].{}({result});
              window._rpc[{id}] = undefined;
            }}"#,
            if status == 0 { "resolve" } else { "reject" }
        ))
    }

    fn set_window_webview(hwnd: HWND, webview: Option<Box<WebView>>) -> Option<Box<WebView>> {
        unsafe {
            match set_window_long(
                hwnd,
                WindowsAndMessaging::GWLP_USERDATA,
                match webview {
                    Some(webview) => Box::into_raw(webview) as _,
                    None => 0_isize,
                },
            ) {
                0 => None,
                ptr => Some(Box::from_raw(ptr as *mut _)),
            }
        }
    }

    fn get_window_webview(hwnd: HWND) -> Option<Box<WebView>> {
        unsafe {
            let data = get_window_long(hwnd, WindowsAndMessaging::GWLP_USERDATA);
            match data {
                0 => None,
                _ => {
                    let webview_ptr = data as *mut WebView;
                    let raw = Box::from_raw(webview_ptr);
                    let webview = raw.clone();
                    mem::forget(raw);
                    Some(webview)
                }
            }
        }
    }
}

fn set_process_dpi_awareness() -> Result<()> {
    unsafe {
        HiDpi::SetProcessDpiAwareness(HiDpi::PROCESS_PER_MONITOR_DPI_AWARE)?;
    }
    Ok(())
}

extern "system" fn window_proc(hwnd: HWND, msg: u32, w_param: WPARAM, l_param: LPARAM) -> LRESULT {
    let webview = match WebView::get_window_webview(hwnd) {
        Some(webview) => webview,
        None => return unsafe { WindowsAndMessaging::DefWindowProcW(hwnd, msg, w_param, l_param) },
    };

    match msg {
        WindowsAndMessaging::WM_SIZE => {
            let size = get_window_size(hwnd);
            unsafe {
                let _ = webview.controller.0.SetBounds(RECT {
                    left: 0,
                    top: 0,
                    right: size.cx,
                    bottom: size.cy,
                });
            }
            if let Some(frame) = webview.frame.as_ref() {
                *frame.size.borrow_mut() = size;
            }
            LRESULT::default()
        }
        WindowsAndMessaging::WM_CLOSE => {
            unsafe {
                let _ = WindowsAndMessaging::DestroyWindow(hwnd);
            }
            LRESULT::default()
        }
        WindowsAndMessaging::WM_DESTROY => {
            webview.terminate();
            LRESULT::default()
        }
        _ => unsafe { WindowsAndMessaging::DefWindowProcW(hwnd, msg, w_param, l_param) },
    }
}

fn get_window_size(hwnd: HWND) -> SIZE {
    let mut client_rect = RECT::default();
    let _ = unsafe { WindowsAndMessaging::GetClientRect(hwnd, &mut client_rect) };
    SIZE {
        cx: client_rect.right - client_rect.left,
        cy: client_rect.bottom - client_rect.top,
    }
}

fn apply_window_material(hwnd: HWND) {
    apply_dwm_backdrop(hwnd);
    apply_acrylic_blur(hwnd);
}

fn apply_dwm_backdrop(hwnd: HWND) {
    unsafe {
        let dark_mode = BOOL(1);
        let _ = Dwm::DwmSetWindowAttribute(
            hwnd,
            Dwm::DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_mode as *const _ as *const c_void,
            mem::size_of_val(&dark_mode) as u32,
        );

        let corner = Dwm::DWMWCP_ROUND;
        let _ = Dwm::DwmSetWindowAttribute(
            hwnd,
            Dwm::DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const _ as *const c_void,
            mem::size_of_val(&corner) as u32,
        );

        let backdrop = Dwm::DWMSBT_TRANSIENTWINDOW;
        let _ = Dwm::DwmSetWindowAttribute(
            hwnd,
            Dwm::DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const _ as *const c_void,
            mem::size_of_val(&backdrop) as u32,
        );
    }
}

fn apply_acrylic_blur(hwnd: HWND) {
    type SetWindowCompositionAttribute =
        unsafe extern "system" fn(HWND, *mut WindowCompositionAttributeData) -> BOOL;

    #[repr(C)]
    struct AccentPolicy {
        accent_state: u32,
        accent_flags: u32,
        gradient_color: u32,
        animation_id: u32,
    }

    #[repr(C)]
    struct WindowCompositionAttributeData {
        attribute: u32,
        data: *mut c_void,
        size_of_data: usize,
    }

    unsafe {
        let Ok(module) = LibraryLoader::GetModuleHandleW(w!("user32.dll")) else {
            return;
        };
        let Some(function) =
            LibraryLoader::GetProcAddress(module, s!("SetWindowCompositionAttribute"))
        else {
            return;
        };
        let set_window_composition_attribute: SetWindowCompositionAttribute =
            mem::transmute(function);

        let mut policy = AccentPolicy {
            accent_state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
            accent_flags: 0,
            gradient_color: ACRYLIC_TINT,
            animation_id: 0,
        };
        let mut data = WindowCompositionAttributeData {
            attribute: WCA_ACCENT_POLICY,
            data: &mut policy as *mut _ as *mut c_void,
            size_of_data: mem::size_of::<AccentPolicy>(),
        };

        if set_window_composition_attribute(hwnd, &mut data) == BOOL(0) {
            let mut fallback_policy = AccentPolicy {
                accent_state: ACCENT_ENABLE_BLURBEHIND,
                accent_flags: 0,
                gradient_color: ACRYLIC_TINT,
                animation_id: 0,
            };
            let mut fallback_data = WindowCompositionAttributeData {
                attribute: WCA_ACCENT_POLICY,
                data: &mut fallback_policy as *mut _ as *mut c_void,
                size_of_data: mem::size_of::<AccentPolicy>(),
            };
            let _ = set_window_composition_attribute(hwnd, &mut fallback_data);
        }
    }
}

#[allow(non_snake_case)]
#[cfg(target_pointer_width = "32")]
unsafe fn set_window_long(window: HWND, index: WINDOW_LONG_PTR_INDEX, value: isize) -> isize {
    unsafe { WindowsAndMessaging::SetWindowLongW(window, index, value as _) as _ }
}

#[allow(non_snake_case)]
#[cfg(target_pointer_width = "64")]
unsafe fn set_window_long(window: HWND, index: WINDOW_LONG_PTR_INDEX, value: isize) -> isize {
    unsafe { WindowsAndMessaging::SetWindowLongPtrW(window, index, value) }
}

#[allow(non_snake_case)]
#[cfg(target_pointer_width = "32")]
unsafe fn get_window_long(window: HWND, index: WINDOW_LONG_PTR_INDEX) -> isize {
    unsafe { WindowsAndMessaging::GetWindowLongW(window, index) as _ }
}

#[allow(non_snake_case)]
#[cfg(target_pointer_width = "64")]
unsafe fn get_window_long(window: HWND, index: WINDOW_LONG_PTR_INDEX) -> isize {
    unsafe { WindowsAndMessaging::GetWindowLongPtrW(window, index) }
}
