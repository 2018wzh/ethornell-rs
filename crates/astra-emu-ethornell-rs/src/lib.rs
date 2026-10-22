//! Thin C ABI adapter. BGI scheduling, composition, audio and save remain native.
use astra_emu_abi::*;
use ethornell_app::embedded::{
    Embedded, FRAME_INTERVAL, Frame, HostWindow, Input, WindowCommand, WindowError, WindowFeedback,
};
use ethornell_app::localization::{DisplayConfiguration, DisplayEncoding};
use std::{
    cell::RefCell,
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
    path::PathBuf,
    ptr,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Instant,
};

static INITIALIZED: AtomicBool = AtomicBool::new(false);
const fn bytes(text: &'static str) -> Bytes {
    Bytes {
        data: text.as_ptr(),
        len: text.len(),
    }
}
struct Schema([ConfigField; 5]);
// These pointers refer exclusively to immutable library-lifetime string literals.
unsafe impl Sync for Schema {}
const fn field(
    id: &'static str,
    label: &'static str,
    group: &'static str,
    description: &'static str,
    default_value: ConfigValue,
) -> ConfigField {
    ConfigField {
        id: bytes(id),
        label: bytes(label),
        group: bytes(group),
        default_value,
        integer_min: 0,
        integer_max: 0,
        number_min: 0.0,
        number_max: 0.0,
        max_bytes: 0,
        flags: 0,
        choices: ptr::null(),
        choice_count: 0,
        description: bytes(description),
        choice_labels: ptr::null(),
        step: 0.0,
        unit: Bytes::EMPTY,
    }
}
const fn value(kind: u32, text: Bytes) -> ConfigValue {
    ConfigValue {
        kind,
        reserved: 0,
        integer: 0,
        number: 0.0,
        text,
    }
}
const fn boolean(id: &'static str, label: &'static str, description: &'static str) -> ConfigField {
    ConfigField {
        integer_max: 1,
        flags: CONFIG_FLAG_ADVANCED,
        ..field(
            id,
            label,
            "高级",
            description,
            value(VALUE_BOOL, Bytes::EMPTY),
        )
    }
}
struct EncodingChoices([Bytes; 2]);
unsafe impl Sync for EncodingChoices {}
static ENCODINGS: EncodingChoices = EncodingChoices([bytes("cp932"), bytes("gbk")]);
static ENCODING_LABELS: EncodingChoices =
    EncodingChoices([bytes("Shift-JIS（日文原版）"), bytes("GBK（简体中文）")]);
static SCHEMA: Schema = Schema([
    boolean(
        "trace",
        "跟踪原生虚拟机",
        "在日志中逐条记录 BGI 虚拟机执行与原生调用，用于排查问题，会明显增加日志量。",
    ),
    boolean(
        "fail_on_stub",
        "遇到未实现调用时停止",
        "脚本调用尚未实现的原生功能时立即结束游戏并报告，关闭时跳过该调用继续运行。",
    ),
    ConfigField {
        max_bytes: 128,
        choices: ENCODINGS.0.as_ptr(),
        choice_count: 2,
        choice_labels: ENCODING_LABELS.0.as_ptr(),
        ..field(
            "text_encoding",
            "文本显示编码",
            "语言",
            "决定游戏画面上的文字按哪种编码解码显示，汉化版通常需要 GBK；资源文件名与脚本逻辑不受影响。",
            value(VALUE_ENUM, bytes("cp932")),
        )
    },
    ConfigField {
        max_bytes: 4096,
        flags: CONFIG_FLAG_FILE_PATH | CONFIG_FLAG_GAME_RELATIVE,
        ..field(
            "font_path",
            "游戏字体文件",
            "语言",
            "用于显示游戏文字的字体文件，路径相对游戏目录；留空时沿用原有字体。",
            value(VALUE_STRING, Bytes::EMPTY),
        )
    },
    ConfigField {
        integer_max: 65535,
        step: 1.0,
        flags: CONFIG_FLAG_ADVANCED,
        ..field(
            "font_index",
            "字体序号",
            "语言",
            "字体集合（TTC）中要使用的字体序号，从 0 开始；非 0 时必须同时指定字体文件。",
            value(VALUE_INTEGER, Bytes::EMPTY),
        )
    },
]);
struct Formats([Bytes; 1]);
unsafe impl Sync for Formats {}
static FORMATS: Formats = Formats([bytes("bgi-bp-arc")]);

#[derive(Default)]
struct OpenConfiguration {
    trace: bool,
    fail_on_stub: bool,
    display: DisplayConfiguration,
}
unsafe fn parse_configuration(
    entries: &[ConfigEntry],
) -> Result<OpenConfiguration, (Status, String)> {
    unsafe { validate_config(&SCHEMA.0, entries) }
        .map_err(|e| failure(INVALID_ARGUMENT, e.reason))?;
    let mut config = OpenConfiguration::default();
    for entry in entries {
        match unsafe { entry.id.text(128) }.map_err(|s| failure(s, "invalid configuration ID"))? {
            "trace" => config.trace = entry.value.integer != 0,
            "fail_on_stub" => config.fail_on_stub = entry.value.integer != 0,
            "text_encoding" => {
                config.display.text_encoding = match unsafe { entry.value.text.text(128) }
                    .map_err(|s| failure(s, "invalid display encoding"))?
                {
                    "cp932" => DisplayEncoding::Cp932,
                    "gbk" => DisplayEncoding::Gbk,
                    _ => unreachable!("schema validates display encoding"),
                }
            }
            "font_path" => {
                config.display.font_path = unsafe { entry.value.text.text(4096) }
                    .map_err(|s| failure(s, "invalid font path"))?
                    .to_owned()
            }
            "font_index" => config.display.font_index = entry.value.integer as u32,
            _ => unreachable!("schema validates configuration IDs"),
        }
    }
    Ok(config)
}

