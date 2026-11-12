// Reads and writes server.properties in the format vanilla uses (java.util.Properties):
// `key=value` lines, `#` or `!` comments, backslash escapes.

use anyhow::{Context, Result};
use std::{fs, io::ErrorKind, path::Path};

#[derive(Default)]
pub struct Properties {
    entries: Vec<(String, String)>,
    added: bool,
}

impl Properties {
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn parse(text: &str) -> Self {
        let mut props = Self::default();
        let mut pending = String::new();
        for line in text.lines() {
            let line = line.trim_start();
            if pending.is_empty() && (line.is_empty() || line.starts_with('#') || line.starts_with('!')) {
                continue;
            }
            // An odd number of trailing backslashes continues the line.
            let trailing = line.len() - line.trim_end_matches('\\').len();
            if trailing % 2 == 1 {
                pending.push_str(&line[..line.len() - 1]);
                continue;
            }
            pending.push_str(line);
            let (key, value) = split_entry(&pending);
            props.set(&key, &value);
            pending.clear();
        }
        props
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }

    pub fn set(&mut self, key: &str, value: &str) {
        match self.entries.iter_mut().find(|(k, _)| k == key) {
            Some(entry) => entry.1 = value.to_owned(),
            None => self.entries.push((key.to_owned(), value.to_owned())),
        }
    }

    /// The value for `key`, adding `default` to the file when the key is missing.
    pub fn get_or(&mut self, key: &str, default: &str) -> String {
        if let Some(v) = self.get(key) {
            return v.to_owned();
        }
        self.set(key, default);
        self.added = true;
        default.to_owned()
    }

    /// True once `get_or` has filled in a key the file didn't have.
    pub fn has_new_keys(&self) -> bool {
        self.added
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let mut out = String::from("#Minecraft server properties\n");
        let mut entries: Vec<_> = self.entries.iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in entries {
            out.push_str(&escape(k, true));
            out.push('=');
            out.push_str(&escape(v, false));
            out.push('\n');
        }
        fs::write(path, out).with_context(|| format!("writing {}", path.display()))
    }
}

fn split_entry(line: &str) -> (String, String) {
    let mut key = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    key.push(unescape(next));
                }
            }
            '=' | ':' => break,
            c if c.is_whitespace() => {
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                }
                if matches!(chars.peek(), Some('=') | Some(':')) {
                    chars.next();
                }
                break;
            }
            c => key.push(c),
        }
    }
    while chars.peek().is_some_and(|c| c.is_whitespace()) {
        chars.next();
    }
    let mut value = String::new();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        value.push(c);
                    }
                }
                Some(next) => value.push(unescape(next)),
                None => {}
            }
        } else {
            value.push(c);
        }
    }
    (key, value)
}

fn unescape(c: char) -> char {
    match c {
        't' => '\t',
        'n' => '\n',
        'r' => '\r',
        'f' => '\x0c',
        other => other,
    }
}

fn escape(s: &str, is_key: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '=' | ':' | '#' | '!' => {
                out.push('\\');
                out.push(c);
            }
            ' ' if is_key || i == 0 => out.push_str("\\ "),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_vanilla_lines() {
        let p = Properties::parse(
            "#Minecraft server properties\n#Tue Nov 10 09:00:00 UTC 2026\nlevel-type=minecraft\\:normal\n\
             motd=A Minecraft Server\nserver-ip=\n  spaced key  =  value  \nnext\\\n  line=joined\n",
        );
        assert_eq!(p.get("level-type"), Some("minecraft:normal"));
        assert_eq!(p.get("motd"), Some("A Minecraft Server"));
        assert_eq!(p.get("server-ip"), Some(""));
        assert_eq!(p.get("spaced"), Some("key  =  value  "));
        assert_eq!(p.get("nextline"), Some("joined"));
    }

    #[test]
    fn unicode_escape() {
        assert_eq!(Properties::parse("motd=\\u00A7aHi").get("motd"), Some("\u{a7}aHi"));
    }

    #[test]
    fn save_round_trips() {
        let mut p = Properties::default();
        p.set("level-type", "minecraft:flat");
        p.set("motd", " \u{a7}6Gold = rich");
        let dir = std::env::temp_dir().join(format!("minerust-props-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("server.properties");
        p.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("level-type=minecraft\\:flat"));
        let back = Properties::load(&path).unwrap();
        assert_eq!(back.get("motd"), Some(" \u{a7}6Gold = rich"));
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn get_or_records_missing_keys() {
        let mut p = Properties::parse("max-players=5\n");
        assert_eq!(p.get_or("max-players", "20"), "5");
        assert!(!p.has_new_keys());
        assert_eq!(p.get_or("motd", "hello"), "hello");
        assert!(p.has_new_keys());
    }
}
