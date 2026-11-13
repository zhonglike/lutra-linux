//! Lutra desktop shell 入口。
//!
//! 进程模型：一个 GTK4 Application，持有两个 Wayland layer-shell 表面
//! ——顶栏（panel）与开始菜单（start menu）。

mod clock;
mod panel;
mod startmenu;
mod taskbar;
mod theme;

use std::cell::Cell;
use std::rc::Rc;

use gtk4::prelude::*;

fn main() -> gtk4::glib::ExitCode {
    let app = gtk4::Application::builder()
        .application_id("os.lutra.Shell")
        .build();

    // 只构建一次面板。后续 activate（例如再次点击 .desktop）直接返回，
    // 避免出现两条顶栏。
    let built = Rc::new(Cell::new(false));
    app.connect_activate(move |app| {
        if built.replace(true) {
            return;
        }
        theme::install();
        panel::build(app);
    });

    app.run()
}
