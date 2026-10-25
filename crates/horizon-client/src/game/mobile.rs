//! iPad bootstrap and lifecycle adapters. Gameplay stays in the shared Game.
use super::*;
use horizon_client::{launch::LaunchConfig, mobile_resources};
use horizon_ios_platform::{self as platform, Event, PickKind};
use std::fs::File;

#[derive(Default)]
pub(super) struct MobileState {
    pub suspended: bool,
    keyboard: bool,
    interrupted: bool,
    last_frame: Option<Instant>,
    last_sample: Option<Instant>,
    last_checkpoint: Option<Instant>,
    observed_frame: u64,
    frame_ms: Vec<f32>,
    samples: Vec<serde_json::Value>,
    reports: Option<std::sync::mpsc::SyncSender<PerformanceSnapshot>>,
    setup_requested: bool,
    setup_visible: bool,
    resources_changed: bool,
    pending_resource_import: bool,
    import: Option<Receiver<std::result::Result<ImportResult, String>>>,
    export: Option<Receiver<std::result::Result<PathBuf, String>>>,
}

struct ImportResult {
    message: String,
    resources_changed: bool,
    refresh_worlds: bool,
}

enum SetupWorkResult {
    Message(String),
    Backup(PathBuf),
}

struct PerformanceSnapshot {
    frames: Vec<f32>,
    samples: Vec<serde_json::Value>,
    render_distance: i32,
    simulation_distance: i32,
}

fn report_writer(path: PathBuf) -> std::sync::mpsc::SyncSender<PerformanceSnapshot> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<PerformanceSnapshot>(1);
    std::thread::spawn(move || {
        while let Ok(snapshot) = rx.recv() {
            let result = (|| -> Result<()> {
                use std::io::Write;
                let stage = path.with_extension("json.tmp");
                let mut file = File::create(&stage)?;
                serde_json::to_writer(
                    &mut file,
                    &serde_json::json!({
                        "schema": 1, "scope": "physical-device-measurement-required",
                        "status": "unreviewed", "frames": snapshot.frames,
                        "samples": snapshot.samples,
                        "render_distance": snapshot.render_distance,
                        "simulation_distance": snapshot.simulation_distance,
                        "automatic_quality_reduction": false
                    }),
                )?;
                file.flush()?;
                file.sync_all()?;
                drop(file);
                std::fs::rename(stage, &path)?;
                Ok(())
            })();
            if let Err(error) = result {
                log::warn!("iPad performance report: {error}");
            }
        }
    });
    tx
}

pub(super) struct Setup {
    config: LaunchConfig,
    window: Option<Arc<Window>>,
    gpu: Option<BootstrapGpu>,
    worker: Option<Receiver<std::result::Result<SetupWorkResult, String>>>,
    message: String,
    suspended: bool,
    smoke: bool,
    reported: bool,
    frames: u64,
    ticks: u64,
}

pub(super) fn run(config: LaunchConfig) -> Result<()> {
    mobile_resources::recover_import_transaction(&config.paths.resources)?;
    let mut setup = Setup {
        config, window: None, gpu: None, worker: None,
        message: "Import the resource ZIP prepared from your own Minecraft Java 26.3 installation. Your worlds stay on this iPad.".into(),
        suspended: false,
        smoke: std::env::var_os("HORIZON_IOS_SMOKE").is_some_and(|v| v == "1"),
        reported: false, frames: 0, ticks: 0,
    };
    if setup.ready() {
        setup.message = "Resources are ready. Play, import a world, or run the benchmark.".into();
    }
    let mut app = App::Setup(Box::new(setup));
    EventLoop::new()?.run_app(&mut app)?;
    Ok(())
}

