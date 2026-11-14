//! 顶栏：开始按钮 / 工作区 / 任务栏 / 系统区（网络、音量、电量、AI 状态、时钟）。

use std::path::Path;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{
    glib, Align, Application, Box as GtkBox, Button, Label, Orientation, Separator, Window,
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::clock;
use crate::startmenu::{spawn_command, StartMenu};
use crate::taskbar::Taskbar;
use crate::theme;

/// 顶栏高度，同时也是 layer-shell 的排他区高度。
pub const PANEL_HEIGHT: i32 = 30;

pub fn build(app: &Application) {
    let panel = GtkBox::new(Orientation::Horizontal, 6);
    panel.add_css_class("panel");
    panel.set_valign(Align::Fill);

    // —— 开始按钮 ——
    let start_button = Button::new();
    start_button.add_css_class("start-button");
    start_button.set_tooltip_text(Some("开始"));
    let start_content = GtkBox::new(Orientation::Horizontal, 7);
    start_content.append(&theme::app_icon("lutra-brand", 17));
    let start_label = Label::new(Some("活动"));
    start_label.add_css_class("start-button-label");
    start_content.append(&start_label);
    start_button.set_child(Some(&start_content));

    let panel_title = Label::new(Some("Lutra"));
    panel_title.add_css_class("panel-title");

    // 任务栏与开始菜单互相引用：菜单启动应用后要让任务栏出现对应按钮。
    let taskbar = Taskbar::new();
    let taskbar_for_menu = taskbar.clone();
    let menu = StartMenu::new(app, PANEL_HEIGHT, move |app| taskbar_for_menu.track(app));

    let menu_for_button = menu.clone();
    start_button.connect_clicked(move |_| menu_for_button.toggle());

    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);

    panel.append(&start_button);
    panel.append(&panel_title);
    panel.append(&separator());
    panel.append(&build_workspaces());
    panel.append(&separator());
    panel.append(taskbar.widget());
    panel.append(&spacer);
    panel.append(&build_tray());

    let window = Window::builder()
        .application(app)
        .decorated(false)
        .resizable(false)
        .default_height(PANEL_HEIGHT)
        .build();
    window.add_css_class("lutra-panel");
    window.set_child(Some(&panel));

    // Wayland layer-shell：置顶、左右拉伸、预留 30px 排他区（窗口最大化时不遮挡顶栏）。
    window.init_layer_shell();
    window.set_layer(Layer::Top);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);
    window.set_exclusive_zone(PANEL_HEIGHT);
    // 顶栏本身不接收键盘，键盘输入交给开始菜单的搜索框。
    window.set_keyboard_mode(KeyboardMode::None);
    window.present();
}

/// 工作区指示。
///
/// 降级说明：labwc 没有 `swaymsg` 式的 IPC 命令通道，shell 无法真正切换
/// 虚拟桌面。这里只做“当前工作区”的视觉指示，点击改变本地高亮；
/// 实际切换请用 rc.xml 里的 `W-1`…`W-4`（GoToDesktop）。
fn build_workspaces() -> GtkBox {
    let workspaces = GtkBox::new(Orientation::Horizontal, 3);
    workspaces.set_valign(Align::Center);

    let buttons: Rc<std::cell::RefCell<Vec<Button>>> =
        Rc::new(std::cell::RefCell::new(Vec::new()));

    for index in 1..=4 {
        let button = Button::with_label(&index.to_string());
        button.add_css_class("workspace");
        if index == 1 {
            button.add_css_class("active");
        }

        let siblings = buttons.clone();
        button.connect_clicked(move |clicked| {
            for sibling in siblings.borrow().iter() {
                sibling.remove_css_class("active");
            }
            clicked.add_css_class("active");
        });

        buttons.borrow_mut().push(button.clone());
        workspaces.append(&button);
    }

    workspaces
}

