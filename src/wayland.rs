//! Wayland side of the switcher.
//!
//! One background thread owns a connection that:
//!   1. enumerates foreign toplevels (`ext_foreign_toplevel_list_v1`),
//!   2. attaches COSMIC's toplevel extension to each of them for state + activation,
//!   3. grabs a live preview of every window through `ext_image_copy_capture_v1`,
//!   4. then waits for the UI's activate/cancel command.
//!
//! Previews are sent to the UI as they arrive, so the overlay can pop up instantly with
//! app icons and fill in thumbnails a frame or two later.

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use cosmic_protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1::{self, ZcosmicToplevelHandleV1},
    zcosmic_toplevel_info_v1::{self, ZcosmicToplevelInfoV1},
};
use cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1::{
    self, ZcosmicToplevelManagerV1,
};
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    event_created_child,
    globals::{registry_queue_init, GlobalListContents},
    protocol::{wl_buffer, wl_output, wl_registry, wl_seat, wl_shm, wl_shm_pool},
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1::{
        self, ExtForeignToplevelImageCaptureSourceManagerV1,
    },
    ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1},
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{
        self, ExtImageCopyCaptureManagerV1, Options as CaptureOptions,
    },
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

use switcher_config::{WorkspaceScope, APP_ID};

use crate::thumbs::{self, Frame};

/// How long we're willing to wait for previews before settling for icons.
const CAPTURE_DEADLINE: Duration = Duration::from_millis(600);

#[derive(Debug, Clone)]
pub struct ToplevelEntry {
    pub app_id:    String,
    pub title:     String,
    /// Index into the thread's internal toplevel list, used to activate the window.
    pub key:       usize,
}

#[derive(Debug, Clone)]
pub struct Thumbnail {
    /// Index into the entries list handed to the UI.
    pub index:  usize,
    pub width:  u32,
    pub height: u32,
    pub rgba:   Vec<u8>,
}

/// What the capture thread tells the UI.
pub enum ThumbEvent {
    Ready(Thumbnail),
    /// No more previews will arrive (they either finished or timed out).
    Finished,
}

pub enum Command {
    Activate(usize),
    /// Ask the compositor to close the window at this index, then quit the switcher.
    Close(usize),
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Screen {
    /// Logical size of the monitor we're opening on.
    pub width:  u32,
    pub height: u32,
}

pub struct Switcher {
    pub entries: Vec<ToplevelEntry>,
    pub screen:  Screen,
    pub cmd:     mpsc::SyncSender<Command>,
    pub thumbs:  mpsc::Receiver<ThumbEvent>,
}

/// Enumerate windows, start capturing previews and return as soon as the window list is
/// known. The background thread keeps running until it is told to activate or cancel.
pub fn spawn(scope: WorkspaceScope, preview_max_dim: u32, corner_radius: f32) -> Result<Switcher> {
    let (list_tx, list_rx)   = mpsc::sync_channel(1);
    let (cmd_tx, cmd_rx)     = mpsc::sync_channel(2);
    let (thumb_tx, thumb_rx) = mpsc::channel();

    std::thread::spawn(move || {
        if let Err(e) = thread_main(
            scope,
            preview_max_dim,
            corner_radius,
            list_tx,
            cmd_rx,
            thumb_tx,
        ) {
            log::error!("wayland thread: {e:#}");
        }
    });

    let (entries, screen) = list_rx.recv().context("wayland thread died before listing windows")?;
    Ok(Switcher { entries, screen, cmd: cmd_tx, thumbs: thumb_rx })
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

struct Toplevel {
    handle:    ExtForeignToplevelHandleV1,
    app_id:    String,
    title:     String,
    activated: bool,
    minimized: bool,
    closed:    bool,
    cosmic:    Option<ZcosmicToplevelHandleV1>,
    outputs:   Vec<wl_output::WlOutput>,
}

struct OutputState {
    output: wl_output::WlOutput,
    width:  u32,
    height: u32,
    scale:  i32,
}

struct Capture {
    /// Index into the entries list (and therefore into `entry_to_toplevel`).
    entry:   usize,
    source:  ExtImageCaptureSourceV1,
    session: ExtImageCopyCaptureSessionV1,
    size:    Option<(u32, u32)>,
    formats: Vec<wl_shm::Format>,
    ready:   bool,
    frame:   Option<ExtImageCopyCaptureFrameV1>,
    buffer:  Option<ShmBuffer>,
    done:    bool,
    success: bool,
}

struct ShmBuffer {
    _file:  std::fs::File,
    map:    memmap2::MmapMut,
    buffer: wl_buffer::WlBuffer,
    width:  u32,
    height: u32,
    src_format: thumbs::SourceFormat,
}

impl ShmBuffer {
    fn new(
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<AppData>,
        width: u32,
        height: u32,
        format: wl_shm::Format,
        src_format: thumbs::SourceFormat,
    ) -> Result<Self> {
        let stride = width as i32 * 4;
        let size   = stride as usize * height as usize;

        let fd: OwnedFd = memfd(size)?;
        let file = std::fs::File::from(fd);
        let map = unsafe { memmap2::MmapMut::map_mut(&file) }
            .context("mmap of the capture buffer failed")?;

        let pool = shm.create_pool(file.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(0, width as i32, height as i32, stride, format, qh, ());
        pool.destroy();

        Ok(Self { _file: file, map, buffer, width, height, src_format })
    }
}

fn memfd(size: usize) -> Result<OwnedFd> {
    let name = std::ffi::CString::new("cosmic-windows-swapper-preview").unwrap();
    let fd = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("memfd_create failed");
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let rc = unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) };
    if rc < 0 {
        return Err(std::io::Error::last_os_error()).context("ftruncate failed");
    }
    Ok(fd)
}

struct AppData {
    toplevels:         Vec<Toplevel>,
    outputs:           Vec<OutputState>,
    captures:          Vec<Capture>,
    entry_to_toplevel: Vec<usize>,

