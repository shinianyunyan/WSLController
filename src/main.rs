#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::HashMap,
    fs,
    os::windows::process::CommandExt,
    process::{Child, Command, Stdio},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use eframe::{
    egui::{
        self, pos2, vec2, Align, Color32, CornerRadius, FontData, FontDefinitions, FontFamily,
        FontId, Frame, Layout, Margin, RichText, ScrollArea, Stroke, Vec2, ViewportBuilder,
        ViewportCommand,
    },
    App, CreationContext, NativeOptions,
};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const KEEP_ALIVE_COMMAND: &str =
    "trap 'exit 0' TERM INT; while true; do sleep 2147483647 & wait $!; done";

fn main() -> Result<()> {
    let native_options = NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([720.0, 520.0])
            .with_min_inner_size([560.0, 420.0])
            .with_title("WSLController")
            .with_icon(app_icon_rgba()),
        ..Default::default()
    };

    eframe::run_native(
        "WSLController",
        native_options,
        Box::new(|cc| Ok(Box::new(WslApp::new(cc)?))),
    )
    .map_err(|err| anyhow::anyhow!("{err}"))
}

#[derive(Debug, Clone)]
struct WslDistro {
    name: String,
    state: WslState,
    version: String,
    is_default: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WslState {
    Running,
    Stopped,
    Unknown,
}

impl WslState {
    fn from_wsl(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "running" => Self::Running,
            "stopped" => Self::Stopped,
            _ => Self::Unknown,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Running => "运行中",
            Self::Stopped => "已停止",
            Self::Unknown => "未知",
        }
    }

    fn color(self) -> Color32 {
        match self {
            Self::Running => Color32::from_rgb(63, 185, 132),
            Self::Stopped => Color32::from_rgb(166, 172, 185),
            Self::Unknown => Color32::from_rgb(216, 170, 78),
        }
    }
}

#[derive(Debug)]
struct WslController {
    distros: Vec<WslDistro>,
    keep_alive: SharedKeepAlive,
    tasks: AsyncTasks,
    last_error: Option<String>,
    last_action: String,
    last_status_refresh: Instant,
}

type SharedKeepAlive = Arc<Mutex<HashMap<String, Child>>>;
type AsyncTasks = Arc<Mutex<HashMap<String, bool>>>;

impl WslController {
    fn new(keep_alive: SharedKeepAlive, tasks: AsyncTasks) -> Self {
        let mut controller = Self {
            distros: Vec::new(),
            keep_alive,
            tasks,
            last_error: None,
            last_action: String::new(),
            last_status_refresh: Instant::now(),
        };
        controller.refresh_distros();
        controller
    }

    fn refresh_distros(&mut self) {
        self.reap_keep_alive();
        match query_wsl_distros() {
            Ok(distros) => {
                self.distros = distros;
                self.last_error = None;
            }
            Err(err) => self.fail(format!("读取 WSL 列表失败：{err}")),
        }
    }

    fn refresh_status_if_needed(&mut self) {
        self.reap_keep_alive();
        if self.last_status_refresh.elapsed() < Duration::from_secs(2) {
            return;
        }

        self.last_status_refresh = Instant::now();
        self.refresh_distros();
    }

