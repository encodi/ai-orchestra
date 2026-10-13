//! A small `robots.txt` reader (RFC 9309 subset): user-agent groups, `Allow`/`Disallow` with the
//! longest matching rule winning, `*` wildcards and the `$` end anchor.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Group {
    agents: Vec<String>,
    /// (allow, pattern)
    rules: Vec<(bool, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Robots {
    groups: Vec<Group>,
    /// Everything is disallowed (the file could not be read reliably).
    pub disallow_all: bool,
}

impl Robots {
    /// No restrictions (no robots.txt, or a client error such as 404).
    pub fn allow_all() -> Self {
        Self::default()
    }

    /// Everything is off limits (server errors: RFC 9309 says to assume this).
    pub fn deny_all() -> Self {
        Self {
            groups: Vec::new(),
            disallow_all: true,
        }
    }

    pub fn parse(text: &str) -> Self {
        let mut groups: Vec<Group> = Vec::new();
        let mut current: Option<Group> = None;
        let mut collecting_agents = false;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
            match key.as_str() {
                "user-agent" => {
                    if !collecting_agents {
                        if let Some(g) = current.take() {
                            groups.push(g);
                        }
                        current = Some(Group::default());
                    }
                    collecting_agents = true;
                    if let Some(g) = current.as_mut() {
                        g.agents.push(value.to_ascii_lowercase());
                    }
                }
                "allow" | "disallow" => {
                    collecting_agents = false;
                    if let Some(g) = current.as_mut() {
                        // An empty Disallow means "allow everything" and adds no restriction.
                        if !value.is_empty() {
                            g.rules.push((key == "allow", value.to_string()));
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(g) = current {
            groups.push(g);
        }
        Self {
            groups,
            disallow_all: false,
        }
    }

    /// May `agent` (a product token such as `ai-orchestra`) fetch `path` (with query)?
    pub fn allowed(&self, agent: &str, path: &str) -> bool {
        if self.disallow_all {
            return false;
        }
        let agent = agent.to_ascii_lowercase();
        // The group naming this agent most specifically wins; `*` is the fallback.
        let specific = self
            .groups
            .iter()
            .filter(|g| {
                g.agents
                    .iter()
                    .any(|a| a != "*" && !a.is_empty() && agent.contains(a.as_str()))
            })
            .collect::<Vec<_>>();
        let chosen: Vec<&Group> = if specific.is_empty() {
            self.groups
                .iter()
                .filter(|g| g.agents.iter().any(|a| a == "*"))
                .collect()
        } else {
            specific
        };
        let mut best: Option<(usize, bool)> = None;
        for group in chosen {
            for (allow, pattern) in &group.rules {
                if matches(pattern, path) {
                    let len = pattern.len();
                    best = match best {
                        Some((l, a)) if l > len || (l == len && a) => Some((l, a)),
                        _ => Some((len, *allow)),
                    };
                }
            }
        }
        best.is_none_or(|(_, allow)| allow)
    }
}

/// Prefix match with `*` wildcards and an optional trailing `$`.
fn matches(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(p) => (p, true),
        None => (pattern, false),
    };
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut rest = path;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(part) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if let Some(pos) = rest.find(part) {
            rest = &rest[pos + part.len()..];
        } else {
            return false;
        }
    }
    !anchored || rest.is_empty() || pattern.ends_with('*')
}

#[cfg(test)]
mod tests {
    use super::*;

    const UA: &str = "ai-orchestra/0.5";

    #[test]
    fn disallow_rules_apply_by_prefix() {
        let r = Robots::parse("User-agent: *\nDisallow: /private/\nDisallow: /tmp\n");
        assert!(!r.allowed(UA, "/private/data.html"));
        assert!(!r.allowed(UA, "/tmp"));
        assert!(!r.allowed(UA, "/tmpfile"));
        assert!(r.allowed(UA, "/public/page.html"));
        assert!(r.allowed(UA, "/"));
    }

    #[test]
    fn the_longest_matching_rule_wins_and_allow_wins_ties() {
        let r = Robots::parse("User-agent: *\nDisallow: /docs/\nAllow: /docs/public/\n");
        assert!(r.allowed(UA, "/docs/public/a.html"));
        assert!(!r.allowed(UA, "/docs/secret.html"));
        let tie = Robots::parse("User-agent: *\nDisallow: /x\nAllow: /x\n");
        assert!(tie.allowed(UA, "/x"));
    }

    #[test]
    fn a_group_for_this_agent_beats_the_wildcard_group() {
        let r = Robots::parse(
            "User-agent: *\nDisallow: /\n\nUser-agent: ai-orchestra\nDisallow: /nope\n",
        );
        assert!(r.allowed(UA, "/anything"));
        assert!(!r.allowed(UA, "/nope"));
        assert!(!r.allowed("otherbot/1", "/anything"));
    }

    #[test]
    fn wildcards_anchors_comments_and_empty_disallow() {
        let r = Robots::parse(
            "# comment\nUser-agent: *  # all\nDisallow: /*.pdf$\nDisallow: /a*/c\nDisallow:\n",
        );
        assert!(!r.allowed(UA, "/files/x.pdf"));
        assert!(
            r.allowed(UA, "/files/x.pdf?download=1"),
            "$ anchors the end"
        );
        assert!(!r.allowed(UA, "/abc/c"));
        assert!(r.allowed(UA, "/abc/d"));
    }

    #[test]
    fn missing_and_unreadable_files_follow_the_rfc() {
        assert!(Robots::allow_all().allowed(UA, "/x"));
        assert!(!Robots::deny_all().allowed(UA, "/x"));
        assert!(Robots::parse("").allowed(UA, "/x"));
        assert!(Robots::parse("garbage without colons").allowed(UA, "/x"));
    }

    #[test]
    fn consecutive_user_agent_lines_share_one_group() {
        let r = Robots::parse("User-agent: botA\nUser-agent: ai-orchestra\nDisallow: /shared\n");
        assert!(!r.allowed(UA, "/shared"));
        assert!(!r.allowed("botA", "/shared"));
    }
}
