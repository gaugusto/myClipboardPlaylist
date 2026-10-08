use std::process::Command;

use anyhow::{Context as _, Result, bail};
use base64::Engine as _;
use serde::Deserialize;

use crate::url::parse_link;

/// Uma fonte de histórico da área de transferência (dms, cliphist, …).
pub trait HistorySource: Send + Sync {
    fn name(&self) -> &str;
    /// Textos do histórico, do mais antigo para o mais recente.
    fn texts(&self) -> Result<Vec<String>>;
}

/// Lê o histórico da fonte e devolve os links válidos, sem repetições,
/// na ordem em que foram copiados.
pub fn fetch_links(source: &dyn HistorySource) -> Result<Vec<String>> {
    let mut links: Vec<String> = Vec::new();
    for link in source.texts()?.iter().filter_map(|t| parse_link(t)) {
        if !links.contains(&link) {
            links.push(link);
        }
    }
    Ok(links)
}

/// Histórico do DankMaterialShell (`dms clipboard history --json`).
pub struct Dms;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DmsEntry {
    id: u64,
    #[serde(default)]
    is_image: bool,
    #[serde(default)]
    preview: String,
    #[serde(default)]
    size: usize,
}

#[derive(Deserialize)]
struct DmsFullEntry {
    data: String,
}

fn run_dms(args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("dms")
        .args(args)
        .output()
        .context("não foi possível executar `dms` (está instalado?)")?;
    if !output.status.success() {
        bail!(
            "`dms {}` falhou: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

impl Dms {
    /// O `preview` pode vir truncado; nesse caso busca o conteúdo completo.
    fn full_text(entry: &DmsEntry) -> Result<String> {
        let out = run_dms(&["clipboard", "get", &entry.id.to_string(), "--json"])?;
        let full: DmsFullEntry = serde_json::from_slice(&out)?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(full.data.trim())?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

impl HistorySource for Dms {
    fn name(&self) -> &str {
        "dms"
    }

    fn texts(&self) -> Result<Vec<String>> {
        let out = run_dms(&["clipboard", "history", "--json"])?;
        let entries: Vec<DmsEntry> = serde_json::from_slice(&out)
            .context("saída inesperada de `dms clipboard history --json`")?;
        let mut texts = Vec::with_capacity(entries.len());
        // O dms lista do mais recente para o mais antigo.
        for entry in entries.iter().rev().filter(|e| !e.is_image) {
            let truncated = entry.preview.len() < entry.size;
            if truncated && entry.preview.trim_start().starts_with("http") {
                texts.push(Self::full_text(entry)?);
            } else {
                texts.push(entry.preview.clone());
            }
        }
        Ok(texts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake(Vec<&'static str>);

    impl HistorySource for Fake {
        fn name(&self) -> &str {
            "fake"
        }
        fn texts(&self) -> Result<Vec<String>> {
            Ok(self.0.iter().map(|s| s.to_string()).collect())
        }
    }

    #[test]
    fn filters_and_dedups_in_order() {
        let source = Fake(vec![
            "https://youtu.be/a",
            "texto qualquer",
            "https://youtu.be/b",
            "https://youtu.be/a",
        ]);
        assert_eq!(
            fetch_links(&source).unwrap(),
            vec!["https://youtu.be/a", "https://youtu.be/b"]
        );
    }
}
