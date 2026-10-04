//! The Wayland side: an xdg toplevel with two shared-memory buffers, redrawn
//! on every frame callback.

use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};

use rustix::fs::{MemfdFlags, ftruncate, memfd_create};
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};
use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm,
    wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

use crate::draw::{BAR_WIDTH, Canvas, Rect, Scene, counter_rect, flash_rect, strip_rect};

/// Sweep speed, in pixels per second (independent of the frame rate).
const BAR_SPEED: f64 = 900.0;
const FLASH_FOR: Duration = Duration::from_millis(100);
const DEFAULT_SIZE: (i32, i32) = (1280, 720);

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    let mut app = App::default();
    queue.roundtrip(&mut app)?;

    let compositor = app.compositor.clone().ok_or("no wl_compositor")?;
    let wm_base = app.wm_base.clone().ok_or("no xdg_wm_base")?;
    app.shm.as_ref().ok_or("no wl_shm")?;
    let surface = compositor.create_surface(&qh, ());
    let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg_surface.get_toplevel(&qh, ());
    toplevel.set_title("Cha test pattern".into());
    toplevel.set_app_id("sh.cha.testpattern".into());
    surface.commit();
    app.surface = Some(surface);

    while !app.exit {
        queue.blocking_dispatch(&mut app)?;
    }
    Ok(())
}

/// One shared-memory buffer and what it last showed.
struct Buffer {
    wl: wl_buffer::WlBuffer,
    offset: usize,
    busy: bool,
    shown: Option<Scene>,
}

struct Pool {
    _fd: OwnedFd,
    ptr: *mut std::ffi::c_void,
    len: usize,
    width: i32,
    height: i32,
    buffers: Vec<Buffer>,
}

impl Drop for Pool {
    fn drop(&mut self) {
        for buffer in &self.buffers {
            buffer.wl.destroy();
        }
        // SAFETY: mapped in `App::allocate` with this length.
        unsafe {
            let _ = munmap(self.ptr, self.len);
        }
    }
}

#[derive(Default)]
struct App {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    surface: Option<wl_surface::WlSurface>,
    pool: Option<Pool>,
    pending_size: Option<(i32, i32)>,
    configured: bool,
    /// A frame callback is outstanding.
    waiting: bool,
    /// The last committed picture.
    last: Option<Scene>,
    frame: u64,
    started: Option<Instant>,
    flash_until: Option<Instant>,
    fps_window: (u64, Option<Instant>),
    exit: bool,
}

impl App {
    fn allocate(
        &mut self,
        qh: &QueueHandle<Self>,
        width: i32,
        height: i32,
    ) -> rustix::io::Result<()> {
        let stride = width * 4;
        let size = (stride * height) as usize;
        let fd = memfd_create("cha-testpattern", MemfdFlags::CLOEXEC)?;
        ftruncate(&fd, (size * 2) as u64)?;
        // SAFETY: a fresh shared mapping of the memfd we just sized.
        let ptr = unsafe {
            mmap(
                std::ptr::null_mut(),
                size * 2,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &fd,
                0,
            )?
        };
        let shm = self.shm.as_ref().expect("checked at startup");
        let wl_pool = shm.create_pool(fd.as_fd(), (size * 2) as i32, qh, ());
        let buffers = (0..2)
            .map(|i| Buffer {
                wl: wl_pool.create_buffer(
                    (i * size) as i32,
                    width,
                    height,
                    stride,
                    wl_shm::Format::Xrgb8888,
                    qh,
                    i,
                ),
                offset: i * size,
                busy: false,
                shown: None,
            })
            .collect();
        wl_pool.destroy();
        self.pool = Some(Pool {
            _fd: fd,
            ptr,
            len: size * 2,
            width,
            height,
            buffers,
        });
        self.last = None;
        Ok(())
    }

    fn scene(&mut self, width: i32) -> Scene {
        let now = Instant::now();
        let started = *self.started.get_or_insert(now);
        let span = f64::from(width + BAR_WIDTH);
        let x = (now.duration_since(started).as_secs_f64() * BAR_SPEED) % span;
        let height = self.pool.as_ref().map_or(0, |p| p.height);
        Scene {
            frame: self.frame,
            bar: Rect::new(x as i32 - BAR_WIDTH, 0, BAR_WIDTH, height),
            flash: self.flash_until.is_some_and(|until| now < until),
        }
    }

