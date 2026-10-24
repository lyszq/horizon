//! Explicit launch paths shared by the desktop and iPad entry points.
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub home: PathBuf,
    pub resources: PathBuf,
    pub cache: PathBuf,
    pub documents: PathBuf,
}

impl AppPaths {
    pub fn in_sandbox(sandbox: &Path) -> Self {
        let home = sandbox.join("Library/Application Support/Horizon");
        Self {
            resources: home.join("resources/26.3"),
            cache: sandbox.join("Library/Caches/Horizon"),
            documents: sandbox.join("Documents"),
            home,
        }
    }

    pub fn create(&self) -> std::io::Result<()> {
        for path in [&self.home, &self.cache, &self.documents] {
            std::fs::create_dir_all(path)?;
        }
        std::fs::create_dir_all(self.home.join("saves"))?;
        if let Some(parent) = self.resources.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct LaunchConfig {
    pub paths: AppPaths,
    pub render_distance: i32,
    pub simulation_distance: i32,
    pub fps_limit: i32,
}

impl LaunchConfig {
    pub fn ipad(sandbox: &Path) -> Self {
        Self {
            paths: AppPaths::in_sandbox(sandbox),
            render_distance: 12,
            simulation_distance: 12,
            fps_limit: 60,
        }
    }

    /// Called once, before any worker starts. Existing vanilla relative file
    /// paths are rooted in the app's private game home on every thread.
    pub fn enter_home(&self) -> std::io::Result<()> {
        self.paths.create()?;
        std::env::set_current_dir(&self.paths.home)
    }

    /// Defaults only: an existing player's options always win.
    pub fn initialize_options(&self) -> std::io::Result<()> {
        use std::io::Write;
        match std::fs::OpenOptions::new().write(true).create_new(true)
            .open(self.paths.home.join("options.txt")) {
            Ok(mut file) => write!(file,
                "renderDistance:{}\nsimulationDistance:{}\nmaxFps:{}\nenableVsync:true\n",
                self.render_distance, self.simulation_distance, self.fps_limit),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn caches_and_persistent_data_have_separate_roots() {
        let p = AppPaths::in_sandbox(Path::new("sandbox"));
        assert!(p.resources.starts_with(&p.home));
        assert!(!p.cache.starts_with(&p.home));
        assert!(!p.documents.starts_with(&p.home));
    }
}
