//! 任务栏：运行中窗口按钮 + 按钮下方居中的运行/激活指示点。
//!
//! ## 为什么是“降级实现”
//!
//! labwc/wlroots 没有 swaymsg 那样的 IPC，GTK4 也没有
//! `wlr-foreign-toplevel-management` 的绑定，libwnck 只适用于 X11，
//! 因此 shell 无法枚举、置顶或最小化**其他进程**的窗口。
//!
//! 这里退化为“由本 shell 启动过的应用”列表：
//! * 点击按钮 = 再次执行该应用的 Exec。单实例应用（如 Firefox）
//!   会通过 xdg-activation 把已有窗口提到前台，效果等同于“激活”；
//! * 真正的“最小化”需要合成器协议支持，当前留空并在接管协议后补上，
//!   不做假动画以免误导用户。

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{Align, Box as GtkBox, Button, Orientation};

use crate::startmenu::{launch_app, DesktopApp};
use crate::theme;

struct TaskItem {
    app: DesktopApp,
    dot: GtkBox,
}

#[derive(Clone)]
pub struct Taskbar {
    root: GtkBox,
    items: Rc<RefCell<Vec<TaskItem>>>,
}

impl Taskbar {
    pub fn new() -> Self {
        let root = GtkBox::new(Orientation::Horizontal, 2);
        root.add_css_class("taskbar");
        root.set_valign(Align::Center);

        Self {
            root,
            items: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn widget(&self) -> &GtkBox {
        &self.root
    }

    /// 登记一个应用：已存在则点亮，不存在则追加一个按钮。
    pub fn track(&self, app: &DesktopApp) {
        if let Some(index) = self
            .items
            .borrow()
            .iter()
            .position(|item| item.app.id == app.id)
        {
            self.mark_active(index);
            return;
        }

        let button = Button::new();
        button.add_css_class("task-button");
        button.set_child(Some(&theme::app_icon(&app.icon, 22)));
        button.set_tooltip_text(Some(app.name.as_str()));

        // 运行指示点：与按钮同处一个纵向盒子，靠 halign(Center) 保证
        // 始终在按钮正下方水平居中。
        let dot = GtkBox::new(Orientation::Horizontal, 0);
        dot.add_css_class("task-dot");
        dot.add_css_class("running");
        dot.set_size_request(5, 5);
        dot.set_halign(Align::Center);
        dot.set_valign(Align::Center);

        let item_box = GtkBox::new(Orientation::Vertical, 0);
        item_box.add_css_class("task-item");
        item_box.set_valign(Align::Center);
        item_box.append(&button);
        item_box.append(&dot);
        self.root.append(&item_box);

        let index = self.items.borrow().len();
        let taskbar = self.clone();
        button.connect_clicked(move |_| taskbar.on_click(index));

        self.items.borrow_mut().push(TaskItem {
            app: app.clone(),
            dot,
        });
        self.mark_active(index);
    }

    fn on_click(&self, index: usize) {
        let app = {
            let items = self.items.borrow();
            let Some(item) = items.get(index) else {
                return;
            };
            item.app.clone()
        };

        // 见文件头注释：没有窗口协议，只能通过重新执行 Exec 触发
        // xdg-activation，让单实例应用把窗口提到前台。
        launch_app(&app);
        self.mark_active(index);
    }

    fn mark_active(&self, index: usize) {
        let items = self.items.borrow();
        let Some(item) = items.get(index) else {
            return;
        };
        item.dot.add_css_class("active");
        item.dot.remove_css_class("running");
    }
}
