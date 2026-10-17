//! Windowless frontend for the original BGI runtime and GPU compositor.
use super::*;
use std::cell::RefCell;
use std::rc::Rc;

pub const FRAME_INTERVAL: Duration = Duration::from_millis(NATIVE_TICK_MS);

pub enum WindowCommand<'a> {
    Title(&'a str),
    Icon { width: u32, height: u32, rgba: &'a [u8] },
    Fullscreen(bool),
    Size(u32, u32),
    Position(i32, i32),
    CursorPosition(i32, i32),
    CursorVisible(bool),
    Visible(bool),
    Minimized(bool),
}


#[derive(Default)]
pub struct WindowFeedback {
    pub size: Option<(u32, u32)>,
    pub fullscreen: Option<bool>,
}
pub trait HostWindow {
    fn command(&mut self, command: WindowCommand<'_>) -> std::result::Result<WindowFeedback, String>;
    fn message(&mut self, kind: u32, title: &str, text: &str, default_first: bool) -> std::result::Result<Option<bool>, String>;
    fn prompt(&mut self, title: &str, prompt: &str, initial: &str) -> std::result::Result<Option<String>, String>;
    fn choose(&mut self, title: &str, prompt: &str, options: &[String]) -> std::result::Result<Option<String>, String>;
    fn cancelled(&self) -> bool;
}

struct DialogBridge {
    host: Rc<RefCell<dyn HostWindow>>,
    error: Option<String>,
}
thread_local! {
    static DIALOG: RefCell<Option<DialogBridge>> = const { RefCell::new(None) };
}

pub(crate) fn unsupported_dialog(operation: &str) -> bool {
    DIALOG.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(bridge) = slot.as_mut() {
            bridge.error.get_or_insert_with(|| format!("embedded host does not support native {operation}"));
            true
        } else { false }
    })
}

pub(crate) fn message_dialog(kind: u32, title: &str, text: &str, default_first: bool) -> Option<Option<bool>> {
    DIALOG.with(|slot| {
        let mut slot = slot.borrow_mut();
        let bridge = slot.as_mut()?;
        Some(match bridge.host.borrow_mut().message(kind, title, text, default_first) {
            Ok(answer) => answer,
            Err(error) => { bridge.error = Some(error); None }
        })
    })
}

pub(crate) fn text_dialog(title: &str, prompt: &str, initial: &str) -> Option<Option<String>> {
    DIALOG.with(|slot| {
        let mut slot = slot.borrow_mut();
        let bridge = slot.as_mut()?;
        Some(match bridge.host.borrow_mut().prompt(title, prompt, initial) {
            Ok(answer) => answer,
            Err(error) => { bridge.error = Some(error); None }
        })
    })
}

pub(crate) fn item_dialog(title: &str, prompt: &str, options: &[String]) -> Option<Option<String>> {
    DIALOG.with(|slot| {
        let mut slot = slot.borrow_mut();
        let bridge = slot.as_mut()?;
        Some(match bridge.host.borrow_mut().choose(title, prompt, options) {
            Ok(answer) => answer,
            Err(error) => { bridge.error = Some(error); None }
        })
    })
}

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub enum Input {
    PointerMove { x: f32, y: f32 },
    PointerButton { button: u32, pressed: bool, x: f32, y: f32 },
    Wheel(f32),
    Key { physical: String, logical: String, pressed: bool },
    Window { width: u32, height: u32, focused: bool, suspended: bool },
}

pub struct Embedded {
    runtime: RuntimeEngine,
    renderer: Renderer<'static>,
    textures: BTreeMap<String, CachedGraphTexture>,
    audio: AudioSystem,
    input: VecDeque<RuntimeInputEvent>,
    host: Rc<RefCell<dyn HostWindow>>,
    suspended_audio: Vec<i32>,
    paused: bool,
    suspended: bool,
    last_frame: Instant,
}

