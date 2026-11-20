use super::tools;
use anyhow::{Context, Result, ensure};
use enensu::{
    device::{DeviceInfo, PlatformTools, Verification},
    install::{self, Options, Progress},
};
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    thread,
};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    platform::windows::IconExtWindows,
    window::{Icon, WindowBuilder},
};
use wry::{
    NewWindowResponse, WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows,
    http::Response,
};

const HOME: &str = "https://enensu.localhost/";
type Reply = mpsc::Sender<Option<PathBuf>>;

enum Message {
    Command(Value),
    Scan(u64, Result<Vec<DeviceInfo>>),
    Progress(Value),
    Backup(PathBuf, Value),
    Firmware(String, Reply),
    Finished(Result<Verification>),
}

struct Host {
    proxy: EventLoopProxy<Message>,
    tools: PlatformTools,
    root: PathBuf,
    devices: Vec<DeviceInfo>,
    modules: Vec<PathBuf>,
    firmware: Option<PathBuf>,
    backup: Option<PathBuf>,
    pending: Option<Reply>,
    scanning: bool,
    busy: bool,
    generation: u64,
}

fn send(view: &WebView, data: Value) {
    // Encode as JSON rather than interpolating file names or device output as JavaScript.
    let _ = view.evaluate_script(&format!("window.enensuReceive?.({data})"));
}

fn file_info(file: &Path) -> Value {
    json!({"name":file.file_name().unwrap_or_default().to_string_lossy(), "path":file,
        "size":file.metadata().map(|m| m.len()).unwrap_or(0)})
}

fn pick_image() -> Option<PathBuf> {
    tinyfiledialogs::open_file_dialog(
        "选择与手机当前固件匹配的启动镜像",
        "",
        Some((&["*.img"], "启动镜像 (*.img)")),
    )
    .map(PathBuf::from)
}

fn open_folder(path: &Path) -> Result<()> {
    ensure!(path.is_dir(), "目录不存在");
    let windows = std::env::var_os("WINDIR").context("找不到 Windows 目录")?;
    Command::new(PathBuf::from(windows).join("explorer.exe"))
        .arg(path)
        .spawn()?;
    Ok(())
}

fn progress(proxy: &EventLoopProxy<Message>, p: Progress<'_>) {
    let (stage, text) = match p {
        Progress::Checking => (0, "正在检查设备与模块兼容性".into()),
        Progress::Connected(s) => (0, format!("已识别设备 {s}")),
        Progress::Reading => (1, "正在获取启动镜像".into()),
        Progress::Patching(part) => (1, format!("正在修补 {part}")),
        Progress::Fastboot => (1, "正在进入 fastboot 并核对解锁状态与槽位".into()),
        Progress::ReadbackMode => (1, "正在另一 fastboot 模式下检查设备回读".into()),
        Progress::ImageSource { target, via } => {
            let method = via.unwrap_or("本地固件");
            let _ = proxy.send_event(Message::Progress(json!({"type":"progress","stage":1,"origin":method,"target":target,"message":format!("镜像来源 · {method} · {target}")})));
            return;
        }
        Progress::FirmwareNeeded(build) => (1, format!("需要与 {build} 匹配的启动镜像")),
        Progress::Saved { source, image } => {
            let event = json!({"type":"progress","stage":1,"message":"源镜像和修补镜像已保存","source":source,"image":image});
            let _ = proxy.send_event(Message::Backup(
                source.parent().unwrap_or(source).to_owned(),
                event,
            ));
            return;
        }
        Progress::Flashing(target) => (2, format!("正在写入 {target}")),
        Progress::Written(target) => (2, format!("fastboot 已确认写入 {target}")),
        Progress::Skipped(target) => (2, format!("{target} 已包含所选模块，已跳过写入")),
        Progress::Rebooting => (3, "正在重启到 Android".into()),
        Progress::Verifying => (3, "正在等待开机并核对模块加载状态".into()),
    };
    let _ = proxy.send_event(Message::Progress(
        json!({"type":"progress","stage":stage,"message":text}),
    ));
}

