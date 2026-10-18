//! Claude Code as the source of skills and MCP servers for every codexp profile.
//!
//! Paths follow Claude's own rule: `$CLAUDE_CONFIG_DIR` (which also holds `.claude.json`) when
//! set, otherwise `~/.claude` and `~/.claude.json`. Only plugins enabled in Claude's
//! `settings.json` contribute.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

fn user_home() -> PathBuf {
    crate::keychain::real_user_home()
        .ok()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn custom_config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
}

pub fn config_dir() -> PathBuf {
    custom_config_dir().unwrap_or_else(|| user_home().join(".claude"))
}

pub fn config_file() -> PathBuf {
    custom_config_dir()
        .map(|d| d.join(".claude.json"))
        .unwrap_or_else(|| user_home().join(".claude.json"))
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// `(plugin name, install path)` of every plugin enabled in Claude's settings.
fn enabled_plugins(dir: &Path) -> Vec<(String, PathBuf)> {
    let enabled = read_json(&dir.join("settings.json"))
        .and_then(|s| s.get("enabledPlugins").and_then(Value::as_object).cloned());
    let installed = read_json(&dir.join("plugins/installed_plugins.json"));
    let (Some(enabled), Some(installed)) = (enabled, installed) else {
        return Vec::new();
    };
    enabled
        .iter()
        .filter(|(_, on)| on.as_bool() == Some(true))
        .filter_map(|(id, _)| {
            let path = installed
                .get("plugins")?
                .get(id)?
                .as_array()?
                .first()?
                .get("installPath")?
                .as_str()?;
            let name = id.split('@').next().unwrap_or(id).to_string();
            Some((name, PathBuf::from(path)))
        })
        .collect()
}

/// Skill directories (holding `SKILL.md`) under `root`, at most `depth` levels down.
fn find_skills(root: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    paths.sort();
    for path in paths {
        if path.join("SKILL.md").is_file() {
            out.push(path);
        } else if depth > 1 {
            find_skills(&path, depth - 1, out);
        }
    }
}

/// Claude's own skills and those of its enabled plugins, keyed by the link name codexp gives
/// them in a profile. Skills that resolve into `~/.agents/skills`, or share a name with one there,
/// are left out: Codex reads that directory itself, so linking them would list them twice.
pub fn skills() -> BTreeMap<String, PathBuf> {
    let dir = config_dir();
    let agents_skills = fs::canonicalize(user_home().join(".agents/skills")).ok();
    let agents_names: HashSet<String> = agents_skills
        .iter()
        .flat_map(|d| fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    let mut found: Vec<(Option<String>, PathBuf)> = Vec::new();
    let mut own = Vec::new();
    find_skills(&dir.join("skills"), 3, &mut own);
    found.extend(own.into_iter().map(|p| (None, p)));
    for (plugin, root) in enabled_plugins(&dir) {
        let mut theirs = Vec::new();
        find_skills(&root.join("skills"), 1, &mut theirs);
        found.extend(theirs.into_iter().map(|p| (Some(plugin.clone()), p)));
    }

    let mut out = BTreeMap::new();
    let mut seen = HashSet::new();
    for (plugin, path) in found {
        let Ok(real) = fs::canonicalize(&path) else {
            continue;
        };
        if agents_skills.as_ref().is_some_and(|a| real.starts_with(a)) || !seen.insert(real.clone())
        {
            continue;
        }
        let base = path.file_name().unwrap().to_string_lossy().to_string();
        if agents_names.contains(&base) {
            continue;
        }
        let name = match plugin {
            Some(p) if out.contains_key(&base) => format!("{p}-{base}"),
            _ => base,
        };
        out.entry(name).or_insert(real);
    }
    out
}

/// Expands Claude's `${VAR}` and `${VAR:-default}` placeholders; `extra` wins over the process
/// environment. Unknown variables without a default expand to an empty string, as in Claude.
fn expand(s: &str, extra: &[(&str, String)]) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        let Some(len) = rest[start..].find('}') else {
            break;
        };
        out.push_str(&rest[..start]);
        let inner = &rest[start + 2..start + len];
        let (var, default) = match inner.split_once(":-") {
            Some((v, d)) => (v, Some(d)),
            None => (inner, None),
        };
        let value = extra
            .iter()
            .find(|(k, _)| *k == var)
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var(var).ok())
            .or_else(|| default.map(str::to_string))
            .unwrap_or_default();
        out.push_str(&value);
        rest = &rest[start + len + 1..];
    }
    out.push_str(rest);
    out
}