    list:        Option<ExtForeignToplevelListV1>,
    info:        Option<ZcosmicToplevelInfoV1>,
    manager:     Option<ZcosmicToplevelManagerV1>,
    seat:        Option<wl_seat::WlSeat>,
    shm:         Option<wl_shm::WlShm>,
    cap_manager: Option<ExtImageCopyCaptureManagerV1>,
    src_manager: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,

    thumb_tx: Option<mpsc::Sender<ThumbEvent>>,
    max_dim:  u32,
    radius:   f32,
}

impl AppData {
    fn new(thumb_tx: mpsc::Sender<ThumbEvent>, max_dim: u32, radius: f32) -> Self {
        Self {
            toplevels: Vec::new(),
            outputs: Vec::new(),
            captures: Vec::new(),
            entry_to_toplevel: Vec::new(),
            list: None,
            info: None,
            manager: None,
            seat: None,
            shm: None,
            cap_manager: None,
            src_manager: None,
            thumb_tx: Some(thumb_tx),
            max_dim,
            radius,
        }
    }

    fn output_size(&self, active: &[wl_output::WlOutput]) -> Screen {
        let pick = |o: &OutputState| Screen {
            width:  (o.width as i32 / o.scale.max(1)) as u32,
            height: (o.height as i32 / o.scale.max(1)) as u32,
        };
        if let Some(o) = self
            .outputs
            .iter()
            .find(|o| active.iter().any(|a| a == &o.output) && o.width > 0)
        {
            return pick(o);
        }
        if let Some(o) = self.outputs.iter().find(|o| o.width > 0) {
            return pick(o);
        }
        Screen { width: 1920, height: 1080 }
    }

    /// Create capture sessions for every entry. Returns immediately; frames are polled.
    fn start_captures(&mut self, qh: &QueueHandle<Self>) {
        let (Some(src_manager), Some(cap_manager)) =
            (self.src_manager.clone(), self.cap_manager.clone())
        else {
            log::warn!("compositor lacks ext-image-copy-capture; showing icons only");
            return;
        };

        for entry in 0..self.entry_to_toplevel.len() {
            let tl_idx = self.entry_to_toplevel[entry];
            if self.toplevels[tl_idx].closed {
                continue;
            }
            let handle = self.toplevels[tl_idx].handle.clone();
            let source  = src_manager.create_source(&handle, qh, ());
            let session = cap_manager.create_session(&source, CaptureOptions::empty(), qh, ());
            self.captures.push(Capture {
                entry,
                source,
                session,
                size: None,
                formats: Vec::new(),
                ready: false,
                frame: None,
                buffer: None,
                done: false,
                success: false,
            });
        }
    }