    fn start_keep_alive(&mut self, distro: &str) {
        if distro.trim().is_empty() {
            self.fail("发行版名称为空");
            return;
        }

        self.reap_keep_alive();
        if self.keep_alive_running(distro) {
            self.last_error = None;
            self.last_action = format!("{distro} 的后台保活已经在运行");
            return;
        }

        match hidden_wsl_command()
            .args(["-d", distro, "--exec", "sh", "-lc", KEEP_ALIVE_COMMAND])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => {
                if let Ok(mut keep_alive) = self.keep_alive.lock() {
                    keep_alive.insert(distro.to_string(), child);
                }
                self.last_error = None;
                self.last_action = format!("已接管 {distro} 后台保活");
                self.refresh_distros();
            }
            Err(err) => self.fail(format!("保活 {distro} 失败：{err}")),
        }
    }

    fn mark_task(&mut self, key: impl Into<String>, label: impl Into<String>) -> bool {
        let key = key.into();
        let already_running = self
            .tasks
            .lock()
            .map(|mut tasks| {
                if tasks.contains_key(&key) {
                    true
                } else {
                    tasks.insert(key, true);
                    false
                }
            })
            .unwrap_or(true);

        if already_running {
            self.last_error = None;
            self.last_action = format!("{} 正在执行", label.into());
            return false;
        }

        true
    }

    fn task_running(&self, key: &str) -> bool {
        self.tasks
            .lock()
            .map(|tasks| tasks.contains_key(key))
            .unwrap_or(false)
    }

    fn finish_task(tasks: &AsyncTasks, key: &str) {
        if let Ok(mut tasks) = tasks.lock() {
            tasks.remove(key);
        }
    }

    fn start_distro(&mut self, distro: &str, sender: mpsc::Sender<AppEvent>) {
        let key = format!("start:{distro}");
        if !self.mark_task(key.clone(), format!("启动 {distro}")) {
            return;
        }

        self.last_error = None;
        self.last_action = format!("正在启动 {distro}");
        let distro = distro.to_string();
        let tasks = Arc::clone(&self.tasks);

        thread::spawn(move || {
            let result = hidden_wsl_command()
                .args(["-d", &distro, "--exec", "true"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|err| format!("启动 {distro} 失败：{err}"))
                .and_then(|status| {
                    if status.success() {
                        Ok(format!("已启动 {distro}"))
                    } else {
                        Err(format!("启动 {distro} 失败，退出码：{status}"))
                    }
                });

            Self::finish_task(&tasks, &key);
            let _ = sender.send(AppEvent::TaskFinished(result));
        });
    }

    fn open_shell_window(&mut self, distro: &str) {
        let title = format!("WSL - {distro}");
        let wt_result = Command::new("wt.exe")
            .args(["new-tab", "--title", &title, "wsl.exe", "-d", distro])
            .spawn();

        if wt_result.is_ok() {
            self.last_error = None;
            self.last_action = format!("已打开 {distro} Shell");
            return;
        }

        match Command::new("cmd.exe")
            .args(["/C", "start", "", "wsl.exe", "-d", distro])
            .spawn()
        {
            Ok(_) => {
                self.last_error = None;
                self.last_action = format!("已打开 {distro} Shell");
            }
            Err(err) => self.fail(format!("打开 {distro} Shell 失败：{err}")),
        }
    }

    fn terminate_distro(&mut self, distro: &str, sender: mpsc::Sender<AppEvent>) {
        let key = format!("terminate:{distro}");
        if !self.mark_task(key.clone(), format!("关闭 {distro}")) {
            return;
        }

        self.kill_keep_alive(distro);
        self.last_error = None;
        self.last_action = format!("正在关闭 {distro}");
        let distro = distro.to_string();
        let tasks = Arc::clone(&self.tasks);

        thread::spawn(move || {
            let result = hidden_wsl_command()
                .args(["--terminate", &distro])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|err| format!("关闭 {distro} 失败：{err}"))
                .and_then(|status| {
                    if status.success() {
                        Ok(format!("已关闭 {distro}"))
                    } else {
                        Err(format!("关闭 {distro} 失败，退出码：{status}"))
                    }
                });

            Self::finish_task(&tasks, &key);
            let _ = sender.send(AppEvent::TaskFinished(result));
        });
    }

    fn shutdown_all(&mut self, sender: mpsc::Sender<AppEvent>) {
        let key = "shutdown_all".to_string();
        if !self.mark_task(key.clone(), "关闭全部 WSL") {
            return;
        }

        self.kill_all_keep_alive();
        self.last_error = None;
        self.last_action = "正在关闭全部 WSL".to_string();
        let tasks = Arc::clone(&self.tasks);

        thread::spawn(move || {
            let result = hidden_wsl_command()
                .arg("--shutdown")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map_err(|err| format!("关闭全部 WSL 失败：{err}"))
                .and_then(|status| {
                    if status.success() {
                        Ok("已关闭全部 WSL 发行版".to_string())
                    } else {
                        Err(format!("关闭全部 WSL 失败，退出码：{status}"))
                    }
                });

            Self::finish_task(&tasks, &key);
            let _ = sender.send(AppEvent::TaskFinished(result));
        });
    }

    fn keep_alive_running(&self, distro: &str) -> bool {
        self.keep_alive
            .lock()
            .map(|keep_alive| keep_alive.contains_key(distro))
            .unwrap_or(false)
    }

    fn running_count(&self) -> usize {
        self.distros
            .iter()
            .filter(|distro| distro.state == WslState::Running)
            .count()
    }

    fn reap_keep_alive(&mut self) {
        if let Ok(mut keep_alive) = self.keep_alive.lock() {
            keep_alive.retain(|_, child| !matches!(child.try_wait(), Ok(Some(_))));
        }
    }

    fn kill_keep_alive(&mut self, distro: &str) {
        if let Ok(mut keep_alive) = self.keep_alive.lock() {
            if let Some(mut child) = keep_alive.remove(distro) {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn kill_all_keep_alive(&mut self) {
        kill_all_keep_alive(&self.keep_alive);
    }

    fn fail(&mut self, message: impl Into<String>) {
        self.last_error = Some(message.into());
    }
}

fn kill_all_keep_alive(keep_alive: &SharedKeepAlive) {
    if let Ok(mut keep_alive) = keep_alive.lock() {
        for (_, mut child) in keep_alive.drain() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn hidden_wsl_command() -> Command {
    let mut command = Command::new("wsl.exe");
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

fn query_wsl_distros() -> Result<Vec<WslDistro>> {
    let output = hidden_wsl_command()
        .args(["-l", "-v"])
        .stdin(Stdio::null())
        .output()
        .context("failed to run wsl.exe -l -v")?;

    if !output.status.success() {
        anyhow::bail!("wsl.exe -l -v exited with {}", output.status);
    }

    Ok(parse_wsl_list(&decode_wsl_output(&output.stdout)))
}

fn parse_wsl_list(text: &str) -> Vec<WslDistro> {
    text.lines()
        .filter_map(|line| {
            let normalized = line.trim();
            if normalized.is_empty() || normalized.to_ascii_lowercase().starts_with("name") {
                return None;
            }

            let is_default = normalized.starts_with('*');
            let cleaned = normalized.trim_start_matches('*').trim();
            let mut parts = cleaned.split_whitespace();
            let name = parts.next()?.to_string();
            let state = parts.next().map(WslState::from_wsl).unwrap_or(WslState::Unknown);
            let version = parts.next().unwrap_or("-").to_string();

            Some(WslDistro {
                name,
                state,
                version,
                is_default,
            })
        })
        .collect()
}

fn decode_wsl_output(bytes: &[u8]) -> String {
    let looks_utf16 = bytes.starts_with(&[0xff, 0xfe])
        || bytes
            .iter()
            .skip(1)
            .step_by(2)
            .take(16)
            .filter(|byte| **byte == 0)
            .count()
            > 8;

    if looks_utf16 {
        let start = if bytes.starts_with(&[0xff, 0xfe]) { 2 } else { 0 };
        let words: Vec<u16> = bytes[start..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&words).replace('\0', "")
    } else {
        String::from_utf8_lossy(bytes).replace('\0', "")
    }
}

struct TrayState {
    _icon: TrayIcon,
    show_id: MenuId,
    shutdown_all_id: MenuId,
    quit_id: MenuId,
}

impl TrayState {
    fn new() -> Result<Self> {
        let menu = Menu::new();
        let show = MenuItem::with_id("show", "显示窗口", true, None);
        let shutdown_all = MenuItem::with_id("shutdown_all", "关闭全部 WSL", true, None);
        let quit = MenuItem::with_id("quit", "退出程序", true, None);
        let separator = PredefinedMenuItem::separator();

        menu.append_items(&[&show, &separator, &shutdown_all, &quit])?;

        let icon = TrayIconBuilder::new()
            .with_tooltip("WSLController")
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_icon(tray_icon_rgba()?)
            .build()?;

        Ok(Self {
            _icon: icon,
            show_id: show.into_id(),
            shutdown_all_id: shutdown_all.into_id(),
            quit_id: quit.into_id(),
        })
    }
}

struct WslApp {
    controller: WslController,
    tray: Option<TrayState>,
    sender: Sender<AppEvent>,
    events: Receiver<AppEvent>,
    should_quit: bool,
}

enum AppEvent {
    Tray(TrayIconEvent),
    Menu(MenuEvent),
    TaskFinished(Result<String, String>),
}

impl WslApp {
    fn new(cc: &CreationContext<'_>) -> Result<Self> {
        setup_style(&cc.egui_ctx);
        let (sender, events) = mpsc::channel();
        let keep_alive = Arc::new(Mutex::new(HashMap::new()));

        let tray_sender = sender.clone();
        let tray_ctx = cc.egui_ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event| {
            let _ = tray_sender.send(AppEvent::Tray(event));
            tray_ctx.request_repaint();
        }));

        let tray = TrayState::new()?;
        let quit_id = tray.quit_id.clone();
        let menu_keep_alive = Arc::clone(&keep_alive);
        let menu_sender = sender.clone();
        let menu_ctx = cc.egui_ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if event.id == quit_id {
                kill_all_keep_alive(&menu_keep_alive);
                std::process::exit(0);
            }

            let _ = menu_sender.send(AppEvent::Menu(event));
            menu_ctx.request_repaint();
        }));

        Ok(Self {
            controller: WslController::new(keep_alive, Arc::new(Mutex::new(HashMap::new()))),
            tray: Some(tray),
            sender,
            events,
            should_quit: false,
        })
    }

    fn hide_window(ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
    }

    fn show_window(ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    fn handle_app_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                AppEvent::Tray(TrayIconEvent::DoubleClick { .. }) => Self::show_window(ctx),
                AppEvent::Tray(_) => {}
                AppEvent::Menu(event) => {
                    let Some(tray) = &self.tray else {
                        continue;
                    };

                    if event.id == tray.show_id {
                        Self::show_window(ctx);
                    } else if event.id == tray.shutdown_all_id {
                        self.controller.shutdown_all(self.sender.clone());
                    } else if event.id == tray.quit_id {
                        self.controller.kill_all_keep_alive();
                        self.should_quit = true;
                    }
                }
                AppEvent::TaskFinished(result) => {
                    match result {
                        Ok(message) => {
                            self.controller.last_error = None;
                            self.controller.last_action = message;
                        }
                        Err(message) => self.controller.fail(message),
                    }
                    self.controller.refresh_distros();
                }
            }
        }
    }
}