impl Host {
    fn scan(&mut self, view: &WebView) {
        if self.scanning || self.busy {
            return;
        }
        self.scanning = true;
        send(view, json!({"type":"scanning"}));
        let (tools, proxy, generation) = (self.tools.clone(), self.proxy.clone(), self.generation);
        thread::spawn(move || {
            let _ = proxy.send_event(Message::Scan(generation, tools.devices()));
        });
    }

    fn files(&self, view: &WebView) {
        send(
            view,
            json!({"type":"files", "modules":self.modules.iter().map(|p| file_info(p)).collect::<Vec<_>>(), "firmware":self.firmware.as_deref().map(file_info)}),
        );
    }

    fn install(&mut self, serial: &str, view: &WebView) -> Result<()> {
        ensure!(
            !self.busy && !self.modules.is_empty(),
            "请先选择至少一个内核模块"
        );
        ensure!(
            self.devices
                .iter()
                .any(|d| d.serial == serial && d.state == "device" && d.lock != Some(true)),
            "请先连接并选择已授权且已解锁的设备"
        );
        let opts = Options::for_desktop(
            serial.into(),
            self.modules.clone(),
            self.firmware.clone(),
            self.root.join("images"),
        );
        self.busy = true;
        self.generation += 1;
        self.backup = None;
        send(view, json!({"type":"started"}));
        let (tools, proxy) = (self.tools.clone(), self.proxy.clone());
        thread::spawn(move || {
            let result = install::run_with(
                opts,
                tools,
                |p| progress(&proxy, p),
                |_, build| {
                    let (tx, rx) = mpsc::channel();
                    proxy
                        .send_event(Message::Firmware(build.into(), tx))
                        .map_err(|_| anyhow::anyhow!("窗口已关闭"))?;
                    rx.recv()?.context("已取消选择固件镜像")
                },
            );
            let _ = proxy.send_event(Message::Finished(result));
        });
        Ok(())
    }

    fn command(&mut self, cmd: &Value, view: &WebView) -> Result<()> {
        let action = cmd["type"].as_str().unwrap_or("");
        match action {
            "ready" => {
                self.files(view);
                self.scan(view);
            }
            "refresh" => self.scan(view),
            "pickModules" if !self.busy => {
                if let Some(files) = tinyfiledialogs::open_file_dialog_multi(
                    "选择内核模块（可多选）",
                    "",
                    Some((&["*.ko"], "内核模块 (*.ko)")),
                ) {
                    for file in files.into_iter().map(PathBuf::from) {
                        if !self.modules.contains(&file) {
                            self.modules.push(file);
                        }
                    }
                    self.files(view);
                }
            }
            "removeModule" if !self.busy => {
                if let Some(i) = cmd["index"]
                    .as_u64()
                    .and_then(|i| usize::try_from(i).ok())
                    .filter(|i| *i < self.modules.len())
                {
                    self.modules.remove(i);
                    self.files(view);
                }
            }
            "pickImage" if !self.busy => {
                if let Some(file) = pick_image() {
                    self.firmware = Some(file);
                    self.files(view);
                }
            }
            "clearImage" if !self.busy => {
                self.firmware = None;
                self.files(view);
            }
            "start" if !self.busy => self.install(cmd["serial"].as_str().unwrap_or(""), view)?,
            "pickFallback" if self.pending.is_some() => {
                if let Some(file) = pick_image() {
                    self.finish_picker(Some(file), view);
                }
            }
            "cancelFallback" => self.finish_picker(None, view),
            "openBackup" => open_folder(self.backup.as_deref().context("没有可打开的镜像备份")?)?,
            "openLicenses" => open_folder(&tools::tool_dir(&self.root))?,
            _ => (),
        }
        Ok(())
    }

    fn finish_picker(&mut self, path: Option<PathBuf>, view: &WebView) {
        if let Some(reply) = self.pending.take() {
            let _ = reply.send(path);
            send(view, json!({"type":"fallbackClosed"}));
        }
    }

