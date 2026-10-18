// SPDX-License-Identifier: Apache-2.0
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use nuono_registry::{
    github::Github,
    model::{Repository, read_json},
    operations::{self, PlanItem},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(version, about = "Nuono 源码插件索引、投稿与发布工具")]
struct Cli {
    #[command(subcommand)]
    command: RegistryCommand,
}

#[derive(Subcommand)]
enum RegistryCommand {
    /// 校验登记文件与宿主版本锁定信息。
    Validate {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    /// 生成兼容 Nuono 的 dist/catalog.json。
    Build {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    /// 使用当前工作目录的受信任 frameworks.json 生成候选检查矩阵。
    Plan {
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    ResolvePr,
    Report,
    Publish,
    Submission,
    Updates,
    CheckSource,
}

fn env(name: &str) -> Result<String> {
    std::env::var(name).with_context(|| format!("缺少环境变量 {name}"))
}

fn registry() -> Result<Repository> {
    Repository::parse(&format!("https://github.com/{}", env("GITHUB_REPOSITORY")?))
}

fn event() -> Result<Value> {
    read_json(Path::new(&env("GITHUB_EVENT_PATH")?))
}

fn execute(command: RegistryCommand) -> Result<()> {
    match command {
        RegistryCommand::Validate { root } => {
            println!("已验证 {} 个登记条目", operations::validate(&root)?.len());
        }
        RegistryCommand::Build { root } => {
            let key = std::env::var("REGISTRY_SIGNING_KEY").ok();
            println!(
                "已生成 {} 个插件的 catalog.json",
                operations::build(&root, key.as_deref())?
            );
        }
        RegistryCommand::Plan { root } => {
            let items = operations::plan(&root, Path::new("."))?;
            let matrix = json!({"include": items});
            if let Some(output) = std::env::var_os("GITHUB_OUTPUT") {
                operations::append_output(
                    Path::new(&output),
                    &[
                        ("matrix", serde_json::to_string(&matrix)?),
                        ("has_entries", (!items.is_empty()).to_string()),
                    ],
                )?;
            } else {
                println!("{}", serde_json::to_string_pretty(&matrix)?);
            }
        }
        RegistryCommand::ResolvePr => {
            let value = operations::resolve_pr(
                &Github::new()?,
                &registry()?,
                &event()?,
                &env("GITHUB_SHA")?,
            )?;
            operations::append_output(
                Path::new(&env("GITHUB_OUTPUT")?),
                &[
                    ("sha", value.sha),
                    ("source_repo", value.source_repo),
                    (
                        "pr",
                        value
                            .number
                            .map(|number| number.to_string())
                            .unwrap_or_default(),
                    ),
                ],
            )?;
        }
        RegistryCommand::Report => {
            operations::report(
                &Github::new()?,
                &registry()?,
                &env("HEAD_SHA")?,
                &env("PREPARE_RESULT")?,
                &env("BUILD_RESULT")?,
                &env("GITHUB_RUN_ID")?,
            )?;
        }
        RegistryCommand::Publish => {
            let registry = registry()?;
            operations::publish(
                &Github::new()?,
                &registry,
                Path::new("."),
                &env("SOURCE_SHA")?,
            )?;
            println!(
                "https://raw.githubusercontent.com/{}/catalog/catalog.json",
                registry.full_name
            );
        }
        RegistryCommand::Submission => {
            operations::submission(
                &Github::new()?,
                &registry()?,
                &event()?,
                &env("GITHUB_RUN_ID")?,
            )?;
        }
        RegistryCommand::Updates => {
            operations::updates(&Github::new()?, &registry()?, Path::new("."))?
        }
        RegistryCommand::CheckSource => {
            let item: PlanItem =
                serde_json::from_str(&env("CHECK_ITEM")?).context("CHECK_ITEM 格式无效")?;
            operations::check_source(item)?;
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = execute(Cli::parse().command) {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}
