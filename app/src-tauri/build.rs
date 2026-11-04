fn main() {
    // 前端资源在编译时被嵌进 exe：任何 html/css/js 改动都要触发重编译，
    // 否则「改了没生效」会被误判成缓存问题。目录级 rerun-if-changed 不可靠，
    // 这里逐个文件登记。
    if let Ok(entries) = std::fs::read_dir("../src") {
        for entry in entries.flatten() {
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }
    tauri_build::build()
}