impl App for WslApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_app_events(ctx);
        self.controller.refresh_status_if_needed();

        if ctx.input(|input| input.viewport().close_requested()) && !self.should_quit {
            Self::hide_window(ctx);
        }

        if self.should_quit {
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        }

        egui::CentralPanel::default()
            .frame(Frame::new().fill(Color32::from_rgb(16, 18, 24)))
            .show(ctx, |ui| {
                ui.add_space(18.0);
                ui.with_layout(Layout::top_down(Align::Center), |ui| {
                    header(ui, &self.controller);
                    ui.add_space(18.0);
                    distro_list(ui, &mut self.controller, self.sender.clone());
                    ui.add_space(12.0);
                    status_line(ui, &self.controller);
                });
            });

        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.should_quit {
            self.controller.kill_all_keep_alive();
        }
    }
}

fn setup_style(ctx: &egui::Context) {
    setup_fonts(ctx);

    let mut style = (*ctx.style()).clone();
    style.visuals.dark_mode = true;
    style.visuals.window_corner_radius = CornerRadius::same(8);
    style.visuals.widgets.inactive.corner_radius = CornerRadius::same(6);
    style.visuals.widgets.hovered.corner_radius = CornerRadius::same(6);
    style.visuals.widgets.active.corner_radius = CornerRadius::same(6);
    style.spacing.button_padding = vec2(12.0, 8.0);
    style.spacing.item_spacing = vec2(10.0, 10.0);
    ctx.set_style(style);
}

