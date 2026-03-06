use serde_json::Value;

#[derive(Debug)]
pub enum ParsedLine {
    Json(Value),
    Malformed { raw_line: String, reason: String },
}

pub fn parse_line(line: &str) -> ParsedLine {
    let trimmed = line.trim_end_matches(&['\n', '\r'][..]);
    if trimmed.is_empty() {
        return ParsedLine::Malformed {
            raw_line: line.to_string(),
            reason: "empty line".to_string(),
        };
    }

    match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => ParsedLine::Json(value),
        Err(err) => ParsedLine::Malformed {
            raw_line: line.to_string(),
            reason: err.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_line, ParsedLine};

    #[test]
    fn parse_valid_json() {
        let line = r#"{"tool":"shell","status":"ok"}"#;
        let parsed = parse_line(line);
        assert!(matches!(parsed, ParsedLine::Json(_)));
    }

    #[test]
    fn parse_malformed_line() {
        let line = "<not-json>";
        let parsed = parse_line(line);
        assert!(matches!(parsed, ParsedLine::Malformed { .. }));
    }
}
