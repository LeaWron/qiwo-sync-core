//! Cross-process exclusion shared by direct file sync and native cleanup.
use anyhow::{Result, anyhow};
use std::{
    fs::{self, File, OpenOptions},
    path::Path,
};

pub struct Guard(File);
impl Guard {
    pub fn acquire(root: &Path) -> Result<Self> {
        fs::create_dir_all(root)?;
        let root = fs::canonicalize(root)?;
        let path = crate::lifecycle::local::safe_path(&root, ".qiwo-sync/operation.lock")?;
        fs::create_dir_all(path.parent().unwrap())?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        file.try_lock()
            .map_err(|_| anyhow!("本机另一项同步或清理任务正在执行"))?;
        Ok(Self(file))
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independently_opened_clients_cannot_mutate_together() {
        let root = std::env::temp_dir().join(format!("qiwo-operation-{}", std::process::id()));
        let first = Guard::acquire(&root).unwrap();
        assert!(Guard::acquire(&root).is_err());
        drop(first);
        assert!(Guard::acquire(&root).is_ok());
        fs::remove_dir_all(root).unwrap();
    }
}