    /// Advance the capture state machine: start frames for sessions with constraints, and
    /// harvest finished ones into the thumbnail channel.
    fn pump_captures(&mut self, qh: &QueueHandle<Self>) {
        let Some(shm) = self.shm.clone() else { return };

        for i in 0..self.captures.len() {
            if self.captures[i].frame.is_some() || !self.captures[i].ready || self.captures[i].done {
                continue;
            }
            let Some((w, h)) = self.captures[i].size else {
                self.captures[i].done = true;
                continue;
            };
            let Some((format, src_format)) = pick_format(&self.captures[i].formats) else {
                self.captures[i].done = true;
                continue;
            };
            let session = self.captures[i].session.clone();

            let buffer = match ShmBuffer::new(&shm, qh, w, h, format, src_format) {
                Ok(b) => b,
                Err(e) => {
                    log::warn!("capture buffer for entry {} failed: {e:#}", self.captures[i].entry);
                    self.captures[i].done = true;
                    continue;
                }
            };

            let frame = session.create_frame(qh, ());
            frame.attach_buffer(&buffer.buffer);
            frame.damage_buffer(0, 0, w as i32, h as i32);
            frame.capture();

            self.captures[i].frame  = Some(frame);
            self.captures[i].buffer = Some(buffer);
        }

        for i in 0..self.captures.len() {
            if !self.captures[i].done || !self.captures[i].success {
                continue;
            }
            let Some(buffer) = self.captures[i].buffer.take() else { continue };
            let frame = self.captures[i].frame.take();

            if buffer.width > 0 && buffer.height > 0 {
                let preview: Frame = thumbs::to_preview(
                    &buffer.map[..],
                    buffer.width as usize * 4,
                    buffer.width,
                    buffer.height,
                    buffer.src_format,
                    self.max_dim,
                    self.radius,
                );
                if preview.width > 0 {
                    if let Some(tx) = &self.thumb_tx {
                        let _ = tx.send(ThumbEvent::Ready(Thumbnail {
                            index:  self.captures[i].entry,
                            width:  preview.width,
                            height: preview.height,
                            rgba:   preview.rgba,
                        }));
                    }
                }
            }

            buffer.buffer.destroy();
            if let Some(f) = frame {
                f.destroy();
            }
        }

        if self.captures.iter().all(|c| c.done) {
            if let Some(tx) = self.thumb_tx.take() {
                let _ = tx.send(ThumbEvent::Finished);
            }
        }
    }

    fn captures_done(&self) -> bool {
        self.thumb_tx.is_none()
    }

