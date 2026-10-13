//! `ai-orchestra doctor`: reports what is installed and logged in, using only
//! local, non-AI commands (`--version`, `auth status`, ...). It never sends a
//! prompt, so it cannot consume subscription quota.

use std::fmt::Write as _;

use serde::Serialize;

use crate::provider::{AgentProvider, AuthState, InvokeContext};
use crate::providers::{run_probe, snippet};

#[derive(Debug, Clone, Serialize)]
pub struct ProviderStatus {
    pub id: String,
    pub display_name: String,
    pub enabled: bool,
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub auth: Option<AuthState>,
    /// Model / effort set in ai-orchestra's config; `None` means the CLI's own
    /// default applies (which this tool does not read).
    pub model: Option<String>,
    pub effort: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub providers: Vec<ProviderStatus>,
    pub warnings: Vec<String>,
}

impl DoctorReport {
    /// True when at least one enabled provider is installed.
    pub fn any_usable(&self) -> bool {
        self.providers.iter().any(|p| p.enabled && p.installed)
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for p in &self.providers {
            let _ = writeln!(out, "{} ({})", p.display_name, p.id);
            if !p.enabled {
                let _ = writeln!(out, "  status:  disabled in config");
                continue;
            }
            if !p.installed {
                let _ = writeln!(out, "  status:  NOT INSTALLED");
            } else {
                let _ = writeln!(out, "  status:  installed");
                if let Some(path) = &p.path {
                    let _ = writeln!(out, "  path:    {path}");
                }
                let _ = writeln!(
                    out,
                    "  version: {}",
                    p.version.as_deref().unwrap_or("unknown")
                );
                let _ = writeln!(out, "  auth:    {}", describe_auth(p.auth.as_ref()));
                let _ = writeln!(out, "  model:   {}", describe_settings(p));
            }
            for w in &p.warnings {
                let _ = writeln!(out, "  warning: {w}");
            }
        }
        for w in &self.warnings {
            let _ = writeln!(out, "warning: {w}");
        }
        out
    }
}

fn describe_settings(p: &ProviderStatus) -> String {
    match (&p.model, &p.effort) {
        (None, None) => "CLI default (model and effort come from the CLI's own config)".to_string(),
        (m, e) => format!(
            "{} · effort {} (from ai-orchestra config)",
            m.as_deref().unwrap_or("CLI default"),
            e.as_deref().unwrap_or("CLI default")
        ),
    }
}

fn describe_auth(auth: Option<&AuthState>) -> String {
    match auth {
        None => "not checked".to_string(),
        Some(AuthState::LoggedIn { method, .. }) => format!("logged in via {method}"),
        Some(AuthState::NotLoggedIn { hint }) => format!("NOT logged in ({hint})"),
        Some(AuthState::Unknown { reason }) => format!("unknown ({reason})"),
    }
}

pub async fn check_provider(provider: &dyn AgentProvider, ctx: &InvokeContext) -> ProviderStatus {
    let mut status = ProviderStatus {
        id: provider.id().to_string(),
        display_name: provider.display_name().to_string(),
        enabled: provider.config().enabled,
        installed: false,
        path: None,
        version: None,
        auth: None,
        model: provider.config().model.clone(),
        effort: provider.config().effort.clone(),
        warnings: Vec::new(),
    };
    if !status.enabled {
        return status;
    }

    let path = match provider.resolve_executable() {
        Ok(p) => p,
        Err(_) => {
            status.warnings.push(format!(
                "`{}` not found on PATH; install it or set providers.{}.executable",
                provider.executable_name(),
                provider.id()
            ));
            return status;
        }
    };
    status.installed = true;
    status.path = Some(path.display().to_string());

    let version_args = provider.version_args();
    let version_args: Vec<&str> = version_args.iter().map(String::as_str).collect();
    status.version = run_probe(ctx, &path, &version_args)
        .await
        .filter(|o| o.success)
        .and_then(|o| {
            let text = if o.stdout.trim().is_empty() {
                o.stderr
            } else {
                o.stdout
            };
            text.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(|l| snippet(l, 100))
        });
    if status.version.is_none() {
        status
            .warnings
            .push("could not read the version; the installation may be broken".to_string());
    }

    let auth = provider.auth_status(ctx, &path).await;
    match &auth {
        AuthState::LoggedIn {
            uses_api_key: true, ..
        } => status.warnings.push(
            "logged in with an API key, which is usage-billed; ai-orchestra expects a \
             subscription login (re-run the CLI's login flow)"
                .to_string(),
        ),
        AuthState::NotLoggedIn { .. } => status
            .warnings
            .push("sign in with the official CLI before using this provider".to_string()),
        _ => {}
    }
    status.auth = Some(auth);

    if !ctx.allow_nested
        && let Some(marker) = provider
            .host_env_markers()
            .iter()
            .find(|m| ctx.host_env.contains(**m))
    {
        status.warnings.push(format!(
            "this shell is inside a {} session (`{marker}` is set); `ask` will refuse to \
             call it recursively. Use a plain terminal",
            provider.display_name()
        ));
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(id: &str) -> ProviderStatus {
        ProviderStatus {
            id: id.into(),
            display_name: id.to_uppercase(),
            enabled: true,
            installed: true,
            path: Some(format!("/bin/{id}")),
            version: Some("1.0".into()),
            auth: Some(AuthState::LoggedIn {
                method: "sub".into(),
                uses_api_key: false,
            }),
            model: None,
            effort: None,
            warnings: vec![],
        }
    }

    #[test]
    fn renders_installed_missing_and_disabled() {
        let mut missing = status("b");
        missing.installed = false;
        missing.path = None;
        missing.warnings.push("install it".into());
        let mut disabled = status("c");
        disabled.enabled = false;
        let report = DoctorReport {
            providers: vec![status("a"), missing, disabled],
            warnings: vec!["global".into()],
        };
        let text = report.render();
        assert!(text.contains("logged in via sub"));
        assert!(text.contains("NOT INSTALLED"));
        assert!(text.contains("disabled in config"));
        assert!(text.contains("warning: install it"));
        assert!(text.contains("warning: global"));
        assert!(report.any_usable());
    }

    #[test]
    fn no_usable_provider_is_detected() {
        let mut s = status("a");
        s.installed = false;
        let report = DoctorReport {
            providers: vec![s],
            warnings: vec![],
        };
        assert!(!report.any_usable());
    }

    impl ProviderStatus {
        fn render_check(&self) -> String {
            DoctorReport {
                providers: vec![self.clone()],
                warnings: vec![],
            }
            .render()
        }
    }

    #[test]
    fn settings_line_distinguishes_cli_defaults_from_configured_values() {
        let mut s = status("a");
        assert!(
            s.render_check()
                .contains("CLI default (model and effort come from")
        );
        s.model = Some("sonnet".into());
        s.effort = Some("high".into());
        assert!(
            s.render_check()
                .contains("sonnet · effort high (from ai-orchestra config)")
        );
    }
}
