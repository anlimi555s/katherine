// identity.rs — 编译时 identity hash 校验。
// 系统提示组装已迁移到 wake.rs。

use std::path::PathBuf;

/// 找 identity.json——先从 katherine-memories/，再回退到旧路径。
fn find_identity() -> PathBuf {
    // 新路径：katherine-memories/identity.json
    if let Ok(home) = std::env::var("KATHERINE_HOME") {
        let p = PathBuf::from(&home).join("katherine-memories").join("identity.json");
        if p.exists() {
            return p;
        }
        // 回退：旧路径 KATHERINE_HOME/identity.json
        let old = PathBuf::from(&home).join("identity.json");
        if old.exists() {
            return old;
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        for ancestor in cwd.ancestors() {
            let p = ancestor.join("katherine-memories").join("identity.json");
            if p.exists() {
                return p;
            }
            let old = ancestor.join("identity.json");
            if old.exists() {
                return old;
            }
        }
    }

    PathBuf::from("katherine-memories").join("identity.json")
}

/// 计算 identity.json 的运行时哈希——和 build.rs 的编译时哈希比对。
pub fn compute_hash() -> String {
    let path = find_identity();
    match std::fs::read(&path) {
        Ok(data) => {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            data.hash(&mut hasher);
            format!("{:x}", hasher.finish())
        }
        Err(_) => "no_identity_file".to_string(),
    }
}
