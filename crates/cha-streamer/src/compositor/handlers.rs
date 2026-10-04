//! Protocol handlers. The window policy is a kiosk's: every top-level window
//! fills the output (maximized, or fullscreen when it asks), the newest one has
//! the keyboard, and dialogs are centered at their own size.

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::renderer::ImportDma;
use smithay::backend::renderer::utils::on_commit_buffer_handler;
use smithay::desktop::{
    PopupKeyboardGrab, PopupKind, PopupPointerGrab, PopupUngrabStrategy, Window,
    find_popup_root_surface, get_popup_toplevel_coords,
};
use smithay::input::dnd::{DnDGrab, DndGrabHandler, GrabType, Source};
use smithay::input::pointer::{CursorImageStatus, Focus, PointerHandle};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::protocol::{wl_buffer, wl_output, wl_seat, wl_surface::WlSurface};
use smithay::reexports::wayland_server::{Client, Resource};
use smithay::utils::{Logical, Point, SERIAL_COUNTER, Serial};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface,
    with_states,
};
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::pointer_constraints::{PointerConstraintsHandler, with_pointer_constraint};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState, set_primary_focus,
};
use smithay::wayland::shell::xdg::decoration::XdgDecorationHandler;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
    XdgToplevelSurfaceData,
};
use smithay::wayland::shm::{ShmHandler, ShmState};

use super::{ClientState, State};

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .expect("every client has our state")
            .compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.stats.commits += 1;
        self.dirty = true;
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.window_for(&root) {
                window.on_commit();
                if &root == surface {
                    self.place_dialog(&window);
                }
            }
        }
        self.initial_configure(surface);
    }
}

impl State {
    /// Sends the first configure of a new toplevel or popup.
    fn initial_configure(&mut self, surface: &WlSurface) {
        if let Some(window) = self.window_for(surface) {
            let sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .is_some_and(|d| d.lock().expect("toplevel data lock").initial_configure_sent)
            });
            if !sent && let Some(toplevel) = window.toplevel() {
                toplevel.send_configure();
            }
        }
        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface)
            && !popup.is_initial_configure_sent()
        {
            // The initial configure is always allowed.
            let _ = popup.send_configure();
        }
    }

    /// Fills the output with a top-level window, or leaves a dialog its size.
    fn configure_toplevel(&self, toplevel: &ToplevelSurface) {
        let (w, h) = self.output_size();
        let dialog = toplevel.parent().is_some();
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
            if dialog {
                return;
            }
            if !state.states.contains(xdg_toplevel::State::Fullscreen) {
                state.states.set(xdg_toplevel::State::Maximized);
            }
            state.size = Some((w, h).into());
        });
    }

    /// Re-fits every window after the output changed size.
    pub(super) fn configure_windows(&mut self) {
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            if let Some(toplevel) = window.toplevel() {
                self.configure_toplevel(toplevel);
                if toplevel.is_initial_configure_sent() {
                    toplevel.send_pending_configure();
                }
            }
        }
    }

    /// Centers a dialog once its size is known.
    fn place_dialog(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else {
            return;
        };
        if toplevel.parent().is_none() {
            return;
        }
        let size = window.geometry().size;
        if size.w <= 0 || size.h <= 0 {
            return;
        }
        let (w, h) = self.output_size();
        let location: Point<i32, Logical> = ((w - size.w) / 2, (h - size.h) / 2).into();
        if self.space.element_location(window) != Some(location) {
            self.space.map_element(window.clone(), location, true);
        }
    }

    /// Gives the keyboard to `window` and marks it the active one.
    fn focus_window(&mut self, window: &Window, serial: Serial) {
        for other in self.space.elements() {
            other.set_activated(other == window);
            if let Some(toplevel) = other.toplevel()
                && toplevel.is_initial_configure_sent()
            {
                toplevel.send_pending_configure();
            }
        }
        if let Some(toplevel) = window.toplevel() {
            let keyboard = self.seat.get_keyboard().expect("the seat has a keyboard");
            keyboard.set_focus(self, Some(toplevel.wl_surface().clone()), serial);
        }
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            return;
        };
        let Some(window) = self.window_for(&root) else {
            return;
        };
        let Some(window_geo) = self.space.element_geometry(&window) else {
            return;
        };
        let (w, h) = self.output_size();
        let mut target = smithay::utils::Rectangle::<i32, Logical>::from_size((w, h).into());
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        target.loc -= window_geo.loc;
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        if self.renderer.import_dmabuf(&dmabuf, None).is_ok() {
            let _ = notifier.successful::<State>();
        } else {
            notifier.failed();
        }
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.configure_toplevel(&surface);
        let window = Window::new_wayland_window(surface);
        self.space.map_element(window.clone(), (0, 0), true);
        self.focus_window(&window, SERIAL_COUNTER.next_serial());
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        let _ = self.popups.track_popup(PopupKind::Xdg(surface));
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let Some(seat) = Seat::<State>::from_resource(&seat) else {
            return;
        };
        let kind = PopupKind::Xdg(surface);
        let Some(root) = find_popup_root_surface(&kind)
            .ok()
            .filter(|root| self.window_for(root).is_some())
        else {
            return;
        };
        let Ok(mut grab) = self.popups.grab_popup(root, kind, &seat, serial) else {
            return;
        };
        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial)
                    || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial)
                    || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        self.configure_toplevel(&surface);
        if surface.is_initial_configure_sent() {
            surface.send_pending_configure();
        }
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<wl_output::WlOutput>,
    ) {
        let (w, h) = self.output_size();
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Fullscreen);
            state.size = Some((w, h).into());
        });
        if surface.is_initial_configure_sent() {
            surface.send_pending_configure();
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Fullscreen);
        });
        self.configure_toplevel(&surface);
        if surface.is_initial_configure_sent() {
            surface.send_pending_configure();
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // Hand the keyboard to the window now on top.
        let next = self
            .space
            .elements()
            .rev()
            .find(|w| w.toplevel().is_some_and(|t| t != &surface))
            .cloned();
        if let Some(window) = next {
            self.focus_window(&window, SERIAL_COUNTER.next_serial());
        }
        self.dirty = true;
    }
}

impl XdgDecorationHandler for State {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        // We draw no decorations, and want none drawn by the apps either.
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        self.new_decoration(toplevel.clone());
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.request_mode(toplevel, DecorationMode::ServerSide);
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<State> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
        self.dirty = true;
    }
}

impl PointerConstraintsHandler for State {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        if pointer.current_focus().as_ref() == Some(surface) {
            with_pointer_constraint(surface, pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
    }
}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl PrimarySelectionHandler for State {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

impl DndGrabHandler for State {}

impl WaylandDndGrabHandler for State {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        match type_ {
            GrabType::Pointer => {
                let pointer = seat.get_pointer().expect("the seat has a pointer");
                let Some(start) = pointer.grab_start_data() else {
                    source.cancel();
                    return;
                };
                let grab = DnDGrab::new_pointer(&self.display_handle, start, source, seat);
                pointer.set_grab(self, grab, serial, Focus::Keep);
            }
            GrabType::Touch => source.cancel(),
        }
    }
}

impl OutputHandler for State {}

smithay::delegate_dispatch2!(State);