impl Setup {
    fn ready(&self) -> bool {
        // The importer validates the entire tree before promotion; this check
        // selects that already committed installation without hashing it per frame.
        std::fs::read(self.config.paths.resources.join("horizon-resources.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|manifest| manifest["schema"] == 7)
    }
    fn update_panel(&self) {
        if let Some(window) = &self.window {
            if let Err(error) = platform::show_setup(
                window.as_ref(),
                &self.message,
                self.ready() && self.worker.is_none(),
            ) {
                log::error!("Horizon setup: {error}");
            }
        }
    }
    pub(super) fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.suspended = false;
        if self.window.is_none() {
            use winit::platform::ios::{ValidOrientations, WindowAttributesExtIOS};
            match event_loop.create_window(
                Window::default_attributes()
                    .with_title("Horizon")
                    .with_valid_orientations(ValidOrientations::Landscape)
                    .with_prefers_status_bar_hidden(true),
            ) {
                Ok(window) => self.window = Some(Arc::new(window)),
                Err(error) => {
                    self.fail(&error.to_string());
                    return;
                }
            }
        }
        if self.gpu.is_none() {
            match pollster::block_on(BootstrapGpu::new(self.window.as_ref().unwrap().clone())) {
                Ok(gpu) => self.gpu = Some(gpu),
                Err(error) => {
                    self.fail(&error.to_string());
                    return;
                }
            }
        }
        self.update_panel();
    }
    pub(super) fn suspend(&mut self) {
        self.suspended = true;
        self.gpu = None;
    }
    fn fail(&mut self, error: &str) {
        self.message = format!("Horizon could not start: {error}");
        log::error!("{}", self.message);
        if self.smoke {
            let _ = std::fs::write(self.config.paths.documents.join("horizon-ios-smoke.json"),
                serde_json::json!({"schema":1,"status":"fail","scope":"bootstrap-metal","error":error}).to_string());
        }
        self.update_panel();
    }
    pub(super) fn window_event(&mut self, _: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.window.as_ref().is_none_or(|w| w.id() != id) || self.suspended {
            return;
        }
        match event {
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
                self.update_panel();
            }
            WindowEvent::RedrawRequested => {
                if let Some(gpu) = &mut self.gpu {
                    match gpu.draw() {
                        Ok(true) => self.frames += 1,
                        Ok(false) => {}
                        Err(error) => self.fail(&error.to_string()),
                    }
                }
                if self.smoke && !self.reported && self.frames >= 3 && self.ticks >= 1 {
                    let m = platform::metrics();
                    let report = serde_json::json!({"schema":1,"status":"pass","scope":"bootstrap-metal",
                        "frames":self.frames,"ticks":self.ticks,"backend":"Metal","system_version":m.system_version,
                        "resident_bytes":m.resident_bytes,"thermal_state":m.thermal_state});
                    let path = self.config.paths.documents.join("horizon-ios-smoke.json");
                    if std::fs::write(&path, report.to_string()).is_ok() {
                        self.reported = true;
                    }
                }
            }
            _ => {}
        }
    }
    pub(super) fn poll(&mut self, event_loop: &ActiveEventLoop) -> Option<Loading> {
        if self.suspended {
            event_loop.set_control_flow(ControlFlow::Wait);
            return None;
        }
        self.ticks += 1;
        if let Some(completion) = self.worker.as_ref().map(Receiver::try_recv) {
            match completion {
                Ok(result) => {
                    self.worker = None;
                    match result {
                        Ok(SetupWorkResult::Message(message)) => self.message = message,
                        Ok(SetupWorkResult::Backup(path)) => {
                            self.message = "Backup ready. Choose where to save it.".into();
                            self.update_panel();
                            if let Some(window) = &self.window
                                && let Err(error) = platform::share_file(window.as_ref(), &path)
                            {
                                self.fail(&error);
                            }
                        }
                        Err(error) => {
                            self.message = format!(
                                "The operation failed; your existing data is preserved.\n{error}"
                            );
                        }
                    }
                    self.update_panel();
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.message = "The operation stopped. Your existing data is preserved.".into();
                    self.update_panel();
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        for event in platform::poll_events() {
            match event {
                Event::ImportResources | Event::ImportWorld if self.worker.is_none() => {
                    let kind = if matches!(event, Event::ImportWorld) {
                        PickKind::World
                    } else {
                        PickKind::Resources
                    };
                    if let Some(window) = &self.window {
                        if let Err(error) = platform::open_picker(window.as_ref(), kind) {
                            self.fail(&error);
                        }
                    }
                }
                Event::Picked { path, kind } if self.worker.is_none() => {
                    let (tx, rx) = std::sync::mpsc::channel();
                    let paths = self.config.paths.clone();
                    self.worker = Some(rx);
                    self.message = "Importing and verifying files…".into();
                    self.update_panel();
                    std::thread::spawn(move || {
                        let result = (|| -> Result<String> {
                            match kind {
                                PickKind::Resources => {
                                    let report = mobile_resources::import_resources_from_path(
                                        &path,
                                        &paths.resources,
                                    )?;
                                    Ok(format!(
                                        "Resources ready: {} verified files. You can now play.",
                                        report.files
                                    ))
                                }
                                PickKind::World => {
                                    let name: String = path
                                        .file_stem()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .chars()
                                        .map(|c| {
                                            if c.is_alphanumeric() || c == '-' || c == '_' {
                                                c
                                            } else {
                                                '_'
                                            }
                                        })
                                        .take(60)
                                        .collect();
                                    let name = if name.is_empty() { "Imported" } else { &name };
                                    let mut destination = paths.home.join("saves").join(name);
                                    for index in 1.. {
                                        if !destination.exists() {
                                            break;
                                        }
                                        destination = paths
                                            .home
                                            .join("saves")
                                            .join(format!("{name}-{index}"));
                                    }
                                    mobile_resources::import_world_archive(
                                        File::open(path)?,
                                        &destination,
                                    )?;
                                    Ok(format!(
                                        "World imported as {}.",
                                        destination
                                            .file_name()
                                            .unwrap_or_default()
                                            .to_string_lossy()
                                    ))
                                }
                            }
                        })()
                        .map_err(|e| e.to_string());
                        let _ = tx.send(result.map(SetupWorkResult::Message));
                    });
                }
                Event::Play | Event::Benchmark if self.ready() && self.worker.is_none() => {
                    let benchmark = matches!(event, Event::Benchmark);
                    match self.loading(benchmark) {
                        Ok(loading) => return Some(loading),
                        Err(error) => self.fail(&error.to_string()),
                    }
                }
                Event::EditTouch if self.ready() && self.worker.is_none() => {
                    match self.loading(false) {
                        Ok(mut loading) => {
                            loading.mobile_edit_touch = true;
                            return Some(loading);
                        }
                        Err(error) => self.fail(&error.to_string()),
                    }
                }
                Event::Share if self.worker.is_none() => self.start_backup(),
                Event::Error(error) => self.fail(&error),
                _ => {}
            }
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(16),
        ));
        None
    }
    fn start_backup(&mut self) {
        let metrics = platform::metrics();
        let report = serde_json::json!({
            "schema": 1,
            "system_version": metrics.system_version,
            "thermal_state": metrics.thermal_state,
            "resident_bytes": metrics.resident_bytes,
            "evidence": "setup-only; no gameplay or performance acceptance"
        });
        let documents = self.config.paths.documents.clone();
        let config = self.config.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.worker = Some(rx);
        self.message = "Preparing diagnostics and world backups…".into();
        self.update_panel();
        std::thread::spawn(move || {
            let result = (|| -> std::result::Result<SetupWorkResult, String> {
                std::fs::write(
                    documents.join("horizon-ipad-diagnostics.json"),
                    report.to_string(),
                )
                .map_err(|error| error.to_string())?;
                export_backups(&config)
                    .map(SetupWorkResult::Backup)
                    .map_err(|error| error.to_string())
            })();
            let _ = tx.send(result);
        });
    }
    fn loading(&mut self, benchmark: bool) -> Result<Loading> {
        let mut options = Options::read()?.ok_or("No launch options")?;
        options.resources = self.config.paths.resources.clone();
        if benchmark {
            options.benchmark = Some("standard".into());
        }
        let settings = Settings::load(&self.config.paths.home.join("options.txt"));
        Behaviours::set_cache_dir(self.config.paths.cache.clone());
        horizon_threads::favour_current();
        platform::audio_active(true).map_err(std::io::Error::other)?;
        let window = self
            .window
            .as_ref()
            .ok_or("iPad window unavailable")?
            .clone();
        self.gpu = None;
        let mut loading = make_loading(
            options,
            settings,
            self.config.paths.resources.clone(),
            None,
            Instant::now(),
        )?;
        loading.renderer = Some(loading_renderer(
            window.clone(),
            false,
            None,
            None,
            loading.settings.horizon.api,
        )?);
        loading.window = Some(window.clone());
        platform::hide_setup(window.as_ref());
        Ok(loading)
    }
}

struct BootstrapGpu {
    instance: wgpu::Instance,
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
}
impl BootstrapGpu {
    async fn new(window: Arc<Window>) -> Result<Self> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::METAL;
        let instance = wgpu::Instance::new(descriptor);
        let surface = instance.create_surface(window.clone())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await?;
        if adapter.get_info().backend != wgpu::Backend::Metal {
            return Err("A Metal adapter is required".into());
        }
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Horizon iPad bootstrap device"),
                // Match the game's open_device: Metal can expose fewer shader
                // varyings than wgpu's generic defaults even on a valid adapter.
                // The bootstrap only clears/presents; it needs no shader stages.
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("No usable Metal surface")?;
        config.present_mode = wgpu::PresentMode::Fifo;
        surface.configure(&device, &config);
        Ok(Self {
            instance,
            window,
            surface,
            device,
            queue,
            config,
        })
    }
    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }
    fn draw(&mut self) -> Result<bool> {
        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                self.surface = self.instance.create_surface(self.window.clone())?;
                self.surface.configure(&self.device, &self.config);
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("Metal surface validation failed".into());
            }
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Horizon iPad bootstrap smoke"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.075,
                            g: 0.09,
                            b: 0.11,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(output);
        Ok(true)
    }
}

