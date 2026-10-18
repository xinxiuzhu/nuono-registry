// SPDX-License-Identifier: Apache-2.0
use crate::{
    github::{Api, Proposal, has_status, open_entry_pull_request, submit_issue, text},
    model::{
        Entry, Framework, Frameworks, METADATA_FIELDS, Metadata, Repository, catalog,
        extensions_catalog, is_commit, load_entries, read_json, validate_frameworks,
    },
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    process::Command,
};

pub fn validate(root: &Path) -> Result<Vec<Entry>> {
    let entries = load_entries(root)?;
    let frameworks: Frameworks = read_json(&root.join("frameworks.json"))?;
    validate_frameworks(&frameworks)?;
    for entry in &entries {
        ensure!(
            frameworks.contains_key(&entry.nuono_version),
            "{}: frameworks.json 尚未登记 Nuono {}",
            entry.id,
            entry.nuono_version
        );
    }
    catalog(&entries, None)?;
    extensions_catalog(&entries, None)?;
    Ok(entries)
}

pub fn build(root: &Path, seed: Option<&str>) -> Result<usize> {
    let entries = validate(root)?;
    let (value, key) = catalog(&entries, seed)?;
    let dist = root.join("dist");
    match fs::remove_dir_all(&dist) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    fs::create_dir_all(&dist)?;
    fs::write(
        dist.join("catalog.json"),
        format!("{}\n", serde_json::to_string_pretty(&value)?),
    )?;
    let (extensions, _) = extensions_catalog(&entries, seed)?;
    fs::write(
        dist.join("extensions-v2.json"),
        format!("{}\n", serde_json::to_string_pretty(&extensions)?),
    )?;
    if let Some(key) = key {
        fs::write(dist.join("signing-public-key.txt"), format!("{key}\n"))?;
    }
    Ok(entries.len())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanItem {
    pub entry: Entry,
    pub framework: Framework,
}

pub fn plan(root: &Path, trusted_root: &Path) -> Result<Vec<PlanItem>> {
    let entries = load_entries(root)?;
    // 候选 PR 不能修改 framework 锁定信息来绕过已指定宿主的构建检查。
    let frameworks: Frameworks = read_json(&trusted_root.join("frameworks.json"))?;
    validate_frameworks(&frameworks)?;
    catalog(&entries, None)?;
    ensure!(entries.len() <= 256, "单次检查最多 256 个登记条目");
    entries
        .into_iter()
        .map(|entry| {
            let framework = frameworks
                .get(&entry.nuono_version)
                .with_context(|| format!("{}: 未登记 Nuono {}", entry.id, entry.nuono_version))?
                .clone();
            Ok(PlanItem { entry, framework })
        })
        .collect()
}

pub fn append_output(path: &Path, fields: &[(&str, String)]) -> Result<()> {
    let mut output = OpenOptions::new().create(true).append(true).open(path)?;
    for (key, value) in fields {
        ensure!(!value.contains(['\r', '\n']), "工作流输出不能包含换行");
        writeln!(output, "{key}={value}")?;
    }
    Ok(())
}

pub struct ResolvedPull {
    pub sha: String,
    pub source_repo: String,
    pub number: Option<u64>,
}

pub fn resolve_pr(
    api: &impl Api,
    registry: &Repository,
    event: &Value,
    fallback_sha: &str,
) -> Result<ResolvedPull> {
    let mut pull = event.get("pull_request").cloned().unwrap_or(Value::Null);
    if let Some(input) = event["inputs"]["pull_request"]
        .as_str()
        .filter(|input| !input.is_empty())
    {
        ensure!(
            input.bytes().all(|b| b.is_ascii_digit()),
            "pull_request 必须为 PR 编号"
        );
        let number = input.parse::<u64>()?;
        ensure!(
            number > 0 && input == number.to_string(),
            "pull_request 必须为正整数编号"
        );
        pull = api.call(
            "GET",
            &format!("/repos/{}/pulls/{number}", registry.full_name),
            None,
        )?;
    }
    let (sha, source_repo, number) = if pull.is_null() {
        (fallback_sha, registry.full_name.as_str(), None)
    } else {
        (
            text(&pull["head"], "sha")?,
            text(&pull["head"]["repo"], "full_name")?,
            Some(pull["number"].as_u64().context("PR 缺少编号")?),
        )
    };
    ensure!(is_commit(sha), "缺少固定校验提交");
    Repository::parse(&format!("https://github.com/{source_repo}"))?;
    Ok(ResolvedPull {
        sha: sha.to_owned(),
        source_repo: source_repo.to_owned(),
        number,
    })
}

pub fn report(
    api: &impl Api,
    registry: &Repository,
    sha: &str,
    prepare: &str,
    build: &str,
    run_id: &str,
) -> Result<()> {
    ensure!(is_commit(sha), "缺少校验提交");
    let success = prepare == "success" && matches!(build, "success" | "skipped");
    api.call("POST", &format!("/repos/{}/check-runs", registry.full_name), Some(&json!({
        "name": "Registry validation", "head_sha": sha, "status": "completed",
        "conclusion": if success { "success" } else { "failure" },
        "details_url": format!("https://github.com/{}/actions/runs/{run_id}", registry.full_name),
        "output": {
            "title": if success { "登记与源码链接检查通过" } else { "登记或源码链接检查失败" },
            "summary": "详见 Actions 中的字段检查和各插件在固定 Nuono commit 下的构建日志。构建通过不代替维护者的源码审核。"
        }
    })))?;
    Ok(())
}

pub fn publish(api: &impl Api, registry: &Repository, root: &Path, source_sha: &str) -> Result<()> {
    ensure!(is_commit(source_sha), "发布必须固定已校验的主分支提交");
    let prefix = format!("/repos/{}", registry.full_name);
    let main = api.call("GET", &format!("{prefix}/git/ref/heads/main"), None)?;
    ensure!(
        text(&main["object"], "sha")? == source_sha,
        "main 已更新，跳过过期工作流以免回退目录"
    );
    let dist = root.join("dist");
    let _: Value = read_json(&dist.join("catalog.json"))?;
    let mut tree = Vec::new();
    for file in [
        "catalog.json",
        "extensions-v2.json",
        "signing-public-key.txt",
    ] {
        let path = dist.join(file);
        if file == "signing-public-key.txt" && !path.try_exists()? {
            continue;
        }
        tree.push(json!({"path": file, "mode": "100644", "type": "blob", "content": fs::read_to_string(path)?}));
    }
    tree.push(json!({"path": "README.md", "mode": "100644", "type": "blob", "content": format!("# Nuono 商店目录\n\n由 [main](https://github.com/{}/tree/main) 的已验证提交 {source_sha} 自动生成。请向 main 提交投稿，勿直接编辑本分支。\n", registry.full_name)}));
    let created = api.call(
        "POST",
        &format!("{prefix}/git/trees"),
        Some(&json!({"tree": tree})),
    )?;
    let old = match api.call("GET", &format!("{prefix}/git/ref/heads/catalog"), None) {
        Ok(value) => Some(value),
        Err(error) if has_status(&error, 404) => None,
        Err(error) => return Err(error),
    };
    let parents = old
        .as_ref()
        .map(|value| text(&value["object"], "sha").map(|sha| vec![sha]))
        .transpose()?
        .unwrap_or_default();
    let commit = api.call("POST", &format!("{prefix}/git/commits"), Some(&json!({"message": format!("catalog: publish {source_sha}"), "tree": text(&created, "sha")?, "parents": parents})))?;
    if old.is_some() {
        api.call(
            "PATCH",
            &format!("{prefix}/git/refs/heads/catalog"),
            Some(&json!({"sha": text(&commit, "sha")?, "force": false})),
        )?;
    } else {
        api.call(
            "POST",
            &format!("{prefix}/git/refs"),
            Some(&json!({"ref": "refs/heads/catalog", "sha": text(&commit, "sha")?})),
        )?;
    }
    Ok(())
}

fn submission_comment(
    api: &impl Api,
    registry: &Repository,
    event: &Value,
    run_id: &str,
    message: &str,
) -> Result<()> {
    let number = event["issue"]["number"]
        .as_u64()
        .context("需要投稿 Issue")?;
    let prefix = format!("/repos/{}/issues/{number}/comments", registry.full_name);
    let marker = "<!-- nuono-registry-submission -->";
    let body = format!(
        "{marker}\n{message}\n\n[查看检查日志](https://github.com/{}/actions/runs/{run_id})",
        registry.full_name
    );
    let comments = api.call("GET", &format!("{prefix}?per_page=100"), None)?;
    let old = comments
        .as_array()
        .context("Issue 评论列表无效")?
        .iter()
        .find(|comment| {
            comment["user"]["login"] == "github-actions[bot]"
                && comment["body"]
                    .as_str()
                    .is_some_and(|body| body.starts_with(marker))
        });
    if let Some(old) = old {
        let id = old["id"].as_u64().context("评论缺少编号")?;
        api.call(
            "PATCH",
            &format!("/repos/{}/issues/comments/{id}", registry.full_name),
            Some(&json!({"body": body})),
        )?;
    } else {
        api.call("POST", &prefix, Some(&json!({"body": body})))?;
    }
    Ok(())
}

pub fn submission(
    api: &impl Api,
    registry: &Repository,
    event: &Value,
    run_id: &str,
) -> Result<()> {
    match submit_issue(api, registry, event) {
        Ok(pull) => {
            let message = match pull {
                Some(pull) => format!(
                    "登记 PR 已创建：{}\n\n请等待构建检查和维护者审核。",
                    text(&pull, "html_url")?
                ),
                None => "该 commit 已经登记，无需重复投稿。".into(),
            };
            submission_comment(api, registry, event, run_id, &message)
        }
        Err(error) => {
            submission_comment(
                api,
                registry,
                event,
                run_id,
                &format!(
                    "投稿未完成：{error:#}\n\n修正源码元数据或表单后，编辑本 Issue 即可重试。"
                ),
            )?;
            Err(error)
        }
    }
}

pub fn updates(api: &impl Api, registry: &Repository, root: &Path) -> Result<()> {
    let pending = api.call(
        "GET",
        &format!(
            "/repos/{}/pulls?state=open&per_page=100",
            registry.full_name
        ),
        None,
    )?;
    let pending = pending.as_array().context("PR 列表无效")?;
    let mut failures = 0;
    for entry in load_entries(root)? {
        let Some(tag) = &entry.release_tag else {
            continue;
        };
        let prefix = format!("updates/{}-", entry.key());
        if pending.iter().any(|pull| {
            pull["head"]["ref"]
                .as_str()
                .is_some_and(|branch| branch.starts_with(&prefix))
        }) {
            continue;
        }
        let result = (|| -> Result<()> {
            let repo = Repository::parse(&entry.repo)?;
            let release = api.call(
                "GET",
                &format!("/repos/{}/releases/latest", repo.full_name),
                None,
            )?;
            let next_tag = text(&release, "tag_name")?;
            if release["draft"] == true || release["prerelease"] == true || next_tag == tag {
                return Ok(());
            }
            let next = crate::github::entry_from_reference(api, &entry.repo, next_tag)?;
            if next.commit == entry.commit {
                return Ok(());
            }
            ensure!(
                next.kind == entry.kind && next.id == entry.id,
                "{} 的新 Release 改变了插件 id",
                entry.id
            );
            let pull = open_entry_pull_request(
                api,
                registry,
                Proposal {
                    branch: &format!("updates/{}-{}", entry.key(), &next.commit[..12]),
                    title: &format!("更新 {} → {}", entry.name, next.version),
                    body: &format!(
                        "发现新的正式 Release：{}\n\n原提交：{}\n新提交：{}\n\n这是更新建议，合并前请检查源码变化和 Registry validation 结果。",
                        text(&release, "html_url")?,
                        entry.commit,
                        next.commit
                    ),
                    entry: &next,
                    expected_repo: &entry.repo,
                },
            )?;
            if let Some(pull) = pull {
                println!("{}", text(&pull, "html_url")?);
            }
            Ok(())
        })();
        if let Err(error) = result {
            failures += 1;
            eprintln!("{}: {error:#}", entry.id);
        }
    }
    ensure!(failures == 0, "{failures} 个插件的更新检查失败");
    Ok(())
}

fn run(program: &str, args: &[&str], cwd: &Path, framework: &Framework) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_BUILD_JOBS", "4")
        .env("RUSTUP_TOOLCHAIN", &framework.toolchain)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .with_context(|| format!("无法运行 {program}"))?;
    ensure!(status.success(), "{program} 失败: {status}");
    Ok(())
}