fn setup_fonts(ctx: &egui::Context) {
    let Some((name, bytes)) = load_chinese_font() else {
        return;
    };

    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert(name.clone(), std::sync::Arc::new(FontData::from_owned(bytes)));

    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, name.clone());
    }

    ctx.set_fonts(fonts);
}

fn load_chinese_font() -> Option<(String, Vec<u8>)> {
    let candidates = [
        ("microsoft_yahei", r"C:\Windows\Fonts\msyh.ttc"),
        ("dengxian", r"C:\Windows\Fonts\Deng.ttf"),
        ("simhei", r"C:\Windows\Fonts\simhei.ttf"),
        ("noto_sans_cjk", r"C:\Windows\Fonts\NotoSansCJK-Regular.ttc"),
    ];

    candidates
        .into_iter()
        .find_map(|(name, path)| fs::read(path).ok().map(|bytes| (name.to_string(), bytes)))
}

fn header(ui: &mut egui::Ui, controller: &WslController) {
    ui.horizontal(|ui| {
        ui.add_space(6.0);
        terminal_mark(ui);
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.label(
                RichText::new("WSLController")
                    .font(FontId::proportional(28.0))
                    .strong()
                    .color(Color32::from_rgb(239, 243, 250)),
            );
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            overview_pill(ui, controller);
        });
    });
}

