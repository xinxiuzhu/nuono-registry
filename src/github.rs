// SPDX-License-Identifier: Apache-2.0
use crate::model::{
    Entry, MAX_BYTES, Metadata, Repository, encode, is_commit, parse_issue, validate_reference,
};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{Method, blocking::Client, redirect::Policy};
use serde_json::{Value, json};
use std::{fmt, io::Read, time::Duration};

pub trait Api {
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value>;
}

pub struct Github {
    client: Client,
    token: Option<String>,
}

impl Github {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(Policy::none())
                .user_agent("nuono-registry")
                .build()?,
            token: std::env::var("GITHUB_TOKEN")
                .ok()
                .filter(|token| !token.is_empty()),
        })
    }
}

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub method: String,
    pub path: String,
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "GitHub {} {}: HTTP {}",
            self.method, self.path, self.status
        )
    }
}

impl std::error::Error for ApiError {}

pub fn has_status(error: &anyhow::Error, status: u16) -> bool {
    error
        .downcast_ref::<ApiError>()
        .is_some_and(|error| error.status == status)
}

impl Api for Github {
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        ensure!(
            path.starts_with('/') && !path.starts_with("//") && !path.chars().any(char::is_control),
            "无效 GitHub API 路径"
        );
        let mut request = self
            .client
            .request(
                Method::from_bytes(method.as_bytes())?,
                format!("https://api.github.com{path}"),
            )
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().context("GitHub 请求失败")?;
        if !response.status().is_success() {
            return Err(ApiError {
                status: response.status().as_u16(),
                method: method.to_owned(),
                path: path.to_owned(),
            }
            .into());
        }
        if response.status().as_u16() == 204 {
            return Ok(Value::Null);
        }
        let mut bytes = Vec::new();
        response
            .take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= MAX_BYTES, "GitHub 响应过大");
        serde_json::from_slice(&bytes).context("GitHub 响应不是 JSON")
    }
}

pub fn text<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("缺少 {field} 字段"))
}

pub fn decode_content(value: &Value) -> Result<Value> {
    let content = text(value, "content")?
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .collect::<String>();
    ensure!(content.len() <= MAX_BYTES, "GitHub 文件内容过大");
    serde_json::from_slice(&STANDARD.decode(content)?).context("GitHub 文件不是有效 JSON")
}

pub fn entry_from_reference(api: &impl Api, repo: &str, reference: &str) -> Result<Entry> {
    validate_reference(reference)?;
    let repo = Repository::parse(repo)?;
    let prefix = format!("/repos/{}", repo.full_name);
    let commit = api.call(
        "GET",
        &format!("{prefix}/commits/{}", encode(reference)),
        None,
    )?;
    let sha = text(&commit, "sha")?;
    ensure!(is_commit(sha), "GitHub 未返回完整 commit");
    let file = api.call(
        "GET",
        &format!("{prefix}/contents/nuono-registry.json?ref={sha}"),
        None,
    )?;
    ensure!(
        text(&file, "type")? == "file"
            && text(&file, "encoding")? == "base64"
            && file["size"].as_u64().is_some_and(|size| size <= 16_384),
        "源码根目录需要小于 16 KiB 的 nuono-registry.json"
    );
    let metadata: Metadata = serde_json::from_value(decode_content(&file)?)
        .context("源码元数据无效：不要填写 repo、commit、release_tag 或其他未声明字段")?;
    let entry = Entry::from_metadata(metadata, repo.url, sha.to_owned(), reference);
    entry.validate(None)?;
    Ok(entry)
}

pub struct Proposal<'a> {
    pub branch: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub entry: &'a Entry,
    pub expected_repo: &'a str,
}

pub fn dispatch_validation(api: &impl Api, prefix: &str, base: &str, pull: &Value) -> Result<()> {
    let number = pull["number"].as_u64().context("PR 缺少编号")?;
    api.call(
        "POST",
        &format!("{prefix}/actions/workflows/validate.yml/dispatches"),
        Some(&json!({"ref": base, "inputs": {"pull_request": number.to_string()}})),
    )?;
    Ok(())
}