/// A JSON string literal is a valid TOML basic string (serde_json escapes only `"`, `\` and
/// control characters).
fn toml_str(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

fn str_map(v: Option<&Value>) -> Vec<(String, String)> {
    v.and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Codex `-c` overrides and process environment that make Claude's MCP servers available.
#[derive(Debug, Default)]
pub struct McpOverrides {
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub warnings: Vec<String>,
}

impl McpOverrides {
    /// Secrets travel through the environment (never `ps`-visible arguments). A name another
    /// server already set to a different value is inlined instead.
    fn pass_env(&mut self, key: &str, value: String) -> bool {
        match self.env.iter().find(|(k, _)| k == key) {
            Some((_, existing)) => existing == &value,
            None => {
                self.env.push((key.to_string(), value));
                true
            }
        }
    }

    /// Translates one Claude server definition into a Codex `mcp_servers.<name>` table.
    fn add(&mut self, name: &str, server: &Value, extra: &[(&str, String)]) {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            self.warnings
                .push(format!("MCP server '{name}' skipped: unsupported name"));
            return;
        }
        let x = |s: &str| expand(s, extra);
        let kind = server.get("type").and_then(Value::as_str);
        let url = server.get("url").and_then(Value::as_str);
        let command = server.get("command").and_then(Value::as_str);
        let mut fields = Vec::new();
        match (kind, command, url) {
            (None | Some("stdio"), Some(command), _) => {
                fields.push(format!("command = {}", toml_str(&x(command))));
                let args: Vec<String> = server
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(|s| toml_str(&x(s)))
                            .collect()
                    })
                    .unwrap_or_default();
                fields.push(format!("args = [{}]", args.join(", ")));
                let (mut forwarded, mut inline) = (Vec::new(), Vec::new());
                for (k, v) in str_map(server.get("env")) {
                    let v = x(&v);
                    if self.pass_env(&k, v.clone()) {
                        forwarded.push(toml_str(&k));
                    } else {
                        inline.push(format!("{} = {}", toml_str(&k), toml_str(&v)));
                    }
                }
                if !forwarded.is_empty() {
                    fields.push(format!("env_vars = [{}]", forwarded.join(", ")));
                }
                if !inline.is_empty() {
                    fields.push(format!("env = {{ {} }}", inline.join(", ")));
                }
            }
            (None | Some("http"), _, Some(url)) => {
                fields.push(format!("url = {}", toml_str(&x(url))));
                let headers: Vec<String> = str_map(server.get("headers"))
                    .into_iter()
                    .enumerate()
                    .map(|(i, (k, v))| {
                        let var = format!(
                            "CODEXP_MCP_SECRET_{}_{i}",
                            name.to_ascii_uppercase().replace('-', "_")
                        );
                        self.env.push((var.clone(), x(&v)));
                        format!("{} = {}", toml_str(&k), toml_str(&var))
                    })
                    .collect();
                if !headers.is_empty() {
                    fields.push(format!("env_http_headers = {{ {} }}", headers.join(", ")));
                }
            }
            _ => {
                self.warnings.push(format!(
                    "MCP server '{name}' skipped: type '{}' is not supported by Codex",
                    kind.unwrap_or("unknown")
                ));
                return;
            }
        }
        self.args.push("-c".to_string());
        self.args
            .push(format!("mcp_servers.{name}={{ {} }}", fields.join(", ")));
    }
}

/// Claude's MCP servers for a session in `cwd`, with Claude's precedence: enabled plugins, then
/// user scope (`mcpServers` of `.claude.json`), then local scope (`projects.<cwd>.mcpServers`).
pub fn mcp_overrides(cwd: &Path) -> McpOverrides {
    let mut servers: BTreeMap<String, (Value, Option<PathBuf>)> = BTreeMap::new();
    for (_, root) in enabled_plugins(&config_dir()) {
        let Some(mcp) = read_json(&root.join(".mcp.json")) else {
            continue;
        };
        let list = mcp.get("mcpServers").cloned().unwrap_or(mcp);
        for (name, server) in list.as_object().into_iter().flatten() {
            servers.insert(name.clone(), (server.clone(), Some(root.clone())));
        }
    }
    if let Some(config) = read_json(&config_file()) {
        let cwd_keys = [
            cwd.display().to_string(),
            fs::canonicalize(cwd)
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        ];
        let local = cwd_keys.iter().find_map(|k| {
            config
                .get("projects")?
                .get(k)?
                .get("mcpServers")?
                .as_object()
        });
        let user = config.get("mcpServers").and_then(Value::as_object);
        for (name, server) in user.into_iter().chain(local).flatten() {
            servers.insert(name.clone(), (server.clone(), None));
        }
    }

    let mut out = McpOverrides::default();
    for (name, (server, plugin_root)) in &servers {
        let extra: Vec<(&str, String)> = plugin_root
            .iter()
            .map(|r| ("CLAUDE_PLUGIN_ROOT", r.display().to_string()))
            .collect();
        out.add(name, server, &extra);
    }
    out
}