fn checkout(
    repo: &str,
    commit: &str,
    target: &Path,
    work: &Path,
    framework: &Framework,
) -> Result<()> {
    let target = target.to_str().context("构建目录必须是 UTF-8")?;
    run("git", &["init", target], work, framework)?;
    run(
        "git",
        &["-C", target, "remote", "add", "origin", repo],
        work,
        framework,
    )?;
    run(
        "git",
        &[
            "-C",
            target,
            "-c",
            "protocol.file.allow=never",
            "fetch",
            "--depth=1",
            "origin",
            commit,
        ],
        work,
        framework,
    )?;
    run(
        "git",
        &["-C", target, "checkout", "--detach", commit],
        work,
        framework,
    )
}

pub fn check_source(item: PlanItem) -> Result<()> {
    let PlanItem { entry, framework } = item;
    entry.validate(None)?;
    validate_frameworks(&BTreeMap::from([(
        entry.nuono_version.clone(),
        framework.clone(),
    )]))?;
    let work = Path::new(".work").join(format!("{}-{}", entry.key(), &entry.commit[..12]));
    fs::create_dir_all(&work)?;
    let work = work.canonicalize()?;
    let host = work.join("Nuono");
    checkout(&framework.repo, &framework.commit, &host, &work, &framework)?;
    fs::create_dir_all(host.join(entry.kind.directory()))?;
    let source = host.join(entry.kind.directory()).join(&entry.id);
    checkout(&entry.repo, &entry.commit, &source, &work, &framework)?;
    let metadata: Metadata = read_json(&source.join("nuono-registry.json"))?;
    let metadata = serde_json::to_value(metadata)?;
    let published = serde_json::to_value(&entry)?;
    for key in METADATA_FIELDS {
        ensure!(
            metadata[*key] == published[*key],
            "{}: {key} 与指定 commit 的源码元数据不一致",
            entry.id
        );
    }
    let source = source.to_str().context("构建目录必须是 UTF-8")?;
    let output = work.join("nuono-verified");
    let output = output.to_str().context("构建目录必须是 UTF-8")?;
    // 只编译和导出应用，不启动宿主、插件生命周期或数据库。
    run(
        "cargo",
        &[
            "run",
            "--locked",
            "--",
            entry.kind.command(),
            "doctor",
            source,
        ],
        &host,
        &framework,
    )?;
    run(
        "cargo",
        &["run", "--locked", "--", "build", "--output", output],
        &host,
        &framework,
    )?;
    println!(
        "已通过 Nuono {} 的统一链接构建: {}@{}",
        entry.nuono_version, entry.id, entry.commit
    );
    Ok(())
}
