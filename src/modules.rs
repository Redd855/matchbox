use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// A resolved, validated module ready for use in compilation.
#[derive(Debug, Clone)]
pub struct ModuleInfo {
    /// The module name (e.g. `"bx-strings"`). Becomes the `bxModules.{name}` namespace.
    pub name: String,
    pub mapping: String,
    pub dependencies: Vec<String>,
    /// Absolute physical path to the module root directory.
    pub path: PathBuf,
    /// Contents of each `bifs/*.bxs` (or `.bx`) file in this module, injected as extra
    /// prelude sources during tree-shaking.
    pub bif_sources: Vec<String>,
    /// Whether this module contains a `matchbox/Cargo.toml` for native Rust compilation.
    pub has_native: bool,
    /// BoxLang class BIFs, registered using their invoke() method at runtime.
    pub bif_classes: Vec<(String, String)>,
    /// Application settings overrides; configure() runs only in the application VM.
    pub settings: serde_json::Value,
}

// ─────────────────────────────────────────────────────────────────────────────
// matchbox.toml deserialization
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    modules: HashMap<String, ManifestEntry>,
    #[allow(dead_code)]
    #[serde(default)]
    datasources: HashMap<String, DatasourceEntry>,
}

#[derive(Debug, Deserialize)]
struct ManifestEntry {
    path: String,
}

#[derive(Debug, Deserialize)]
struct BoxJson {
    #[serde(default)]
    dependencies: HashMap<String, String>,
    #[serde(default)]
    #[serde(rename = "devDependencies")]
    dev_dependencies: HashMap<String, String>,
}

/// A datasource configuration entry from `matchbox.toml`.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct DatasourceEntry {
    pub driver: String,
    #[serde(default = "default_ds_host")]
    pub host: String,
    #[serde(default = "default_ds_port")]
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: String,
    #[serde(rename = "maxConnections", default = "default_max_connections")]
    pub max_connections: u32,
}

fn default_ds_host() -> String {
    "localhost".to_string()
}
fn default_ds_port() -> u16 {
    5432
}
fn default_max_connections() -> u32 {
    10
}

// ─────────────────────────────────────────────────────────────────────────────
// Public API
// ─────────────────────────────────────────────────────────────────────────────

