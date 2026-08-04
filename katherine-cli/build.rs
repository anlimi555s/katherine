// build.rs — 编译时绑定 identity.json 的 SHA-256 哈希。
// 引擎启动时不匹配则拒绝启动。

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    // 找到 identity.json
    let identity_path = find_identity();
    let hash = match fs::read(&identity_path) {
        Ok(data) => {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            data.hash(&mut hasher);
            format!("{:x}", hasher.finish())
        }
        Err(_) => "no_identity_file".to_string(),
    };

    // 写入环境变量供 main.rs 使用
    println!("cargo:rustc-env=KATHERINE_IDENTITY_HASH={hash}");
    println!(
        "cargo:rerun-if-changed={}",
        identity_path.display()
    );
}

fn find_identity() -> PathBuf {
    // 优先 KATHERINE_HOME，然后上级目录
    if let Ok(home) = env::var("KATHERINE_HOME") {
        let p = PathBuf::from(&home).join("identity.json");
        if p.exists() {
            return p;
        }
    }
    // 从 crate 目录向上找
    let mut dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()));
    loop {
        let p = dir.join("identity.json");
        if p.exists() {
            return p;
        }
        if !dir.pop() {
            break;
        }
    }
    PathBuf::from("identity.json")
}
