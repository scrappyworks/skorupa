//! One connector on the logind seat, through Smithay's safe GBM/GLES backend.

use std::error::Error;
use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::drm::compositor::{FrameFlags, PrimaryPlaneElement};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements};
use smithay::backend::drm::{
    DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmEvent, DrmEventTime, DrmNode, NodeType,
};
use smithay::backend::input::{InputEvent, KeyState, KeyboardKeyEvent};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::ImportMemWl;
use smithay::backend::renderer::element::RenderElementStates;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::multigpu::{GpuManager, MultiRenderer, gbm::GbmGlesBackend};
use smithay::backend::session::libseat::{LibSeatSession, LibSeatSessionNotifier};
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{all_gpus, primary_gpu};
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::drm::control::{Device, ModeTypeFlags, connector};
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::protocol::wl_shm::Format;
use smithay::utils::{Clock, DeviceFd, Monotonic};

use crate::{Compositor, Config, frame, state::State};

type Api = GbmGlesBackend<GlesRenderer, DrmDeviceFd>;
type Renderer<'a> = MultiRenderer<'a, 'a, Api, Api>;
type Element<'a> = WaylandSurfaceRenderElement<Renderer<'a>>;
type Allocator = GbmAllocator<DrmDeviceFd>;
type Exporter = GbmFramebufferExporter<DrmDeviceFd>;

pub(crate) struct Drm {
    manager: DrmOutputManager<Allocator, Exporter, (), DrmDeviceFd>,
    surface: DrmOutput<Allocator, Exporter, (), DrmDeviceFd>,
    gpu: GpuManager<Api>,
    node: DrmNode,
    active: bool,
}

pub(crate) struct Devices {
    session: LibSeatSessionNotifier,
    drm: DrmDeviceNotifier,
    input: Libinput,
}

fn select_connector(
    drm: &DrmDevice,
) -> Result<
    (
        connector::Info,
        smithay::reexports::drm::control::crtc::Handle,
        smithay::reexports::drm::control::Mode,
    ),
    Box<dyn Error>,
> {
    let resources = drm.resource_handles()?;
    let (connector, crtc) = resources
        .connectors()
        .iter()
        .filter_map(|handle| drm.get_connector(*handle, true).ok())
        .filter(|info| info.state() == connector::State::Connected && !info.modes().is_empty())
        .find_map(|info| {
            let crtc = info
                .encoders()
                .iter()
                .filter_map(|encoder| drm.get_encoder(*encoder).ok())
                .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
                .next()?;
            Some((info, crtc))
        })
        .ok_or("no connected connector with a usable CRTC")?;
    let mode = connector
        .modes()
        .iter()
        .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
        .copied()
        .unwrap_or(connector.modes()[0]);
    Ok((connector, crtc, mode))
}