/// Discover all modules for the current compilation.
///
/// Sources (in priority order — CLI `--module` flags override manifest entries with the same
/// directory name):
/// 1. `box.json` dependencies (following CommandBox conventions)
/// 2. `matchbox.toml` in `project_dir`
/// 3. `modules/` and `boxlang_modules/` directory scanning
/// 4. `extra_module_paths` collected from `--module <path>` CLI flags
///
/// Returns an empty `Vec` when neither source provides any modules, so callers can always
/// call this unconditionally.
pub fn discover_modules(
    project_dir: &Path,
    extra_module_paths: &[PathBuf],
) -> Result<Vec<ModuleInfo>> {
    // Collect (name, raw_path) pairs.  Later entries for the same name replace earlier ones.
    let mut entries: Vec<(String, PathBuf)> = Vec::new();

    // 1. Read box.json if present (CommandBox convention)
    let box_json_path = project_dir.join("box.json");
    if box_json_path.exists() {
        let text = std::fs::read_to_string(&box_json_path)
            .with_context(|| format!("Failed to read {}", box_json_path.display()))?;
        let box_json: BoxJson = serde_json::from_str(&text)
            .with_context(|| format!("Failed to parse {}", box_json_path.display()))?;

        // Merge dependencies and devDependencies
        let mut all_deps = box_json.dependencies;
        all_deps.extend(box_json.dev_dependencies);

        for (name, value) in all_deps {
            // If value looks like a relative path (starts with . or contains /)
            if value.starts_with('.') || value.contains('/') || value.contains('\\') {
                let raw = Path::new(&value);
                let path = if raw.is_absolute() {
                    raw.to_path_buf()
                } else {
                    project_dir.join(raw)
                };
                if path.exists() {
                    entries.push((name, path));
                    continue;
                }
            }

            // Otherwise, look in modules/ or boxlang_modules/
            let mod_path = project_dir.join("modules").join(&name);
            if mod_path.exists() {
                entries.push((name, mod_path));
            } else {
                let bx_mod_path = project_dir.join("boxlang_modules").join(&name);
                if bx_mod_path.exists() {
                    entries.push((name, bx_mod_path));
                }
            }
        }
    }

    // 2. Read matchbox.toml if present.
    let manifest_path = project_dir.join("matchbox.toml");
    if manifest_path.exists() {
        let text = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
        let manifest: Manifest = toml::from_str(&text)
            .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;
        for (name, entry) in manifest.modules {
            let raw = Path::new(&entry.path);
            let path = if raw.is_absolute() {
                raw.to_path_buf()
            } else {
                project_dir.join(raw)
            };
            entries.retain(|(n, _)| n != &name);
            entries.push((name, path));
        }
    }

    // 3. Scan modules/ and boxlang_modules/ for any folders with ModuleConfig.bx not already added
    for dir_name in &["modules", "boxlang_modules"] {
        let dir_path = project_dir.join(dir_name);
        if dir_path.is_dir() {
            if let Ok(dir_entries) = std::fs::read_dir(dir_path) {
                for entry in dir_entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() && path.join("ModuleConfig.bx").exists() {
                        let name = path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        if !entries.iter().any(|(n, _)| n == &name) {
                            entries.push((name, path));
                        }
                    }
                }
            }
        }
    }

    // 4. --module CLI overrides: derive name from the directory name, replace manifest entry
    //    with the same name so CLI always wins.
    for raw in extra_module_paths {
        let path = if raw.is_absolute() {
            raw.clone()
        } else {
            project_dir.join(raw)
        };
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();
        entries.retain(|(n, _)| n != &name);
        entries.push((name, path));
    }

    // 3. Validate and resolve each entry.
    let mut modules = Vec::new();
    for (mut name, path) in entries {
        let path = path.canonicalize().with_context(|| {
            format!("Module '{}': path does not exist: {}", name, path.display())
        })?;

        let descriptor = path.join("ModuleConfig.bx");
        if !descriptor.exists() {
            bail!(
                "Module '{}' at '{}' is missing ModuleConfig.bx",
                name,
                path.display()
            );
        }

        let package_path = path.join("box.json");
        let package: serde_json::Value = if package_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(package_path)?)?
        } else {
            serde_json::json!({})
        };
        if let Some(declared) = package["boxlang"]["moduleName"].as_str() {
            name = declared.to_owned();
        }
        let source = std::fs::read_to_string(&descriptor)?;
        let metadata = descriptor_metadata(&source)?;
        if !metadata.enabled {
            continue;
        }
        let mapping = metadata.mapping.unwrap_or_else(|| name.clone());

        // Collect bifs/*.bxs sources (sorted for determinism).
        let bifs_dir = path.join("bifs");
        let mut bif_sources = Vec::new();
        let mut bif_classes = Vec::new();
        if bifs_dir.is_dir() {
            let mut bif_files: Vec<_> = std::fs::read_dir(&bifs_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| {
                    let entry_path = e.path();
                    let ext = entry_path.extension().and_then(|x| x.to_str());
                    matches!(ext, Some("bxs") | Some("bx"))
                })
                .collect();
            bif_files.sort_by_key(|e| e.path());
            for entry in bif_files {
                let src = std::fs::read_to_string(entry.path()).with_context(|| {
                    format!(
                        "Module '{}': failed to read {}",
                        name,
                        entry.path().display()
                    )
                })?;
                let file = entry.path();
                let ast = matchbox_compiler::parser::parse(&src, file.to_str())?;
                if ast.iter().any(|s| {
                    matches!(
                        s.kind,
                        matchbox_compiler::ast::StatementKind::ClassDecl { .. }
                    )
                }) {
                    bif_classes.push((
                        file.file_stem().unwrap().to_string_lossy().into_owned(),
                        src,
                    ));
                } else {
                    bif_sources.push(src);
                }
            }
        }

        // Published artifacts take precedence; never silently compile a mismatched target.
        let has_artifacts = package["matchbox"]["artifacts"]
            .as_object()
            .is_some_and(|a| !a.is_empty());
        let has_native = !has_artifacts && path.join("matchbox").join("Cargo.toml").exists();

        let config_path = project_dir.join("boxlang.json");
        let config: serde_json::Value = if config_path.exists() {
            serde_json::from_str(&std::fs::read_to_string(config_path)?)?
        } else {
            serde_json::json!({})
        };
        if config["modules"][&name]["enabled"] == false {
            continue;
        }
        let settings = config["modules"][&name]["settings"]
            .as_object()
            .map(|m| serde_json::Value::Object(m.clone()))
            .unwrap_or_else(|| serde_json::json!({}));

        modules.push(ModuleInfo {
            name,
            mapping,
            dependencies: metadata.dependencies,
            path,
            bif_sources,
            bif_classes,
            has_native,
            settings,
        });
    }

    order_modules(modules)
}