    fn handle(&mut self, message: Message, view: &WebView) {
        match message {
            Message::Command(cmd) => {
                if let Err(err) = self.command(&cmd, view) {
                    send(view, json!({"type":"notice","message": format!("{err:#}")}));
                }
            }
            Message::Scan(generation, res) => {
                self.scanning = false;
                if self.busy || generation != self.generation {
                    return;
                }
                let error = res.as_ref().err().map(|e| format!("{e:#}"));
                self.devices = res.unwrap_or_default();
                let rows: Vec<_> = self.devices.iter().map(|d| json!({"serial":d.serial,"model":d.model,"state":d.state,"kernel":d.kernel,"slot":d.slot,"locked":d.lock})).collect();
                send(view, json!({"type":"devices","devices":rows,"error":error}));
            }
            Message::Progress(data) => send(view, data),
            Message::Backup(path, data) => {
                self.backup = Some(path);
                send(view, data);
            }
            Message::Firmware(build, tx) => {
                self.pending = Some(tx);
                send(view, json!({"type":"fallback","build":build}));
            }
            Message::Finished(result) => {
                self.busy = false;
                let (status, detail) = match result {
                    Ok(Verification::Loaded) => {
                        ("loaded", "Android 已启动，所选模块已加载。".into())
                    }
                    Ok(Verification::Unverified) => (
                        "unverified",
                        "Android 已启动，当前 ADB 权限无法核实模块加载状态。".into(),
                    ),
                    Err(e) => ("failed", format!("{e:#}")),
                };
                send(
                    view,
                    json!({"type":"finished","status":status,"message":detail}),
                );
                self.scan(view);
            }
        }
    }
}

pub fn run() -> Result<()> {
    let root = tools::root()?;
    fs::create_dir_all(&root)?;
    if !tools::ensure_webview(&root)? {
        return Ok(());
    }
    let tools = tools::prepare(&root)?;
    let event_loop = EventLoopBuilder::<Message>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let window = WindowBuilder::new()
        .with_title("enenSU · 内核模块刷入")
        .with_window_icon(Icon::from_resource(101, None).ok())
        .with_inner_size(LogicalSize::new(1080, 802))
        .with_min_inner_size(LogicalSize::new(640, 500))
        .build(&event_loop)?;
    let ipc = proxy.clone();
    let mut context = WebContext::new(Some(root.join("WebView2")));
    let view = WebViewBuilder::new_with_web_context(&mut context)
        .with_https_scheme(true)
        .with_devtools(false)
        .with_browser_accelerator_keys(false)
        .with_default_context_menus(false)
        .with_custom_protocol("enensu".into(), |_, req| {
            let (mime, body): (_, &[u8]) = match req.uri().path() {
                "/" | "/index.html" => (
                    "text/html; charset=utf-8",
                    include_bytes!("../../desktop/ui/index.html"),
                ),
                "/app.js" => (
                    "application/javascript; charset=utf-8",
                    include_bytes!("../../desktop/dist/app.js"),
                ),
                "/app.css" => (
                    "text/css; charset=utf-8",
                    include_bytes!("../../desktop/dist/app.css"),
                ),
                _ => {
                    return Response::builder()
                        .status(404)
                        .body(Cow::Borrowed(&[][..]))
                        .unwrap();
                }
            };
            Response::builder()
                .header("Content-Type", mime)
                .header("X-Content-Type-Options", "nosniff")
                .body(Cow::Borrowed(body))
                .unwrap()
        })
        .with_navigation_handler(|url| matches!(url.as_str(), HOME | "enensu://localhost/"))
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_ipc_handler(move |r| {
            if r.uri().scheme_str() == Some("https")
                && r.uri().host() == Some("enensu.localhost")
                && let Ok(cmd) = serde_json::from_str(r.body())
            {
                let _ = ipc.send_event(Message::Command(cmd));
            }
        })
        .with_url("enensu://localhost/")
        .build(&window)?;
    let mut host = Host {
        proxy,
        tools,
        root,
        devices: vec![],
        modules: vec![],
        firmware: None,
        backup: None,
        pending: None,
        scanning: false,
        busy: false,
        generation: 0,
    };
    event_loop.run(move |event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(message) => host.handle(message, &view),
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } if host.busy => send(
                &view,
                json!({"type":"notice","message":"刷入任务进行中，任务结束后即可关闭窗口。"}),
            ),
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => *flow = ControlFlow::Exit,
            _ => (),
        }
    })
}
