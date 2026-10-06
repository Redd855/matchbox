//! Compile module descriptors without executing them; resolve prebuilt native artifacts.
use crate::modules::ModuleInfo;
use anyhow::{Context, Result, bail};
use matchbox_compiler::{ast::*, parser};
use matchbox_vm::{
    Chunk,
    bundle::{Artifact, Module, TARGET},
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
    sha256: String,
    abi: u32,
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
            for (bif_index, (name, source)) in module.bif_classes.iter().enumerate() {
                let class = format!("__mb_bif_{index}_{bif_index}");
                let instance = format!("__mb_bif_instance_{index}_{bif_index}");
                let mut ast = parser::parse(source, Some(name))?;
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
                        name: name.clone(),
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
                instance,
                descriptor,
                bifs,
                settings: module.settings.clone(),
                artifact,
            })
        })
        .collect()
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
    if location.sha256.len() != 64
        || !location
            .sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        bail!("Artifact SHA-256 must be 64 lowercase hexadecimal characters");
    }
    let (filename, bytes) = match (&location.path, &location.url) {
        (Some(path), None) => {
            let path = root.join(path).canonicalize()?;
            if !path.starts_with(root.canonicalize()?) {
                bail!("Native artifact path escapes module package");
            }
            (
                path.file_name()
                    .context("Artifact has no filename")?
                    .to_string_lossy()
                    .into_owned(),
                fs::read(path)?,
            )
        }
        (None, Some(url)) => {
            // The extraction filename need not preserve a remote server's URL/path structure.
            let suffix = if target.contains("windows") {
                "dll"
            } else if target.contains("apple") {
                "dylib"
            } else {
                "so"
            };
            (
                format!("plugin.{suffix}"),
                matchbox_vm::plugins::download(url).map_err(anyhow::Error::msg)?,
            )
        }
        _ => bail!("Native artifact must specify exactly one of path or url"),
    };
    let artifact = Artifact {
        target: target.into(),
        abi: location.abi,
        filename,
        sha256: location.sha256.clone(),
        bytes,
    };
    if artifact.sha256 != matchbox_vm::bundle::hash(&artifact.bytes) {
        bail!("Published native artifact hash does not match its bytes");
    }
    Ok(Some(artifact))
}
pub fn output_target(target: Option<&str>) -> &str {
    match target {
        None | Some("native") => TARGET,
        Some(target) => target,
    }
}