impl Drm {
    pub(crate) fn open() -> Result<(Self, Devices, Output, Config), Box<dyn Error>> {
        let (mut session, notifier) = LibSeatSession::new()?;
        let seat = session.seat();
        let path = primary_gpu(&seat)?
            .or_else(|| all_gpus(&seat).ok()?.into_iter().next())
            .ok_or("no DRM device on this seat")?;
        let card = DrmNode::from_path(&path)?;
        let fd = session.open(
            &path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )?;
        let fd = DrmDeviceFd::new(DeviceFd::from(fd));
        let (drm, drm_notifier) = DrmDevice::new(fd.clone(), false)?;
        if !drm.is_atomic() {
            return Err("the DRM device does not support atomic modesetting".into());
        }
        let gbm = GbmDevice::new(fd)?;
        let node = card
            .node_with_type(NodeType::Render)
            .and_then(Result::ok)
            .unwrap_or(card);
        let mut gpu =
            GpuManager::<Api>::new(GbmGlesBackend::default()).map_err(|err| err.to_string())?;
        gpu.as_mut().add_node(node, gbm.clone())?;

        let (connector, crtc, mode) = select_connector(&drm)?;
        let wl_mode = Mode::from(mode);
        let (width, height) = connector.size().unwrap_or_default();
        let output = Output::new(
            format!(
                "{}-{}",
                connector.interface().as_str(),
                connector.interface_id()
            ),
            PhysicalProperties {
                size: (i32::try_from(width)?, i32::try_from(height)?).into(),
                subpixel: Subpixel::Unknown,
                make: "Skorupa".into(),
                model: "DRM".into(),
                serial_number: String::new(),
            },
        );
        output.set_preferred(wl_mode);
        output.change_current_state(Some(wl_mode), None, None, Some((0, 0).into()));
        let config = Config {
            output_size: (wl_mode.size.w, wl_mode.size.h),
            refresh_mhz: wl_mode.refresh,
            ..Config::default()
        };

        let mut renderer = gpu.single_renderer(&node).map_err(|err| err.to_string())?;
        let formats = renderer
            .as_mut()
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied()
            .collect::<FormatSet>();
        let allocator = GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        );
        let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(node).into());
        let mut manager = DrmOutputManager::new(
            drm,
            allocator,
            exporter,
            Some(gbm),
            [Fourcc::Abgr8888, Fourcc::Argb8888, Fourcc::Xrgb8888],
            formats,
        );
        let surface = manager
            .lock()
            .initialize_output::<_, Element<'_>>(
                crtc,
                mode,
                &[connector.handle()],
                &output,
                None,
                &mut renderer,
                &DrmOutputRenderElements::default(),
            )
            .map_err(|err| err.to_string())?;
        drop(renderer);

        let mut input =
            Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.into());
        input
            .udev_assign_seat(&seat)
            .map_err(|()| "libinput could not assign the seat")?;
        let drm = Self {
            manager,
            surface,
            gpu,
            node,
            active: true,
        };
        let devices = Devices {
            session: notifier,
            drm: drm_notifier,
            input,
        };
        Ok((drm, devices, output, config))
    }

    pub(crate) fn shm_formats(&mut self) -> Result<Vec<Format>, String> {
        let renderer = self
            .gpu
            .single_renderer(&self.node)
            .map_err(|err| err.to_string())?;
        Ok(renderer.shm_formats().collect())
    }

    pub(crate) fn render(
        &mut self,
        windows: &[crate::state::Window],
        placements: &[frame::Placement],
    ) -> Result<(bool, RenderElementStates), Box<dyn Error>> {
        if !self.active {
            return Ok((false, RenderElementStates::default()));
        }
        let mut renderer = self
            .gpu
            .single_renderer(&self.node)
            .map_err(|err| err.to_string())?;
        let elements = frame::elements(&mut renderer, windows, placements);
        let frame = self
            .surface
            .render_frame(
                &mut renderer,
                &elements,
                frame::BACKGROUND,
                FrameFlags::DEFAULT,
            )
            .map_err(|err| err.to_string())?;
        if frame.needs_sync()
            && let PrimaryPlaneElement::Swapchain(element) = &frame.primary_element
        {
            element.sync.wait()?;
        }
        if !frame.is_empty {
            self.surface
                .queue_frame(())
                .map_err(|err| err.to_string())?;
        }
        Ok((!frame.is_empty, frame.states))
    }

    pub(crate) fn complete(&mut self) -> bool {
        self.active
            && self
                .surface
                .frame_submitted()
                .is_ok_and(|frame| frame.is_some())
    }

    pub(crate) fn pause(&mut self) {
        self.active = false;
        self.manager.pause();
    }

    pub(crate) fn activate(&mut self) -> Result<(), String> {
        self.manager
            .lock()
            .activate(false)
            .map_err(|err| err.to_string())?;
        self.surface.reset_buffers();
        self.active = true;
        Ok(())
    }
}

impl Devices {
    pub(crate) fn register(
        self,
        handle: &LoopHandle<'_, Compositor>,
    ) -> Result<(), Box<dyn Error>> {
        let Self {
            session,
            drm,
            mut input,
        } = self;
        let clock = Clock::<Monotonic>::new();
        handle.insert_source(
            LibinputInputBackend::new(input.clone()),
            move |event, (), compositor| {
                if let InputEvent::Keyboard { event } = event {
                    compositor.key(
                        event.key_code().raw(),
                        event.state() == KeyState::Pressed,
                        Duration::from(clock.now()),
                    );
                }
            },
        )?;

        handle.insert_source(session, move |event, (), compositor| {
            let state: &mut State = &mut compositor.state;
            match event {
                SessionEvent::PauseSession => {
                    input.suspend();
                    state.pause_drm();
                }
                SessionEvent::ActivateSession => {
                    if let Err(err) = input.resume() {
                        tracing::warn!(?err, "libinput resume failed");
                    }
                    state.activate_drm();
                }
            }
        })?;

        let clock = Clock::<Monotonic>::new();
        handle.insert_source(drm, move |event, metadata, compositor| match event {
            DrmEvent::VBlank(_) => {
                let time = metadata.as_ref().and_then(|metadata| match metadata.time {
                    DrmEventTime::Monotonic(time) => Some(time),
                    DrmEventTime::Realtime(_) => None,
                });
                let sequence = metadata.as_ref().map_or(0, |metadata| metadata.sequence);
                compositor
                    .state
                    .complete_drm(Duration::from(clock.now()), time, sequence);
            }
            DrmEvent::Error(err) => tracing::warn!(%err, "DRM event failed"),
        })?;
        Ok(())
    }
}
