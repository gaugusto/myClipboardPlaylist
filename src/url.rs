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
    Some(url.into())
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
}
