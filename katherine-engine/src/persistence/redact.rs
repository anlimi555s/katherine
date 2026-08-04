// persistence/redact.rs — 密钥涂抹。
// 写入磁盘前正则匹配敏感字段 → [REDACTED]。
// 内存里保留明文（需要 token 调 API），磁盘上不可见。

use regex::Regex;
use std::sync::LazyLock;

static SECRET_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    vec![
        // Bearer token
        Regex::new(r"(?i)bearer\s+[a-zA-Z0-9\-_.+=/]+").unwrap(),
        // API key header: x-api-key / api-key / authorization
        Regex::new(r"(?i)(x-api-key|api-key|apikey|api_key)\s*[:=]\s*[^\s,;]+").unwrap(),
        // Auth token: ANTHROPIC_AUTH_TOKEN / DEEPSEEK_KEY / OPENAI_API_KEY
        Regex::new(r#"(?i)(ANTHROPIC_AUTH_TOKEN|DEEPSEEK_KEY|OPENAI_API_KEY|KATHERINE_HUB_KEY)\s*=\s*[^\s"']+"#).unwrap(),
        // sk- / ds- prefixed keys (OpenAI / DeepSeek)
        Regex::new(r"\b(sk|ds)-[a-zA-Z0-9]{20,}\b").unwrap(),
        // JWT tokens (eyJ pattern)
        Regex::new(r"\beyJ[a-zA-Z0-9\-_.]{30,}\b").unwrap(),
        // Generic: password / secret / token = value
        Regex::new(r#"(?i)(password|secret|token)\s*[:=]\s*[^\s,;"]+"#).unwrap(),
    ]
});

/// 涂抹文本中的密钥——替换为 [REDACTED]。
/// 只改 value 部分，不破坏 JSON 结构。
pub fn redact_secrets(text: &str) -> String {
    let mut result = text.to_string();
    for pattern in SECRET_PATTERNS.iter() {
        result = pattern.replace_all(&result, |caps: &regex::Captures| {
            let full = caps.get(0).unwrap().as_str();
            // 保留 key/prefix 部分，替换 value
            if let Some(equals) = full.find('=') {
                format!("{}=[REDACTED]", &full[..equals])
            } else if let Some(colon) = full.find(':') {
                format!("{}:[REDACTED]", &full[..colon])
            } else if let Some(space) = full.rfind(' ') {
                format!("{} [REDACTED]", &full[..space])
            } else {
                "[REDACTED]".to_string()
            }
        }).to_string();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_bearer_token() {
        let input = "Authorization: Bearer sk-abc123def456ghijklmnop";
        let result = redact_secrets(input);
        assert!(!result.contains("sk-abc123"));
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn redact_api_key() {
        let input = "ANTHROPIC_AUTH_TOKEN=sk-abc12345678901234567890";
        let result = redact_secrets(input);
        assert!(result.contains("ANTHROPIC_AUTH_TOKEN=[REDACTED]"));
        assert!(!result.contains("sk-abc"));
    }

    #[test]
    fn redact_deepseek_key() {
        let input = r#"{"api_key": "ds-abcdef12345678901234567890"}"#;
        let result = redact_secrets(input);
        assert!(!result.contains("ds-abcdef"));
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn preserve_safe_text() {
        let input = "Hello, this is a normal message about files and paths";
        let result = redact_secrets(input);
        assert_eq!(input, result);
    }

    #[test]
    fn redact_password_field() {
        let input = "password=mysecret123";
        let result = redact_secrets(input);
        assert!(result.contains("[REDACTED]"));
        assert!(!result.contains("mysecret123"));
    }

    #[test]
    fn redact_x_api_key_header() {
        let input = "x-api-key: abcdef1234567890";
        let result = redact_secrets(input);
        assert!(result.contains("[REDACTED]"));
    }
}