pub(super) fn poll_loading(loading: &mut Loading) -> Result<()> {
    for event in platform::poll_events() {
        match event {
            Event::Text(text) => loading.mobile_text(&text)?,
            Event::Backspace => loading.mobile_backspace()?,
            Event::Enter => loading.mobile_enter()?,
            _ => {}
        }
    }
    let wants = loading.mobile_wants_keyboard();
    if let Some(window) = &loading.window {
        platform::keyboard(window.as_ref(), wants);
    }
    Ok(())
}

/// Only called after the live world has closed and its writer has flushed.
/// World originals remain read-only; the user chooses the share destination.
fn export_backups(config: &LaunchConfig) -> Result<PathBuf> {
    use std::io::{Read, Write};
    let output = config.paths.documents.join("Horizon-backup.zip");
    let stage = output.with_extension("zip.tmp");
    let mut bundle = zip::ZipWriter::new(File::create(&stage)?);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    match std::fs::read_dir(config.paths.home.join("saves")) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if !entry.file_type()?.is_dir() || !entry.path().join("level.dat").is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let temporary = config.paths.cache.join("world-export.zip");
                let file = mobile_resources::export_world_archive(
                    &entry.path(),
                    &name,
                    File::create(&temporary)?,
                )?;
                drop(file);
                bundle.start_file(format!("worlds/{name}.zip"), options)?;
                std::io::copy(&mut File::open(&temporary)?, &mut bundle)?;
                std::fs::remove_file(temporary)?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    for name in [
        "horizon-ipad-performance.json",
        "horizon-ipad-diagnostics.json",
        "horizon-ios-smoke.json",
    ] {
        if let Ok(mut file) = File::open(config.paths.documents.join(name)) {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            bundle.start_file(format!("diagnostics/{name}"), options)?;
            bundle.write_all(&bytes)?;
        }
    }
    bundle.finish()?.sync_all()?;
    std::fs::rename(stage, &output)?;
    Ok(output)
}

