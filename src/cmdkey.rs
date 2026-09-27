//! Turns a raw Bash command line into a stable grouping key such as
//! `cargo test`, `npm run build` or `./scripts/ci.sh`.
//!
//! This is only used for grouping and reporting. Whether RTK covers a command
//! is decided by RTK itself (`rtk rewrite`), never by these heuristics.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdInfo {
    /// Grouping key, e.g. `git status`.
    pub key: String,
    /// The command segment the key came from, with env prefixes removed.
    pub primary: String,
    /// The command was written as `rtk <cmd>`.
    pub rtk_prefixed: bool,
    /// The command was `rtk proxy <cmd>`: RTK deliberately bypassed.
    pub rtk_proxy: bool,
    /// `RTK_DISABLED=1` was set for the command.
    pub rtk_disabled: bool,
}

/// Tokens of a shell line: words, or separators between commands.
#[derive(Debug, PartialEq)]
enum Tok {
    Word(String),
    Sep,
}

/// Quote-aware tokenizer. Separators: `&&`, `||`, `;`, `|`, `&`, newline.
/// Redirections are kept as words; they are dropped later.
fn tokenize(line: &str) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut cur = String::new();
    let mut has_word = false;
    let mut chars = line.chars().peekable();
    let flush = |cur: &mut String, has_word: &mut bool, toks: &mut Vec<Tok>| {
        if *has_word {
            toks.push(Tok::Word(std::mem::take(cur)));
            *has_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                has_word = true;
                for n in chars.by_ref() {
                    if n == '\'' {
                        break;
                    }
                    cur.push(n);
                }
            }
            '"' => {
                has_word = true;
                while let Some(n) = chars.next() {
                    match n {
                        '"' => break,
                        '\\' => {
                            if let Some(e) = chars.next() {
                                cur.push(e);
                            }
                        }
                        _ => cur.push(n),
                    }
                }
            }
            '\\' => {
                if let Some(n) = chars.next() {
                    if n != '\n' {
                        cur.push(n);
                        has_word = true;
                    }
                }
            }
            ' ' | '\t' => flush(&mut cur, &mut has_word, &mut toks),
            '\n' | ';' => {
                flush(&mut cur, &mut has_word, &mut toks);
                toks.push(Tok::Sep);
            }
            '&' | '|' => {
                // `2>&1`, `&>` and `>&` are redirections, not separators.
                if c == '&' && (cur.ends_with('>') || chars.peek() == Some(&'>')) {
                    cur.push(c);
                    has_word = true;
                    continue;
                }
                flush(&mut cur, &mut has_word, &mut toks);
                if chars.peek() == Some(&c) {
                    chars.next();
                }
                toks.push(Tok::Sep);
            }
            _ => {
                cur.push(c);
                has_word = true;
            }
        }
    }
    flush(&mut cur, &mut has_word, &mut toks);
    toks
}

fn segments(line: &str) -> Vec<Vec<String>> {
    let mut segs = Vec::new();
    let mut cur = Vec::new();
    for t in tokenize(line) {
        match t {
            Tok::Word(w) => cur.push(w),
            Tok::Sep => {
                if !cur.is_empty() {
                    segs.push(std::mem::take(&mut cur));
                }
            }
        }
    }
    if !cur.is_empty() {
        segs.push(cur);
    }
    segs
}

fn is_env_assignment(w: &str) -> bool {
    match w.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

fn is_redirect(w: &str) -> bool {
    let w = w.trim_start_matches(|c: char| c.is_ascii_digit());
    w.starts_with('>') || w.starts_with('<') || w.starts_with("&>")
}

/// Segments that set up the environment rather than doing work.
fn is_setup(words: &[String]) -> bool {
    matches!(
        words.first().map(String::as_str),
        Some(
            "cd" | "pushd"
                | "popd"
                | "export"
                | "set"
                | "source"
                | "."
                | "true"
                | "false"
                | "unset"
                | "shopt"
                | "trap"
                | "wait"
                | "sleep"
        )
    )
}

/// Strip env assignments, wrappers (`env`, `time`, `timeout N`, `nice`,
/// `sudo`) and redirections. Returns remaining words and whether
/// `RTK_DISABLED=1` was set.
fn strip_prefixes(words: &[String]) -> (Vec<String>, bool) {
    let mut disabled = false;
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if is_env_assignment(w) {
            if w == "RTK_DISABLED=1" {
                disabled = true;
            }
            i += 1;
        } else if matches!(
            w,
            "env" | "time" | "nice" | "sudo" | "command" | "exec" | "nohup"
        ) {
            i += 1;
            while i < words.len() && words[i].starts_with('-') {
                i += 1;
            }
        } else if w == "timeout" {
            i += 1;
            while i < words.len() && words[i].starts_with('-') {
                i += 1;
            }
            i += 1; // duration
        } else {
            break;
        }
    }
    let rest = words[i.min(words.len())..]
        .iter()
        .filter(|w| !is_redirect(w))
        .cloned()
        .collect();
    (rest, disabled)
}

