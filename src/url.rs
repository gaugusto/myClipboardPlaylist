/// Retorna a URL normalizada se `text` for um único link http(s) válido.
pub fn parse_link(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.contains(char::is_whitespace) {
        return None;
    }
    let url = url::Url::parse(text).ok()?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return None;
    }
    if is_obviously_not_video(&url) {
        return None;
    }
    Some(url.into())
}

/// Extensões de arquivos que nunca são vídeo.
const NON_VIDEO_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "avif", "svg", "ico", "bmp", "pdf", "doc", "docx", "xls",
    "xlsx", "ppt", "pptx", "odt", "ods", "txt", "csv", "json", "xml", "zip", "rar", "7z", "gz",
    "tar", "exe", "deb", "rpm", "apk", "dmg", "iso",
];

/// Descarta, sem consultar o yt-dlp, links que claramente não são vídeo: arquivos de
/// imagem ou documento e fotos de posts do X/Twitter (`/status/<id>/photo/<n>`).
fn is_obviously_not_video(url: &url::Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    let ext = path.rsplit_once('.').map(|(_, ext)| ext);
    if ext.is_some_and(|ext| NON_VIDEO_EXTENSIONS.contains(&ext)) {
        return true;
    }
    let host = url.host_str().unwrap_or_default();
    let host = host.strip_prefix("www.").unwrap_or(host);
    matches!(host, "x.com" | "twitter.com" | "mobile.twitter.com") && path.contains("/photo/")
}

#[cfg(test)]
mod tests {
    use super::parse_link;

    #[test]
    fn accepts_http_links() {
        assert!(parse_link("https://www.youtube.com/watch?v=dQw4w9WgXcQ").is_some());
        assert!(parse_link("  http://vimeo.com/123\n").is_some());
    }

    #[test]
    fn rejects_other_text() {
        assert!(parse_link("").is_none());
        assert!(parse_link("olá mundo").is_none());
        assert!(parse_link("ftp://example.com/x").is_none());
        assert!(parse_link("file:///etc/passwd").is_none());
        assert!(parse_link("https://a.com/x https://b.com/y").is_none());
    }

    #[test]
    fn rejects_obvious_non_videos() {
        assert!(parse_link("https://site.com/imagem.JPG").is_none());
        assert!(parse_link("https://site.com/doc.pdf?download=1").is_none());
        assert!(parse_link("https://x.com/fulano/status/123/photo/1").is_none());
        assert!(parse_link("https://x.com/fulano/status/123").is_some());
        assert!(parse_link("https://site.com/video.mp4").is_some());
    }
}
