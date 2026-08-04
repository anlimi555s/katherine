// launcher.rs — Katherine 启动器。双击这个 exe 启动引擎 + 打开浏览器。
// 编译为 Katherine.exe，放在 target/release/ 旁边。

use std::process::{Command, Stdio};
use std::path::PathBuf;

fn main() {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));

    let engine_path = exe_dir.join("katherine-cli.exe");
    if !engine_path.exists() {
        eprintln!("找不到引擎: {}", engine_path.display());
        eprintln!("请确保 Katherine.exe 和 katherine-cli.exe 在同一个目录");
        std::process::exit(1);
    }

    let katherine_home = exe_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| exe_dir.clone());

    println!("Katherine 启动中...");
    println!("引擎: {}", engine_path.display());

    // 启动引擎（后台，不显示窗口）
    let _engine = Command::new(&engine_path)
        .arg("serve")
        .arg("--port")
        .arg("9876")
        .env("KATHERINE_HOME", &katherine_home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("无法启动引擎");

    // 等引擎就绪
    std::thread::sleep(std::time::Duration::from_secs(3));

    // 打开前端
    let html_path = katherine_home.join("katherine-memories").join("katherine.html");
    println!("前端: {}", html_path.display());
    let _ = Command::new("cmd")
        .args(["/c", "start", "", &html_path.to_string_lossy()])
        .spawn();

    println!("Katherine 已启动。浏览器应该打开了。");
    println!("可以关闭这个窗口。");
}