/// A subcommand-looking word: short, lowercase-ish, not a flag or path.
fn looks_like_subcommand(w: &str) -> bool {
    !w.is_empty()
        && w.len() <= 24
        && !w.starts_with('-')
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
        && w.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

/// Tools whose `run`/`exec`/`-m` argument names the real work.
fn takes_third_word(program: &str, sub: &str) -> bool {
    matches!(
        (program, sub),
        (
            "npm" | "pnpm" | "yarn" | "bun",
            "run" | "exec" | "x" | "dlx"
        ) | ("uv" | "poetry" | "pipenv" | "pdm" | "hatch", "run")
            | ("python" | "python3", "-m")
            | ("cargo", "run" | "make")
            | ("docker" | "podman", "compose")
            | ("go", "run" | "tool")
    )
}

fn program_name(first: &str) -> String {
    // Absolute paths to system binaries collapse to the binary name; relative
    // paths (project scripts like ./scripts/ci.sh) are kept as written.
    if first.starts_with('/') {
        first.rsplit('/').next().unwrap_or(first).to_string()
    } else {
        first.to_string()
    }
}

fn key_from_words(words: &[String]) -> String {
    let Some(first) = words.first() else {
        return String::new();
    };
    let program = program_name(first);
    if matches!(
        program.as_str(),
        "for" | "while" | "until" | "if" | "case" | "select" | "function" | "{" | "(" | "[" | "[["
    ) {
        return program;
    }
    let mut key = vec![program.clone()];
    if let Some(second) = words.get(1) {
        if takes_third_word(&program, second) {
            key.push(second.clone());
            if let Some(third) = words.get(2)
                && (looks_like_subcommand(third) || program.starts_with("python"))
            {
                key.push(third.clone());
            }
        } else if looks_like_subcommand(second) && !program.contains('/') {
            key.push(second.clone());
        }
    }
    key.join(" ")
}

pub fn classify(command: &str) -> CmdInfo {
    let segs = segments(command);
    let mut disabled_any = false;
    let mut chosen: Option<Vec<String>> = None;
    for seg in &segs {
        let (words, disabled) = strip_prefixes(seg);
        disabled_any |= disabled;
        if words.is_empty() || is_setup(&words) {
            continue;
        }
        chosen = Some(words);
        break;
    }
    let words = chosen.unwrap_or_else(|| segs.first().cloned().unwrap_or_default());

    let (rtk_prefixed, rtk_proxy, words) = match words.first().map(|w| program_name(w)) {
        Some(p) if p == "rtk" => {
            if words.get(1).map(String::as_str) == Some("proxy") {
                (true, true, words[2..].to_vec())
            } else {
                (true, false, words[1..].to_vec())
            }
        }
        _ => (false, false, words),
    };

    CmdInfo {
        key: key_from_words(&words),
        primary: words.join(" "),
        rtk_prefixed,
        rtk_proxy,
        rtk_disabled: disabled_any,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: &str) -> String {
        classify(c).key
    }

    #[test]
    fn basic_keys() {
        assert_eq!(key("git status"), "git status");
        assert_eq!(key("git -C /tmp status"), "git");
        assert_eq!(key("cargo test --all 2>&1 | tail -20"), "cargo test");
        assert_eq!(key("npm run build"), "npm run build");
        assert_eq!(key("python -m pytest -x tests/"), "python -m pytest");
        assert_eq!(key("./scripts/ci.sh --fast"), "./scripts/ci.sh");
        assert_eq!(key("/usr/bin/make check"), "make check");
        assert_eq!(key("ls -la"), "ls");
        assert_eq!(key("docker compose up -d"), "docker compose up");
    }

    #[test]
    fn skips_setup_segments_and_prefixes() {
        assert_eq!(key("cd /repo && make test"), "make test");
        assert_eq!(key("FOO=\"a b\" timeout 60 cargo build"), "cargo build");
        assert_eq!(key("export X=1; set -e; pytest -q"), "pytest");
        assert_eq!(key("for r in a b; do echo $r; done"), "for");
    }

    #[test]
    fn quotes_do_not_split() {
        assert_eq!(key("git commit -m \"a && b\""), "git commit");
        assert_eq!(key("echo 'x | y'"), "echo");
    }

    #[test]
    fn detects_rtk_forms() {
        let c = classify("rtk git status");
        assert!(c.rtk_prefixed && !c.rtk_proxy);
        assert_eq!(c.key, "git status");
        let c = classify("rtk proxy cargo test");
        assert!(c.rtk_proxy);
        assert_eq!(c.key, "cargo test");
        let c = classify("RTK_DISABLED=1 cargo test");
        assert!(c.rtk_disabled && !c.rtk_prefixed);
        assert_eq!(c.key, "cargo test");
    }

    #[test]
    fn redirects_are_not_separators() {
        assert_eq!(key("make build 2>&1 > out.log"), "make build");
        assert_eq!(key("make build &> out.log"), "make build");
    }
}
