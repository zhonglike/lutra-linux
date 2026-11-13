//! 开始菜单：解析 `*.desktop` 建立应用索引，提供搜索过滤、分类视图与启动。
//!
//! 索引来源遵循 XDG 约定：`$XDG_DATA_HOME/applications` 优先，其次
//! `$XDG_DATA_DIRS`（默认 `/usr/local/share:/usr/share`）下的 `applications`。
//! 同名 `.desktop` 以先出现的目录为准。

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

use gtk4::gdk;
use gtk4::prelude::*;
use gtk4::{glib, pango};
use gtk4::{
    Application, Box as GtkBox, Button, Entry, FlowBox, Label, Orientation, PolicyType,
    ScrolledWindow, SelectionMode, Window,
};
use gtk4_layer_shell::{Edge, KeyboardInteractivity, Layer, LayerShell};

use crate::theme;

/// 搜索一次最多渲染的条目数，避免输入时重建过多控件。
const MAX_RESULTS: usize = 80;

/// 分类顺序与中文标题（取 `.desktop` 里 Categories 的第一个可识别项）。
const CATEGORY_ORDER: [(&str, &str); 11] = [
    ("Network", "网络"),
    ("AudioVideo", "影音"),
    ("Graphics", "图形"),
    ("Office", "办公"),
    ("Development", "开发"),
    ("Game", "游戏"),
    ("Education", "教育"),
    ("Science", "科学"),
    ("System", "系统"),
    ("Utility", "工具"),
    ("Settings", "设置"),
];

const OTHER_CATEGORY: &str = "Other";

/// `.desktop` 中 shell 需要的那部分字段。
#[derive(Clone, Debug)]
pub struct DesktopApp {
    pub id: String,
    pub name: String,
    pub comment: String,
    pub exec: String,
    pub icon: String,
    pub categories: Vec<String>,
    pub terminal: bool,
}

impl DesktopApp {
    fn category_key(&self) -> &'static str {
        for category in &self.categories {
            for (key, _) in CATEGORY_ORDER {
                if category.as_str() == key {
                    return key;
                }
            }
        }
        OTHER_CATEGORY
    }

    fn matches(&self, needle: &str) -> bool {
        let haystack = format!(
            "{} {} {} {} {}",
            self.name,
            self.comment,
            self.exec,
            self.id,
            self.categories.join(" ")
        )
        .to_lowercase();
        haystack.contains(needle)
    }
}

pub struct StartMenu {
    window: Window,
    entry: Entry,
    list: GtkBox,
    apps: Rc<Vec<DesktopApp>>,
    on_launch: Rc<dyn Fn(&DesktopApp)>,
    first_hit: Rc<RefCell<Option<DesktopApp>>>,
    visible: Rc<Cell<bool>>,
}

