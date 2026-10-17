//! "Firefox on macOS": a human label for a session's device, from its User-Agent. A display
//! hint only; it is never used for a security decision (anyone can send any User-Agent).

/// A short label such as "Chrome on Windows", "Safari on iPhone" or "Unknown device".
pub fn describe(user_agent: Option<&str>) -> String {
    let Some(ua) = user_agent.filter(|u| !u.trim().is_empty()) else {
        return "Unknown device".to_owned();
    };
    // Order matters: Edge and Opera say "Chrome", Chrome says "Safari".
    let browser = if ua.contains("Edg/") {
        "Edge"
    } else if ua.contains("OPR/") || ua.contains("Opera") {
        "Opera"
    } else if ua.contains("Firefox/") || ua.contains("FxiOS/") {
        "Firefox"
    } else if ua.contains("Chrome/") || ua.contains("CriOS/") {
        "Chrome"
    } else if ua.contains("Safari/") {
        "Safari"
    } else if ua.contains("curl/") {
        "curl"
    } else {
        "A browser"
    };
    let os = if ua.contains("iPhone") {
        "iPhone"
    } else if ua.contains("iPad") {
        "iPad"
    } else if ua.contains("Android") {
        "Android"
    } else if ua.contains("Mac OS X") || ua.contains("Macintosh") {
        "macOS"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("CrOS") {
        "ChromeOS"
    } else if ua.contains("Linux") {
        "Linux"
    } else {
        return browser.to_owned();
    };
    format!("{browser} on {os}")
}

#[cfg(test)]
mod tests {
    use super::describe;

    #[test]
    fn labels_common_browsers() {
        let mac_chrome = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
        assert_eq!(describe(Some(mac_chrome)), "Chrome on macOS");
        let iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1";
        assert_eq!(describe(Some(iphone)), "Safari on iPhone");
        let edge = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0 Safari/537.36 Edg/141.0";
        assert_eq!(describe(Some(edge)), "Edge on Windows");
        let ff = "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0";
        assert_eq!(describe(Some(ff)), "Firefox on Linux");
        assert_eq!(describe(None), "Unknown device");
        assert_eq!(describe(Some("curl/8.0")), "curl");
    }
}
