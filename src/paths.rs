use std::path::PathBuf;

fn runtime_dir() -> PathBuf {
    let mut p = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    p.push("nicotine");
    let _ = std::fs::create_dir_all(&p);
    p
}

pub fn lock_file_path() -> PathBuf {
    runtime_dir().join("nicotine-cycle.lock")
}

pub fn index_file_path() -> PathBuf {
    runtime_dir().join("nicotine-index")
}
