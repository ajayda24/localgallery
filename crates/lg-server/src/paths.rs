//! The portable folder layout. Everything the app writes lives under `root`.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Paths {
    pub root: PathBuf,
    pub data: PathBuf,
    pub models: PathBuf,
    pub engines: PathBuf,
    /// Built-in manifests and templates.
    pub manifests: PathBuf,
    /// Built web UI (index.html and assets).
    pub ui: PathBuf,
}

impl Paths {
    pub fn new(root: &Path) -> Self {
        let root = lg_types::canonical(root);
        Self {
            data: root.join("data"),
            models: root.join("models"),
            engines: root.join("engines"),
            manifests: root.join("manifests"),
            ui: root.join("ui"),
            root,
        }
    }

    pub fn outputs(&self) -> PathBuf {
        self.data.join("outputs")
    }
    pub fn thumbs(&self) -> PathBuf {
        self.data.join("thumbs")
    }
    pub fn assets(&self) -> PathBuf {
        self.data.join("assets")
    }
    pub fn logs(&self) -> PathBuf {
        self.data.join("logs")
    }
    pub fn tools(&self) -> PathBuf {
        self.data.join("tools")
    }
    pub fn user_manifests(&self) -> PathBuf {
        self.data.join("user-manifests")
    }
    pub fn db(&self) -> PathBuf {
        self.data.join("localgallery.db")
    }
    /// ComfyUI's own folders (input, output, temp, user) live under data/engine.
    pub fn engine_data(&self) -> PathBuf {
        self.data.join("engine")
    }

    pub fn create_all(&self) -> std::io::Result<()> {
        for d in [
            &self.data,
            &self.models,
            &self.engines,
            &self.outputs(),
            &self.thumbs(),
            &self.assets(),
            &self.logs(),
            &self.tools(),
            &self.user_manifests(),
            &self.engine_data(),
            &self.data.join("tmp"),
            &self.data.join("cache"),
        ] {
            std::fs::create_dir_all(d)?;
        }
        for f in lg_modelscan::FOLDERS {
            std::fs::create_dir_all(self.models.join(f))?;
        }
        // Marker: this folder is the app; nothing is written elsewhere.
        let marker = self.root.join("portable.txt");
        if !marker.exists() {
            let _ = std::fs::write(
                marker,
                "LocalGallery keeps all of its data in this folder.\nDelete the folder to uninstall.\n",
            );
        }
        Ok(())
    }

    /// A path relative to the data folder, for storing in the database.
    pub fn rel_data(&self, p: &Path) -> String {
        p.strip_prefix(&self.data)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// Resolve a stored relative path, refusing anything outside the data folder.
    pub fn abs_data(&self, rel: &str) -> Option<PathBuf> {
        let p = Path::new(rel);
        if p.is_absolute()
            || p.components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return None;
        }
        Some(self.data.join(p))
    }
}