    /// Draws the next frame into a free buffer and commits it.
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let Some(surface) = self.surface.clone() else {
            return;
        };
        let Some((width, height)) = self.pool.as_ref().map(|p| (p.width, p.height)) else {
            return;
        };
        let Some(index) = self
            .pool
            .as_ref()
            .and_then(|p| p.buffers.iter().position(|b| !b.busy))
        else {
            // Both buffers are with the compositor; draw when one comes back.
            return;
        };
        let scene = self.scene(width);
        let pool = self.pool.as_mut().expect("checked above");
        let full = Rect::new(0, 0, width, height);
        let changing = |prev: &Scene, now: &Scene| {
            let mut rects = vec![prev.bar, now.bar, counter_rect(), strip_rect(height)];
            if prev.flash != now.flash {
                rects.push(flash_rect(width, height));
            }
            rects
        };
        let buffer = &mut pool.buffers[index];
        // What this buffer needs repainting: everything that differs from what it
        // showed last (it may be two frames old).
        let repaint = match &buffer.shown {
            Some(shown) => changing(shown, &scene),
            None => vec![full],
        };
        // SAFETY: the buffer's region of our mapping; the compositor released it.
        let pixels = unsafe {
            std::slice::from_raw_parts_mut(
                (pool.ptr as *mut u8).add(buffer.offset) as *mut u32,
                (width * height) as usize,
            )
        };
        let mut canvas = Canvas {
            pixels,
            width,
            height,
        };
        for rect in &repaint {
            canvas.paint(*rect, &scene);
        }
        buffer.shown = Some(scene);
        buffer.busy = true;
        // What the compositor needs to update: the change since the last commit.
        let damage = match &self.last {
            Some(last) => changing(last, &scene),
            None => vec![full],
        };
        surface.attach(Some(&buffer.wl), 0, 0);
        for r in damage {
            surface.damage_buffer(r.x, r.y, r.w, r.h);
        }
        surface.frame(qh, ());
        surface.commit();
        self.waiting = true;
        self.last = Some(scene);
        self.frame += 1;
        self.count_frame();
    }

    fn count_frame(&mut self) {
        let now = Instant::now();
        let (count, since) = &mut self.fps_window;
        *count += 1;
        let since = *since.get_or_insert(now);
        if now.duration_since(since) >= Duration::from_secs(10) {
            let fps = *count as f64 / now.duration_since(since).as_secs_f64();
            println!("cha-testpattern: frame {} ({fps:.1} fps)", self.frame);
            self.fps_window = (0, Some(now));
        }
    }

    fn flash(&mut self) {
        self.flash_until = Some(Instant::now() + FLASH_FOR);
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for App {
    fn event(
        app: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" => {
                    app.compositor = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => app.wm_base = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => {
                    registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(5), qh, ());
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for App {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for App {
    fn event(
        app: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => {
                app.pending_size = Some(if width > 0 && height > 0 {
                    (width, height)
                } else {
                    DEFAULT_SIZE
                });
            }
            xdg_toplevel::Event::Close => app.exit = true,
            _ => {}
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for App {
    fn event(
        app: &mut Self,
        xdg_surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            let size = app.pending_size.take().unwrap_or(DEFAULT_SIZE);
            let current = app.pool.as_ref().map(|p| (p.width, p.height));
            if current != Some(size) {
                app.pool = None;
                if let Err(err) = app.allocate(qh, size.0, size.1) {
                    eprintln!("cha-testpattern: allocating buffers: {err}");
                    app.exit = true;
                    return;
                }
            }
            if !app.configured || !app.waiting {
                app.configured = true;
                app.draw(qh);
            }
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for App {
    fn event(
        app: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            app.waiting = false;
            app.draw(qh);
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for App {
    fn event(
        app: &mut Self,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        index: &usize,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event
            && let Some(pool) = app.pool.as_mut()
            && let Some(buffer) = pool.buffers.get_mut(*index)
        {
            buffer.busy = false;
            // A frame callback arrived while both buffers were out.
            if !app.waiting {
                app.draw(qh);
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for App {
    fn event(
        _: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
            if caps.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for App {
    fn event(
        app: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button {
            state: WEnum::Value(wl_pointer::ButtonState::Pressed),
            ..
        } = event
        {
            app.flash();
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for App {
    fn event(
        app: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The keymap's fd is dropped (closed) with the event; we don't need it.
        if let wl_keyboard::Event::Key {
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
        {
            app.flash();
        }
    }
}

delegate_noop!(App: ignore wl_compositor::WlCompositor);
delegate_noop!(App: ignore wl_shm::WlShm);
delegate_noop!(App: ignore wl_shm_pool::WlShmPool);
delegate_noop!(App: ignore wl_surface::WlSurface);