fn overview_pill(ui: &mut egui::Ui, controller: &WslController) {
    let running = controller.running_count();
    let total = controller.distros.len();
    let color = if running > 0 {
        Color32::from_rgb(63, 185, 132)
    } else {
        Color32::from_rgb(166, 172, 185)
    };
    let text = format!("{running}/{total} 运行中");

    Frame::new()
        .fill(Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), 36))
        .stroke(Stroke::new(1.0, color))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .font(FontId::proportional(14.0))
                    .color(Color32::from_rgb(235, 241, 247)),
            );
        });
}

fn distro_list(ui: &mut egui::Ui, controller: &mut WslController, sender: Sender<AppEvent>) {
    let max_width = (ui.available_width() - 24.0).max(480.0);

    Frame::new()
        .fill(Color32::from_rgb(22, 26, 34))
        .stroke(Stroke::new(1.0, Color32::from_rgb(45, 52, 66)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(max_width);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("WSL 发行版")
                        .font(FontId::proportional(15.0))
                        .strong()
                        .color(Color32::from_rgb(216, 224, 235)),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if action_button(ui, "刷新", ButtonTone::Neutral, "重新读取 wsl.exe -l -v")
                        .clicked()
                    {
                        controller.refresh_distros();
                    }
                });
            });

            ui.add_space(8.0);
            if controller.distros.is_empty() {
                empty_state(ui);
            } else {
                ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(310.0)
                    .show(ui, |ui| {
                        let distros = controller.distros.clone();
                        for distro in distros {
                            distro_card(ui, controller, &distro, sender.clone());
                            ui.add_space(10.0);
                        }
                    });
            }
        });
}

fn empty_state(ui: &mut egui::Ui) {
    Frame::new()
        .fill(Color32::from_rgb(16, 18, 24))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(20))
        .show(ui, |ui| {
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.label(
                    RichText::new("未检测到 WSL 发行版")
                        .font(FontId::proportional(16.0))
                        .color(Color32::from_rgb(188, 198, 212)),
                );
            });
        });
}

fn distro_card(
    ui: &mut egui::Ui,
    controller: &mut WslController,
    distro: &WslDistro,
    sender: Sender<AppEvent>,
) {
    let keep_alive = controller.keep_alive_running(&distro.name);
    let start_label = if controller.task_running(&format!("start:{}", distro.name)) {
        "启动中"
    } else {
        "启动"
    };
    let close_label = if controller.task_running(&format!("terminate:{}", distro.name)) {
        "关闭中"
    } else {
        "关闭"
    };

    Frame::new()
        .fill(Color32::from_rgb(29, 34, 44))
        .stroke(Stroke::new(1.0, Color32::from_rgb(54, 62, 78)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(&distro.name)
                                .font(FontId::proportional(20.0))
                                .strong()
                                .color(Color32::from_rgb(242, 245, 249)),
                        );
                        if distro.is_default {
                            small_tag(ui, "默认");
                        }
                        if keep_alive {
                            small_tag(ui, "保活中");
                        }
                    });
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        state_tag(ui, distro.state);
                        ui.label(
                            RichText::new(format!("WSL {}", distro.version))
                                .font(FontId::proportional(13.0))
                                .color(Color32::from_rgb(159, 169, 184)),
                        );
                    });
                });

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if action_button(
                        ui,
                        close_label,
                        ButtonTone::Danger,
                        "终止该发行版和对应保活进程",
                    )
                    .clicked()
                    {
                        controller.terminate_distro(&distro.name, sender.clone());
                    }
                    if action_button(ui, "保活", ButtonTone::Neutral, "接管该发行版并在后台保持运行")
                        .clicked()
                    {
                        controller.start_keep_alive(&distro.name);
                    }
                    if action_button(ui, start_label, ButtonTone::Neutral, "启动该发行版")
                        .clicked()
                    {
                        controller.start_distro(&distro.name, sender.clone());
                    }
                    if action_button(ui, "Shell", ButtonTone::Primary, "打开该发行版 Shell")
                        .clicked()
                    {
                        controller.open_shell_window(&distro.name);
                    }
                });
            });
        });
}

#[derive(Clone, Copy)]
enum ButtonTone {
    Primary,
    Neutral,
    Danger,
}