impl StartMenu {
    pub fn new(
        app: &Application,
        top_offset: i32,
        on_launch: impl Fn(&DesktopApp) + 'static,
    ) -> Rc<Self> {
        let apps = Rc::new(load_apps());

        // —— 头部：Lutra 标志 + 版本 ——
        let title = Label::new(Some("Lutra"));
        title.add_css_class("start-title");
        title.set_halign(gtk4::Align::Start);
        let version = Label::new(Some("Rill · 1.0"));
        version.add_css_class("start-version");
        version.set_halign(gtk4::Align::Start);
        let titles = GtkBox::new(Orientation::Vertical, 1);
        titles.append(&title);
        titles.append(&version);

        let head = GtkBox::new(Orientation::Horizontal, 12);
        head.add_css_class("start-head");
        head.append(&theme::app_icon("lutra-brand", 46));
        head.append(&titles);

        // —— 搜索框 ——
        let entry = Entry::new();
        entry.add_css_class("start-entry");
        entry.set_placeholder_text(Some("搜索应用…"));
        entry.set_hexpand(true);

        let search = GtkBox::new(Orientation::Horizontal, 8);
        search.add_css_class("start-search");
        search.append(&theme::app_icon("tray-search", 13));
        search.append(&entry);

        // —— 应用列表（分类视图 / 搜索结果共用）——
        let list = GtkBox::new(Orientation::Vertical, 12);
        list.add_css_class("start-list");

        let scroller = ScrolledWindow::builder()
            .hscrollbar_policy(PolicyType::Never)
            .vscrollbar_policy(PolicyType::Automatic)
            .min_content_height(300)
            .max_content_height(430)
            .propagate_natural_height(true)
            .child(&list)
            .build();
        scroller.add_css_class("start-scroll");

        // —— 底部：当前用户 + 退出会话 ——
        let user = env::var("USER").unwrap_or_else(|_| "lutra".to_string());
        let initial = user
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_else(|| "L".to_string());
        let avatar = Label::new(Some(initial.as_str()));
        avatar.add_css_class("start-avatar");
        avatar.set_xalign(0.5);
        avatar.set_yalign(0.5);
        let user_label = Label::new(Some(user.as_str()));
        user_label.add_css_class("start-user");

        let user_box = GtkBox::new(Orientation::Horizontal, 8);
        user_box.add_css_class("start-user-box");
        user_box.append(&avatar);
        user_box.append(&user_label);

        let power = Button::new();
        power.add_css_class("start-power");
        power.set_child(Some(&theme::app_icon("tray-power", 15)));
        power.set_tooltip_text(Some("退出会话"));
        power.connect_clicked(|_| {
            // 降级：labwc 未提供会话管理的 D-Bus 接口，这里直接结束合成器，
            // systemd 会把会话单元一并停掉。
            spawn_command("labwc --exit");
        });

        let foot_spacer = GtkBox::new(Orientation::Horizontal, 0);
        foot_spacer.set_hexpand(true);
        let foot = GtkBox::new(Orientation::Horizontal, 10);
        foot.add_css_class("start-foot");
        foot.append(&user_box);
        foot.append(&foot_spacer);
        foot.append(&power);

        // —— 整体容器 ——
        let content = GtkBox::new(Orientation::Vertical, 14);
        content.add_css_class("start-menu");
        content.append(&head);
        content.append(&search);
        content.append(&scroller);
        content.append(&foot);

        let window = Window::builder()
            .application(app)
            .decorated(false)
            .resizable(false)
            .default_width(430)
            .build();
        window.add_css_class("lutra-start-menu");
        window.set_child(Some(&content));

        // 弹层：Overlay 层、贴左上方；`.start-menu` 自带的 8px margin 让阴影有
        // 绘制空间，所以这里的偏移量比设计稿小 8px。
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_anchor(Edge::Top, true);
        window.set_anchor(Edge::Left, true);
        window.set_margin(Edge::Top, top_offset);
        window.set_margin(Edge::Left, 2);
        window.set_exclusive_zone(0);
        window.set_keyboard_interactivity(KeyboardInteractivity::None);
        window.set_visible(false);

        let visible = Rc::new(Cell::new(false));
        let on_launch: Rc<dyn Fn(&DesktopApp)> = Rc::new(on_launch);
        let menu = Rc::new(Self {
            window: window.clone(),
            entry: entry.clone(),
            list: list.clone(),
            apps,
            on_launch,
            first_hit: Rc::new(RefCell::new(None)),
            visible: visible.clone(),
        });

        menu.populate("");

        let menu_for_change = menu.clone();
        entry.connect_changed(move |entry| {
            menu_for_change.populate(&entry.text());
        });

        let menu_for_enter = menu.clone();
        entry.connect_activate(move |_| {
            let hit = menu_for_enter.first_hit.borrow().as_ref().cloned();
            if let Some(app) = hit {
                menu_for_enter.activate(&app);
            }
        });

        let menu_for_key = menu.clone();
        let key = gtk4::EventControllerKey::new();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::Escape {
                menu_for_key.hide();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(key);

        let menu_for_close = menu.clone();
        window.connect_close_request(move |window| {
            window.set_visible(false);
            menu_for_close.visible.set(false);
            glib::Propagation::Stop
        });

        menu
    }

    pub fn toggle(&self) {
        if self.visible.get() {
            self.hide();
        } else {
            self.show();
        }
    }

    pub fn show(&self) {
        // 每次打开都回到分类视图，避免上次的搜索词影响下一次使用。
        if !self.entry.text().is_empty() {
            self.entry.set_text("");
        }
        // 弹层需要键盘焦点才能输入搜索词，只在可见期间独占键盘。
        self.window
            .set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        self.window.present();
        self.entry.grab_focus();
        self.visible.set(true);
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
        self.window
            .set_keyboard_interactivity(KeyboardInteractivity::None);
        self.visible.set(false);
    }

    fn activate(&self, app: &DesktopApp) {
        launch_app(app);
        (self.on_launch)(app);
        // 收起菜单并清空搜索词（set_text 会触发 changed → 重建分类视图）
        self.hide();
        self.entry.set_text("");
    }

    /// 重建列表：空查询走分类视图，非空查询走搜索结果。
    fn populate(&self, query: &str) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }

        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            // 空查询时回车不启动任何东西，避免误触
            self.first_hit.replace(None);
            for (label, apps) in self.grouped() {
                self.append_section(label, &apps);
            }
            return;
        }

        let hits: Vec<DesktopApp> = self
            .apps
            .iter()
            .filter(|app| app.matches(&needle))
            .take(MAX_RESULTS)
            .cloned()
            .collect();
        self.first_hit.replace(hits.first().cloned());

        if hits.is_empty() {
            let empty = Label::new(Some("没有匹配的应用"));
            empty.add_css_class("start-empty");
            empty.set_halign(gtk4::Align::Center);
            self.list.append(&empty);
        } else {
            let refs: Vec<&DesktopApp> = hits.iter().collect();
            self.append_section("搜索结果", &refs);
        }
    }

    fn grouped(&self) -> Vec<(&'static str, Vec<&DesktopApp>)> {
        let mut sections: Vec<(&'static str, Vec<&DesktopApp>)> = Vec::new();

        for (key, label) in CATEGORY_ORDER {
            let items: Vec<&DesktopApp> = self
                .apps
                .iter()
                .filter(|app| app.category_key() == key)
                .collect();
            if !items.is_empty() {
                sections.push((label, items));
            }
        }

        let others: Vec<&DesktopApp> = self
            .apps
            .iter()
            .filter(|app| app.category_key() == OTHER_CATEGORY)
            .collect();
        if !others.is_empty() {
            sections.push(("其他", others));
        }

        sections
    }

    fn append_section(&self, label: &str, apps: &[&DesktopApp]) {
        let heading = Label::new(Some(label));
        heading.add_css_class("start-category");
        heading.set_halign(gtk4::Align::Start);
        self.list.append(&heading);

        let flow = FlowBox::builder()
            .max_children_per_line(4)
            .min_children_per_line(4)
            .homogeneous(true)
            .selection_mode(SelectionMode::None)
            .column_spacing(4)
            .row_spacing(4)
            .build();
        flow.add_css_class("start-grid");
        for app in apps {
            flow.insert(&self.app_tile(app), -1);
        }
        self.list.append(&flow);
    }

    fn app_tile(&self, app: &DesktopApp) -> Button {
        let name = Label::new(Some(app.name.as_str()));
        name.add_css_class("start-app-label");
        name.set_ellipsize(pango::EllipsizeMode::End);
        name.set_max_width_chars(9);
        name.set_lines(1);

        let content = GtkBox::new(Orientation::Vertical, 7);
        content.set_halign(gtk4::Align::Center);
        content.append(&theme::app_icon(&app.icon, 30));
        content.append(&name);

        let button = Button::new();
        button.add_css_class("start-app");
        button.set_tooltip_text(Some(app.comment.as_str()));
        button.set_child(Some(&content));

        let app = app.clone();
        let window = self.window.clone();
        let on_launch = self.on_launch.clone();
        let visible = self.visible.clone();
        button.connect_clicked(move |_| {
            launch_app(&app);
            on_launch(&app);
            window.set_visible(false);
            visible.set(false);
        });

        button
    }
}