#[derive(Clone, Copy)]
struct Services {
    host: HostServices,
    epoch: u64,
}
// HostServices callbacks are thread-safe and remain alive until close/failed open returns.
unsafe impl Send for Services {}
unsafe impl Sync for Services {}
impl Services {
    fn cancelled(self) -> bool {
        self.host
            .cancellation
            .is_cancelled
            .is_none_or(|callback| unsafe {
                callback(self.host.cancellation.context, self.epoch) != 0
            })
    }
    fn invalidate(self) {
        if let Some(callback) = self.host.window.invalidate {
            unsafe { callback(self.host.window.context, self.epoch, 1) };
        }
    }
}
fn window_request_error(status: Status, message: String) -> WindowError {
    match status {
        CANCELLED | STALE_CONTEXT => WindowError::Interrupted(message),
        CORE_ERROR | UNSUPPORTED => WindowError::Operation(message),
        _ => WindowError::Protocol(message),
    }
}
struct Host {
    services: Services,
    cancelled: Arc<AtomicBool>,
    next_request: u64,
}
impl Host {
    fn request(
        &mut self,
        mut request: WindowRequest,
        input: &mut [u8],
    ) -> Result<Option<WindowResult>, WindowError> {
        if self.cancelled() {
            return Err(WindowError::Interrupted("session cancelled".into()));
        }
        let callback = self
            .services
            .host
            .window
            .request
            .ok_or("native window service is unavailable")?;
        self.next_request = self
            .next_request
            .checked_add(1)
            .ok_or("window request identity exhausted")?;
        request.request_id = self.next_request;
        let data = if input.is_empty() {
            ptr::null_mut()
        } else {
            input.as_mut_ptr()
        };
        let mut result = WindowResult {
            selected_button: 0,
            reserved: 0,
            valid_fields: 0,
            fullscreen: 0,
            width: 0,
            height: 0,
            x: 0,
            y: 0,
            input: TextBuffer {
                data,
                capacity: input.len(),
                len: 0,
            },
            visible: 0,
            minimized: 0,
        };
        let mut error = Error::default();
        let status = unsafe {
            callback(
                self.services.host.window.context,
                &request,
                &mut result,
                &mut error,
            )
        };
        if status == CANCELLED && request.kind == WINDOW_MODAL && !self.cancelled() {
            return Ok(None);
        }
        if status != OK {
            let message = format!(
                "native window operation {} flags {} at ({}, {}) service status {status}: {}: {}",
                request.kind,
                request.flags,
                request.x,
                request.y,
                String::from_utf8_lossy(
                    &error.code[..(error.code_len as usize).min(error.code.len())]
                ),
                String::from_utf8_lossy(
                    &error.message[..(error.message_len as usize).min(error.message.len())]
                ),
            );
            return Err(if self.cancelled() {
                WindowError::Interrupted(message)
            } else {
                window_request_error(status, message)
            });
        }
        if result.reserved != 0
            || result.input.data != data
            || result.input.capacity != input.len()
            || result.input.len > input.len()
        {
            return Err("invalid native window result".into());
        }
        Ok(Some(result))
    }
    fn base(&self, kind: u32) -> WindowRequest {
        WindowRequest {
            session_epoch: self.services.epoch,
            context_epoch: 1,
            request_id: 0,
            kind,
            flags: 0,
            text: Bytes::EMPTY,
            pixels: Bytes::EMPTY,
            width: 0,
            height: 0,
            x: 0,
            y: 0,
            autohide_ms: 0,
            buttons: ptr::null(),
            button_count: 0,
            caption: Bytes::EMPTY,
            initial_text: Bytes::EMPTY,
        }
    }
}
impl HostWindow for Host {
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || self.services.cancelled()
    }
    fn command(&mut self, command: WindowCommand<'_>) -> Result<WindowFeedback, WindowError> {
        let mut request = self.base(0);
        let mut required = 0;
        match command {
            WindowCommand::Title(title) => {
                request.kind = WINDOW_TITLE;
                request.text = Bytes::new(title.as_bytes());
            }
            WindowCommand::Icon {
                width,
                height,
                rgba,
            } => {
                request.kind = WINDOW_ICON;
                request.width = width;
                request.height = height;
                request.pixels = Bytes::new(rgba);
            }
            WindowCommand::Fullscreen(fullscreen) => {
                request.kind = WINDOW_FULLSCREEN;
                request.flags = if fullscreen {
                    FULLSCREEN_BORDERLESS
                } else {
                    FULLSCREEN_WINDOWED
                };
                required = WINDOW_RESULT_MODE;
            }
            WindowCommand::Size(width, height) => {
                request.kind = WINDOW_SIZE;
                request.width = width;
                request.height = height;
                required = WINDOW_RESULT_SIZE;
            }
            WindowCommand::Position(x, y) => {
                request.kind = WINDOW_POSITION;
                request.x = x;
                request.y = y;
                required = WINDOW_RESULT_POSITION;
            }
            WindowCommand::CursorPosition(x, y) => {
                request.kind = WINDOW_POINTER;
                request.flags = POINTER_SET_POSITION;
                request.x = x;
                request.y = y;
            }
            WindowCommand::CursorVisible(visible) => {
                request.kind = WINDOW_POINTER;
                request.flags = POINTER_SET_VISIBILITY | if visible { POINTER_VISIBLE } else { 0 };
            }
            WindowCommand::Visible(visible) => {
                request.kind = WINDOW_VISIBILITY;
                request.flags = u32::from(visible);
                required = WINDOW_RESULT_VISIBILITY;
            }
            WindowCommand::Minimized(minimized) => {
                request.kind = WINDOW_MINIMIZED;
                request.flags = u32::from(minimized);
                required = WINDOW_RESULT_MINIMIZED;
            }
        }
        if request.text.len > 4096 {
            return Err("native window title exceeds ABI bound".into());
        }
        let result = self
            .request(request, &mut [])?
            .ok_or_else(|| WindowError::Interrupted("native window request cancelled".into()))?;
        if result.valid_fields & required != required {
            return Err("host omitted actual native window result".into());
        }
        if (result.valid_fields & WINDOW_RESULT_VISIBILITY != 0 && result.visible > 1)
            || (result.valid_fields & WINDOW_RESULT_MINIMIZED != 0 && result.minimized > 1)
        {
            return Err("host returned invalid native window state".into());
        }
        let size = (result.valid_fields & WINDOW_RESULT_SIZE != 0)
            .then_some((result.width, result.height));
        if size.is_some_and(|(w, h)| w == 0 || h == 0) {
            return Err("host returned an empty native window size".into());
        }
        let fullscreen = if result.valid_fields & WINDOW_RESULT_MODE != 0 {
            match result.fullscreen {
                FULLSCREEN_WINDOWED => Some(false),
                FULLSCREEN_BORDERLESS => Some(true),
                _ => return Err("host returned unsupported fullscreen mode".into()),
            }
        } else {
            None
        };
        Ok(WindowFeedback { size, fullscreen })
    }
    fn message(
        &mut self,
        kind: u32,
        title: &str,
        text: &str,
        default_first: bool,
    ) -> Result<Option<bool>, String> {
        if title.len() > MAX_MODAL_TEXT || text.len() > MAX_MODAL_TEXT {
            return Err("native modal text exceeds ABI bound".into());
        }
        let labels: &[&str] = match kind {
            0 => &["OK"],
            1 => &["Yes", "No"],
            2 => &["OK", "Cancel"],
            3 => &["Retry", "Cancel"],
            _ => return Err("unknown native modal kind".into()),
        };
        let mut buttons = [
            WindowButton {
                id: 1,
                reserved: 0,
                label: Bytes::new(labels[0].as_bytes()),
            },
            WindowButton {
                id: 2,
                reserved: 0,
                label: Bytes::EMPTY,
            },
        ];
        if labels.len() == 2 {
            buttons[1].label = Bytes::new(labels[1].as_bytes());
        }
        // Ordering carries the native default button without changing explicit result IDs.
        if !default_first && labels.len() == 2 {
            buttons.swap(0, 1);
        }
        let mut request = self.base(WINDOW_MODAL);
        request.text = Bytes::new(text.as_bytes());
        request.caption = Bytes::new(title.as_bytes());
        request.buttons = buttons.as_ptr();
        request.button_count = labels.len();
        let Some(result) = self
            .request(request, &mut [])
            .map_err(WindowError::into_message)?
        else {
            return Ok(None);
        };
        if result.valid_fields & WINDOW_RESULT_MODAL == 0
            || !buttons[..labels.len()]
                .iter()
                .any(|b| b.id == result.selected_button)
        {
            return Err("host returned no valid native modal selection".into());
        }
        Ok(Some(result.selected_button == 1))
    }
    fn prompt(
        &mut self,
        title: &str,
        prompt: &str,
        initial: &str,
    ) -> Result<Option<String>, String> {
        if [title.len(), prompt.len(), initial.len()]
            .into_iter()
            .any(|len| len > MAX_MODAL_TEXT)
        {
            return Err("native text input exceeds ABI bound".into());
        }
        let buttons = [
            WindowButton {
                id: 1,
                reserved: 0,
                label: bytes("OK"),
            },
            WindowButton {
                id: 2,
                reserved: 0,
                label: bytes("Cancel"),
            },
        ];
        let mut request = self.base(WINDOW_MODAL);
        request.flags = MODAL_TEXT_INPUT;
        request.caption = Bytes::new(title.as_bytes());
        request.text = Bytes::new(prompt.as_bytes());
        request.initial_text = Bytes::new(initial.as_bytes());
        request.buttons = buttons.as_ptr();
        request.button_count = buttons.len();
        let mut input = vec![0; MAX_MODAL_TEXT];
        let Some(result) = self
            .request(request, &mut input)
            .map_err(WindowError::into_message)?
        else {
            return Ok(None);
        };
        if result.valid_fields & WINDOW_RESULT_MODAL == 0 {
            return Err("host omitted text input result".into());
        }
        match result.selected_button {
            1 => {
                input.truncate(result.input.len);
                String::from_utf8(input)
                    .map(Some)
                    .map_err(|e| e.to_string())
            }
            2 => Ok(None),
            _ => Err("host returned invalid text input selection".into()),
        }
    }
    fn choose(
        &mut self,
        title: &str,
        prompt: &str,
        options: &[String],
    ) -> Result<Option<String>, String> {
        if title.len() > MAX_MODAL_TEXT
            || prompt.len() > MAX_MODAL_TEXT
            || options.is_empty()
            || options.len() > MAX_MODAL_BUTTONS
            || options
                .iter()
                .any(|text| text.len() > MAX_MODAL_BUTTON_LABEL)
        {
            return Err("native selection exceeds modal bounds".into());
        }
        let buttons: Vec<_> = options
            .iter()
            .enumerate()
            .map(|(index, label)| WindowButton {
                id: index as u32 + 1,
                reserved: 0,
                label: Bytes::new(label.as_bytes()),
            })
            .collect();
        let mut request = self.base(WINDOW_MODAL);
        request.caption = Bytes::new(title.as_bytes());
        request.text = Bytes::new(prompt.as_bytes());
        request.buttons = buttons.as_ptr();
        request.button_count = buttons.len();
        let Some(result) = self
            .request(request, &mut [])
            .map_err(WindowError::into_message)?
        else {
            return Ok(None);
        };
        if result.valid_fields & WINDOW_RESULT_MODAL == 0 {
            return Err("host omitted selection result".into());
        }
        result
            .selected_button
            .checked_sub(1)
            .and_then(|index| options.get(index as usize))
            .cloned()
            .map(Some)
            .ok_or_else(|| "host returned invalid native item selection".into())
    }
}