fn action_button(
    ui: &mut egui::Ui,
    text: &str,
    tone: ButtonTone,
    tooltip: &str,
) -> egui::Response {
    let (base_fill, hover_fill, active_fill, stroke, text_color) = match tone {
        ButtonTone::Primary => (
            Color32::from_rgb(49, 130, 191),
            Color32::from_rgb(61, 145, 210),
            Color32::from_rgb(37, 111, 170),
            Color32::from_rgb(83, 165, 224),
            Color32::from_rgb(245, 250, 255),
        ),
        ButtonTone::Neutral => (
            Color32::from_rgb(43, 50, 64),
            Color32::from_rgb(53, 61, 78),
            Color32::from_rgb(34, 40, 52),
            Color32::from_rgb(69, 79, 98),
            Color32::from_rgb(220, 228, 238),
        ),
        ButtonTone::Danger => (
            Color32::from_rgb(92, 44, 52),
            Color32::from_rgb(113, 52, 62),
            Color32::from_rgb(76, 35, 42),
            Color32::from_rgb(154, 78, 90),
            Color32::from_rgb(255, 224, 228),
        ),
    };

    let (rect, response) = ui.allocate_exact_size(vec2(72.0, 32.0), egui::Sense::click());
    let pressed = response.is_pointer_button_down_on();
    let hovered = response.hovered();
    let fill = if pressed {
        active_fill
    } else if hovered {
        hover_fill
    } else {
        base_fill
    };
    let paint_rect = if pressed {
        rect.translate(vec2(0.0, 1.0))
    } else {
        rect
    };
    let painter = ui.painter_at(rect.expand(2.0));

    if hovered && !pressed {
        painter.rect_filled(
            paint_rect.expand(1.0),
            CornerRadius::same(7),
            Color32::from_rgba_premultiplied(stroke.r(), stroke.g(), stroke.b(), 30),
        );
    }

    painter.rect_filled(paint_rect, CornerRadius::same(6), fill);
    painter.rect_stroke(
        paint_rect,
        CornerRadius::same(6),
        Stroke::new(if pressed { 1.5 } else { 1.0 }, stroke),
        egui::StrokeKind::Outside,
    );
    painter.text(
        paint_rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        FontId::proportional(13.0),
        text_color,
    );

    response.on_hover_text(tooltip)
}

fn state_tag(ui: &mut egui::Ui, state: WslState) {
    let color = state.color();
    Frame::new()
        .fill(Color32::from_rgba_premultiplied(color.r(), color.g(), color.b(), 34))
        .stroke(Stroke::new(1.0, color))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.label(
                RichText::new(state.label())
                    .font(FontId::proportional(12.0))
                    .color(Color32::from_rgb(236, 242, 248)),
            );
        });
}

fn small_tag(ui: &mut egui::Ui, text: &str) {
    Frame::new()
        .fill(Color32::from_rgb(43, 49, 62))
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::symmetric(7, 3))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .font(FontId::proportional(12.0))
                    .color(Color32::from_rgb(190, 202, 218)),
            );
        });
}

fn status_line(ui: &mut egui::Ui, controller: &WslController) {
    if controller.last_error.is_none() && controller.last_action.is_empty() {
        return;
    }

    let text = controller
        .last_error
        .as_deref()
        .unwrap_or(controller.last_action.as_str());
    let color = if controller.last_error.is_some() {
        Color32::from_rgb(238, 112, 112)
    } else {
        Color32::from_rgb(172, 181, 194)
    };

    ui.label(
        RichText::new(text)
            .font(FontId::proportional(13.0))
            .color(color),
    );
}

fn terminal_mark(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(58.0), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, CornerRadius::same(8), Color32::from_rgb(39, 114, 159));
    painter.rect_filled(
        rect.shrink(6.0),
        CornerRadius::same(5),
        Color32::from_rgb(16, 18, 24),
    );
    painter.line_segment(
        [
            pos2(rect.left() + 16.0, rect.top() + 20.0),
            pos2(rect.left() + 26.0, rect.center().y),
        ],
        Stroke::new(2.5, Color32::from_rgb(95, 211, 167)),
    );
    painter.line_segment(
        [
            pos2(rect.left() + 26.0, rect.center().y),
            pos2(rect.left() + 16.0, rect.bottom() - 20.0),
        ],
        Stroke::new(2.5, Color32::from_rgb(95, 211, 167)),
    );
    painter.line_segment(
        [
            pos2(rect.left() + 32.0, rect.bottom() - 18.0),
            pos2(rect.right() - 14.0, rect.bottom() - 18.0),
        ],
        Stroke::new(2.5, Color32::from_rgb(230, 238, 245)),
    );
}