/// Read `[datasources.<name>]` sections from `matchbox.toml` in `project_dir`.
///
/// Returns a map of `datasource_name → DatasourceEntry`. Returns an empty map
/// when no matchbox.toml is found or when the file contains no `[datasources]`
/// section.
#[allow(dead_code)]
pub fn read_datasource_configs(project_dir: &Path) -> Result<HashMap<String, DatasourceEntry>> {
    let manifest_path = project_dir.join("matchbox.toml");
    if !manifest_path.exists() {
        return Ok(HashMap::new());
    }
    let text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest: Manifest = toml::from_str(&text)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;
    Ok(manifest.datasources)
}

struct DescriptorMetadata {
    enabled: bool,
    mapping: Option<String>,
    dependencies: Vec<String>,
}
fn descriptor_metadata(source: &str) -> Result<DescriptorMetadata> {
    use matchbox_compiler::ast::*;
    fn string(expression: &Expression) -> Option<String> {
        match &expression.kind {
            ExpressionKind::Literal(Literal::String(parts))
                if parts.iter().all(|p| matches!(p, StringPart::Text(_))) =>
            {
                Some(
                    parts
                        .iter()
                        .filter_map(|p| {
                            if let StringPart::Text(s) = p {
                                Some(s.as_str())
                            } else {
                                None
                            }
                        })
                        .collect(),
                )
            }
            _ => None,
        }
    }
    let mut result = DescriptorMetadata {
        enabled: true,
        mapping: None,
        dependencies: vec![],
    };
    for statement in matchbox_compiler::parser::parse(source, Some("ModuleConfig.bx"))? {
        let StatementKind::ClassDecl { members, .. } = statement.kind else {
            continue;
        };
        for member in members {
            let ClassMember::Statement(Statement {
                kind: StatementKind::Expression(expression),
                ..
            }) = member
            else {
                continue;
            };
            let ExpressionKind::Assignment {
                target: AssignmentTarget::Member { base, member },
                value,
            } = expression.kind
            else {
                continue;
            };
            if !matches!(base.kind, ExpressionKind::Identifier(ref name) if name.eq_ignore_ascii_case("this"))
            {
                continue;
            }
            match member.to_lowercase().as_str() {
                "enabled" => {
                    let ExpressionKind::Literal(Literal::Boolean(enabled)) = value.kind else {
                        bail!("Module enabled metadata must be a boolean literal");
                    };
                    result.enabled = enabled;
                }
                "mapping" => {
                    result.mapping =
                        Some(string(&value).context("Module mapping must be a string literal")?)
                }
                "dependencies" => {
                    let ExpressionKind::Literal(Literal::Array(values)) = value.kind else {
                        bail!("Module dependencies must be an array literal");
                    };
                    result.dependencies = values
                        .iter()
                        .map(|v| string(v).context("Module dependencies must contain names"))
                        .collect::<Result<_>>()?;
                }
                _ => (),
            }
        }
    }
    Ok(result)
}
fn order_modules(modules: Vec<ModuleInfo>) -> Result<Vec<ModuleInfo>> {
    let mut names = HashMap::new();
    for (index, module) in modules.iter().enumerate() {
        if names.insert(module.name.to_lowercase(), index).is_some() {
            bail!("Duplicate module name: {}", module.name);
        }
    }
    fn visit(
        index: usize,
        modules: &[ModuleInfo],
        names: &HashMap<String, usize>,
        states: &mut [u8],
        order: &mut Vec<usize>,
    ) -> Result<()> {
        if states[index] == 2 {
            return Ok(());
        }
        if states[index] == 1 {
            bail!(
                "Circular module dependency involving {}",
                modules[index].name
            );
        }
        states[index] = 1;
        for dependency in &modules[index].dependencies {
            let next = *names.get(&dependency.to_lowercase()).with_context(|| {
                format!(
                    "Module {} requires missing module {dependency}",
                    modules[index].name
                )
            })?;
            visit(next, modules, names, states, order)?;
        }
        states[index] = 2;
        order.push(index);
        Ok(())
    }
    let mut states = vec![0; modules.len()];
    let mut order = Vec::new();
    for index in 0..modules.len() {
        visit(index, &modules, &names, &mut states, &mut order)?;
    }
    Ok(order
        .into_iter()
        .map(|index| modules[index].clone())
        .collect())
}

pub fn mappings(modules: &[ModuleInfo]) -> Vec<(String, PathBuf)> {
    modules
        .iter()
        .flat_map(|module| {
            [
                (module.name.clone(), module.path.clone()),
                (module.mapping.clone(), module.path.clone()),
            ]
        })
        .collect()
}