struct Published {
    frame: Frame,
    identity: FrameIdentity,
    timestamp_ns: u64,
}
struct State {
    status: u32,
    info: SurfaceInfo,
    output: u64,
    generation: u64,
    latest: Option<Arc<Published>>,
    failure: Option<(Status, String)>,
}
struct Shared {
    state: Mutex<State>,
    cancelled: Arc<AtomicBool>,
}
enum Command {
    Input(Vec<(Input, Option<u64>)>),
    Pause(bool),
    Wake,
}
struct Handle {
    shared: Arc<Shared>,
    sender: SyncSender<Command>,
    worker: Option<JoinHandle<()>>,
    services: Services,
}
impl Drop for Handle {
    fn drop(&mut self) {
        if self.worker.is_some() {
            stop(self);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}
fn surface(width: u32, height: u32) -> SurfaceInfo {
    SurfaceInfo {
        kind: SURFACE_CPU,
        pixel_format: PIXEL_RGBA8,
        color: COLOR_SRGB,
        alpha: ALPHA_IGNORED,
        rows: ROWS_TOP_DOWN,
        logical: Extent { width, height },
        raster: Extent { width, height },
        stride: u64::from(width) * 4,
    }
}
fn fail(shared: &Shared, status: Status, message: String) {
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    state.status = SESSION_FAILED;
    state.failure = Some((status, message));
}
fn run(
    root: PathBuf,
    config: OpenConfiguration,
    window: WindowState,
    services: Services,
    shared: Arc<Shared>,
    receiver: Receiver<Command>,
    ready: SyncSender<Result<SurfaceInfo, String>>,
) {
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
        let host = Rc::new(RefCell::new(Host {
            services,
            cancelled: shared.cancelled.clone(),
            next_request: 0,
        }));
        let mut engine = Embedded::open(
            &root,
            config.trace,
            config.fail_on_stub,
            &config.display,
            host,
        )
        .map_err(|e| e.to_string())?;
        engine
            .input(Input::Window {
                width: window.drawable_width,
                height: window.drawable_height,
                focused: window.focused != 0,
                suspended: window.suspended != 0,
            })
            .map_err(|e| e.to_string())?;
        if services.cancelled() || shared.cancelled.load(Ordering::Acquire) {
            return Err("open cancelled".into());
        }
        let (width, height) = engine.dimensions();
        let info = surface(width, height);
        {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.info = info;
            state.status = if engine.is_paused() {
                SESSION_PAUSED
            } else {
                SESSION_RUNNING
            };
        }
        if ready.send(Ok(info)).is_err() {
            return Ok(());
        }
        let origin = Instant::now();
        let mut deadline = Instant::now();
        loop {
            if services.cancelled() || shared.cancelled.load(Ordering::Acquire) {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                if let Some(frame) = engine.tick().map_err(|e| e.to_string())? {
                    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.info.raster
                        != (Extent {
                            width: frame.width,
                            height: frame.height,
                        })
                    {
                        state.output = state
                            .output
                            .checked_add(1)
                            .ok_or("output identity exhausted")?;
                        state.info = surface(frame.width, frame.height);
                    }
                    state.generation = state
                        .generation
                        .checked_add(1)
                        .ok_or("frame identity exhausted")?;
                    let identity = FrameIdentity {
                        session_epoch: services.epoch,
                        output_epoch: state.output,
                        generation: state.generation,
                    };
                    state.latest = Some(Arc::new(Published {
                        frame,
                        identity,
                        timestamp_ns: origin.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                    }));
                }
                {
                    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                    state.status = if engine.is_finished() {
                        SESSION_FINISHED
                    } else if engine.is_paused() {
                        SESSION_PAUSED
                    } else {
                        SESSION_RUNNING
                    };
                }
                if engine.is_finished() {
                    break;
                }
                deadline = Instant::now() + FRAME_INTERVAL;
            }
            let command =
                match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(command) => command,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
            match command {
                Command::Input(events) => {
                    let output = shared
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .output;
                    for (event, displayed_output) in events {
                        if displayed_output.is_none_or(|value| value == output) {
                            engine.input(event).map_err(|e| e.to_string())?;
                        }
                    }
                }
                Command::Pause(paused) => engine.pause(paused),
                Command::Wake => {}
            }
        }
        engine.shutdown().map_err(|error| error.to_string())
    }));
    match result {
        Ok(Ok(())) => {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.status = SESSION_FINISHED;
        }
        Ok(Err(message)) if services.cancelled() || shared.cancelled.load(Ordering::Acquire) => {
            let _ = ready.try_send(Err(message));
            shared
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .status = SESSION_FINISHED;
        }
        Ok(Err(message)) => {
            let _ = ready.try_send(Err(message.clone()));
            fail(&shared, CORE_ERROR, message);
        }
        Err(_) => {
            let message = "native worker panicked".to_string();
            let _ = ready.try_send(Err(message.clone()));
            fail(&shared, PANICKED, message);
        }
    }
}