/// 返回所有可见应用，按名称排序；同名 `.desktop` 只保留第一个。
fn load_apps() -> Vec<DesktopApp> {
    let mut apps = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for dir in application_dirs() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("desktop"))
            .collect();
        files.sort();

        for path in files {
            let Some(id) = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
            else {
                continue;
            };
            if seen.contains(&id) {
                continue;
            }
            if let Some(app) = parse_desktop_file(&path, &id) {
                seen.insert(id);
                apps.push(app);
            }
        }
    }

    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

fn application_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(home) = env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }

    let data_dirs =
        env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_string());
    for dir in data_dirs.split(':') {
        if dir.is_empty() {
            continue;
        }
        dirs.push(PathBuf::from(dir).join("applications"));
    }

    dirs
}

fn parse_desktop_file(path: &Path, id: &str) -> Option<DesktopApp> {
    let content = fs::read_to_string(path).ok()?;

    let mut name = None;
    let mut localized_name = None;
    let mut comment = String::new();
    let mut exec = None;
    let mut icon = String::new();
    let mut categories: Vec<String> = Vec::new();
    let mut terminal = false;
    let mut in_entry = false;

    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            // 只认主组，跳过 [Desktop Action ...] 等附加组
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();

        match key.trim() {
            "Type" => {
                if value != "Application" {
                    return None;
                }
            }
            "Name" => name = Some(value.to_string()),
            "Name[zh_CN]" => localized_name = Some(value.to_string()),
            "Comment" => comment = value.to_string(),
            "Exec" => exec = Some(value.to_string()),
            "Icon" => icon = value.to_string(),
            "Categories" => {
                categories = value
                    .split(';')
                    .filter(|part| !part.is_empty())
                    .map(|part| part.to_string())
                    .collect();
            }
            "Terminal" => terminal = value == "true",
            "NoDisplay" | "Hidden" => {
                if value == "true" {
                    return None;
                }
            }
            _ => {}
        }
    }

    let name = localized_name.or(name)?;
    let exec = exec?;
    if icon.is_empty() {
        icon = "application-x-executable".to_string();
    }

    Some(DesktopApp {
        id: id.to_string(),
        name,
        comment,
        exec,
        icon,
        categories,
        terminal,
    })
}

/// 启动一个应用：去掉 Exec 里的字段码，需要终端的套一层 foot。
pub fn launch_app(app: &DesktopApp) {
    let mut command = strip_field_codes(&app.exec);
    if app.terminal {
        command = format!("foot -e {}", command);
    }
    spawn_command(&command);
}

/// 以脱离 shell 的方式执行命令（子进程由独立线程回收，避免僵尸）。
pub fn spawn_command(command: &str) {
    match Command::new("/bin/sh").arg("-c").arg(command).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(err) => eprintln!("lutra-shell: 无法启动 `{command}`: {err}"),
    }
}

/// 去掉 `%f`/`%U`/`%i` 之类字段码，`%%` 还原成 `%`。
fn strip_field_codes(exec: &str) -> String {
    exec.split_whitespace()
        .map(|token| {
            if token == "%%" {
                "%".to_string()
            } else {
                token.to_string()
            }
        })
        .filter(|token| !(token.starts_with('%') && token.len() == 2))
        .collect::<Vec<_>>()
        .join(" ")
}
