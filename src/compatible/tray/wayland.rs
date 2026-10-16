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
    Connection, Dispatch, QueueHandle, WEnum,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};
use smithay_client_toolkit::reexports::protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_relative_pointer, delegate_seat, delegate_shm, delegate_xdg_shell,
    delegate_xdg_window,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        pointer::{AxisScroll, BTN_LEFT, PointerEvent, PointerEventKind, PointerHandler},
        relative_pointer::{RelativeMotionEvent, RelativePointerHandler, RelativePointerState},
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

use super::bubble::{
    LyricBubbleContent, LyricOrientation, adjusted_font_size, load_font, load_font_size,
    save_font_size,
};
use super::{
    ExitSignal, PlaybackInfo,
    bubble_controls::{self, BubbleControls, BubbleFrame, BubbleLayout, BubbleRect},
    controls::ControlPanel,
};
use crate::i18n::Locale;

const FRAME_INTERVAL: Duration = Duration::from_millis(40);
const DRAG_FRAME_INTERVAL: Duration = Duration::from_millis(16);
const DRAG_THRESHOLD: f64 = 4.0;
const DEFAULT_MARGIN: i32 = 16;
const RENDER_SCREEN_WIDTH: u16 = 1920;
const RENDER_SCREEN_HEIGHT: u16 = 1080;

pub(super) enum Command {
    Toggle(String),
    Update(String),
    SetOrientation(LyricOrientation),
    ShowControls(Option<(i32, i32)>),
    Shutdown,
}

#[derive(Clone)]
pub(super) struct WaylandBubbleSender {
    commands: Sender<Command>,
    alive: Arc<AtomicBool>,
}

impl WaylandBubbleSender {
    pub(super) fn context_menu(&self, x: i32, y: i32) -> bool {
        self.send(Command::ShowControls(Some((x, y))))
    }

