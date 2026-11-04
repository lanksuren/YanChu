// 一直用窗口子系统：开发构建也不再弹控制台窗口。
// 日志走控制面板「设置 → 开发者模式」；命令行模式需要输出时仍可重定向拿到。
#![windows_subsystem = "windows"]

fn main() {
    yanchu_lib::run()
}
