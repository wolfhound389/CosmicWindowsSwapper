//! Development helper: grab the whole output through `ext_image_copy_capture_v1` and write
//! a PPM. Used to eyeball the overlay without the desktop screenshot portal (which needs a
//! writable runtime dir and a confirmation dialog).
//!
//! Reachable as `cosmic-ext-app-switcher --screenshot /tmp/shot.ppm`.

use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};

use anyhow::{Context, Result};
use wayland_client::{
    Connection, Dispatch, QueueHandle, WEnum,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool},
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1::{self, ExtImageCaptureSourceV1},
    ext_output_image_capture_source_manager_v1::{
        self, ExtOutputImageCaptureSourceManagerV1,
    },
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{
        self, ExtImageCopyCaptureManagerV1, Options as CaptureOptions,
    },
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};

struct Buffer {
    _file:  std::fs::File,
    map:    memmap2::MmapMut,
    buffer: wl_buffer::WlBuffer,
    width:  u32,
    height: u32,
}

struct Shot {
    shm:     Option<wl_shm::WlShm>,
    src_mgr: Option<ExtOutputImageCaptureSourceManagerV1>,
    cap_mgr: Option<ExtImageCopyCaptureManagerV1>,
    outputs: Vec<wl_output::WlOutput>,
    modes:   Vec<Option<(u32, u32)>>,

    source:  Option<ExtImageCaptureSourceV1>,
    session: Option<ExtImageCopyCaptureSessionV1>,
    frame:   Option<ExtImageCopyCaptureFrameV1>,

    size:    Option<(u32, u32)>,
    formats: Vec<wl_shm::Format>,
    ready:   bool,
    done:    bool,
    failed:  bool,
    buffer:  Option<Buffer>,
}

