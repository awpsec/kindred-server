//! Imported prompt placeholders are text substitutions, never shell evaluation.
use crate::commands::Parameter;
use anyhow::{Result, ensure};
use serde_json::{Map, Value, json};

#[derive(Clone)]
struct Token<'a> {
    start: usize,
    end: usize,
    text: &'a str,
    position: Option<usize>,
}

fn tokens(body: &str, index: usize) -> Vec<Token<'_>> {
    let bytes = body.as_bytes();
    let mut result = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            i += 1;
            continue;
        }
        let escapes = bytes[..i].iter().rev().take_while(|b| **b == b'\\').count();
        if escapes % 2 == 1 {
            i += 1;
            continue;
        }
        let start = i;
        let tail = &body[i..];
        let (end, position) = if let Some(rest) = tail.strip_prefix("$ARGUMENTS[") {
            let n = rest.bytes().take_while(u8::is_ascii_digit).count();
            if n == 0 || rest.as_bytes().get(n) != Some(&b']') {
                i += 1;
                continue;
            }
            (
                i + 11 + n + 1,
                Some(rest[..n].parse().unwrap_or(usize::MAX)),
            )
        } else if tail.starts_with("$ARGUMENTS") {
            (i + 10, None)
        } else if tail.starts_with("$@") {
            (i + 2, None)
        } else {
            let n = tail[1..].bytes().take_while(u8::is_ascii_digit).count();
            if n == 0 {
                i += 1;
                continue;
            }
            let number: usize = tail[1..1 + n].parse().unwrap_or(usize::MAX);
            (
                i + 1 + n,
                Some(number.checked_sub(index).unwrap_or(usize::MAX)),
            )
        };
        if bytes
            .get(end)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            i = end;
            continue;
        }
        result.push(Token {
            start,
            end,
            text: &body[start..end],
            position,
        });
        i = end;
    }
    result
}

/// Old .claude/commands files often use $1 as their first argument. An explicit
/// $0 or $ARGUMENTS[N] identifies modern indexing. The editor can override either.
pub fn index(package: &Value) -> usize {
    if let Some(n @ 0..=1) = package["argument_index"].as_u64() {
        return n as usize;
    }
    let path = package["source"]
        .as_str()
        .unwrap_or("")
        .replace('\\', "/")
        .to_lowercase();
    let body = package["body"].as_str().unwrap_or("");
    let parsed = tokens(body, 0);
    if path.split('/').any(|p| p == ".pi" || p == ".codex") {
        return 1;
    }
    if parsed
        .iter()
        .any(|t| t.text == "$0" || t.text.starts_with("$ARGUMENTS["))
    {
        return 0;
    }
    if path.contains(".claude/commands/") && parsed.iter().any(|t| t.position.is_some()) {
        return 1;
    }
    if path.split('/').any(|p| p == ".claude") || body.contains("CLAUDE_") {
        return 0;
    }
    1
}

pub fn inferred_legacy(package: &Value) -> bool {
    package["argument_index"].is_null()
        && package["source"]
            .as_str()
            .unwrap_or("")
            .replace('\\', "/")
            .to_lowercase()
            .contains(".claude/commands/")
        && index(package) == 1
        && tokens(package["body"].as_str().unwrap_or(""), 1)
            .iter()
            .any(|t| t.position.is_some())
}

pub fn automatic(stored: &[Parameter]) -> bool {
    stored.len() == 1 && stored[0].name == "arguments" && !stored[0].required && stored[0].rest
}

pub fn parameters(package: &Value, stored: Vec<Parameter>) -> Vec<Parameter> {
    if package.is_null() || !automatic(&stored) {
        return stored;
    }
    let refs = tokens(package["body"].as_str().unwrap_or(""), index(package));
    let required = refs
        .iter()
        .filter_map(|t| t.position)
        .max()
        .map(|n| n.saturating_add(1))
        .unwrap_or(0);
    let hint = package["argument_hint"].as_str().unwrap_or("").trim();
    let hints: Vec<_> = hint.split_whitespace().collect();
    let simple = !hints.is_empty()
        && hints.iter().all(|h| {
            let h = h.trim_matches(['[', ']', '<', '>']).trim_end_matches("...");
            !h.is_empty()
                && h.len() <= 32
                && h.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        });
    let count = required.max(if simple { hints.len() } else { 0 });
    // Leave complex/large schemas as free input, but still bind and validate every
    // referenced position when queuing. Never silently renumber a placeholder.
    if count == 0 || count > 7 {
        return stored;
    }
    let mut used = std::collections::HashSet::new();
    let mut result = vec![];
    for i in 0..count {
        let hint = if simple {
            hints.get(i).copied().unwrap_or("")
        } else {
            ""
        };
        let mut name = hint
            .trim_matches(['[', ']', '<', '>'])
            .trim_end_matches("...")
            .to_lowercase()
            .replace('_', "-");
        if !crate::commands::slug(&name) || name == "arguments" || used.contains(&name) {
            name = format!("input-{}", i + 1);
        }
        while used.contains(&name) {
            name.push_str("-2");
        }
        used.insert(name.clone());
        result.push(Parameter {
            name,
            description: format!("Input {} · ${}", i + 1, i + index(package)),
            required: i < required || hint.starts_with('<'),
            rest: i + 1 == count
                && hint.contains("...")
                && !refs.iter().any(|token| token.position == Some(i)),
        });
    }
    // Imported CLIs accept additional text even when the hint lists fewer inputs.
    if !result.last().unwrap().rest {
        result.push(Parameter {
            name: "arguments".into(),
            description: "Additional input".into(),
            required: false,
            rest: true,
        });
    }
    // Hints are descriptions, not a way to create required-after-optional schemas.
    if let Some(last) = result.iter().rposition(|p| p.required) {
        for p in &mut result[..=last] {
            p.required = true;
        }
    }
    result
}

pub fn expand(package: &Value, raw: &str, parts: &[String]) -> Result<(String, Value)> {
    let body = package["body"].as_str().unwrap_or("");
    let mut output = String::new();
    let mut bindings = Map::new();
    let mut cursor = 0;
    for token in tokens(body, index(package)) {
        let value = if let Some(position) = token.position {
            parts.get(position).map(String::as_str).ok_or_else(|| anyhow::anyhow!("Missing input for {}. This imported command counts from ${}; check its Parameters and Positional arguments in Settings → Skills.", token.text, index(package)))?
        } else {
            raw
        };
        ensure!(
            output.len() + token.start - cursor + value.len() <= 64000,
            "Expanded command exceeds the message limit"
        );
        output.push_str(&body[cursor..token.start]);
        output.push_str(value);
        bindings.insert(token.text.into(), json!(value));
        cursor = token.end;
    }
    ensure!(
        output.len() + body.len() - cursor <= 64000,
        "Expanded command exceeds the message limit"
    );
    output.push_str(&body[cursor..]);
    Ok((output, Value::Object(bindings)))
}

pub fn dynamic_shell(body: &str) -> bool {
    let mut shell_fence = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if shell_fence && trimmed == "```" {
            return true;
        }
        if trimmed == "```!" {
            shell_fence = true;
            continue;
        }
        let mut chars = line.char_indices().peekable();
        let mut previous = None;
        while let Some((i, ch)) = chars.next() {
            if ch == '!' && previous.is_none_or(char::is_whitespace) && line[i..].starts_with("!`")
            {
                let rest = &line[i + 2..];
                if let Some(end) = rest.find('`') {
                    if !rest[..end].trim().is_empty() {
                        return true;
                    }
                }
            }
            previous = Some(ch);
        }
    }
    false
}