    fn abandon_captures(&mut self) {
        for cap in self.captures.drain(..) {
            if let Some(f) = cap.frame {
                f.destroy();
            }
            if let Some(b) = cap.buffer {
                b.buffer.destroy();
            }
            cap.session.destroy();
            cap.source.destroy();
        }
        if let Some(tx) = self.thumb_tx.take() {
            let _ = tx.send(ThumbEvent::Finished);
        }
    }
}

/// Pick the cheapest format the compositor offers for previews: opaque XRGB first, then
/// the alpha variants (cosmic-comp often only advertises ABGR8888).
fn pick_format(formats: &[wl_shm::Format]) -> Option<(wl_shm::Format, thumbs::SourceFormat)> {
    use thumbs::SourceFormat as S;
    use wl_shm::Format as F;
    let has = |f: F| formats.iter().any(|x| *x == f);

    if has(F::Xrgb8888) {
        return Some((F::Xrgb8888, S::Xrgb8888));
    }
    if has(F::Abgr8888) {
        return Some((F::Abgr8888, S::Abgr8888));
    }
    if has(F::Argb8888) {
        return Some((F::Argb8888, S::Argb8888));
    }
    if has(F::Xbgr8888) {
        return Some((F::Xbgr8888, S::Xbgr8888));
    }
    None
}

// ---------------------------------------------------------------------------
// Thread entry point
// ---------------------------------------------------------------------------

fn thread_main(
    scope: WorkspaceScope,
    max_dim: u32,
    radius: f32,
    list_tx: mpsc::SyncSender<(Vec<ToplevelEntry>, Screen)>,
    cmd_rx: mpsc::Receiver<Command>,
    thumb_tx: mpsc::Sender<ThumbEvent>,
) -> Result<()> {
    let conn = Connection::connect_to_env().context("cannot connect to the Wayland display")?;
    let (globals, mut queue) = registry_queue_init::<AppData>(&conn)?;
    let qh = queue.handle();

    let mut data = AppData::new(thumb_tx, max_dim, radius);

    data.list = Some(
        globals
            .bind::<ExtForeignToplevelListV1, _, _>(&qh, 1..=1, ())
            .context("ext_foreign_toplevel_list_v1 is missing")?,
    );
    data.info = Some(
        globals.bind(&qh, 1..=3, ())
            .context("zcosmic_toplevel_info_v1 is missing")?,
    );
    data.manager = Some(
        globals.bind(&qh, 1..=1, ())
            .context("zcosmic_toplevel_manager_v1 is missing")?,
    );
    data.seat = Some(globals.bind(&qh, 1..=7, ()).context("wl_seat is missing")?);
    data.shm  = Some(globals.bind(&qh, 1..=1, ()).context("wl_shm is missing")?);
    data.cap_manager = globals.bind(&qh, 1..=1, ()).ok();
    data.src_manager = globals.bind(&qh, 1..=1, ()).ok();

    // Bind every wl_output up front so toplevel output_enter events can be resolved.
    for g in globals.contents().clone_list() {
        if g.interface == "wl_output" {
            let output: wl_output::WlOutput =
                globals.registry().bind(g.name, g.version.min(4), &qh, ());
            data.outputs.push(OutputState { output, width: 0, height: 0, scale: 1 });
        }
    }

    // Let the compositor announce toplevels, output modes and our output binds.
    for _ in 0..3 {
        queue.roundtrip(&mut data)?;
    }

    // Attach the COSMIC extension to every foreign handle (needed for state + activation).
    let info = data.info.clone().unwrap();
    for i in 0..data.toplevels.len() {
        let handle = data.toplevels[i].handle.clone();
        let cosmic = info.get_cosmic_toplevel(&handle, &qh, ());
        data.toplevels[i].cosmic = Some(cosmic);
    }
    for _ in 0..3 {
        queue.roundtrip(&mut data)?;
    }

    // --- Build the entry list -------------------------------------------------
    let mut order: Vec<usize> = (0..data.toplevels.len())
        .filter(|&i| {
            let t = &data.toplevels[i];
            !t.closed && t.app_id != APP_ID && (!t.app_id.is_empty() || !t.title.is_empty())
        })
        .collect();
    order.sort_by_key(|&i| if data.toplevels[i].activated { 0 } else { 1 });

    let active_outputs: Vec<wl_output::WlOutput> = data
        .toplevels
        .iter()
        .find(|t| t.activated)
        .map(|t| t.outputs.clone())
        .unwrap_or_default();

    order.retain(|&i| match scope {
        WorkspaceScope::AllWorkspaces | WorkspaceScope::CurrentWorkspace => true,
        WorkspaceScope::CurrentOutput => {
            active_outputs.is_empty()
                || data.toplevels[i].outputs.iter().any(|o| active_outputs.contains(o))
        }
    });

    let entries: Vec<ToplevelEntry> = order
        .iter()
        .map(|&i| ToplevelEntry {
            app_id:    data.toplevels[i].app_id.clone(),
            title:     data.toplevels[i].title.clone(),
            key:       i,
        })
        .collect();

    let screen = data.output_size(&active_outputs);
    data.entry_to_toplevel = order;

    log::debug!("{} toplevels in scope, screen {screen:?}", entries.len());
    list_tx.send((entries, screen)).ok();

    // --- Previews ------------------------------------------------------------
    data.start_captures(&qh);
    let deadline = Instant::now() + CAPTURE_DEADLINE;

    loop {
        queue.roundtrip(&mut data)?;
        data.pump_captures(&qh);

        if let Ok(cmd) = cmd_rx.try_recv() {
            data.abandon_captures();
            return run_command(&conn, &data, cmd);
        }
        if data.captures_done() || Instant::now() > deadline {
            data.abandon_captures();
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    // --- Wait for the user's decision ---------------------------------------
    if let Ok(cmd) = cmd_rx.recv() {
        run_command(&conn, &data, cmd)?;
    }
    Ok(())
}

fn run_command(conn: &Connection, data: &AppData, cmd: Command) -> Result<()> {
    match cmd {
        Command::Activate(key) => {
            if let (Some(toplevel), Some(manager), Some(seat)) = (
                data.toplevels.get(key),
                data.manager.as_ref(),
                data.seat.as_ref(),
            ) {
                if let Some(cosmic) = &toplevel.cosmic {
                    manager.activate(cosmic, seat);
                    conn.flush().ok();
                }
            }
        }
        Command::Close(key) => {
            if let (Some(toplevel), Some(manager)) =
                (data.toplevels.get(key), data.manager.as_ref())
            {
                if let Some(cosmic) = &toplevel.cosmic {
                    manager.close(cosmic);
                    conn.flush().ok();
                }
            }
        }
        Command::Cancel => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Protocol dispatch
// ---------------------------------------------------------------------------

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for AppData {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for AppData {
    fn event(
        data: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            data.toplevels.push(Toplevel {
                handle: toplevel,
                app_id: String::new(),
                title: String::new(),
                activated: false,
                minimized: false,
                closed: false,
                cosmic: None,
                outputs: Vec::new(),
            });
        }
    }

    event_created_child!(AppData, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE =>
            (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for AppData {
    fn event(
        data: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(t) = data.toplevels.iter_mut().find(|t| &t.handle == handle) else {
            return;
        };
        match event {
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => t.app_id = app_id,
            ext_foreign_toplevel_handle_v1::Event::Title { title } => t.title = title,
            ext_foreign_toplevel_handle_v1::Event::Closed => t.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<ZcosmicToplevelInfoV1, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &ZcosmicToplevelInfoV1,
        _: zcosmic_toplevel_info_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }

    event_created_child!(AppData, ZcosmicToplevelInfoV1, [
        zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE =>
            (ZcosmicToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ZcosmicToplevelHandleV1, ()> for AppData {
    fn event(
        data: &mut Self,
        handle: &ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(t) = data
            .toplevels
            .iter_mut()
            .find(|t| t.cosmic.as_ref() == Some(handle))
        else {
            return;
        };
        match event {
            zcosmic_toplevel_handle_v1::Event::AppId { app_id } => t.app_id = app_id,
            zcosmic_toplevel_handle_v1::Event::Title { title } => t.title = title,
            zcosmic_toplevel_handle_v1::Event::State { state } => {
                let mut activated = false;
                let mut minimized = false;
                for chunk in state.chunks_exact(4) {
                    let value = u32::from_ne_bytes(chunk.try_into().unwrap());
                    match value {
                        2 => activated = true, // state::activated
                        1 => minimized = true, // state::minimized
                        _ => {}
                    }
                }
                t.activated = activated;
                t.minimized = minimized;
            }
            zcosmic_toplevel_handle_v1::Event::OutputEnter { output } => {
                if !t.outputs.contains(&output) {
                    t.outputs.push(output);
                }
            }
            zcosmic_toplevel_handle_v1::Event::OutputLeave { output } => {
                t.outputs.retain(|o| o != &output);
            }
            zcosmic_toplevel_handle_v1::Event::Closed => t.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<ZcosmicToplevelManagerV1, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &ZcosmicToplevelManagerV1,
        _: zcosmic_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtForeignToplevelImageCaptureSourceManagerV1, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &ExtForeignToplevelImageCaptureSourceManagerV1,
        _: ext_foreign_toplevel_image_capture_source_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtImageCaptureSourceV1, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &ExtImageCaptureSourceV1,
        _: ext_image_capture_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtImageCopyCaptureManagerV1, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &ExtImageCopyCaptureManagerV1,
        _: ext_image_copy_capture_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for AppData {
    fn event(
        data: &mut Self,
        session: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(cap) = data.captures.iter_mut().find(|c| &c.session == session) else {
            return;
        };
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                cap.size = Some((width, height));
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat { format } => {
                if let WEnum::Value(f) = format {
                    cap.formats.push(f);
                }
            }
            ext_image_copy_capture_session_v1::Event::Done => cap.ready = true,
            ext_image_copy_capture_session_v1::Event::Stopped => {
                cap.done  = true;
                cap.ready = false;
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for AppData {
    fn event(
        data: &mut Self,
        frame: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(cap) = data
            .captures
            .iter_mut()
            .find(|c| c.frame.as_ref() == Some(frame))
        else {
            return;
        };
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => {
                cap.done    = true;
                cap.success = true;
            }
            ext_image_copy_capture_frame_v1::Event::Failed { .. } => {
                cap.done    = true;
                cap.success = false;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_shm::WlShm, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &wl_shm::WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &wl_shm_pool::WlShmPool,
        _: wl_shm_pool::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &wl_buffer::WlBuffer,
        _: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for AppData {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, ()> for AppData {
    fn event(
        data: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(o) = data.outputs.iter_mut().find(|o| &o.output == output) else {
            return;
        };
        match event {
            wl_output::Event::Mode { flags, width, height, .. } => {
                if let WEnum::Value(flags) = flags {
                    if flags.contains(wl_output::Mode::Current) {
                        o.width  = width.max(0) as u32;
                        o.height = height.max(0) as u32;
                    }
                }
            }
            wl_output::Event::Scale { factor } => o.scale = factor.max(1),
            _ => {}
        }
    }
}