pub fn capture_screen(path: &std::path::Path) -> Result<()> {
    let conn = Connection::connect_to_env().context("cannot connect to the Wayland display")?;
    let (globals, mut queue) = registry_queue_init::<Shot>(&conn)?;
    let qh = queue.handle();

    let mut shot = Shot {
        shm:     Some(globals.bind(&qh, 1..=1, ()).context("wl_shm")?),
        src_mgr: Some(
            globals
                .bind(&qh, 1..=1, ())
                .context("ext_output_image_capture_source_manager_v1")?,
        ),
        cap_mgr: Some(
            globals
                .bind(&qh, 1..=1, ())
                .context("ext_image_copy_capture_manager_v1")?,
        ),
        outputs: Vec::new(),
        modes:   Vec::new(),
        source:  None,
        session: None,
        frame:   None,
        size:    None,
        formats: Vec::new(),
        ready:   false,
        done:    false,
        failed:  false,
        buffer:  None,
    };

    for g in globals.contents().clone_list() {
        if g.interface == "wl_output" {
            let output: wl_output::WlOutput =
                globals.registry().bind(g.name, g.version.min(4), &qh, ());
            shot.outputs.push(output);
            shot.modes.push(None);
        }
    }

    for _ in 0..3 {
        queue.roundtrip(&mut shot)?;
    }

    let idx = shot
        .modes
        .iter()
        .position(|m| m.is_some())
        .context("no output with a current mode")?;
    let output = shot.outputs[idx].clone();

    let src_mgr  = shot.src_mgr.clone().unwrap();
    let cap_mgr  = shot.cap_mgr.clone().unwrap();
    let source   = src_mgr.create_source(&output, &qh, ());
    let session  = cap_mgr.create_session(&source, CaptureOptions::empty(), &qh, ());
    shot.source  = Some(source);
    shot.session = Some(session.clone());

    // Wait for the session constraints.
    for _ in 0..20 {
        queue.roundtrip(&mut shot)?;
        if shot.ready {
            break;
        }
    }

    let (w, h) = shot.size.context("capture session never sent its buffer size")?;
    let has = |f: wl_shm::Format| shot.formats.iter().any(|x| *x == f);
    let (format, order) = if has(wl_shm::Format::Xrgb8888) {
        (wl_shm::Format::Xrgb8888, crate::thumbs::SourceFormat::Xrgb8888)
    } else if has(wl_shm::Format::Abgr8888) {
        (wl_shm::Format::Abgr8888, crate::thumbs::SourceFormat::Abgr8888)
    } else if has(wl_shm::Format::Argb8888) {
        (wl_shm::Format::Argb8888, crate::thumbs::SourceFormat::Argb8888)
    } else if has(wl_shm::Format::Xbgr8888) {
        (wl_shm::Format::Xbgr8888, crate::thumbs::SourceFormat::Xbgr8888)
    } else {
        anyhow::bail!("no supported shm format for output capture: {:?}", shot.formats);
    };

    let shm = shot.shm.clone().unwrap();
    let stride = w as i32 * 4;
    let size = stride as usize * h as usize;
    let fd: OwnedFd = memfd(size)?;
    let file = std::fs::File::from(fd);
    let map = unsafe { memmap2::MmapMut::map_mut(&file)? };
    let pool = shm.create_pool(file.as_fd(), size as i32, &qh, ());
    let wl_buffer = pool.create_buffer(0, w as i32, h as i32, stride, format, &qh, ());
    pool.destroy();

    let frame = session.create_frame(&qh, ());
    frame.attach_buffer(&wl_buffer);
    frame.damage_buffer(0, 0, w as i32, h as i32);
    frame.capture();
    shot.frame  = Some(frame);
    shot.buffer = Some(Buffer { _file: file, map, buffer: wl_buffer, width: w, height: h });

    for _ in 0..120 {
        queue.roundtrip(&mut shot)?;
        if shot.done {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    anyhow::ensure!(shot.done && !shot.failed, "output capture failed");

    let buffer = shot.buffer.take().unwrap();
    let mut file = std::fs::File::create(path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    write!(file, "P6\n{} {}\n255\n", buffer.width, buffer.height)?;
    let [r, g, b] = order.rgb_order();
    let mut rgb = Vec::with_capacity((buffer.width * buffer.height * 3) as usize);
    for px in buffer.map.chunks_exact(4) {
        rgb.extend_from_slice(&[px[r], px[g], px[b]]);
    }
    file.write_all(&rgb)?;
    buffer.buffer.destroy();
    if let Some(f) = shot.frame.take() {
        f.destroy();
    }
    session.destroy();
    if let Some(s) = shot.source.take() {
        s.destroy();
    }
    conn.flush().ok();
    Ok(())
}

fn memfd(size: usize) -> Result<OwnedFd> {
    let name = std::ffi::CString::new("cosmic-app-switcher-shot").unwrap();
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

// --- dispatch --------------------------------------------------------------

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Shot {
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

impl Dispatch<wl_output::WlOutput, ()> for Shot {
    fn event(
        data: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(i) = data.outputs.iter().position(|o| o == output) else { return };
        if let wl_output::Event::Mode { flags, width, height, .. } = event {
            if let WEnum::Value(flags) = flags {
                if flags.contains(wl_output::Mode::Current) {
                    data.modes[i] = Some((width.max(0) as u32, height.max(0) as u32));
                }
            }
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for Shot {
    fn event(
        data: &mut Self,
        session: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if data.session.as_ref() != Some(session) {
            return;
        }
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                data.size = Some((width, height));
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat { format } => {
                if let WEnum::Value(f) = format {
                    data.formats.push(f);
                }
            }
            ext_image_copy_capture_session_v1::Event::Done => data.ready = true,
            ext_image_copy_capture_session_v1::Event::Stopped => data.failed = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for Shot {
    fn event(
        data: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Ready => data.done = true,
            ext_image_copy_capture_frame_v1::Event::Failed { .. } => {
                data.done = true;
                data.failed = true;
            }
            _ => {}
        }
    }
}

macro_rules! noop_dispatch {
    ($iface:ty, $event:path, $data:ty) => {
        impl Dispatch<$iface, ()> for Shot {
            fn event(
                _: &mut Self,
                _: &$iface,
                _: $event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

noop_dispatch!(wl_shm::WlShm, wl_shm::Event, ());
noop_dispatch!(wl_shm_pool::WlShmPool, wl_shm_pool::Event, ());
noop_dispatch!(wl_buffer::WlBuffer, wl_buffer::Event, ());
noop_dispatch!(ExtImageCaptureSourceV1, ext_image_capture_source_v1::Event, ());
noop_dispatch!(
    ExtOutputImageCaptureSourceManagerV1,
    ext_output_image_capture_source_manager_v1::Event,
    ()
);
noop_dispatch!(ExtImageCopyCaptureManagerV1, ext_image_copy_capture_manager_v1::Event, ());
