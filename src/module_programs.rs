//! Compile module descriptors without executing them; resolve prebuilt native artifacts.
use crate::modules::ModuleInfo;
use anyhow::{Context, Result, bail};
use matchbox_compiler::{ast::*, parser};
use matchbox_vm::{
    Chunk,
    bundle::{Artifact, Bif, BifMember, Module, SharedLibrary, TARGET},
};
use serde::Deserialize;
use std::{collections::HashMap, fs, path::Path};

#[derive(Deserialize)]
struct Package {
    #[serde(default)]
    matchbox: Native,
}
#[derive(Default, Deserialize)]
struct Native {
    #[serde(default)]
    artifacts: HashMap<String, ArtifactLocation>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactLocation {
    path: Option<String>,
    url: Option<String>,
    filename: Option<String>,
    sha256: String,
    abi: u32,
    #[serde(default)]
    libraries: Vec<LibraryLocation>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LibraryLocation {
    path: Option<String>,
    url: Option<String>,
    filename: Option<String>,
    sha256: String,
}

pub fn prepare(modules: &[ModuleInfo], target: &str) -> Result<Vec<Module>> {
    modules
        .iter()
        .enumerate()
        .map(|(index, module)| {
            let source = fs::read_to_string(module.path.join("ModuleConfig.bx"))?;
            let class = format!("__mb_config_{index}");
            let instance = format!("__mb_module_{index}");
            let mut ast = parser::parse(&source, Some("ModuleConfig.bx"))?;
            rename_class(&mut ast, &class)?;
            // Both named and anonymous descriptor classes are supported.
            ast.extend(parser::parse(
                &format!("{instance} = new {class}();"),
                None,
            )?);
            let descriptor = compile(
                &ast,
                &source,
                &module.path.join("ModuleConfig.bx").to_string_lossy(),
                modules,
            )?;
            let mut bifs_ast = Vec::new();
            let mut bif_registrations = Vec::new();
            for (bif_index, (name, source)) in module.bif_classes.iter().enumerate() {
                let class = format!("__mb_bif_{index}_{bif_index}");
                let instance = format!("__mb_bif_instance_{index}_{bif_index}");
                let mut ast = parser::parse(source, Some(name))?;
                let function = format!("__mb_bif_function_{index}_{bif_index}");
                let (aliases, members) = bif_annotations(&ast, name)?;
                bif_registrations.push(Bif {
                    instance: instance.clone(),
                    function: function.clone(),
                    aliases,
                    members,
                });
                rename_class(&mut ast, &class)?;
                let (params, return_type) = ast
                    .iter()
                    .find_map(|s| {
                        let StatementKind::ClassDecl { members, .. } = &s.kind else {
                            return None;
                        };
                        members.iter().find_map(|m| {
                            let ClassMember::Statement(s) = m else {
                                return None;
                            };
                            let StatementKind::FunctionDecl {
                                name,
                                params,
                                return_type,
                                ..
                            } = &s.kind
                            else {
                                return None;
                            };
                            name.eq_ignore_ascii_case("invoke")
                                .then(|| (params.clone(), return_type.clone()))
                        })
                    })
                    .with_context(|| format!("Module {} BIF {name} needs invoke()", module.name))?;
                let call = Expression::new(
                    ExpressionKind::FunctionCall {
                        base: Box::new(Expression::new(
                            ExpressionKind::MemberAccess {
                                base: Box::new(Expression::new(
                                    ExpressionKind::Identifier(instance.clone()),
                                    1,
                                )),
                                member: "invoke".into(),
                            },
                            1,
                        )),
                        args: params
                            .iter()
                            .map(|p| Argument {
                                name: None,
                                value: Expression::new(
                                    ExpressionKind::Identifier(p.name.clone()),
                                    1,
                                ),
                            })
                            .collect(),
                    },
                    1,
                );
                ast.extend(parser::parse(
                    &format!("{instance} = new {class}();"),
                    None,
                )?);
                ast.push(Statement::new(
                    StatementKind::FunctionDecl {
                        name: function,
                        attributes: vec![],
                        modifiers: FunctionModifiers::default(),
                        return_type,
                        params,
                        body: FunctionBody::Block(vec![Statement::new(
                            StatementKind::Return(Some(call)),
                            1,
                        )]),
                    },
                    1,
                ));
                bifs_ast.extend(ast);
            }
            for source in &module.bif_sources {
                bifs_ast.extend(parser::parse(source, None)?);
            }
            let bifs = compile(
                &bifs_ast,
                "",
                &module.path.join("bifs/module-bifs.bx").to_string_lossy(),
                modules,
            )?;
            let artifact = read_artifact(&module.path, target)?;
            Ok(Module {
                name: module.name.clone(),
                path: module.path.to_string_lossy().into_owned(),
                mapping: module.mapping.clone(),
                dependencies: module.dependencies.clone(),
                instance,
                descriptor,
                bifs,
                bif_registrations,
                settings: module.settings.clone(),
                artifact,
            })
        })
        .collect()
}
fn annotation_value(expression: &Expression) -> Result<serde_json::Value> {
    use serde_json::Value;
    Ok(match &expression.kind {
        ExpressionKind::Identifier(name) => Value::String(name.clone()),
        ExpressionKind::Literal(Literal::String(parts)) => Value::String(
            parts
                .iter()
                .map(|part| match part {
                    StringPart::Text(text) => Ok(text.as_str()),
                    _ => bail!("BIF annotations must be literals"),
                })
                .collect::<Result<String>>()?,
        ),
        ExpressionKind::Literal(Literal::Array(values)) => {
            Value::Array(values.iter().map(annotation_value).collect::<Result<_>>()?)
        }
        ExpressionKind::Literal(Literal::Struct(values)) => Value::Object(
            values
                .iter()
                .map(|(key, value)| {
                    let key = annotation_value(key)?;
                    Ok((
                        key.as_str()
                            .context("Annotation keys must be strings")?
                            .to_lowercase(),
                        annotation_value(value)?,
                    ))
                })
                .collect::<Result<_>>()?,
        ),
        ExpressionKind::Literal(Literal::Boolean(value)) => Value::Bool(*value),
        ExpressionKind::Literal(Literal::Number(value)) => serde_json::json!(value),
        ExpressionKind::Literal(Literal::Null) => Value::Null,
        _ => bail!("BIF annotations must be literals"),
    })
}
fn bif_annotations(ast: &[Statement], name: &str) -> Result<(Vec<String>, Vec<BifMember>)> {
    let attributes = ast
        .iter()
        .find_map(|statement| match &statement.kind {
            StatementKind::ClassDecl { attributes, .. } => Some(attributes),
            _ => None,
        })
        .context("BIF needs a class")?;
    let annotation = |key: &str| -> Result<serde_json::Value> {
        let Some(attribute) = attributes
            .iter()
            .rev()
            .find(|a| a.name.eq_ignore_ascii_case(key))
        else {
            return Ok(serde_json::Value::Null);
        };
        if attribute.args.len() > 1 {
            return Ok(serde_json::Value::Array(
                attribute
                    .args
                    .iter()
                    .map(|arg| annotation_value(&arg.value))
                    .collect::<Result<_>>()?,
            ));
        }
        attribute
            .args
            .first()
            .map(|arg| annotation_value(&arg.value))
            .unwrap_or_else(|| Ok(serde_json::Value::String(String::new())))
    };
    let mut aliases = match annotation("boxBif")? {
        serde_json::Value::String(alias) if !alias.trim().is_empty() => vec![alias],
        serde_json::Value::Array(aliases) => {
            let mut result = aliases
                .iter()
                .map(|alias| {
                    alias
                        .as_str()
                        .map(str::to_owned)
                        .context("BoxBIF aliases must be strings")
                })
                .collect::<Result<Vec<_>>>()?;
            result.push(name.into());
            result
        }
        _ => vec![name.into()],
    };
    if aliases.iter().any(|name| name.trim().is_empty()) {
        bail!("BoxBIF aliases cannot be empty");
    }
    let mut seen = std::collections::HashSet::new();
    aliases.retain(|name| seen.insert(name.to_lowercase()));
    let mut members = Vec::new();
    let mut add_member = |type_name: &str, record: &serde_json::Value| -> Result<()> {
        let type_name = type_name.to_lowercase();
        let default_name = name.to_lowercase().replace(&type_name, "");
        let member = record
            .get("name")
            .map(|name| name.as_str().context("BoxMember name must be a string"))
            .transpose()?
            .unwrap_or(&default_name)
            .to_owned();
        let object_argument = record
            .get("objectargument")
            .map(|argument| {
                argument
                    .as_str()
                    .context("BoxMember objectArgument must be a string")
            })
            .transpose()?
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        members.push(BifMember {
            name: member,
            type_name,
            object_argument,
        });
        Ok(())
    };
    match annotation("boxMember")? {
        serde_json::Value::String(type_name) if type_name.trim().is_empty() => {
            bail!("BoxMember requires a type")
        }
        serde_json::Value::String(type_name) => add_member(&type_name, &serde_json::json!({}))?,
        serde_json::Value::Object(types) => {
            for (type_name, record) in types {
                if !record.is_object() {
                    bail!("BoxMember value must be a struct");
                }
                add_member(&type_name, &record)?;
            }
        }
        _ => (),
    }
    Ok((aliases, members))
}
fn rename_class(ast: &mut [Statement], name: &str) -> Result<()> {
    let mut found = false;
    for statement in ast {
        if let StatementKind::ClassDecl { name: old, .. } = &mut statement.kind {
            if found {
                bail!("Module descriptor/BIF must contain one class");
            }
            *old = name.into();
            found = true;
        }
    }
    if !found {
        bail!("Module descriptor/BIF must contain a class");
    }
    Ok(())
}
fn compile(
    ast: &[Statement],
    source: &str,
    filename: &str,
    modules: &[ModuleInfo],
) -> Result<Chunk> {
    matchbox_compiler::compile_with_treeshaking(
        filename,
        ast,
        source,
        vec![],
        false,
        false,
        &crate::modules::mappings(modules),
        &[],
    )
}
fn read_artifact(root: &Path, target: &str) -> Result<Option<Artifact>> {
    let package = root.join("box.json");
    if !package.exists() {
        return Ok(None);
    }
    let package: Package = serde_json::from_str(&fs::read_to_string(package)?)?;
    if package.matchbox.artifacts.is_empty() {
        return Ok(None);
    }
    let location = package
        .matchbox
        .artifacts
        .get(target)
        .with_context(|| format!("No native artifact published for {target}"))?;
    if location.abi != 1 {
        bail!("Unsupported native artifact ABI {}", location.abi);
    }
    let remote_filename = if target.contains("windows") {
        "plugin.dll"
    } else if target.contains("apple") {
        "plugin.dylib"
    } else {
        "plugin.so"
    };
    let filename = location
        .filename
        .as_deref()
        .or_else(|| location.url.as_ref().map(|_| remote_filename));
    let plugin = read_library(
        root,
        location.path.as_deref(),
        location.url.as_deref(),
        filename,
        &location.sha256,
    )?;
    let libraries = location
        .libraries
        .iter()
        .map(|library| {
            read_library(
                root,
                library.path.as_deref(),
                library.url.as_deref(),
                library.filename.as_deref(),
                &library.sha256,
            )
        })
        .collect::<Result<_>>()?;
    let artifact = Artifact {
        target: target.into(),
        abi: location.abi,
        filename: plugin.filename,
        sha256: plugin.sha256,
        bytes: plugin.bytes,
        libraries,
    };
    artifact.validate_files()?;
    Ok(Some(artifact))
}
fn read_library(
    root: &Path,
    path: Option<&str>,
    url: Option<&str>,
    filename: Option<&str>,
    sha256: &str,
) -> Result<SharedLibrary> {
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        bail!("Artifact SHA-256 must be 64 lowercase hexadecimal characters");
    }
    if let Some(filename) = filename {
        matchbox_vm::bundle::validate_filename(filename)?;
    }
    let (filename, bytes) = match (path, url) {
        (Some(path), None) => {
            let packaged = root.join(path);
            // Keep a package symlink's name, not the resolved versioned filename.
            let filename = filename.map(str::to_owned).unwrap_or(
                packaged
                    .file_name()
                    .context("Artifact has no filename")?
                    .to_string_lossy()
                    .into_owned(),
            );
            matchbox_vm::bundle::validate_filename(&filename)?;
            let path = packaged.canonicalize()?;
            if !path.starts_with(root.canonicalize()?) {
                bail!("Native artifact path escapes module package");
            }
            (filename, fs::read(path)?)
        }
        (None, Some(url)) => (
            filename
                .context("Remote dependent libraries require a filename")?
                .to_owned(),
            matchbox_vm::plugins::download(url).map_err(anyhow::Error::msg)?,
        ),
        _ => bail!("Native artifact must specify exactly one of path or url"),
    };
    if sha256 != matchbox_vm::bundle::hash(&bytes) {
        bail!("Published native artifact hash does not match its bytes: {filename}");
    }
    Ok(SharedLibrary {
        filename,
        sha256: sha256.into(),
        bytes,
    })
}
pub fn output_target(target: Option<&str>) -> &str {
    match target {
        None | Some("native") => TARGET,
        Some(target) => target,
    }
}