fn app_icon_rgba() -> egui::IconData {
    let (rgba, width, height) = icon_pixels(128);
    egui::IconData {
        rgba,
        width,
        height,
    }
}

fn tray_icon_rgba() -> Result<Icon> {
    let (rgba, width, height) = icon_pixels(32);
    Icon::from_rgba(rgba, width, height).context("failed to build tray icon")
}

fn icon_pixels(size: u32) -> (Vec<u8>, u32, u32) {
    let mut rgba = vec![0_u8; (size * size * 4) as usize];
    let scale = size as f32 / 58.0;

    for y in 0..size {
        for x in 0..size {
            let idx = ((y * size + x) * 4) as usize;
            let xf = x as f32 / scale;
            let yf = y as f32 / scale;

            let outer_alpha = rounded_rect_alpha(xf, yf, 0.0, 0.0, 58.0, 58.0, 9.0);
            if outer_alpha <= 0.0 {
                continue;
            }

            let inner_alpha = rounded_rect_alpha(xf, yf, 6.0, 6.0, 46.0, 46.0, 5.0);
            let mut color = if inner_alpha > 0.0 {
                mix(
                    [39, 114, 159, 255],
                    [16, 18, 24, 255],
                    inner_alpha,
                )
            } else {
                [39, 114, 159, 255]
            };

            let prompt_alpha = polyline_alpha(
                xf,
                yf,
                &[(17.0, 20.0), (27.0, 29.0), (17.0, 38.0)],
                2.7,
            );
            if prompt_alpha > 0.0 {
                color = mix(color, [95, 211, 167, 255], prompt_alpha);
            }

            let cursor_alpha = segment_alpha(xf, yf, (33.0, 40.0), (45.0, 40.0), 2.9);
            if cursor_alpha > 0.0 {
                color = mix(color, [230, 238, 245, 255], cursor_alpha);
            }

            color[3] = (outer_alpha * 255.0).round() as u8;
            rgba[idx..idx + 4].copy_from_slice(&color);
        }
    }

    (rgba, size, size)
}

fn rounded_rect_alpha(
    x: f32,
    y: f32,
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    radius: f32,
) -> f32 {
    let right = left + width;
    let bottom = top + height;
    let closest_x = x.clamp(left + radius, right - radius);
    let closest_y = y.clamp(top + radius, bottom - radius);
    let dx = x - closest_x;
    let dy = y - closest_y;
    (radius + 0.75 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0)
}

fn polyline_alpha(x: f32, y: f32, points: &[(f32, f32)], width: f32) -> f32 {
    points
        .windows(2)
        .map(|pair| segment_alpha(x, y, pair[0], pair[1], width))
        .fold(0.0, f32::max)
}

fn segment_alpha(x: f32, y: f32, start: (f32, f32), end: (f32, f32), width: f32) -> f32 {
    let (x1, y1) = start;
    let (x2, y2) = end;
    let vx = x2 - x1;
    let vy = y2 - y1;
    let len_sq = vx * vx + vy * vy;
    if len_sq <= f32::EPSILON {
        return 0.0;
    }

    let t = (((x - x1) * vx + (y - y1) * vy) / len_sq).clamp(0.0, 1.0);
    let px = x1 + t * vx;
    let py = y1 + t * vy;
    let distance = ((x - px).powi(2) + (y - py).powi(2)).sqrt();
    ((width / 2.0 + 0.65 - distance).clamp(0.0, 1.0)).min(1.0)
}

fn mix(base: [u8; 4], overlay: [u8; 4], alpha: f32) -> [u8; 4] {
    let a = alpha.clamp(0.0, 1.0);
    [
        lerp_u8(base[0], overlay[0], a),
        lerp_u8(base[1], overlay[1], a),
        lerp_u8(base[2], overlay[2], a),
        lerp_u8(base[3], overlay[3], a),
    ]
}

fn lerp_u8(from: u8, to: u8, alpha: f32) -> u8 {
    (from as f32 + (to as f32 - from as f32) * alpha).round() as u8
}