impl Embedded {
    pub fn open(root: &Path, trace: bool, fail_on_stub: bool, host: Rc<RefCell<dyn HostWindow>>) -> Result<Self> {
        let check_cancelled = || {
            if host.borrow().cancelled() { Err(EthornellError::Other("open cancelled".into())) } else { Ok(()) }
        };
        check_cancelled()?;
        let manager = ResourceManager::open_game(root)?;
        check_cancelled()?;
        let bytes = manager.read_decoded_from_archive("system.arc", "ipl._bp")?;
        check_cancelled()?;
        let native_root = game_root_path(&manager);
        let mut runtime = RuntimeEngine::new("system.arc:ipl._bp", &bytes, manager, native_root, trace, fail_on_stub, true);
        check_cancelled()?;
        let width = runtime.api.screen_width.max(1) as u32;
        let height = runtime.api.screen_height.max(1) as u32;
        let renderer = pollster::block_on(Renderer::offscreen(width, height))?;
        runtime.api.graphics_memory_metric = renderer.graphics_memory_metric();
        check_cancelled()?;
        let audio = AudioSystem::new()?;
        check_cancelled()?;
        host.borrow_mut().command(WindowCommand::Title("ethornell-rs runtime")).map_err(EthornellError::Other)?;
        if let Some(icon) = crate::window_icon::load(&runtime.api.native_root) {
            let icon = if icon.width() > 256 || icon.height() > 256 {
                image::imageops::thumbnail(&icon, 256, 256)
            } else { icon };
            host.borrow_mut().command(WindowCommand::Icon { width: icon.width(), height: icon.height(), rgba: icon.as_raw() })
                .map_err(EthornellError::Other)?;
        }
        check_cancelled()?;
        DIALOG.with(|slot| *slot.borrow_mut() = Some(DialogBridge { host: host.clone(), error: None }));
        Ok(Self { runtime, renderer, textures: BTreeMap::new(), audio, input: VecDeque::new(), host,
            suspended_audio: Vec::new(), paused: false, suspended: false, last_frame: Instant::now() })
    }

    pub fn dimensions(&self) -> (u32, u32) { self.renderer.surface_size() }
    pub fn is_paused(&self) -> bool { self.paused || self.suspended }
    pub fn is_finished(&self) -> bool { self.runtime.api.quit_requested }
    pub fn shutdown(&mut self) -> Result<()> { self.audio.shutdown() }

    fn set_pause_state(&mut self, paused: bool, suspended: bool) {
        let before = self.is_paused();
        self.paused = paused;
        self.suspended = suspended;
        match (before, self.is_paused()) {
            (false, true) => {
                self.suspended_audio = self.audio.suspend_playing();
                self.input.clear();
                reset_input(&mut self.runtime.api);
            }
            (true, false) => {
                self.audio.resume_suspended(&self.suspended_audio);
                self.suspended_audio.clear();
            }
            _ => {}
        }
        if before != self.is_paused() { self.last_frame = Instant::now(); }
    }
    pub fn pause(&mut self, paused: bool) { self.set_pause_state(paused, self.suspended); }

    pub fn input(&mut self, event: Input) -> Result<()> {
        let api = &mut self.runtime.api;
        let event = match event {
            Input::Window { width, height, focused, suspended } => {
                api.window_surface_width = width.min(i32::MAX as u32) as i32;
                api.window_surface_height = height.min(i32::MAX as u32) as i32;
                api.window_focused = focused;
                if !focused {
                    self.input.clear();
                    reset_input(api);
                }
                self.set_pause_state(self.paused, suspended);
                return Ok(());
            }
            _ if self.paused || self.suspended => return Ok(()),
            Input::PointerMove { x, y } => RuntimeInputEvent::MouseMove { x, y },
            Input::PointerButton { button: 1, pressed, x, y } => {
                if api.handle_user_modeless_pointer(x, y, pressed) { return Ok(()); }
                if pressed { RuntimeInputEvent::MousePress { x, y } } else { RuntimeInputEvent::MouseRelease { x, y } }
            }
            Input::PointerButton { button, pressed, x, y } => {
                let button = match button { 2 => 2, 3 => 4, _ => return Err(EthornellError::Other("unsupported pointer button".into())) };
                if pressed { RuntimeInputEvent::ButtonPress { button, x, y } } else { RuntimeInputEvent::ButtonRelease { button, x, y } }
            }
            Input::Wheel(delta_y) => RuntimeInputEvent::MouseWheel { delta_y },
            Input::Key { physical, logical, pressed } => {
                let code = match physical.as_str() {
                    "ArrowLeft" => Some(KeyCode::ArrowLeft), "ArrowRight" => Some(KeyCode::ArrowRight),
                    "ArrowUp" => Some(KeyCode::ArrowUp), "ArrowDown" => Some(KeyCode::ArrowDown),
                    "Enter" => Some(KeyCode::Enter), "NumpadEnter" => Some(KeyCode::NumpadEnter),
                    "Space" => Some(KeyCode::Space), "Backspace" => Some(KeyCode::Backspace),
                    "Delete" => Some(KeyCode::Delete), "Home" => Some(KeyCode::Home), "End" => Some(KeyCode::End),
                    _ => None,
                };
                if pressed && (code.is_some_and(|code| api.handle_native_edit_key(code))
                    || (!logical.is_empty() && (logical != physical || logical.chars().count() == 1)
                        && logical.chars().all(|c| !c.is_control()) && api.append_native_edit_text(&logical))) {
                    return Ok(());
                }
                if api.has_visible_user_modeless_dialog() { return Ok(()); }
                let Some(descriptor) = code.and_then(input_descriptor_for_keycode) else { return Ok(()); };
                if pressed { RuntimeInputEvent::KeyPress { descriptor } } else { RuntimeInputEvent::KeyRelease { descriptor } }
            }
        };
        queue_runtime_input_event(&mut self.input, event);
        Ok(())
    }

