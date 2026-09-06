use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use smithay_client_toolkit::reexports::calloop::EventLoop;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{AxisScroll, BTN_LEFT, PointerEvent, PointerEventKind, PointerHandler},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};

use super::bubble::{LyricBubbleContent, LyricOrientation, load_font, render_lyric_bubble};

const FRAME_INTERVAL: Duration = Duration::from_millis(40);
const DRAG_THRESHOLD: f64 = 4.0;
const DEFAULT_MARGIN: i32 = 16;
const RENDER_SCREEN_WIDTH: u16 = 1920;
const RENDER_SCREEN_HEIGHT: u16 = 1080;

enum Command {
    Toggle(String),
    Update(String),
    SetOrientation(LyricOrientation),
    Shutdown,
}

#[derive(Clone)]
pub(super) struct WaylandBubbleSender {
    commands: Sender<Command>,
    alive: Arc<AtomicBool>,
}

impl WaylandBubbleSender {
    pub(super) fn toggle(&self, lyric: String) -> bool {
        self.send(Command::Toggle(lyric))
    }

    pub(super) fn update(&self, lyric: String) -> bool {
        self.send(Command::Update(lyric))
    }

    pub(super) fn set_vertical(&self, vertical: bool) -> bool {
        let orientation = if vertical {
            LyricOrientation::Vertical
        } else {
            LyricOrientation::Horizontal
        };
        self.send(Command::SetOrientation(orientation))
    }

    fn send(&self, command: Command) -> bool {
        self.alive.load(Ordering::Acquire) && self.commands.send(command).is_ok()
    }
}

pub(super) struct WaylandBubble {
    sender: WaylandBubbleSender,
    thread: Option<JoinHandle<()>>,
}

struct AliveGuard(Arc<AtomicBool>);

impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl WaylandBubble {
    pub(super) fn start(initial_lyric: String) -> Result<Self> {
        let (command_tx, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let alive = Arc::new(AtomicBool::new(true));
        let thread_alive = alive.clone();
        let thread = thread::Builder::new()
            .name("ctlyrics-wayland-lyric".to_string())
            .spawn(move || {
                let _alive = AliveGuard(thread_alive);
                if let Err(error) = run(initial_lyric, command_rx, ready_tx) {
                    tracing::debug!(%error, "Wayland lyric window stopped");
                }
            })
            .context("failed to start Wayland lyric window thread")?;
        ready_rx
            .recv_timeout(Duration::from_secs(2))
            .context("Wayland lyric window initialization timed out")??;
        Ok(Self {
            sender: WaylandBubbleSender {
                commands: command_tx,
                alive,
            },
            thread: Some(thread),
        })
    }

    pub(super) fn sender(&self) -> WaylandBubbleSender {
        self.sender.clone()
    }

    pub(super) fn shutdown(mut self) {
        let _ = self.sender.commands.send(Command::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

enum ShellSurface {
    Layer(LayerSurface),
    Xdg(Window),
}

impl ShellSurface {
    fn wl_surface(&self) -> &wl_surface::WlSurface {
        match self {
            Self::Layer(layer) => layer.wl_surface(),
            Self::Xdg(window) => window.wl_surface(),
        }
    }

    fn commit(&self) {
        match self {
            Self::Layer(layer) => layer.commit(),
            Self::Xdg(window) => window.commit(),
        }
    }

    fn set_size(&self, width: u32, height: u32) {
        match self {
            Self::Layer(layer) => layer.set_size(width, height),
            Self::Xdg(window) => {
                window.set_min_size(Some((width, height)));
                window.set_max_size(Some((width, height)));
            }
        }
    }
}

struct DragState {
    grab: (f64, f64),
    serial: u32,
    started: bool,
}

struct State {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    compositor: CompositorState,
    layer_shell: Option<LayerShell>,
    xdg_shell: Option<XdgShell>,
    shm: Shm,
    pool: SlotPool,
    buffer: Option<Buffer>,
    buffer_size: (u16, u16),
    shell: Option<ShellSurface>,
    pointer: Option<wl_pointer::WlPointer>,
    pointer_seat: Option<wl_seat::WlSeat>,
    active_output: Option<wl_output::WlOutput>,
    screen_size: (u16, u16),
    content: Option<LyricBubbleContent>,
    lyric: String,
    orientation: LyricOrientation,
    position: Option<(i32, i32)>,
    drag: Option<DragState>,
    visible: bool,
    configured: bool,
    dirty: bool,
    frame_ready: bool,
    exit: bool,
    animation_started: Instant,
    last_draw: Instant,
}

fn run(
    initial_lyric: String,
    commands: Receiver<Command>,
    ready: mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    let (mut event_loop, qh, mut state) = match initialize(initial_lyric) {
        Ok(initialized) => initialized,
        Err(error) => {
            let message = error.to_string();
            let _ = ready.send(Err(anyhow!(message.clone())));
            return Err(anyhow!(message));
        }
    };
    let _ = ready.send(Ok(()));

    while !state.exit {
        loop {
            match commands.try_recv() {
                Ok(command) => state.apply(command, &qh),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    state.exit = true;
                    break;
                }
            }
        }
        event_loop.dispatch(Duration::from_millis(20), &mut state)?;
        if state.visible
            && state.configured
            && state.frame_ready
            && (state.dirty || state.last_draw.elapsed() >= FRAME_INTERVAL)
        {
            state.draw(&qh)?;
        }
    }
    Ok(())
}

fn initialize(
    initial_lyric: String,
) -> Result<(EventLoop<'static, State>, QueueHandle<State>, State)> {
    let connection = Connection::connect_to_env()?;
    let (globals, event_queue) = registry_queue_init(&connection)?;
    let qh = event_queue.handle();
    let event_loop: EventLoop<State> = EventLoop::try_new()?;
    WaylandSource::new(connection.clone(), event_queue)
        .insert(event_loop.handle())
        .map_err(|error| anyhow!(error.to_string()))?;
    let compositor = CompositorState::bind(&globals, &qh)?;
    let layer_shell = LayerShell::bind(&globals, &qh).ok();
    let xdg_shell = XdgShell::bind(&globals, &qh).ok();
    if layer_shell.is_none() && xdg_shell.is_none() {
        return Err(anyhow!("neither layer shell nor xdg shell is available"));
    }
    let shm = Shm::bind(&globals, &qh)?;
    let font = load_font(&initial_lyric).context("no usable lyric font")?;
    let content = Some(LyricBubbleContent::new(&font, &initial_lyric));
    let pool = SlotPool::new(4096, &shm)?;
    let state = State {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        compositor,
        layer_shell,
        xdg_shell,
        shm,
        pool,
        buffer: None,
        buffer_size: (0, 0),
        shell: None,
        pointer: None,
        pointer_seat: None,
        active_output: None,
        screen_size: (RENDER_SCREEN_WIDTH, RENDER_SCREEN_HEIGHT),
        content,
        lyric: initial_lyric,
        orientation: LyricOrientation::Horizontal,
        position: None,
        drag: None,
        visible: false,
        configured: false,
        dirty: false,
        frame_ready: true,
        exit: false,
        animation_started: Instant::now(),
        last_draw: Instant::now(),
    };
    Ok((event_loop, qh, state))
}

impl State {
    fn apply(&mut self, command: Command, qh: &QueueHandle<Self>) {
        match command {
            Command::Toggle(lyric) => {
                if self.lyric != lyric {
                    self.lyric = lyric;
                    self.rebuild_content();
                }
                if self.visible {
                    self.hide();
                } else {
                    if self.position.is_none() {
                        self.position = Some((DEFAULT_MARGIN, DEFAULT_MARGIN));
                    }
                    self.visible = true;
                    self.create_surface(qh);
                }
            }
            Command::Update(lyric) => {
                self.lyric = lyric;
                self.rebuild_content();
                self.animation_started = Instant::now();
                self.dirty = true;
                self.update_size();
            }
            Command::SetOrientation(orientation) => {
                self.orientation = orientation;
                self.animation_started = Instant::now();
                self.dirty = true;
                self.update_size();
            }
            Command::Shutdown => self.exit = true,
        }
    }

    fn rebuild_content(&mut self) {
        self.content = load_font(&self.lyric)
            .as_ref()
            .map(|font| LyricBubbleContent::new(font, &self.lyric));
    }

    fn rendered(&self) -> Option<(Vec<u8>, u16, u16)> {
        self.content.as_ref().map(|content| {
            render_lyric_bubble(
                content,
                self.orientation,
                self.animation_started.elapsed(),
                self.screen_size.0,
                self.screen_size.1,
            )
        })
    }

    fn create_surface(&mut self, qh: &QueueHandle<Self>) {
        if self.shell.is_some() {
            return;
        }
        let surface = self.compositor.create_surface(qh);
        let (width, height) = self
            .rendered()
            .map(|(_, width, height)| (width, height))
            .unwrap_or((80, 42));
        self.configured = false;
        self.buffer = None;
        self.buffer_size = (0, 0);
        self.frame_ready = true;
        if let Some(layer_shell) = &self.layer_shell {
            let layer = layer_shell.create_layer_surface(
                qh,
                surface,
                Layer::Top,
                Some("ctlyrics-lyric"),
                None,
            );
            let (x, y) = self.position.unwrap_or((DEFAULT_MARGIN, DEFAULT_MARGIN));
            layer.set_anchor(Anchor::TOP | Anchor::LEFT);
            layer.set_keyboard_interactivity(KeyboardInteractivity::None);
            layer.set_exclusive_zone(-1);
            layer.set_margin(y, 0, 0, x);
            layer.set_size(u32::from(width), u32::from(height));
            layer.commit();
            self.shell = Some(ShellSurface::Layer(layer));
        } else {
            let Some(xdg_shell) = &self.xdg_shell else {
                self.visible = false;
                return;
            };
            let window = xdg_shell.create_window(surface, WindowDecorations::None, qh);
            window.set_title("ctlyrics lyrics");
            window.set_app_id("ctlyrics");
            window.set_min_size(Some((u32::from(width), u32::from(height))));
            window.set_max_size(Some((u32::from(width), u32::from(height))));
            window.commit();
            self.shell = Some(ShellSurface::Xdg(window));
        }
    }

    fn hide(&mut self) {
        self.shell = None;
        self.buffer = None;
        self.buffer_size = (0, 0);
        self.drag = None;
        self.visible = false;
        self.configured = false;
        self.frame_ready = true;
    }

    fn update_size(&mut self) {
        let rendered = self.rendered();
        let Some(shell) = &self.shell else {
            return;
        };
        let Some((_, width, height)) = rendered else {
            return;
        };
        if let ShellSurface::Layer(layer) = shell {
            let position = clamp_position(
                self.position.unwrap_or((DEFAULT_MARGIN, DEFAULT_MARGIN)),
                (width, height),
                self.screen_size,
            );
            self.position = Some(position);
            layer.set_margin(position.1, 0, 0, position.0);
        }
        shell.set_size(u32::from(width), u32::from(height));
        shell.commit();
    }

    fn draw(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let Some((rgba, width, height)) = self.rendered() else {
            return Ok(());
        };
        let Some(surface) = self.shell.as_ref().map(|shell| shell.wl_surface().clone()) else {
            return Ok(());
        };
        if self.buffer_size != (width, height) {
            self.buffer = None;
            self.buffer_size = (width, height);
        }
        let stride = i32::from(width) * 4;
        if self.buffer.is_none() {
            self.buffer = Some(
                self.pool
                    .create_buffer(
                        i32::from(width),
                        i32::from(height),
                        stride,
                        wl_shm::Format::Argb8888,
                    )?
                    .0,
            );
        }
        let buffer = self
            .buffer
            .as_mut()
            .context("missing Wayland lyric buffer")?;
        let Some(canvas) = self.pool.canvas(buffer) else {
            self.dirty = true;
            return Ok(());
        };
        for (source, destination) in rgba.chunks_exact(4).zip(canvas.chunks_exact_mut(4)) {
            destination.copy_from_slice(&[source[2], source[1], source[0], source[3]]);
        }
        surface.damage_buffer(0, 0, i32::from(width), i32::from(height));
        surface.frame(qh, surface.clone());
        buffer.attach_to(&surface)?;
        surface.commit();
        self.dirty = false;
        self.frame_ready = false;
        self.last_draw = Instant::now();
        Ok(())
    }

    fn move_layer(&mut self, pointer_position: (f64, f64)) {
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        let delta = (
            pointer_position.0 - drag.grab.0,
            pointer_position.1 - drag.grab.1,
        );
        if !drag.started && delta.0.abs().max(delta.1.abs()) < DRAG_THRESHOLD {
            return;
        }
        drag.started = true;
        let Some(ShellSurface::Layer(layer)) = &self.shell else {
            return;
        };
        let (x, y) = self.position.unwrap_or((DEFAULT_MARGIN, DEFAULT_MARGIN));
        let size = self
            .rendered()
            .map(|(_, width, height)| (width, height))
            .unwrap_or((80, 42));
        let position = clamp_position(
            (
                (f64::from(x) + delta.0).round() as i32,
                (f64::from(y) + delta.1).round() as i32,
            ),
            size,
            self.screen_size,
        );
        self.position = Some(position);
        layer.set_margin(position.1, 0, 0, position.0);
        layer.commit();
    }

    fn pointer_axis(&mut self, vertical: AxisScroll) {
        let delta = if vertical.discrete != 0 {
            f64::from(vertical.discrete)
        } else {
            vertical.absolute
        };
        if delta == 0.0 {
            return;
        }
        self.orientation = if delta > 0.0 {
            LyricOrientation::Vertical
        } else {
            LyricOrientation::Horizontal
        };
        self.animation_started = Instant::now();
        self.dirty = true;
        self.update_size();
    }

    fn update_output_metrics(&mut self, output: &wl_output::WlOutput) {
        let Some((width, height)) = self
            .output_state
            .info(output)
            .and_then(|info| info.logical_size)
        else {
            return;
        };
        let (Ok(width), Ok(height)) = (u16::try_from(width), u16::try_from(height)) else {
            return;
        };
        self.screen_size = (width, height);
        self.dirty = true;
        self.update_size();
    }
}

fn clamp_position(
    position: (i32, i32),
    bubble_size: (u16, u16),
    screen_size: (u16, u16),
) -> (i32, i32) {
    (
        position
            .0
            .clamp(0, i32::from(screen_size.0.saturating_sub(bubble_size.0))),
        position
            .1
            .clamp(0, i32::from(screen_size.1.saturating_sub(bubble_size.1))),
    )
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _factor: i32,
    ) {
        self.dirty = true;
    }

    fn transform_changed(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.frame_ready = true;
    }

    fn surface_enter(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        if self
            .shell
            .as_ref()
            .is_some_and(|shell| shell.wl_surface() == surface)
        {
            self.active_output = Some(output.clone());
            self.update_output_metrics(output);
        }
    }

    fn surface_leave(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        output: &wl_output::WlOutput,
    ) {
        if self
            .shell
            .as_ref()
            .is_some_and(|shell| shell.wl_surface() == surface)
            && self.active_output.as_ref() == Some(output)
        {
            self.active_output = None;
        }
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if self.active_output.as_ref() == Some(&output) {
            self.update_output_metrics(&output);
        }
    }

    fn output_destroyed(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if self.active_output.as_ref() == Some(&output) {
            self.active_output = None;
        }
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
    ) {
    }

    fn new_capability(
        &mut self,
        _connection: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => {
                    self.pointer = Some(pointer);
                    self.pointer_seat = Some(seat);
                }
                Err(error) => tracing::debug!(%error, "failed to create Wayland pointer"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Pointer && self.pointer_seat.as_ref() == Some(&seat) {
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
            self.pointer_seat = None;
            self.drag = None;
        }
    }

    fn remove_seat(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
    ) {
        if self.pointer_seat.as_ref() == Some(&seat) {
            self.pointer = None;
            self.pointer_seat = None;
            self.drag = None;
        }
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for (index, event) in events.iter().enumerate() {
            if self
                .shell
                .as_ref()
                .is_none_or(|shell| &event.surface != shell.wl_surface())
            {
                continue;
            }
            match event.kind {
                PointerEventKind::Press {
                    time: _,
                    button,
                    serial,
                } if button == BTN_LEFT => {
                    self.drag = Some(DragState {
                        grab: event.position,
                        serial,
                        started: false,
                    });
                }
                PointerEventKind::Motion { .. } => {
                    if events[index + 1..]
                        .iter()
                        .any(|later| matches!(later.kind, PointerEventKind::Motion { .. }))
                    {
                        continue;
                    }
                    let is_xdg = matches!(self.shell, Some(ShellSurface::Xdg(_)));
                    let should_start = self.drag.as_ref().is_some_and(|drag| {
                        !drag.started
                            && (event.position.0 - drag.grab.0)
                                .abs()
                                .max((event.position.1 - drag.grab.1).abs())
                                >= DRAG_THRESHOLD
                    });
                    if is_xdg && should_start {
                        if let (Some(ShellSurface::Xdg(window)), Some(seat), Some(drag)) =
                            (&self.shell, &self.pointer_seat, self.drag.as_mut())
                        {
                            window.move_(seat, drag.serial);
                            drag.started = true;
                        }
                    } else if !is_xdg {
                        self.move_layer(event.position);
                    }
                }
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    let clicked = self.drag.take().is_some_and(|drag| !drag.started);
                    if clicked {
                        self.hide();
                    }
                }
                PointerEventKind::Axis { vertical, .. } => self.pointer_axis(vertical),
                _ => {}
            }
        }
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _connection: &Connection, qh: &QueueHandle<Self>, layer: &LayerSurface) {
        if matches!(&self.shell, Some(ShellSurface::Layer(current)) if current == layer) {
            self.shell = None;
            self.layer_shell = None;
            self.configured = false;
            if self.visible {
                self.create_surface(qh);
            }
        }
    }

    fn configure(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        _configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if matches!(&self.shell, Some(ShellSurface::Layer(current)) if current == layer) {
            self.configured = true;
            self.dirty = true;
        }
    }
}

impl WindowHandler for State {
    fn request_close(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        window: &Window,
    ) {
        if matches!(&self.shell, Some(ShellSurface::Xdg(current)) if current == window) {
            self.hide();
        }
    }

    fn configure(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        window: &Window,
        _configure: WindowConfigure,
        _serial: u32,
    ) {
        if matches!(&self.shell, Some(ShellSurface::Xdg(current)) if current == window) {
            self.configured = true;
            self.dirty = true;
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_registry!(State);

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(State);
delegate_output!(State);
delegate_shm!(State);
delegate_seat!(State);
delegate_pointer!(State);
delegate_layer!(State);
delegate_xdg_shell!(State);
delegate_xdg_window!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragged_position_is_clamped_to_the_render_area() {
        assert_eq!(
            clamp_position((-20, 2000), (100, 80), (1920, 1080)),
            (0, 1000)
        );
    }
}
