//! 主题：加载内嵌 CSS，并把图标名解析到 Lutra 自带图标目录。

use std::path::{Path, PathBuf};

use gtk4::prelude::*;
use gtk4::{gdk, CssProvider, Image};

/// 编译期把设计稿对应的样式表打进二进制，运行时不需要额外的资源路径。
const SHELL_CSS: &str = include_str!("../data/style.css");

pub fn install() {
    let provider = CssProvider::new();
    provider.load_from_string(SHELL_CSS);

    if let Some(display) = gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// 图标搜索顺序：显式环境变量 → 当前工作目录（开发期）→ 随二进制安装的目录
/// → 系统安装路径。
fn icon_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(dir) = std::env::var_os("LUTRA_SHELL_ICONS") {
        dirs.push(PathBuf::from(dir));
    }
    dirs.push(PathBuf::from("data/icons"));

    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            dirs.push(bin_dir.join("icons"));
            dirs.push(bin_dir.join("../share/lutra-shell/icons"));
        }
    }

    dirs.push(PathBuf::from("/usr/share/lutra-shell/icons"));
    dirs
}

/// 解析图标：`Icon=` 既可能是绝对路径，也可能是图标主题里的名字。
pub fn icon_path(name: &str) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.is_absolute() {
        return if direct.exists() {
            Some(direct.to_path_buf())
        } else {
            None
        };
    }

    for dir in icon_dirs() {
        for ext in ["svg", "png"] {
            let candidate = dir.join(format!("{name}.{ext}"));
            if candidate.exists() {
                return Some(candidate);
            }
        }
        let plain = dir.join(name);
        if plain.exists() {
            return Some(plain);
        }
    }
    None
}

/// 构造固定尺寸的图标控件；找不到自带图标时退回 GTK 图标主题。
pub fn app_icon(name: &str, size: i32) -> Image {
    let image = match icon_path(name) {
        Some(path) => Image::from_file(&path),
        None => Image::from_icon_name(name),
    };
    image.set_pixel_size(size);
    image
}