impl Game<'_> {
    /// A release from one device cannot release a mapping held by another.
    /// Only iPad merges these sources; desktop input behavior is unchanged.
    pub(super) fn mobile_input_held(
        &self,
        input: std::result::Result<KeyCode, MouseButton>,
    ) -> bool {
        if !self.focused || self.mobile.suspended || self.mobile_owns_input() {
            return false;
        }
        let touch = match input {
            Ok(key) => self.touch_holds_default_key(key),
            Err(button) => self.touch_holds_mouse(button),
        };
        touch
            || self
                .physical_keys
                .iter()
                .any(|key| self.settings.keys.translate(*key) == Some(input))
            || self
                .physical_mouse
                .iter()
                .any(|button| self.settings.keys.translate_mouse(*button) == Some(input))
            || self.mobile_pad_holds_input(input)
    }

    /// Native file operations own the input until the saved title screen is
    /// returned to the game. Physical controllers must not open a world while
    /// a backup worker is reading its files.
    pub(super) fn mobile_owns_input(&self) -> bool {
        self.mobile.setup_requested || self.mobile.setup_visible
    }

    pub(super) fn mobile_suspend(&mut self, event_loop: &ActiveEventLoop) {
        if self.mobile.suspended {
            return;
        }
        if let Err(e) = self.cancel_touch_input(event_loop) {
            log::warn!("Touch release: {e}");
        }
        self.mobile.suspended = true;
        self.focused = false;
        self.keys.clear();
        self.physical_keys.clear();
        self.physical_mouse.clear();
        self.mobile_reset_pad();
        self.left = false;
        self.right = false;
        self.toggles = Default::default();
        self.capture(false);
        self.audio.pause();
        if let Err(e) = platform::audio_active(false) {
            log::warn!("Audio suspend: {e}");
        }
        if let Some(window) = &self.window {
            platform::keyboard(window.as_ref(), false);
        }
        self.mobile.keyboard = false;
        if let Some(renderer) = &mut self.renderer {
            renderer.release_surface();
        }
        if let Some(server) = &self.integrated {
            server.set_paused(true);
        } else if self.remote.is_some() {
            self.message = Some((
                "The app is suspended. The server connection may time out.".into(),
                Instant::now(),
            ));
        }
        self.mobile_checkpoint();
        self.write_mobile_report();
    }
    pub(super) fn mobile_resume(&mut self) {
        if !self.mobile.suspended {
            return;
        }
        self.mobile.suspended = false;
        self.mobile_reset_pad();
        self.focused = true;
        self.previous = Instant::now();
        self.mobile.last_frame = None;
        if let Some(renderer) = &mut self.renderer {
            if let Err(e) = renderer.recreate_surface() {
                self.error = Some(e);
            }
        }
        if let Err(e) = platform::audio_active(true) {
            log::warn!("Audio resume: {e}");
        }
        self.audio.resume();
        if let Some(server) = &self.integrated {
            server.set_paused(self.pauses_game());
        }
    }
    pub(super) fn mobile_memory_warning(&mut self) {
        if let Some(renderer) = &mut self.renderer {
            renderer.trim_unused_memory();
        }
        self.mobile_checkpoint();
        log::warn!(
            "iPad memory pressure: released unused GPU arena pages; gameplay and graphics settings preserved"
        );
    }
    fn mobile_checkpoint(&mut self) {
        self.mobile.last_checkpoint = Some(Instant::now());
        if self.title.is_some() || self.player.is_none() {
            return;
        }
        let token = platform::BackgroundTask::begin();
        if let Some(server) = &self.integrated {
            server.checkpoint(move |result| {
                if let Err(error) = result {
                    log::error!("iPad checkpoint failed: {error}");
                }
                drop(token);
            });
        } else {
            if let Err(error) = self.save_game() {
                log::error!("iPad checkpoint failed: {error}");
            }
            drop(token);
        }
        self.mobile.last_checkpoint = Some(Instant::now());
    }
    pub(super) fn mobile_poll(&mut self, event_loop: &ActiveEventLoop) {
        if self.mobile.suspended {
            return;
        }
        if self.touch_editor.is_none()
            && let Err(e) = self.touch_tick(event_loop)
        {
            log::warn!("Touch gesture: {e}");
        }
        if let Some(window) = &self.window {
            if self.touch_editor.is_none() && !self.mobile_owns_input() {
                if let Err(error) = platform::setup_accessory(window.as_ref()) {
                    log::warn!("iPad Files control: {error}");
                }
            } else {
                platform::hide_accessory(window.as_ref());
            }
        }
        if self.mobile.setup_requested
            && self.pending_save.is_none()
            && self.world_loader.is_none()
            && self.rest_loader.is_none()
        {
            if self.title.is_none() {
                if let Err(e) = self.close_world() {
                    log::error!("Save before opening files: {e}");
                    self.mobile.setup_requested = false;
                }
            } else {
                self.mobile.setup_requested = false;
                self.mobile.setup_visible = true;
                if let Some(window) = &self.window {
                    let message = if self.mobile.resources_changed {
                        "Resources were updated. Close and reopen Horizon before playing or running the benchmark."
                    } else {
                        "Your world has been saved. Import a world, share a backup, or return to the game."
                    };
                    let ready = !self.mobile.resources_changed
                        && self.mobile.import.is_none()
                        && self.mobile.export.is_none();
                    let _ = platform::show_setup(window.as_ref(), message, ready);
                }
            }
        }
        if let Some(completion) = self.mobile.import.as_ref().map(Receiver::try_recv) {
            match completion {
                Ok(result) => {
                    self.mobile.import = None;
                    self.mobile.pending_resource_import = false;
                    let message = match result {
                        Ok(import) => {
                            if import.resources_changed {
                                self.mobile.resources_changed = true;
                            }
                            if import.refresh_worlds {
                                self.title = Some(Title::new(
                                    std::env::current_dir().unwrap_or_default().join("saves"),
                                    &self.options.resources,
                                ));
                            }
                            import.message
                        }
                        Err(error) => {
                            format!("Import failed; existing data was preserved. {error}")
                        }
                    };
                    if let Some(window) = &self.window {
                        let ready = !self.mobile.resources_changed && self.mobile.export.is_none();
                        let _ = platform::show_setup(window.as_ref(), &message, ready);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.mobile.import = None;
                    if self.mobile.pending_resource_import {
                        self.mobile.resources_changed = true;
                    }
                    self.mobile.pending_resource_import = false;
                    if let Some(window) = &self.window {
                        let ready = !self.mobile.resources_changed && self.mobile.export.is_none();
                        let _ = platform::show_setup(
                            window.as_ref(),
                            if self.mobile.resources_changed {
                                "The resource import ended without a result. Close and reopen Horizon before playing to refresh its resource state."
                            } else {
                                "The import stopped before reporting a result. Existing data was preserved."
                            },
                            ready,
                        );
                    }
                }
            }
        }
        if let Some(completion) = self.mobile.export.as_ref().map(Receiver::try_recv) {
            match completion {
                Ok(result) => {
                    self.mobile.export = None;
                    match result {
                        Ok(path) => {
                            if let Some(window) = &self.window {
                                let _ = platform::show_setup(
                                    window.as_ref(),
                                    "Backup ready. Choose where to save it.",
                                    !self.mobile.resources_changed && self.mobile.import.is_none(),
                                );
                                if let Err(error) = platform::share_file(window.as_ref(), &path) {
                                    log::error!("Share backup: {error}");
                                    let _ = platform::show_setup(
                                        window.as_ref(),
                                        &format!(
                                            "Backup is ready, but the share sheet could not open: {error}"
                                        ),
                                        !self.mobile.resources_changed
                                            && self.mobile.import.is_none(),
                                    );
                                }
                            }
                        }
                        Err(error) => {
                            if let Some(window) = &self.window {
                                let _ = platform::show_setup(
                                    window.as_ref(),
                                    &format!("Backup export failed: {error}"),
                                    !self.mobile.resources_changed && self.mobile.import.is_none(),
                                );
                            }
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.mobile.export = None;
                    if let Some(window) = &self.window {
                        let _ = platform::show_setup(
                            window.as_ref(),
                            "Backup export stopped before completion.",
                            !self.mobile.resources_changed && self.mobile.import.is_none(),
                        );
                    }
                }
            }
        }
        for event in platform::poll_events() {
            if self.touch_editor.is_some() {
                match event {
                    Event::AudioInterrupted => {
                        self.mobile.interrupted = true;
                        self.audio.pause();
                    }
                    Event::AudioResume | Event::AudioRouteChanged => {
                        self.mobile.interrupted = false;
                        if let Err(e) = platform::audio_active(true) {
                            log::warn!("Audio route: {e}");
                        }
                        self.audio.reload(&self.options.resources);
                    }
                    _ => {}
                }
                continue;
            }
            match event {
                Event::ShowSetup => {
                    if let Err(error) = self.cancel_touch_input(event_loop) {
                        log::warn!("Touch release before opening Files: {error}");
                    }
                    self.keys.clear();
                    self.physical_keys.clear();
                    self.physical_mouse.clear();
                    self.mobile_reset_pad();
                    self.pending_entry = None;
                    if let Some(loader) = self.world_loader.take() {
                        self.world_loader = Some(if loader.cancelled {
                            loader
                        } else {
                            loader.cancel()
                        });
                    }
                    self.left = false;
                    self.right = false;
                    self.capture(false);
                    self.mobile.setup_requested = true;
                }
                Event::Play if self.mobile.setup_visible => {
                    if self.mobile.resources_changed {
                        if let Some(window) = &self.window {
                            let _ = platform::show_setup(
                                window.as_ref(),
                                "Resources were updated. Close and reopen Horizon before playing or running the benchmark.",
                                false,
                            );
                        }
                    } else if self.mobile.import.is_none() && self.mobile.export.is_none() {
                        self.mobile_reset_pad();
                        self.mobile.setup_visible = false;
                        if let Some(window) = &self.window {
                            platform::hide_setup(window.as_ref());
                        }
                    }
                }
                Event::Benchmark if self.mobile.setup_visible => {
                    if self.mobile.resources_changed {
                        if let Some(window) = &self.window {
                            let _ = platform::show_setup(
                                window.as_ref(),
                                "Resources were updated. Close and reopen Horizon before playing or running the benchmark.",
                                false,
                            );
                        }
                    } else if self.mobile.import.is_none() && self.mobile.export.is_none() {
                        self.mobile_start_benchmark();
                    }
                }
                Event::EditTouch
                    if self.mobile.setup_visible
                        && self.mobile.import.is_none()
                        && self.mobile.export.is_none() =>
                {
                    self.mobile.setup_visible = false;
                    self.begin_touch_edit();
                }
                Event::ImportResources | Event::ImportWorld
                    if self.mobile.setup_visible
                        && self.mobile.import.is_none()
                        && self.mobile.export.is_none() =>
                {
                    let kind = if matches!(event, Event::ImportWorld) {
                        PickKind::World
                    } else {
                        PickKind::Resources
                    };
                    if let Some(window) = &self.window {
                        if let Err(e) = platform::open_picker(window.as_ref(), kind) {
                            log::error!("Import picker: {e}");
                        }
                    }
                }
                Event::Picked { path, kind }
                    if self.mobile.setup_visible
                        && self.mobile.import.is_none()
                        && self.mobile.export.is_none() =>
                {
                    let resources = std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .map(|root| LaunchConfig::ipad(&root).paths.resources)
                        .unwrap_or_else(|| self.options.resources.clone());
                    let saves = std::env::current_dir().unwrap_or_default().join("saves");
                    let (tx, rx) = std::sync::mpsc::channel();
                    self.mobile.import = Some(rx);
                    self.mobile.pending_resource_import = kind == PickKind::Resources;
                    if let Some(window) = &self.window {
                        let _ = platform::show_setup(
                            window.as_ref(),
                            "Importing and verifying files…",
                            false,
                        );
                    }
                    std::thread::spawn(move || {
                        let result = (|| -> Result<ImportResult> {
                            if kind == PickKind::Resources {
                                mobile_resources::recover_import_transaction(&resources)?;
                                mobile_resources::import_resources_from_path(&path, &resources)?;
                                Ok(ImportResult {
                                    message: "Resources updated. Close and reopen Horizon before playing with them.".into(),
                                    resources_changed: true,
                                    refresh_worlds: false,
                                })
                            } else {
                                let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis();
                                let destination = saves.join(format!("Imported-{stamp}"));
                                mobile_resources::import_world_archive(File::open(path)?, &destination)?;
                                Ok(ImportResult {
                                    message: "World imported. Return to the game to open it.".into(),
                                    resources_changed: false,
                                    refresh_worlds: true,
                                })
                            }
                        })().map_err(|e| e.to_string());
                        let _ = tx.send(result);
                    });
                }
                Event::Share
                    if self.mobile.setup_visible
                        && self.mobile.import.is_none()
                        && self.mobile.export.is_none() =>
                {
                    self.mobile_start_backup()
                }
                Event::Text(text) => {
                    if let Err(e) = self.mobile_text(&text) {
                        log::warn!("Native text: {e}");
                    }
                }
                Event::Backspace => {
                    if let Err(e) = self.mobile_backspace() {
                        log::warn!("Native backspace: {e}");
                    }
                }
                Event::Enter => {
                    if let Err(e) = self.mobile_enter() {
                        log::warn!("Native Enter: {e}");
                    }
                }
                Event::MouseButton { button, pressed } => {
                    let button = match button {
                        platform::MouseButton::Left => MouseButton::Left,
                        platform::MouseButton::Right => MouseButton::Right,
                        platform::MouseButton::Middle => MouseButton::Middle,
                    };
                    if let Some(window) = &self.window {
                        self.window_event(
                            event_loop,
                            window.id(),
                            WindowEvent::MouseInput {
                                device_id: DeviceId::dummy(),
                                state: if pressed {
                                    ElementState::Pressed
                                } else {
                                    ElementState::Released
                                },
                                button,
                            },
                        );
                    }
                }
                Event::MouseWheel(x, y) => {
                    if let Some(window) = &self.window {
                        self.window_event(
                            event_loop,
                            window.id(),
                            WindowEvent::MouseWheel {
                                device_id: DeviceId::dummy(),
                                delta: winit::event::MouseScrollDelta::LineDelta(
                                    x as f32, y as f32,
                                ),
                                phase: winit::event::TouchPhase::Moved,
                            },
                        );
                    }
                }
                Event::MouseMotion(x, y) if self.screen == Screen::Playing && self.captured => {
                    self.mouse.on_move(0.0, 0.0, x, y, true, true);
                    self.last_input = Instant::now();
                }
                Event::AudioInterrupted => {
                    self.mobile.interrupted = true;
                    self.audio.pause();
                }
                Event::AudioResume | Event::AudioRouteChanged => {
                    self.mobile.interrupted = false;
                    if let Err(e) = platform::audio_active(true) {
                        log::warn!("Audio route: {e}");
                    }
                    self.audio.reload(&self.options.resources);
                }
                _ => {}
            }
        }
        let wants = self.touch_editor.is_none() && self.mobile_wants_keyboard();
        if wants != self.mobile.keyboard {
            if let Some(window) = &self.window {
                platform::keyboard(window.as_ref(), wants);
            }
            self.mobile.keyboard = wants;
        }
        let now = Instant::now();
        if self.frames != self.mobile.observed_frame {
            self.mobile.observed_frame = self.frames;
            if let Some(previous) = self.mobile.last_frame.replace(now) {
                self.mobile
                    .frame_ms
                    .push((now - previous).as_secs_f32() * 1000.0);
            }
        }
        if self
            .mobile
            .last_sample
            .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1))
        {
            let m = platform::metrics();
            let size = self
                .renderer
                .as_ref()
                .map(|r| [r.config.width, r.config.height]);
            let tick_ms = self.integrated.as_ref().map(|s| s.smoothed_tick_ms());
            self.mobile.samples.push(
                serde_json::json!({"seconds":self.started.elapsed().as_secs_f64(),
                "resident_bytes":m.resident_bytes,"thermal_state":m.thermal_state,"tick_ms":tick_ms,
                "frames":self.frames,"resolution":size}),
            );
            self.mobile.last_sample = Some(now);
        }
        if self
            .mobile
            .last_checkpoint
            .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(30))
        {
            self.mobile_checkpoint();
            self.write_mobile_report();
        }
        let _ = event_loop;
    }
    fn mobile_start_benchmark(&mut self) {
        if self.title.is_none() || self.renderer.is_none() {
            if let Some(window) = &self.window {
                let _ = platform::show_setup(
                    window.as_ref(),
                    "Return to the saved title screen before starting the benchmark.",
                    true,
                );
            }
            return;
        }
        self.settings.vsync = false;
        self.settings.max_fps = 260;
        self.settings.inactivity_fps_limit = "minimized".into();
        self.options.no_pause = true;
        self.apply_settings();
        let game_dir = match std::env::current_dir() {
            Ok(path) => path,
            Err(error) => {
                if let Some(window) = &self.window {
                    let _ = platform::show_setup(
                        window.as_ref(),
                        &format!("The benchmark could not start: {error}"),
                        true,
                    );
                }
                return;
            }
        };
        let control = self
            .control
            .get_or_insert_with(horizon_client::control::Server::local);
        horizon_client::benchmark::start(
            control.sender(),
            horizon_client::benchmark::Setup {
                scenario: "standard".into(),
                started: self.started,
                app: horizon_bench::App {
                    name: "horizon-client".into(),
                    version: horizon_client::crash::version(),
                    commit: option_env!("HORIZON_COMMIT").map(str::to_owned),
                    mods: Vec::new(),
                },
                simulation_distance: self.settings.simulation_distance as u32,
                vsync: false,
                fps_cap: None,
                offscreen: self.options.offscreen,
                game_dir,
            },
        );
        self.mobile.setup_visible = false;
        if let Some(window) = &self.window {
            platform::hide_setup(window.as_ref());
        }
    }
    fn mobile_start_backup(&mut self) {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            if let Some(window) = &self.window {
                let _ = platform::show_setup(
                    window.as_ref(),
                    "Backup export failed: the application sandbox is unavailable.",
                    !self.mobile.resources_changed,
                );
            }
            return;
        };
        let config = LaunchConfig::ipad(&home);
        let metrics = platform::metrics();
        let report = serde_json::json!({
            "schema": 1,
            "system_version": metrics.system_version,
            "thermal_state": metrics.thermal_state,
            "resident_bytes": metrics.resident_bytes,
            "evidence": "gameplay; physical-device performance acceptance not claimed"
        });
        let documents = config.paths.documents.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        self.mobile.export = Some(rx);
        if let Some(window) = &self.window {
            let _ = platform::show_setup(
                window.as_ref(),
                "Preparing diagnostics and world backups…",
                false,
            );
        }
        std::thread::spawn(move || {
            let result = (|| -> std::result::Result<PathBuf, String> {
                std::fs::write(
                    documents.join("horizon-ipad-diagnostics.json"),
                    report.to_string(),
                )
                .map_err(|error| error.to_string())?;
                export_backups(&config).map_err(|error| error.to_string())
            })();
            let _ = tx.send(result);
        });
    }
    fn write_mobile_report(&mut self) {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let path = PathBuf::from(home).join("Documents/horizon-ipad-performance.json");
        let writer = self
            .mobile
            .reports
            .get_or_insert_with(|| report_writer(path));
        // Serialization and disk I/O stay off the render thread. A bounded
        // queue prevents slow storage from accumulating pending snapshots.
        let _ = writer.try_send(PerformanceSnapshot {
            frames: self.mobile.frame_ms.clone(),
            samples: self.mobile.samples.clone(),
            render_distance: self.settings.render_distance,
            simulation_distance: self.settings.simulation_distance,
        });
    }
}
