//! 系统区时钟：`周一 23:37` 形式，按分钟刷新（10s 轮询即可覆盖分钟跳变）。

use gtk4::prelude::*;
use gtk4::{glib, Label};

pub fn build() -> Label {
    let label = Label::new(None);
    label.add_css_class("tray-clock");
    refresh(&label);

    let clock = label.clone();
    glib::timeout_add_seconds_local(10, move || {
        refresh(&clock);
        glib::ControlFlow::Continue
    });

    label
}

fn refresh(label: &Label) {
    label.set_label(&now_text());

    if let Ok(now) = glib::DateTime::now_local() {
        let today = format!(
            "{:04}-{:02}-{:02}",
            now.year(),
            now.month(),
            now.day_of_month()
        );
        label.set_tooltip_text(Some(today.as_str()));
    }
}

fn now_text() -> String {
    match glib::DateTime::now_local() {
        Ok(now) => {
            let weekday = match now.day_of_week() {
                1 => "日",
                2 => "一",
                3 => "二",
                4 => "三",
                5 => "四",
                6 => "五",
                _ => "六",
            };
            format!("周{} {:02}:{:02}", weekday, now.hour(), now.minute())
        }
        Err(_) => "--:--".to_string(),
    }
}