    /// One original cooperative scheduler pass; called by the core-owned loop.
    pub fn tick(&mut self) -> Result<Option<Frame>> {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last_frame).as_millis().min(u64::MAX as u128) as u64;
        self.last_frame = now;
        if self.is_paused() { return Ok(None); }
        let report = runtime_frontend::drive_runtime_frame(&mut self.runtime, &mut self.input, &mut None,
            RuntimeFramePacing, normalize_engine_elapsed_ms(elapsed), elapsed, Some(&mut self.audio), "embedded");
        if let Some(error) = DIALOG.with(|slot| slot.borrow_mut().as_mut().and_then(|bridge| bridge.error.take())) {
            return Err(EthornellError::Other(error));
        }
        if let Some(report) = report.last_report.as_ref() && report.stop_reason.is_fatal() {
            return Err(EthornellError::Other(format!("native VM failed at {} pc={} offset={:?}: {:?}", report.program, report.pc, report.offset, report.stop_reason)));
        }
        self.apply_window_requests()?;
        let api = &mut self.runtime.api;
        if std::mem::take(&mut api.pending_window_close_request) && api.handle_host_window_close("embedded") {
            api.quit_requested = true;
        }
        let width = api.screen_width.max(1) as u32;
        let height = api.screen_height.max(1) as u32;
        if width > 16384 || height > 16384 || u64::from(width) * u64::from(height) * 4 > 512 * 1024 * 1024 {
            return Err(EthornellError::Other("native frame exceeds host surface limits".into()));
        }
        if self.renderer.surface_size() != (width, height) { self.renderer.resize(width, height); }
        self.renderer.set_virtual_size(width as f32, height as f32);
        let commands = crate::composition::compose_runtime_frame(&mut self.runtime, &mut self.renderer, &mut self.textures,
            vec![RenderCommand::Clear { color: [0.0, 0.0, 0.0, 1.0] }]);
        self.renderer.submit(&commands);
        let host = &self.host;
        let rgba = self.renderer.capture_rgba(|| host.borrow().cancelled())?;
        self.runtime.api.presentation_state = self.runtime.api.engine_time_ms.min(i32::MAX as u64) as i32;
        Ok(Some(Frame { width, height, rgba }))
    }

    fn apply_window_requests(&mut self) -> Result<()> {
        let api = &mut self.runtime.api;
        let mut host = self.host.borrow_mut();
        let mut actual_size = None;
        let mut actual_fullscreen = None;
        let pending_title = api.pending_window_title.take();
        let mut command = |request| -> Result<()> {
            let feedback = host.command(request).map_err(EthornellError::Other)?;
            if feedback.size.is_some() { actual_size = feedback.size; }
            if feedback.fullscreen.is_some() { actual_fullscreen = feedback.fullscreen; }
            Ok(())
        };
        if let Some(title) = pending_title.as_ref() { command(WindowCommand::Title(title))?; }
        if let Some(value) = api.pending_fullscreen.take() { command(WindowCommand::Fullscreen(value))?; }
        if let Some((width, height)) = api.pending_window_size.take() { command(WindowCommand::Size(width, height))?; }
        if let Some((x, y)) = api.pending_window_position.take() { command(WindowCommand::Position(x, y))?; }
        if let Some(value) = api.pending_cursor_visible.take() { command(WindowCommand::CursorVisible(value))?; }
        if let Some((x, y)) = api.pending_cursor_position.take() { command(WindowCommand::CursorPosition(x as i32, y as i32))?; }
        if let Some(visible) = api.pending_window_visible.take() { command(WindowCommand::Visible(visible))?; }
        if std::mem::take(&mut api.pending_window_minimize) { command(WindowCommand::Minimized(true))?; }
        if let Some((width, height)) = actual_size {
            api.window_surface_width = width.min(i32::MAX as u32) as i32;
            api.window_surface_height = height.min(i32::MAX as u32) as i32;
        }
        if let Some(fullscreen) = actual_fullscreen { api.window_mode = i32::from(fullscreen); }
        Ok(())
    }
}

impl Drop for Embedded {
    fn drop(&mut self) {
        if let Err(error) = self.audio.shutdown() {
            tracing::error!(%error, "native audio shutdown failed after joining workers");
        }
        DIALOG.with(|slot| *slot.borrow_mut() = None);
    }
}

fn reset_input(api: &mut RuntimeTraceApi) {
    api.clear_native_input_records();
    api.input_physically_held.clear();
    api.pending_input_state = None;
    api.pending_input_descriptor = None;
    api.pending_click = None;
    api.pending_object_state = None;
    api.mouse_pressed = false;
    api.finish_graph_knob_drag();
}