    #[cfg(test)]
    pub(super) fn test_channel() -> (Self, Receiver<Command>) {
        let (commands, receiver) = mpsc::channel();
        (
            Self {
                commands,
                alive: Arc::new(AtomicBool::new(true)),
            },
            receiver,
        )
    }

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
    pub(super) fn start(locale: Locale, playback: PlaybackInfo, exit: ExitSignal) -> Result<Self> {
        let (command_tx, command_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let alive = Arc::new(AtomicBool::new(true));
        let thread_alive = alive.clone();
        let thread = thread::Builder::new()
            .name("ctlyrics-wayland-lyric".to_string())
            .spawn(move || {
                let _alive = AliveGuard(thread_alive);
                if let Err(error) = run(locale, playback, exit, command_rx, ready_tx) {
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

pub(super) enum ShellSurface {
    Layer(LayerSurface),
    Xdg(Window),
}

impl ShellSurface {
    pub(super) fn wl_surface(&self) -> &wl_surface::WlSurface {
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

    pub(super) fn set_size(&self, width: u32, height: u32) {
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
    origin: (i32, i32),
    displacement: (f64, f64),
}

/// All positions and sizes here are compositor logical coordinates, not physical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OutputGeometry {
    origin: (i32, i32),
    size: (u16, u16),
}

impl OutputGeometry {
    fn to_global(self, local: (i32, i32)) -> (i32, i32) {
        (
            self.origin.0.saturating_add(local.0),
            self.origin.1.saturating_add(local.1),
        )
    }

    fn to_local(self, global: (i32, i32)) -> (i32, i32) {
        (
            global.0.saturating_sub(self.origin.0),
            global.1.saturating_sub(self.origin.1),
        )
    }

    fn contains(self, point: (f64, f64)) -> bool {
        point.0 >= f64::from(self.origin.0)
            && point.1 >= f64::from(self.origin.1)
            && point.0 < f64::from(self.origin.0) + f64::from(self.size.0)
            && point.1 < f64::from(self.origin.1) + f64::from(self.size.1)
    }

    fn distance_squared(self, point: (f64, f64)) -> f64 {
        let dx = point.0
            - point.0.clamp(
                f64::from(self.origin.0),
                f64::from(self.origin.0) + f64::from(self.size.0),
            );
        let dy = point.1
            - point.1.clamp(
                f64::from(self.origin.1),
                f64::from(self.origin.1) + f64::from(self.size.1),
            );
        dx * dx + dy * dy
    }
}

#[derive(Clone, Copy)]
struct DragBounds {
    min: (f64, f64),
    max: (f64, f64),
}

impl DragBounds {
    fn for_outputs(outputs: &[OutputGeometry], bubble: (u16, u16)) -> Option<Self> {
        let first = outputs.first()?;
        let mut min = (f64::from(first.origin.0), f64::from(first.origin.1));
        let mut max = min;
        for output in outputs {
            min.0 = min.0.min(f64::from(output.origin.0));
            min.1 = min.1.min(f64::from(output.origin.1));
            max.0 = max
                .0
                .max(f64::from(output.origin.0) + f64::from(output.size.0));
            max.1 = max
                .1
                .max(f64::from(output.origin.1) + f64::from(output.size.1));
        }
        Some(Self {
            min,
            max: (
                (max.0 - f64::from(bubble.0)).max(min.0),
                (max.1 - f64::from(bubble.1)).max(min.1),
            ),
        })
    }
}

fn drag_output(
    outputs: &[OutputGeometry],
    pointer: (f64, f64),
    preferred: Option<usize>,
) -> Option<usize> {
    // Keep mirrored/overlapping outputs stable, but use half-open bounds at shared edges.
    if let Some(index) = preferred.filter(|&index| {
        outputs
            .get(index)
            .is_some_and(|output| output.contains(pointer))
    }) {
        return Some(index);
    }
    outputs
        .iter()
        .position(|output| output.contains(pointer))
        .or_else(|| {
            outputs
                .iter()
                .enumerate()
                .min_by(|(a, left), (b, right)| {
                    left.distance_squared(pointer)
                        .total_cmp(&right.distance_squared(pointer))
                        .then_with(|| (Some(*b) == preferred).cmp(&(Some(*a) == preferred)))
                })
                .map(|(index, _)| index)
        })
}

impl DragState {
    fn new(grab: (f64, f64), serial: u32, origin: (i32, i32)) -> Self {
        Self {
            grab,
            serial,
            started: false,
            origin,
            displacement: (0.0, 0.0),
        }
    }

    fn relative_motion(&mut self, delta: (f64, f64), bounds: DragBounds) -> Option<(i32, i32)> {
        self.displacement.0 += delta.0;
        self.displacement.1 += delta.1;
        if !self.started
            && self.displacement.0.abs().max(self.displacement.1.abs()) < DRAG_THRESHOLD
        {
            return None;
        }
        self.started = true;
        let x = (f64::from(self.origin.0) + self.displacement.0).clamp(bounds.min.0, bounds.max.0);
        let y = (f64::from(self.origin.1) + self.displacement.1).clamp(bounds.min.1, bounds.max.1);
        // Retain fractional motion, but discard overshoot so reversing at an edge is immediate.
        self.displacement = (x - f64::from(self.origin.0), y - f64::from(self.origin.1));
        Some((x.round() as i32, y.round() as i32))
    }
}

pub(super) struct State {
    registry_state: RegistryState,
    output_state: OutputState,
    seat_state: SeatState,
    relative_pointer_state: RelativePointerState,
    compositor: CompositorState,
    layer_shell: Option<LayerShell>,
    xdg_shell: Option<XdgShell>,
    shm: Shm,
    pool: SlotPool,
    buffer: Option<Buffer>,
    buffer_size: (u16, u16),
    shell: Option<ShellSurface>,
    // Keep the initial layer mapped until button release: destroying/unmapping it
    // during an output handoff would cancel the compositor's implicit pointer grab.
    drag_source: Option<ShellSurface>,
    pointer: Option<wl_pointer::WlPointer>,
    relative_pointer: Option<zwp_relative_pointer_v1::ZwpRelativePointerV1>,
    pointer_seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    keyboard_seat: Option<wl_seat::WlSeat>,
    keyboard_surface: Option<wl_surface::WlSurface>,
    active_output: Option<wl_output::WlOutput>,
    screen_size: (u16, u16),
    content: Option<LyricBubbleContent>,
    lyric: String,
    font_size: u16,
    orientation: LyricOrientation,
    position: Option<(i32, i32)>,
    bubble_size: (u16, u16),
    bubble_layout: BubbleLayout,
    bubble_controls: BubbleControls,
    bubble_input_rects: Vec<BubbleRect>,
    pending_layer_position: Option<(i32, i32)>,
    last_move: Instant,
    drag: Option<DragState>,
    visible: bool,
    configured: bool,
    dirty: bool,
    frame_ready: bool,
    exit: bool,
    animation_started: Instant,
    last_draw: Instant,
    controls: ControlPanel,
}

fn run(
    locale: Locale,
    playback: PlaybackInfo,
    exit: ExitSignal,
    commands: Receiver<Command>,
    ready: mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    let (mut event_loop, qh, mut state) = match initialize(locale, playback, exit) {
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
        state.flush_layer_move(false, &qh);
        if let Err(error) = state.controls.draw(&mut state.pool, &qh) {
            tracing::warn!(%error, "Wayland playback control panel failed");
            state.controls.close();
        }
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
    locale: Locale,
    playback: PlaybackInfo,
    exit: ExitSignal,
) -> Result<(EventLoop<'static, State>, QueueHandle<State>, State)> {
    let initial_lyric = playback.snapshot().lyric;
    let connection = Connection::connect_to_env()?;
    let (globals, event_queue) = registry_queue_init(&connection)?;
    let qh = event_queue.handle();
    let event_loop: EventLoop<State> = EventLoop::try_new()?;
    WaylandSource::new(connection.clone(), event_queue)
        .insert(event_loop.handle())
        .map_err(|error| anyhow!(error.to_string()))?;
    let compositor = CompositorState::bind(&globals, &qh)?;
    let mut layer_shell = LayerShell::bind(&globals, &qh).ok();
    let xdg_shell = XdgShell::bind(&globals, &qh).ok();
    let has_relative_pointer = globals.contents().with_list(|globals| {
        globals
            .iter()
            .any(|global| global.interface == "zwp_relative_pointer_manager_v1")
    });
    if layer_shell.is_some() && !has_relative_pointer && xdg_shell.is_some() {
        tracing::debug!(
            "relative pointer unavailable; using compositor-managed lyric window dragging"
        );
        layer_shell = None;
    }
    if layer_shell.is_none() && xdg_shell.is_none() {
        return Err(anyhow!("neither layer shell nor xdg shell is available"));
    }
    let shm = Shm::bind(&globals, &qh)?;
    let font = load_font(&initial_lyric).context("no usable lyric font")?;
    let font_size = load_font_size();
    let content = Some(LyricBubbleContent::new(&font, &initial_lyric, font_size));
    let pool = SlotPool::new(4096, &shm)?;
    let state = State {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        seat_state: SeatState::new(&globals, &qh),
        relative_pointer_state: RelativePointerState::bind(&globals, &qh),
        compositor,
        layer_shell,
        xdg_shell,
        shm,
        pool,
        buffer: None,
        buffer_size: (0, 0),
        shell: None,
        drag_source: None,
        pointer: None,
        relative_pointer: None,
        pointer_seat: None,
        keyboard: None,
        keyboard_seat: None,
        keyboard_surface: None,
        active_output: None,
        screen_size: (RENDER_SCREEN_WIDTH, RENDER_SCREEN_HEIGHT),
        content,
        lyric: initial_lyric,
        font_size,
        orientation: LyricOrientation::Horizontal,
        position: None,
        bubble_size: (80, 42),
        bubble_layout: BubbleLayout::from_size(84, 76),
        bubble_controls: BubbleControls::new(playback.clone()),
        bubble_input_rects: Vec::new(),
        pending_layer_position: None,
        last_move: Instant::now(),
        drag: None,
        visible: false,
        configured: false,
        dirty: false,
        frame_ready: true,
        exit: false,
        animation_started: Instant::now(),
        last_draw: Instant::now(),
        controls: ControlPanel::new(locale, playback, exit),
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
            Command::ShowControls(position) => {
                let location = position.and_then(|(x, y)| {
                    self.output_state.outputs().find_map(|output| {
                        let info = self.output_state.info(&output)?;
                        let (ox, oy) = info.logical_position?;
                        let (width, height) = info.logical_size?;
                        (x >= ox
                            && y >= oy
                            && i64::from(x) < i64::from(ox) + i64::from(width)
                            && i64::from(y) < i64::from(oy) + i64::from(height))
                        .then_some((output, (x.saturating_sub(ox), y.saturating_sub(oy))))
                    })
                });
                let (output, anchor) = if let Some((output, anchor)) = location {
                    (Some(output), Some(anchor))
                } else {
                    (
                        self.active_output
                            .clone()
                            .or_else(|| self.output_state.outputs().next()),
                        None,
                    )
                };
                self.controls.show(
                    &self.compositor,
                    self.layer_shell.as_ref(),
                    self.xdg_shell.as_ref(),
                    anchor,
                    output.as_ref(),
                    qh,
                );
            }
            Command::Shutdown => self.exit = true,
        }
    }

    fn rebuild_content(&mut self) {
        self.content = load_font(&self.lyric)
            .as_ref()
            .map(|font| LyricBubbleContent::new(font, &self.lyric, self.font_size));
    }

    fn rendered(&self) -> Option<BubbleFrame> {
        self.content.as_ref().map(|content| {
            self.bubble_controls.render(
                content,
                self.orientation,
                self.animation_started.elapsed(),
                self.screen_size,
                self.drag.is_some(),
            )
        })
    }

    fn create_surface(&mut self, qh: &QueueHandle<Self>) {
        if self.shell.is_some() {
            return;
        }
        if self.layer_shell.is_some()
            && let Some(geometry) = self
                .active_output
                .as_ref()
                .and_then(|output| self.output_geometry(output))
        {
            self.screen_size = geometry.size;
        }
        let surface = self.compositor.create_surface(qh);
        self.bubble_layout = self
            .rendered()
            .map(|frame| frame.layout)
            .unwrap_or(BubbleLayout::from_size(84, 76));
        let (width, height) = (self.bubble_layout.width, self.bubble_layout.height);
        self.bubble_input_rects.clear();
        self.bubble_size = (width, height);
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
                self.active_output.as_ref(),
            );
            let (x, y) = clamp_position(
                self.position.unwrap_or((DEFAULT_MARGIN, DEFAULT_MARGIN)),
                self.bubble_size,
                self.screen_size,
            );
            self.position = Some((x, y));
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
        self.drag_source = None;
        self.buffer = None;
        self.buffer_size = (0, 0);
        self.drag = None;
        self.bubble_controls.cancel();
        self.bubble_input_rects.clear();
        self.pending_layer_position = None;
        self.visible = false;
        self.configured = false;
        self.frame_ready = true;
    }

    fn update_size(&mut self) {
        let rendered = self.rendered();
        let Some(shell) = &self.shell else {
            return;
        };
        let Some(frame) = rendered else {
            return;
        };
        let (width, height) = (frame.layout.width, frame.layout.height);
        self.bubble_layout = frame.layout;
        self.bubble_size = (width, height);
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
        let Some(BubbleFrame {
            pixels: rgba,
            layout,
            expanded,
        }) = self.rendered()
        else {
            return Ok(());
        };
        let (width, height) = (layout.width, layout.height);
        if self.bubble_size != (width, height) {
            self.update_size();
        }
        self.bubble_layout = layout;
        let Some(surface) = self.shell.as_ref().map(|shell| shell.wl_surface().clone()) else {
            return Ok(());
        };
        if self.buffer_size != (width, height) {
            self.buffer = None;
            self.buffer_size = (width, height);
        }
        let stride = i32::from(width) * 4;
        // Some compositors retain the current shm buffer until it is replaced.
        // Waiting for release in that case would prevent the hover state from drawing.
        if self
            .buffer
            .as_ref()
            .is_some_and(|buffer| self.pool.canvas(buffer).is_none())
        {
            self.buffer = None;
        }
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
        let rects = layout.input_rects(expanded);
        if self.bubble_input_rects != rects {
            let region = Region::new(&self.compositor)?;
            for rect in &rects {
                region.add(
                    i32::from(rect.x),
                    i32::from(rect.y),
                    i32::from(rect.width),
                    i32::from(rect.height),
                );
            }
            surface.set_input_region(Some(region.wl_region()));
            self.bubble_input_rects = rects;
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

    fn output_geometry(&self, output: &wl_output::WlOutput) -> Option<OutputGeometry> {
        let info = self.output_state.info(output)?;
        let origin = info.logical_position?;
        let (width, height) = info.logical_size?;
        let size = (u16::try_from(width).ok()?, u16::try_from(height).ok()?);
        (size.0 > 0 && size.1 > 0).then_some(OutputGeometry { origin, size })
    }

    fn current_geometry(&self) -> OutputGeometry {
        self.active_output
            .as_ref()
            .and_then(|output| self.output_geometry(output))
            .unwrap_or(OutputGeometry {
                origin: (0, 0),
                size: self.screen_size,
            })
    }

    fn output_geometries(&self) -> Vec<(wl_output::WlOutput, OutputGeometry)> {
        self.output_state
            .outputs()
            .filter_map(|output| {
                self.output_geometry(&output)
                    .map(|geometry| (output, geometry))
            })
            .collect()
    }

    fn switch_layer_output(
        &mut self,
        output: wl_output::WlOutput,
        geometry: OutputGeometry,
        position: (i32, i32),
        qh: &QueueHandle<Self>,
    ) -> Result<()> {
        if self.drag.is_some() && self.drag_source.is_none() {
            if let Some(shell) = &self.shell {
                // Keep a transparent, mapped source surface for the ongoing implicit grab.
                // Attaching a null buffer here would unmap it and lose motion/release events.
                let (width, height) = self.bubble_size;
                let (buffer, canvas) = self.pool.create_buffer(
                    i32::from(width),
                    i32::from(height),
                    i32::from(width) * 4,
                    wl_shm::Format::Argb8888,
                )?;
                canvas.fill(0);
                let surface = shell.wl_surface();
                let region = Region::new(&self.compositor)?;
                surface.set_input_region(Some(region.wl_region()));
                buffer.attach_to(surface)?;
                surface.damage_buffer(0, 0, i32::from(width), i32::from(height));
                surface.commit();
            }
            self.drag_source = self.shell.take();
        } else {
            self.shell = None;
        }
        self.active_output = Some(output);
        self.screen_size = geometry.size;
        self.position = Some(clamp_position(
            geometry.to_local(position),
            self.bubble_size,
            geometry.size,
        ));
        self.create_surface(qh);
        self.dirty = true;
        Ok(())
    }

    fn flush_layer_move(&mut self, force: bool, qh: &QueueHandle<Self>) {
        if !force && self.last_move.elapsed() < DRAG_FRAME_INTERVAL {
            return;
        }
        let Some(position) = self.pending_layer_position.take() else {
            return;
        };
        if !matches!(self.shell, Some(ShellSurface::Layer(_))) {
            return;
        }
        let outputs = self.output_geometries();
        let geometries: Vec<_> = outputs.iter().map(|(_, geometry)| *geometry).collect();
        let grab = self
            .drag
            .as_ref()
            .map(|drag| drag.grab)
            .unwrap_or((0.0, 0.0));
        let pointer = (
            f64::from(position.0) + grab.0,
            f64::from(position.1) + grab.1,
        );
        let preferred = outputs
            .iter()
            .position(|(output, _)| Some(output) == self.active_output.as_ref());
        let geometry = if let Some(index) = drag_output(&geometries, pointer, preferred) {
            let (output, geometry) = &outputs[index];
            if self.active_output.as_ref() != Some(output) {
                if let Err(error) =
                    self.switch_layer_output(output.clone(), *geometry, position, qh)
                {
                    tracing::warn!(%error, "failed to move lyric bubble to another output");
                }
                self.last_move = Instant::now();
                return;
            }
            *geometry
        } else {
            self.current_geometry()
        };
        let position = clamp_position(geometry.to_local(position), self.bubble_size, geometry.size);
        if self.position == Some(position) {
            return;
        }
        self.position = Some(position);
        let Some(ShellSurface::Layer(layer)) = &self.shell else {
            return;
        };
        layer.set_margin(position.1, 0, 0, position.0);
        layer.commit();
        self.last_move = Instant::now();
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
        let font_size = adjusted_font_size(self.font_size, delta < 0.0);
        if font_size == self.font_size {
            return;
        }
        self.font_size = font_size;
        if let Err(error) = save_font_size(font_size) {
            tracing::debug!(%error, "failed to save lyric font size");
        }
        self.rebuild_content();
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
        surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        if self.controls.owns(surface) {
            self.controls.frame_ready = true;
        } else if self
            .shell
            .as_ref()
            .is_some_and(|shell| shell.wl_surface() == surface)
        {
            self.frame_ready = true;
        }
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
            && matches!(self.shell, Some(ShellSurface::Xdg(_)))
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
        qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // A removed output may own the hidden grab source as well as the visible surface.
        self.drag = None;
        self.drag_source = None;
        self.pending_layer_position = None;
        self.bubble_controls.cancel();
        self.dirty = true;
        if self.active_output.as_ref() == Some(&output) {
            self.active_output = None;
            if matches!(self.shell, Some(ShellSurface::Layer(_))) {
                self.shell = None;
                self.configured = false;
                if self.visible {
                    self.create_surface(qh);
                }
            }
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
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = Some(seat.get_keyboard(qh, ()));
            self.keyboard_seat = Some(seat.clone());
        }
        if capability == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(pointer) => {
                    self.relative_pointer = self
                        .relative_pointer_state
                        .get_relative_pointer(&pointer, qh)
                        .ok();
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
        if capability == Capability::Keyboard && self.keyboard_seat.as_ref() == Some(&seat) {
            if let Some(keyboard) = self.keyboard.take() {
                keyboard.release();
            }
            self.keyboard_seat = None;
            self.keyboard_surface = None;
            self.controls.close();
        }
        if capability == Capability::Pointer && self.pointer_seat.as_ref() == Some(&seat) {
            if let Some(relative_pointer) = self.relative_pointer.take() {
                relative_pointer.destroy();
            }
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
            self.pointer_seat = None;
            self.drag = None;
            self.drag_source = None;
            self.pending_layer_position = None;
            self.controls.cancel_drag();
            self.bubble_controls.cancel();
        }
    }

    fn remove_seat(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
    ) {
        if self.keyboard_seat.as_ref() == Some(&seat) {
            if let Some(keyboard) = self.keyboard.take() {
                keyboard.release();
            }
            self.keyboard_seat = None;
            self.keyboard_surface = None;
            self.controls.close();
        }
        if self.pointer_seat.as_ref() == Some(&seat) {
            if let Some(relative_pointer) = self.relative_pointer.take() {
                relative_pointer.destroy();
            }
            if let Some(pointer) = self.pointer.take() {
                pointer.release();
            }
            self.pointer_seat = None;
            self.drag = None;
            self.drag_source = None;
            self.pending_layer_position = None;
            self.controls.cancel_drag();
            self.bubble_controls.cancel();
        }
    }
}

impl PointerHandler for State {
    fn pointer_frame(
        &mut self,
        _connection: &Connection,
        qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for (index, event) in events.iter().enumerate() {
            if self.controls.owns(&event.surface) {
                self.controls.pointer_event(event);
                continue;
            }
            let on_drag_source = self
                .drag_source
                .as_ref()
                .is_some_and(|shell| &event.surface == shell.wl_surface());
            if !on_drag_source
                && self
                    .shell
                    .as_ref()
                    .is_none_or(|shell| &event.surface != shell.wl_surface())
            {
                continue;
            }
            match event.kind {
                PointerEventKind::Enter { .. } => {
                    self.bubble_controls.enter();
                    self.dirty = true;
                }
                PointerEventKind::Leave { .. } => {
                    self.bubble_controls.leave();
                    self.dirty = true;
                }
                PointerEventKind::Press {
                    time: _,
                    button,
                    serial,
                } if button == BTN_LEFT => {
                    if self
                        .bubble_controls
                        .press(event.position, self.bubble_layout)
                    {
                        self.dirty = true;
                        continue;
                    }
                    self.bubble_controls.start_lyric_drag(self.bubble_layout);
                    let origin = self
                        .current_geometry()
                        .to_global(self.position.unwrap_or((DEFAULT_MARGIN, DEFAULT_MARGIN)));
                    self.drag = Some(DragState::new(event.position, serial, origin));
                }
                PointerEventKind::Motion { .. } => {
                    if !on_drag_source {
                        self.bubble_controls
                            .motion(event.position, self.bubble_layout);
                    }
                    self.dirty = true;
                    if self.bubble_controls.pressed() {
                        continue;
                    }
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
                    } else if !is_xdg && self.relative_pointer.is_none() && should_start {
                        // A layer-only compositor without relative motion cannot safely move the
                        // surface. Still distinguish a drag from a click so release does not hide it.
                        if let Some(drag) = self.drag.as_mut() {
                            drag.started = true;
                        }
                    }
                }
                PointerEventKind::Release { button, .. } if button == BTN_LEFT => {
                    if self.bubble_controls.pressed() {
                        if let Some(action) = self
                            .bubble_controls
                            .release(event.position, self.bubble_layout)
                        {
                            bubble_controls::dispatch(action);
                        }
                        self.dirty = true;
                        continue;
                    }
                    self.flush_layer_move(true, qh);
                    self.bubble_controls.finish_lyric_drag();
                    let clicked = self.drag.take().is_some_and(|drag| !drag.started);
                    if self.drag_source.is_some() {
                        self.bubble_controls.leave();
                    }
                    self.drag_source = None;
                    self.dirty = true;
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

// Only the context menu takes keyboard focus. The lyric surface remains non-interactive.
impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        keyboard: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _data: &(),
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if state.keyboard.as_ref() != Some(keyboard) {
            return;
        }
        match event {
            wl_keyboard::Event::Enter { surface, .. } => state.keyboard_surface = Some(surface),
            wl_keyboard::Event::Leave { surface, .. } => {
                if state.controls.owns(&surface) {
                    state.controls.close();
                }
                if state.keyboard_surface.as_ref() == Some(&surface) {
                    state.keyboard_surface = None;
                }
            }
            // Wayland keyboard keycodes use evdev codes; KEY_ESC is 1.
            wl_keyboard::Event::Key {
                key: 1,
                state: WEnum::Value(wl_keyboard::KeyState::Pressed),
                ..
            } if state
                .keyboard_surface
                .as_ref()
                .is_some_and(|surface| state.controls.owns(surface)) =>
            {
                state.controls.close()
            }
            _ => {}
        }
    }
}

impl RelativePointerHandler for State {
    fn relative_pointer_motion(
        &mut self,
        _connection: &Connection,
        _qh: &QueueHandle<Self>,
        relative_pointer: &zwp_relative_pointer_v1::ZwpRelativePointerV1,
        pointer: &wl_pointer::WlPointer,
        event: RelativeMotionEvent,
    ) {
        if self.relative_pointer.as_ref() != Some(relative_pointer)
            || self.pointer.as_ref() != Some(pointer)
            || !matches!(self.shell, Some(ShellSurface::Layer(_)))
            || self.drag.is_none()
        {
            return;
        }
        let mut geometries: Vec<_> = self
            .output_geometries()
            .into_iter()
            .map(|(_, geometry)| geometry)
            .collect();
        if geometries.is_empty() {
            geometries.push(self.current_geometry());
        }
        if let Some(bounds) = DragBounds::for_outputs(&geometries, self.bubble_size)
            && let Some(drag) = self.drag.as_mut()
            && let Some(position) = drag.relative_motion(event.delta, bounds)
        {
            self.pending_layer_position = Some(position);
        }
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _connection: &Connection, qh: &QueueHandle<Self>, layer: &LayerSurface) {
        if matches!(&self.controls.shell, Some(ShellSurface::Layer(current)) if current == layer) {
            self.controls.close();
            return;
        }
        if matches!(&self.drag_source, Some(ShellSurface::Layer(current)) if current == layer) {
            self.drag_source = None;
            self.drag = None;
            self.pending_layer_position = None;
            self.bubble_controls.cancel();
            self.dirty = true;
            return;
        }
        if matches!(&self.shell, Some(ShellSurface::Layer(current)) if current == layer) {
            self.shell = None;
            self.drag = None;
            self.drag_source = None;
            self.pending_layer_position = None;
            self.bubble_controls.cancel();
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
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if matches!(&self.controls.shell, Some(ShellSurface::Layer(current)) if current == layer) {
            self.controls.configure_layer(configure.new_size);
            return;
        }
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
        if matches!(&self.controls.shell, Some(ShellSurface::Xdg(current)) if current == window) {
            self.controls.close();
            return;
        }
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
        if matches!(&self.controls.shell, Some(ShellSurface::Xdg(current)) if current == window) {
            self.controls.configured = true;
            self.controls.dirty = true;
            return;
        }
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
delegate_relative_pointer!(State);
delegate_layer!(State);
delegate_xdg_shell!(State);
delegate_xdg_window!(State);

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(bubble: (u16, u16), screen: (u16, u16)) -> DragBounds {
        DragBounds::for_outputs(
            &[OutputGeometry {
                origin: (0, 0),
                size: screen,
            }],
            bubble,
        )
        .unwrap()
    }

    #[test]
    fn relative_drag_accumulates_motion_without_waiting_for_window_updates() {
        let mut drag = DragState::new((20.0, 10.0), 1, (100, 100));
        let size = (100, 80);
        let bounds = bounds(size, (1920, 1080));

        // The compositor may not have applied any window move yet. Each physical delta
        // must still be added exactly once, independently of surface-local coordinates.
        assert_eq!(drag.relative_motion((10.0, 5.0), bounds), Some((110, 105)));
        assert_eq!(drag.relative_motion((10.0, 5.0), bounds), Some((120, 110)));
        assert_eq!(drag.relative_motion((0.0, 0.0), bounds), Some((120, 110)));
        assert_eq!(drag.relative_motion((-5.0, -5.0), bounds), Some((115, 105)));
    }

    #[test]
    fn relative_drag_preserves_fractional_motion() {
        let mut drag = DragState::new((0.0, 0.0), 1, (100, 100));
        let size = (100, 80);
        let bounds = bounds(size, (1920, 1080));
        assert_eq!(drag.relative_motion((4.0, 0.0), bounds), Some((104, 100)));
        for _ in 0..4 {
            drag.relative_motion((0.25, 0.25), bounds);
        }
        assert_eq!(drag.relative_motion((0.0, 0.0), bounds), Some((105, 101)));
    }

    #[test]
    fn relative_drag_keeps_small_movements_as_clicks() {
        let mut drag = DragState::new((20.0, 10.0), 1, (100, 100));
        let size = (100, 80);
        let bounds = bounds(size, (1920, 1080));
        assert_eq!(drag.relative_motion((1.5, 0.0), bounds), None);
        assert_eq!(drag.relative_motion((1.5, 0.0), bounds), None);
        assert!(!drag.started);
        assert_eq!(drag.relative_motion((1.0, 0.0), bounds), Some((104, 100)));
        assert!(drag.started);
    }

    #[test]
    fn relative_drag_can_reverse_immediately_at_screen_edges() {
        let mut drag = DragState::new((0.0, 0.0), 1, (10, 10));
        let size = (100, 80);
        let bounds = bounds(size, (800, 600));
        assert_eq!(drag.relative_motion((-100.0, -100.0), bounds), Some((0, 0)));
        assert_eq!(drag.relative_motion((1.0, 1.0), bounds), Some((1, 1)));
        assert_eq!(
            drag.relative_motion((1000.0, 1000.0), bounds),
            Some((700, 520))
        );
        assert_eq!(drag.relative_motion((-1.0, -1.0), bounds), Some((699, 519)));
    }

    #[test]
    fn dragged_position_is_clamped_to_the_render_area() {
        assert_eq!(
            clamp_position((-20, 2000), (100, 80), (1920, 1080)),
            (0, 1000)
        );
    }

    #[test]
    fn drag_crosses_right_output_and_converts_to_local_margins() {
        let outputs = [
            OutputGeometry {
                origin: (0, 0),
                size: (1920, 1080),
            },
            OutputGeometry {
                origin: (1920, 0),
                size: (1280, 720),
            },
        ];
        let mut drag = DragState::new((20.0, 10.0), 1, (1800, 100));
        let bounds = DragBounds::for_outputs(&outputs, (100, 80)).unwrap();
        let position = drag.relative_motion((200.0, 5.0), bounds).unwrap();
        assert_eq!(position, (2000, 105));
        assert_eq!(drag_output(&outputs, (2020.0, 115.0), Some(0)), Some(1));
        assert_eq!(outputs[1].to_local(position), (80, 105));
        // Handoffs must not reset displacement or replace global coordinates with local margins.
        assert_eq!(drag.relative_motion((10.0, 0.0), bounds), Some((2010, 105)));
        let back = drag.relative_motion((-200.0, 0.0), bounds).unwrap();
        assert_eq!(back, (1810, 105));
        assert_eq!(drag_output(&outputs, (1830.0, 115.0), Some(1)), Some(0));
    }

    #[test]
    fn drag_crosses_outputs_left_and_above_the_primary() {
        let outputs = [
            OutputGeometry {
                origin: (0, 0),
                size: (1920, 1080),
            },
            OutputGeometry {
                origin: (-1280, 0),
                size: (1280, 720),
            },
            OutputGeometry {
                origin: (0, -900),
                size: (1600, 900),
            },
        ];
        let bounds = DragBounds::for_outputs(&outputs, (100, 80)).unwrap();
        let mut drag = DragState::new((20.0, 10.0), 1, (100, 100));
        let left = drag.relative_motion((-400.0, 0.0), bounds).unwrap();
        assert_eq!(left, (-300, 100));
        assert_eq!(drag_output(&outputs, (-280.0, 110.0), Some(0)), Some(1));
        assert_eq!(outputs[1].to_local(left), (980, 100));
        let above = drag.relative_motion((400.0, -400.0), bounds).unwrap();
        assert_eq!(above, (100, -300));
        assert_eq!(drag_output(&outputs, (120.0, -290.0), Some(1)), Some(2));
        assert_eq!(outputs[2].to_local(above), (100, 600));
    }

    #[test]
    fn handoff_follows_the_grab_point_not_the_bubble_left_edge() {
        let outputs = [
            OutputGeometry {
                origin: (0, 0),
                size: (800, 600),
            },
            OutputGeometry {
                origin: (800, 0),
                size: (800, 600),
            },
        ];
        let grab = (90.0, 10.0);
        let mut drag = DragState::new(grab, 1, (690, 100));
        let bounds = DragBounds::for_outputs(&outputs, (100, 80)).unwrap();
        let position = drag.relative_motion((20.0, 0.0), bounds).unwrap();
        let pointer = (
            f64::from(position.0) + grab.0,
            f64::from(position.1) + grab.1,
        );
        assert_eq!(pointer.0, 800.0);
        assert_eq!(drag_output(&outputs, pointer, Some(0)), Some(1));
        assert_eq!(
            clamp_position(outputs[1].to_local(position), (100, 80), outputs[1].size),
            (0, 100)
        );
        // Clamping the displayed bubble at an output edge must not consume pointer movement.
        assert_eq!(drag.relative_motion((100.0, 0.0), bounds), Some((810, 100)));
    }

    #[test]
    fn offset_and_scaled_outputs_use_logical_geometry() {
        let outputs = [
            OutputGeometry {
                origin: (0, 0),
                size: (1920, 1080),
            },
            // A 3840x2160 display at 2x scale, positioned below and offset to the left.
            OutputGeometry {
                origin: (-400, 1080),
                size: (1920, 1080),
            },
        ];
        let global = (-200, 1200);
        assert_eq!(drag_output(&outputs, (-180.0, 1210.0), Some(0)), Some(1));
        let local = outputs[1].to_local(global);
        assert_eq!(local, (200, 120));
        assert_eq!(outputs[1].to_global(local), global);
        assert_eq!(drag_output(&outputs, (100.0, 1080.0), Some(0)), Some(1));
    }

    #[test]
    fn gaps_choose_the_nearest_real_output_instead_of_an_invisible_area() {
        let outputs = [
            OutputGeometry {
                origin: (0, 0),
                size: (800, 600),
            },
            OutputGeometry {
                origin: (1000, 300),
                size: (800, 600),
            },
        ];
        assert_eq!(drag_output(&outputs, (850.0, 400.0), Some(0)), Some(0));
        assert_eq!(drag_output(&outputs, (950.0, 400.0), Some(0)), Some(1));
        assert_eq!(drag_output(&outputs, (900.0, 400.0), Some(1)), Some(1));
        let local = outputs[1].to_local((930, 390));
        assert_eq!(clamp_position(local, (100, 80), outputs[1].size), (0, 90));
    }

    #[test]
    fn overlapping_outputs_keep_the_current_output_stable() {
        let output = OutputGeometry {
            origin: (0, 0),
            size: (800, 600),
        };
        assert_eq!(
            drag_output(&[output, output], (200.0, 100.0), Some(1)),
            Some(1)
        );
        assert_eq!(
            drag_output(&[output, output], (200.0, 100.0), Some(0)),
            Some(0)
        );
        assert_eq!(drag_output(&[], (200.0, 100.0), None), None);
        assert!(DragBounds::for_outputs(&[], (100, 80)).is_none());
    }

    #[test]
    fn outer_desktop_edge_discards_overshoot_in_negative_coordinates() {
        let outputs = [OutputGeometry {
            origin: (-1280, -720),
            size: (1280, 720),
        }];
        let bounds = DragBounds::for_outputs(&outputs, (100, 80)).unwrap();
        let mut drag = DragState::new((10.0, 10.0), 1, (-1000, -500));
        assert_eq!(
            drag.relative_motion((-1000.0, -1000.0), bounds),
            Some((-1280, -720))
        );
        assert_eq!(
            drag.relative_motion((1.0, 1.0), bounds),
            Some((-1279, -719))
        );
        assert_eq!(
            drag.relative_motion((2000.0, 2000.0), bounds),
            Some((-100, -80))
        );
        assert_eq!(
            drag.relative_motion((-1.0, -1.0), bounds),
            Some((-101, -81))
        );
    }

    #[test]
    fn another_drag_after_handoff_starts_in_global_coordinates() {
        let output = OutputGeometry {
            origin: (-1280, 720),
            size: (1280, 720),
        };
        let bounds = DragBounds::for_outputs(&[output], (100, 80)).unwrap();
        let mut drag = DragState::new((20.0, 10.0), 2, output.to_global((500, 200)));
        let position = drag.relative_motion((15.0, 10.0), bounds).unwrap();
        assert_eq!(output.to_local(position), (515, 210));
    }

    #[test]
    fn continuous_drag_visits_all_three_outputs_in_an_l_shaped_layout() {
        let outputs = [
            OutputGeometry {
                origin: (0, 0),
                size: (1920, 1080),
            },
            OutputGeometry {
                origin: (-1920, 0),
                size: (1920, 1080),
            },
            OutputGeometry {
                origin: (-1920, 1080),
                size: (1920, 1080),
            },
        ];
        let bounds = DragBounds::for_outputs(&outputs, (100, 80)).unwrap();
        let mut drag = DragState::new((20.0, 10.0), 1, (100, 200));
        let mut current = Some(0);
        for (delta, target, local) in [
            ((-400.0, 0.0), 1, (1620, 200)),
            ((0.0, 1000.0), 2, (1620, 120)),
            ((0.0, -1000.0), 1, (1620, 200)),
            ((400.0, 0.0), 0, (100, 200)),
        ] {
            let position = drag.relative_motion(delta, bounds).unwrap();
            current = drag_output(
                &outputs,
                (
                    f64::from(position.0) + drag.grab.0,
                    f64::from(position.1) + drag.grab.1,
                ),
                current,
            );
            assert_eq!(current, Some(target));
            assert_eq!(outputs[target].to_local(position), local);
        }
    }
}