fn boundary(error: *mut Error, action: impl FnOnce() -> Result<(), (Status, String)>) -> Status {
    let result = catch_unwind(AssertUnwindSafe(action));
    let failure = match result {
        Ok(Ok(())) => return OK,
        Ok(Err(failure)) => failure,
        Err(_) => (PANICKED, "adapter panicked".into()),
    };
    if !error.is_null() && error.is_aligned() {
        unsafe { (*error).set("ethornell.embedded", &failure.1) };
    }
    failure.0
}
fn failure(status: Status, message: &str) -> (Status, String) {
    (status, message.into())
}
unsafe fn handle<'a>(session: Session) -> Result<&'a Handle, (Status, String)> {
    if session.is_null() || !(session as *const Handle).is_aligned() {
        return Err(failure(INVALID_ARGUMENT, "invalid session handle"));
    }
    Ok(unsafe { &*session.cast::<Handle>() })
}
fn initialized() -> Result<(), (Status, String)> {
    if INITIALIZED.load(Ordering::Acquire) {
        Ok(())
    } else {
        Err(failure(INVALID_STATE, "initialize diagnostics first"))
    }
}
unsafe extern "C" fn initialize(diagnostics: *const Diagnostics, error: *mut Error) -> Status {
    boundary(error, || {
        if diagnostics.is_null() || !diagnostics.is_aligned() {
            return Err(failure(INVALID_ARGUMENT, "missing diagnostics"));
        }
        let diagnostics = unsafe { *diagnostics };
        if !diagnostics.header.accepts::<Diagnostics>()
            || diagnostics.emit.is_none()
            || diagnostics.flush.is_none()
        {
            return Err(failure(INVALID_ARGUMENT, "invalid diagnostics"));
        }
        #[cfg(feature = "diagnostic-bridge")]
        unsafe { diagnostic_bridge::install(diagnostics) }
            .map_err(|s| failure(s, "diagnostic bridge installation failed"))?;
        INITIALIZED.store(true, Ordering::Release);
        Ok(())
    })
}
unsafe extern "C" fn describe(output: *mut Descriptor, error: *mut Error) -> Status {
    boundary(error, || {
        initialized()?;
        if output.is_null() || !output.is_aligned() {
            return Err(failure(INVALID_ARGUMENT, "missing descriptor output"));
        }
        unsafe {
            *output = Descriptor {
                header: Header::of::<Descriptor>(),
                family_id: bytes("ethornell"),
                plugin_id: bytes("ethornell-rs"),
                version: bytes("0.1.0"),
                capabilities: CAP_CPU_SURFACE,
                formats: FORMATS.0.as_ptr(),
                format_count: FORMATS.0.len(),
                configuration: SCHEMA.0.as_ptr(),
                configuration_count: SCHEMA.0.len(),
                display_name: bytes("ethornell-rs"),
                family_name: bytes("Ethornell / BGI"),
                render: RenderExtentSupport::default(),
            };
        }
        Ok(())
    })
}
unsafe extern "C" fn probe(path: Bytes, output: *mut ProbeResult, error: *mut Error) -> Status {
    boundary(error, || {
        initialized()?;
        if output.is_null() || !output.is_aligned() {
            return Err(failure(INVALID_ARGUMENT, "missing probe output"));
        }
        let path = unsafe { path.text(32768) }.map_err(|s| failure(s, "invalid game path"))?;
        let manager = ethornell_archive::ResourceManager::open_game(path)
            .map_err(|_| failure(NO_MATCH, "BGI archive root not found"))?;
        manager
            .read_decoded_from_archive("system.arc", "ipl._bp")
            .map_err(|_| failure(NO_MATCH, "BGI bootstrap not found"))?;
        unsafe {
            *output = ProbeResult {
                game_id: bytes("ethornell-bgi"),
                format: FORMATS.0[0],
                confidence_permyriad: 9000,
                reserved: 0,
            };
        }
        Ok(())
    })
}
unsafe extern "C" fn open(
    request: *const OpenRequest,
    output: *mut OpenResult,
    error: *mut Error,
) -> Status {
    boundary(error, || {
        if output.is_null() || !output.is_aligned() {
            return Err(failure(INVALID_ARGUMENT, "missing open output"));
        }
        unsafe {
            *output = OpenResult {
                session: ptr::null_mut(),
                context_epoch: 0,
                output_epoch: 0,
                surface: surface(1, 1),
            };
        }
        initialized()?;
        if request.is_null() || !request.is_aligned() {
            return Err(failure(INVALID_ARGUMENT, "missing open request"));
        }
        let request = unsafe { &*request };
        if !request.header.accepts::<OpenRequest>()
            || !request.host.header.accepts::<HostServices>()
            || request.session_epoch == 0
            || request.host.cancellation.is_cancelled.is_none()
        {
            return Err(failure(
                INVALID_ARGUMENT,
                "invalid open request or cancellation service",
            ));
        }
        RenderExtentSupport::default()
            .validate_request(CAP_CPU_SURFACE, request.render_extent)
            .map_err(|s| failure(s, "render extent is not supported"))?;
        if request.host.window.request.is_some() != request.host.window.invalidate.is_some() {
            return Err(failure(
                INVALID_ARGUMENT,
                "window service must support cancellation",
            ));
        }
        if request.window.focused > 1
            || request.window.suspended > 1
            || !request.window.scale_factor.is_finite()
            || request.window.scale_factor <= 0.0
        {
            return Err(failure(INVALID_ARGUMENT, "invalid initial window state"));
        }
        let services = Services {
            host: request.host,
            epoch: request.session_epoch,
        };
        if services.cancelled() {
            return Err(failure(CANCELLED, "open cancelled"));
        }
        let root = PathBuf::from(
            unsafe { request.game_path.text(32768) }
                .map_err(|s| failure(s, "invalid game path"))?,
        );
        let configuration = unsafe {
            borrowed_array(
                request.configuration,
                request.configuration_count,
                MAX_CONFIG_FIELDS,
            )
        }
        .map_err(|s| failure(s, "invalid configuration span"))?;
        let config = unsafe { parse_configuration(configuration) }?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                status: SESSION_WAITING,
                info: surface(1280, 720),
                output: 1,
                generation: 0,
                latest: None,
                failure: None,
            }),
            cancelled: Arc::new(AtomicBool::new(false)),
        });
        let (sender, receiver) = mpsc::sync_channel(64);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker_shared = shared.clone();
        let window = request.window;
        let worker = thread::Builder::new()
            .name("ethornell-native".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || {
                run(
                    root,
                    config,
                    window,
                    services,
                    worker_shared,
                    receiver,
                    ready_tx,
                )
            })
            .map_err(|e| (CORE_ERROR, e.to_string()))?;
        let mut handle = Box::new(Handle {
            shared,
            sender,
            worker: Some(worker),
            services,
        });
        let ready = ready_rx.recv();
        let info = match ready {
            Ok(Ok(info)) if !services.cancelled() => info,
            result => {
                stop(&handle);
                if let Some(worker) = handle.worker.take() {
                    let _ = worker.join();
                }
                let message = match result {
                    Ok(Err(message)) => message,
                    Err(e) => e.to_string(),
                    _ => "open cancelled".into(),
                };
                return Err((
                    if services.cancelled() {
                        CANCELLED
                    } else {
                        CORE_ERROR
                    },
                    message,
                ));
            }
        };
        tracing::info!(
            logical_width = info.logical.width,
            logical_height = info.logical.height,
            raster_width = info.raster.width,
            raster_height = info.raster.height,
            output_width = window.drawable_width,
            output_height = window.drawable_height,
            "ethornell native embedded opened"
        );
        unsafe {
            *output = OpenResult {
                session: Box::into_raw(handle).cast(),
                context_epoch: 1,
                output_epoch: 1,
                surface: info,
            };
        }
        Ok(())
    })
}
unsafe extern "C" fn input(
    session: Session,
    events: *const InputEvent,
    count: usize,
    error: *mut Error,
) -> Status {
    boundary(error, || {
        let handle = unsafe { handle(session)? };
        let events = unsafe { borrowed_array(events, count, 256) }
            .map_err(|s| failure(s, "invalid input batch"))?;
        let state = handle
            .shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if state.status == SESSION_FAILED || state.status == SESSION_FINISHED {
            return Err(failure(INVALID_STATE, "session is not accepting input"));
        }
        if handle.shared.cancelled.load(Ordering::Acquire) || handle.services.cancelled() {
            return Err(failure(CANCELLED, "session cancelled"));
        }
        let mut batch = Vec::with_capacity(events.len());
        for event in events {
            if event.pressed > 1
                || event.repeat > 1
                || !(event.x as f32).is_finite()
                || !(event.y as f32).is_finite()
            {
                return Err(failure(INVALID_ARGUMENT, "invalid input values"));
            }
            if event.kind == INPUT_WINDOW
                && (event.window.focused > 1
                    || event.window.suspended > 1
                    || !event.window.scale_factor.is_finite()
                    || event.window.scale_factor <= 0.0)
            {
                return Err(failure(INVALID_ARGUMENT, "invalid window state"));
            }
            let pointer = matches!(
                event.kind,
                INPUT_POINTER_MOVE | INPUT_POINTER_BUTTON | INPUT_WHEEL
            );
            if pointer
                && (event.displayed.session_epoch != handle.services.epoch
                    || event.displayed.output_epoch != state.output
                    || event.displayed.generation == 0
                    || event.displayed.generation > state.generation)
            {
                return Err(failure(
                    STALE_CONTEXT,
                    "input references another frame output",
                ));
            }
            let value = match event.kind {
                INPUT_POINTER_MOVE => Input::PointerMove {
                    x: event.x as f32,
                    y: event.y as f32,
                },
                INPUT_POINTER_BUTTON => {
                    if !matches!(
                        event.button,
                        BUTTON_PRIMARY | BUTTON_SECONDARY | BUTTON_MIDDLE
                    ) {
                        return Err(failure(UNSUPPORTED, "unsupported pointer button"));
                    }
                    Input::PointerButton {
                        button: event.button,
                        pressed: event.pressed != 0,
                        x: event.x as f32,
                        y: event.y as f32,
                    }
                }
                INPUT_WHEEL => Input::Wheel(event.y as f32),
                INPUT_KEY => Input::Key {
                    physical: unsafe { event.physical_key.text(128) }
                        .map_err(|s| failure(s, "invalid physical key"))?
                        .into(),
                    logical: unsafe { event.logical_key.text(256) }
                        .map_err(|s| failure(s, "invalid logical key"))?
                        .into(),
                    pressed: event.pressed != 0,
                },
                INPUT_WINDOW => Input::Window {
                    width: event.window.drawable_width,
                    height: event.window.drawable_height,
                    focused: event.window.focused != 0,
                    suspended: event.window.suspended != 0,
                },
                _ => return Err(failure(UNSUPPORTED, "unsupported input kind")),
            };
            batch.push((value, pointer.then_some(event.displayed.output_epoch)));
        }
        drop(state);
        handle
            .sender
            .try_send(Command::Input(batch))
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => failure(CAPACITY, "native input queue is full"),
                mpsc::TrySendError::Disconnected(_) => {
                    failure(INVALID_STATE, "native worker stopped")
                }
            })?;
        Ok(())
    })
}
unsafe extern "C" fn control(session: Session, command: u32, error: *mut Error) -> Status {
    boundary(error, || {
        let handle = unsafe { handle(session)? };
        let pause = match command {
            CONTROL_PAUSE => true,
            CONTROL_RESUME => false,
            _ => {
                return Err(failure(
                    UNSUPPORTED,
                    "control not supported; reopen to restart native VM",
                ));
            }
        };
        handle
            .sender
            .try_send(Command::Pause(pause))
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => failure(CAPACITY, "native command queue is full"),
                mpsc::TrySendError::Disconnected(_) => {
                    failure(INVALID_STATE, "native worker stopped")
                }
            })?;
        Ok(())
    })
}
unsafe extern "C" fn state(
    session: Session,
    output: *mut SessionState,
    error: *mut Error,
) -> Status {
    boundary(error, || {
        let handle = unsafe { handle(session)? };
        if output.is_null() || !output.is_aligned() {
            return Err(failure(INVALID_ARGUMENT, "missing state output"));
        }
        let state = handle
            .shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut detail = Error::default();
        if let Some((_, message)) = &state.failure {
            detail.set("ethornell.native", message);
        }
        unsafe {
            *output = SessionState {
                state: state.status,
                flags: 0,
                context_epoch: 1,
                output_epoch: state.output,
                error: detail,
            };
        }
        Ok(())
    })
}
unsafe extern "C" fn release(owner: *mut c_void) {
    if !owner.is_null() {
        drop(unsafe { Arc::from_raw(owner.cast::<Published>()) });
    }
}
unsafe extern "C" fn frame(
    session: Session,
    displayed: FrameIdentity,
    output: *mut FrameSurface,
    error: *mut Error,
) -> Status {
    boundary(error, || {
        let handle = unsafe { handle(session)? };
        if output.is_null() || !output.is_aligned() {
            return Err(failure(INVALID_ARGUMENT, "missing frame output"));
        }
        let state = handle
            .shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some((status, message)) = &state.failure {
            return Err((*status, message.clone()));
        }
        let mut frame = FrameSurface {
            state: FRAME_PENDING,
            reserved: 0,
            identity: FrameIdentity {
                session_epoch: handle.services.epoch,
                output_epoch: state.output,
                generation: 0,
            },
            timestamp_ns: 0,
            info: state.info,
            cpu: CpuLease {
                pixels: Bytes::EMPTY,
                owner: ptr::null_mut(),
                release: None,
            },
        };
        if let Some(latest) = &state.latest {
            frame.identity = latest.identity;
            frame.timestamp_ns = latest.timestamp_ns;
            if displayed == latest.identity {
                frame.state = FRAME_UNCHANGED;
            } else {
                frame.state = FRAME_READY;
                frame.cpu.pixels = Bytes::new(&latest.frame.rgba);
                frame.cpu.owner = Arc::into_raw(latest.clone()).cast_mut().cast();
                frame.cpu.release = Some(release);
            }
        }
        unsafe {
            *output = frame;
        }
        Ok(())
    })
}
fn stop(handle: &Handle) {
    handle.shared.cancelled.store(true, Ordering::Release);
    handle.services.invalidate();
    let _ = handle.sender.try_send(Command::Wake);
}
unsafe extern "C" fn cancel(session: Session) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Ok(handle) = unsafe { handle(session) } {
            stop(handle);
        }
    }));
}
unsafe extern "C" fn close(session: Session, error: *mut Error) -> Status {
    boundary(error, || {
        unsafe { handle(session)? };
        let mut handle = unsafe { Box::from_raw(session.cast::<Handle>()) };
        stop(&handle);
        if let Some(worker) = handle.worker.take() {
            worker
                .join()
                .map_err(|_| failure(PANICKED, "native worker join failed"))?;
        }
        Ok(())
    })
}
unsafe extern "C" fn extension(_name: Bytes, _version: u32, output: *mut *const c_void) -> Status {
    if output.is_null() || !output.is_aligned() {
        return INVALID_ARGUMENT;
    }
    unsafe {
        *output = ptr::null();
    }
    UNSUPPORTED
}
static API: CoreApi = CoreApi {
    header: Header::of::<CoreApi>(),
    initialize: Some(initialize),
    describe: Some(describe),
    probe: Some(probe),
    open: Some(open),
    input: Some(input),
    control: Some(control),
    state: Some(state),
    frame: Some(frame),
    cancel: Some(cancel),
    close: Some(close),
    extension: Some(extension),
};
/// AstraEMU C ABI entry point.
///
/// # Safety
///
/// `output` must be null or a valid, aligned pointer writable as `*const CoreApi`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn astra_emu_query(version: u32, output: *mut *const CoreApi) -> Status {
    if output.is_null() || !output.is_aligned() {
        return INVALID_ARGUMENT;
    }
    unsafe {
        *output = ptr::null();
    }
    if version != ABI_VERSION {
        return UNSUPPORTED;
    }
    unsafe {
        *output = &API;
    }
    OK
}
pub const QUERY: Query = astra_emu_query;

