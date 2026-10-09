use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context as _, Result};

use crate::metadata::Metadata;

/// Arquivo, dentro do diretório do cache, com os metadados por URL.
const INDEX_FILE: &str = "metadata.json";
/// Arquivo com os links em que o yt-dlp não achou vídeo.
const REJECTED_FILE: &str = "nao_videos.json";

/// Cache em disco dos metadados (título, duração, thumbnail) de cada vídeo, para não
/// consultar o yt-dlp de novo a cada abertura do app. Fica em
/// `$XDG_CACHE_HOME/myclipboardplaylist` (ou `~/.cache/myclipboardplaylist`).
pub struct Cache {
    /// `None` se não há como descobrir o diretório: o cache só funciona em memória.
    dir: Option<PathBuf>,
    entries: Mutex<HashMap<String, Metadata>>,
    /// Links que não são vídeo, para não voltarem à fila a cada abertura.
    rejected: Mutex<HashSet<String>>,
}

fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("myclipboardplaylist"))
}

/// Lê o índice, descartando entradas cuja thumbnail sumiu do disco.
fn read_index(dir: &Path) -> HashMap<String, Metadata> {
    let Ok(data) = fs::read(dir.join(INDEX_FILE)) else {
        return HashMap::new();
    };
    let mut entries: HashMap<String, Metadata> = serde_json::from_slice(&data).unwrap_or_default();
    entries.retain(|_, m| m.thumb_file.as_ref().is_none_or(|f| f.exists()));
    entries
}

impl Cache {
    pub fn load() -> Self {
        Self::at(cache_dir())
    }

    fn at(dir: Option<PathBuf>) -> Self {
        let entries = dir.as_deref().map(read_index).unwrap_or_default();
        let rejected = dir
            .as_deref()
            .and_then(|d| fs::read(d.join(REJECTED_FILE)).ok())
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        Self {
            dir,
            entries: Mutex::new(entries),
            rejected: Mutex::new(rejected),
        }
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<String, Metadata>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get(&self, url: &str) -> Option<Metadata> {
        self.entries().get(url).cloned()
    }

    fn rejected(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.rejected.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Grava `data` em `file` dentro do diretório do cache, de forma atômica.
    fn write(&self, file: &str, data: &[u8]) -> Result<()> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        fs::create_dir_all(dir).context("não foi possível criar o diretório do cache")?;
        let tmp = dir.join(format!("{file}.tmp"));
        fs::write(&tmp, data)?;
        fs::rename(&tmp, dir.join(file)).context("não foi possível gravar o cache")
    }

    /// Guarda os metadados e grava o índice no disco.
    pub fn insert(&self, url: String, meta: Metadata) -> Result<()> {
        let mut entries = self.entries();
        entries.insert(url, meta);
        self.write(INDEX_FILE, &serde_json::to_vec(&*entries)?)
    }

    /// O link já foi consultado e não é um vídeo.
    pub fn is_rejected(&self, url: &str) -> bool {
        self.rejected().contains(url)
    }

    /// Marca o link como "não é vídeo" e grava a lista no disco.
    pub fn reject(&self, url: String) -> Result<()> {
        let mut rejected = self.rejected();
        if !rejected.insert(url) {
            return Ok(());
        }
        self.write(REJECTED_FILE, &serde_json::to_vec(&*rejected)?)
    }

    /// Grava os bytes da thumbnail de `url` e devolve o caminho do arquivo.
    pub fn save_thumbnail(&self, url: &str, bytes: &[u8], ext: &str) -> Result<Option<PathBuf>> {
        let Some(dir) = &self.dir else {
            return Ok(None);
        };
        let dir = dir.join("thumbs");
        fs::create_dir_all(&dir).context("não foi possível criar o diretório do cache")?;
        let mut hasher = DefaultHasher::new();
        url.hash(&mut hasher);
        let path = dir.join(format!("{:016x}.{ext}", hasher.finish()));
        fs::write(&path, bytes).context("não foi possível gravar a thumbnail")?;
        Ok(Some(path))
    }

    /// Esvazia o cache, em memória e no disco.
    pub fn clear(&self) -> Result<()> {
        let mut entries = self.entries();
        entries.clear();
        self.rejected().clear();
        match &self.dir {
            Some(dir) if dir.exists() => {
                fs::remove_dir_all(dir).context("não foi possível limpar o cache")
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(thumb_file: Option<PathBuf>) -> Metadata {
        Metadata {
            title: Some("Vídeo".to_owned()),
            duration: Some(42.0),
            is_live: false,
            thumbnail: Some("https://example.com/t.jpg".to_owned()),
            thumb_file,
        }
    }

    #[test]
    fn persists_and_clears() {
        let dir = std::env::temp_dir().join(format!("mcpl-cache-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let cache = Cache::at(Some(dir.clone()));
        let thumb = cache
            .save_thumbnail("https://v/1", b"bytes", "jpg")
            .unwrap();
        cache
            .insert("https://v/1".to_owned(), sample(thumb.clone()))
            .unwrap();
        // Entrada cuja thumbnail não existe mais é descartada ao recarregar.
        let missing = Some(dir.join("thumbs/sumiu.jpg"));
        cache
            .insert("https://v/2".to_owned(), sample(missing))
            .unwrap();

        let reloaded = Cache::at(Some(dir.clone()));
        let meta = reloaded.get("https://v/1").unwrap();
        assert_eq!(meta.title.as_deref(), Some("Vídeo"));
        assert_eq!(meta.thumb_file, thumb);
        assert!(reloaded.get("https://v/2").is_none());

        reloaded.reject("https://nao/video".to_owned()).unwrap();
        assert!(Cache::at(Some(dir.clone())).is_rejected("https://nao/video"));

        reloaded.clear().unwrap();
        assert!(reloaded.get("https://v/1").is_none());
        assert!(!reloaded.is_rejected("https://nao/video"));
        assert!(!dir.exists());
    }
}