pub fn open_entry_pull_request(
    api: &impl Api,
    registry: &Repository,
    proposal: Proposal<'_>,
) -> Result<Option<Value>> {
    let Proposal {
        branch,
        title,
        body,
        entry,
        expected_repo,
    } = proposal;
    entry.validate(None)?;
    let prefix = format!("/repos/{}", registry.full_name);
    let info = api.call("GET", &prefix, None)?;
    let base = text(&info, "default_branch")?;
    let path = format!("{prefix}/contents/entries/{}.json", entry.id);
    let existing = match api.call("GET", &format!("{path}?ref={}", encode(base)), None) {
        Ok(value) => Some(value),
        Err(error) if has_status(&error, 404) => None,
        Err(error) => return Err(error),
    };
    if let Some(existing) = &existing {
        let old: Entry = serde_json::from_value(decode_content(existing)?)?;
        old.validate(None)?;
        ensure!(
            Repository::parse(&old.repo)?
                .url
                .eq_ignore_ascii_case(&Repository::parse(expected_repo)?.url),
            "该 id 已登记给其他仓库，无法通过投稿替换归属"
        );
        if old.commit == entry.commit {
            return Ok(None);
        }
    }
    let pulls = api.call(
        "GET",
        &format!(
            "{prefix}/pulls?state=open&head={}:{}&per_page=1",
            registry.owner,
            encode(branch)
        ),
        None,
    )?;
    if let Some(pull) = pulls.as_array().context("PR 列表无效")?.first() {
        dispatch_validation(api, &prefix, base, pull)?;
        return Ok(Some(pull.clone()));
    }
    let reference = api.call(
        "GET",
        &format!("{prefix}/git/ref/heads/{}", encode(base)),
        None,
    )?;
    let sha = text(&reference["object"], "sha")?;
    let already_written = match api.call(
        "POST",
        &format!("{prefix}/git/refs"),
        Some(&json!({"ref": format!("refs/heads/{branch}"), "sha": sha})),
    ) {
        Ok(_) => false,
        Err(error) if has_status(&error, 422) => {
            let pending = api.call("GET", &format!("{path}?ref={}", encode(branch)), None)?;
            ensure!(
                decode_content(&pending)? == serde_json::to_value(entry)?,
                "分支 {branch} 已包含不同内容，请维护者检查后重试"
            );
            true
        }
        Err(error) => return Err(error),
    };
    if !already_written {
        let mut data = json!({"message": format!("registry: register {} {}", entry.id, entry.version), "branch": branch, "content": STANDARD.encode(format!("{}\n", serde_json::to_string_pretty(entry)?))});
        if let Some(existing) = existing {
            data["sha"] = text(&existing, "sha")?.into();
        }
        api.call("PUT", &path, Some(&data))?;
    }
    let pull = api.call(
        "POST",
        &format!("{prefix}/pulls"),
        Some(&json!({"title": title, "body": body, "base": base, "head": branch})),
    )?;
    // GITHUB_TOKEN 创建的 PR 不触发通常的 PR 事件，显式启动校验。
    dispatch_validation(api, &prefix, base, &pull)?;
    Ok(Some(pull))
}

pub fn submit_issue(api: &impl Api, registry: &Repository, event: &Value) -> Result<Option<Value>> {
    let issue = &event["issue"];
    let number = issue["number"]
        .as_u64()
        .filter(|number| *number > 0)
        .context("需要投稿 Issue")?;
    ensure!(issue.get("pull_request").is_none(), "需要投稿 Issue");
    let (repo, reference) = parse_issue(text(issue, "body")?)?;
    let entry = entry_from_reference(api, &repo.url, &reference)?;
    open_entry_pull_request(
        api,
        registry,
        Proposal {
            branch: &format!("submissions/issue-{number}"),
            title: &format!("收录 {} {}", entry.name, entry.version),
            body: &format!(
                "来自投稿 #{number}。\n\n源码：{}\n固定提交：{}\n\n合并前请检查 Registry validation 结果和源码变更。\n\nCloses #{number}",
                entry.repo, entry.commit
            ),
            entry: &entry,
            expected_repo: &repo.url,
        },
    )
}