#[cfg(test)]
mod window_error_tests {
    use super::*;

    #[test]
    fn cancellation_and_stale_statuses_preserve_interruption() {
        for status in [CANCELLED, STALE_CONTEXT] {
            assert!(matches!(
                window_request_error(status, "interrupted".into()),
                WindowError::Interrupted(_)
            ));
        }
    }

    #[test]
    fn only_host_operation_failures_are_best_effort_candidates() {
        for status in [CORE_ERROR, UNSUPPORTED] {
            assert!(matches!(
                window_request_error(status, "host failed".into()),
                WindowError::Operation(_)
            ));
        }
        for status in [INVALID_ARGUMENT, INVALID_STATE, PANICKED, CAPACITY] {
            assert!(matches!(
                window_request_error(status, "protocol failed".into()),
                WindowError::Protocol(_)
            ));
        }
    }
}

#[cfg(test)]
mod localization_config_tests {
    use super::*;
    fn text(id: &'static str, kind: u32, value: &'static str) -> ConfigEntry {
        ConfigEntry {
            id: bytes(id),
            value: ConfigValue {
                kind,
                reserved: 0,
                integer: 0,
                number: 0.0,
                text: bytes(value),
            },
        }
    }
    #[test]
    fn display_options_are_typed_and_default_to_original_behavior() {
        let config = unsafe { parse_configuration(&[]) }.unwrap();
        assert_eq!(config.display, DisplayConfiguration::default());
        let entries = [
            text("text_encoding", VALUE_ENUM, "gbk"),
            text("font_path", VALUE_STRING, "fonts/public.ttc"),
            ConfigEntry {
                id: bytes("font_index"),
                value: ConfigValue {
                    kind: VALUE_INTEGER,
                    reserved: 0,
                    integer: 1,
                    number: 0.0,
                    text: Bytes::EMPTY,
                },
            },
        ];
        let config = unsafe { parse_configuration(&entries) }.unwrap();
        assert_eq!(config.display.text_encoding, DisplayEncoding::Gbk);
        assert_eq!(config.display.font_path, "fonts/public.ttc");
        assert_eq!(config.display.font_index, 1);
        assert!(!config.trace && !config.fail_on_stub);
    }
    #[test]
    fn descriptor_and_schema_satisfy_v3_contract() {
        INITIALIZED.store(true, Ordering::Release);
        let mut descriptor = std::mem::MaybeUninit::<Descriptor>::uninit();
        assert_eq!(
            unsafe { describe(descriptor.as_mut_ptr(), ptr::null_mut()) },
            OK
        );
        let descriptor = unsafe { descriptor.assume_init() };
        assert_eq!(unsafe { descriptor.validate() }, Ok(()));
        assert_eq!(descriptor.capabilities & CAP_RENDER_EXTENT, 0);
        assert_eq!(
            unsafe { descriptor.plugin_id.text(128) },
            Ok("ethornell-rs")
        );
        assert_eq!(
            unsafe { descriptor.display_name.text(64) },
            Ok("ethornell-rs")
        );
        assert_eq!(
            unsafe { descriptor.family_name.text(64) },
            Ok("Ethornell / BGI")
        );
        assert!(unsafe { validate_config(&SCHEMA.0, &[]) }.is_ok());
        assert_eq!(
            RenderExtentSupport::default().validate_request(
                descriptor.capabilities,
                Extent {
                    width: 1280,
                    height: 720
                }
            ),
            Err(INVALID_ARGUMENT)
        );
    }
    #[test]
    fn unknown_or_mistyped_display_encodings_are_not_silent_fallbacks() {
        for entry in [
            text("text_encoding", VALUE_ENUM, "automatic"),
            text("text_encoding", VALUE_STRING, "gbk"),
            text("font_path", VALUE_ENUM, "font.ttf"),
        ] {
            assert!(unsafe { parse_configuration(&[entry]) }.is_err());
        }
    }
}