fn build_tray() -> GtkBox {
    let tray = GtkBox::new(Orientation::Horizontal, 12);
    tray.add_css_class("tray");
    tray.set_valign(Align::Center);

    // 网络状态：不引入 libnm，直接读 /sys/class/net 的 operstate。
    let net_button = icon_button("tray-network", 14, "网络");
    net_button.connect_clicked(|_| spawn_command("lutra-settings"));

    // 音量：读取音量需要 libpulse/wpctl，这里退化为打开音量控制面板。
    let volume_button = icon_button("tray-volume", 14, "音量");
    volume_button.connect_clicked(|_| spawn_command("pavucontrol"));

    // 电量：无电池设备（台式机/虚拟机）时整块隐藏。
    let battery_label = Label::new(None);
    battery_label.add_css_class("tray-battery");
    let battery = GtkBox::new(Orientation::Horizontal, 4);
    battery.add_css_class("tray-battery-box");
    battery.set_valign(Align::Center);
    battery.append(&theme::app_icon("tray-battery", 14));
    battery.append(&battery_label);
    battery.set_visible(false);

    // AI 服务状态：扫描 /proc 找 lutrad，避免依赖 systemctl。
    let ai_dot = GtkBox::new(Orientation::Horizontal, 0);
    ai_dot.add_css_class("ai-dot");
    ai_dot.set_size_request(6, 6);
    ai_dot.set_valign(Align::Center);

    let ai_label = Label::new(Some("AI"));
    ai_label.add_css_class("ai-label");

    let ai_content = GtkBox::new(Orientation::Horizontal, 5);
    ai_content.add_css_class("ai-status");
    ai_content.append(&ai_dot);
    ai_content.append(&ai_label);

    let ai_button = Button::new();
    ai_button.add_css_class("tray-button");
    ai_button.set_child(Some(&ai_content));
    ai_button.connect_clicked(|_| spawn_command("lutra-agent-console"));

    let clock = clock::build();

    tray.append(&net_button);
    tray.append(&volume_button);
    tray.append(&battery);
    tray.append(&ai_button);
    tray.append(&clock);

    // 状态刷新：启动时一次，之后每 30s 一次。
    let refresh: Rc<dyn Fn()> = {
        let net_button = net_button.clone();
        let battery = battery.clone();
        let battery_label = battery_label.clone();
        let ai_button = ai_button.clone();
        let ai_dot = ai_dot.clone();

        Rc::new(move || {
            let online = network_online();
            net_button.set_tooltip_text(Some(if online {
                "网络：已连接"
            } else {
                "网络：未连接"
            }));

            match read_battery() {
                Some((percent, charging)) => {
                    battery.set_visible(true);
                    battery_label.set_label(&format!("{percent}%"));
                    let tooltip = if charging {
                        format!("电量：充电中 {percent}%")
                    } else {
                        format!("电量：{percent}%")
                    };
                    battery.set_tooltip_text(Some(tooltip.as_str()));
                }
                None => battery.set_visible(false),
            }

            let running = lutrad_running();
            if running {
                ai_dot.add_css_class("online");
            } else {
                ai_dot.remove_css_class("online");
            }
            ai_button.set_tooltip_text(Some(if running {
                "AI 服务：运行中"
            } else {
                "AI 服务：未运行"
            }));
        })
    };

    refresh();
    let tick = refresh.clone();
    glib::timeout_add_seconds_local(30, move || {
        tick();
        glib::ControlFlow::Continue
    });

    tray
}

fn icon_button(icon: &str, size: i32, tooltip: &str) -> Button {
    let button = Button::new();
    button.add_css_class("tray-button");
    button.set_tooltip_text(Some(tooltip));
    button.set_child(Some(&theme::app_icon(icon, size)));
    button
}

fn separator() -> Separator {
    let separator = Separator::new(Orientation::Vertical);
    separator.add_css_class("panel-separator");
    separator.set_margin_top(7);
    separator.set_margin_bottom(7);
    separator
}

fn network_online() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name == "lo" {
            continue;
        }
        let state =
            std::fs::read_to_string(entry.path().join("operstate")).unwrap_or_default();
        if state.trim() == "up" {
            return true;
        }
    }
    false
}

fn read_battery() -> Option<(u8, bool)> {
    let entries = std::fs::read_dir(Path::new("/sys/class/power_supply")).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("BAT") {
            continue;
        }
        let path = entry.path();
        let capacity = std::fs::read_to_string(path.join("capacity")).ok()?;
        let percent: u8 = capacity.trim().parse().ok()?;
        let status = std::fs::read_to_string(path.join("status")).unwrap_or_default();
        return Some((percent, status.trim() == "Charging"));
    }
    None
}

/// 判断 lutrad 是否在跑：扫描 /proc/*/comm，不依赖 systemctl/pidof。
fn lutrad_running() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) {
            if comm.trim() == "lutrad" {
                return true;
            }
        }
    }
    false
}
