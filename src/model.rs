// SPDX-License-Identifier: Apache-2.0
use anyhow::{Context, Result, ensure};
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::Read,
    path::Path,
};

pub const MAX_BYTES: usize = 2 * 1024 * 1024;
pub const METADATA_FIELDS: &[&str] = &[
    "id",
    "name",
    "version",
    "description",
    "author",
    "license",
    "nuono_version",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub license: String,
    pub nuono_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub license: String,
    pub nuono_version: String,
    pub repo: String,
    pub commit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_tag: Option<String>,
}

impl Entry {
    pub fn from_metadata(
        metadata: Metadata,
        repo: String,
        commit: String,
        reference: &str,
    ) -> Self {
        Self {
            id: metadata.id,
            name: metadata.name,
            version: metadata.version,
            description: metadata.description,
            author: metadata.author,
            license: metadata.license,
            nuono_version: metadata.nuono_version,
            repo,
            commit,
            release_tag: (!is_commit(reference)).then(|| reference.to_owned()),
        }
    }

    pub fn validate(&self, filename: Option<&str>) -> Result<()> {
        for (field, value) in serde_json::to_value(self)?
            .as_object()
            .context("登记不是对象")?
        {
            let text = value.as_str().context("登记字段必须为字符串")?;
            ensure!(
                !text.is_empty() && text.trim() == text && text.chars().count() <= 4096,
                "{field} 必须是非空字符串，最多 4096 字符"
            );
        }
        let mut id = self.id.bytes();
        ensure!(
            self.id.len() <= 80
                && id
                    .next()
                    .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                && id.all(|b| b.is_ascii_lowercase()
                    || b.is_ascii_digit()
                    || b == b'-'
                    || b == b'_')
                && self.id != "hub",
            "id 必须为小写字母、数字、连字符或下划线，不能为 hub"
        );
        if let Some(filename) = filename {
            ensure!(
                filename == format!("{}.json", self.id),
                "文件名必须与 id 一致"
            );
        }
        semver::Version::parse(&self.version).context("version 必须是完整 SemVer 版本")?;
        semver::Version::parse(&self.nuono_version).context("nuono_version 必须是精确版本")?;
        ensure!(is_commit(&self.commit), "commit 必须是小写 40 位 SHA");
        Repository::parse(&self.repo)?;
        if let Some(tag) = &self.release_tag {
            validate_reference(tag)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    pub owner: String,
    pub name: String,
    pub full_name: String,
    pub url: String,
}

impl Repository {
    pub fn parse(input: &str) -> Result<Self> {
        let path = input
            .strip_prefix("https://github.com/")
            .context("只接受 https://github.com/owner/repo 仓库地址")?;
        let (owner, name) = path.split_once('/').context("仓库地址缺少 owner/repo")?;
        let name = name.strip_suffix(".git").unwrap_or(name);
        ensure!(
            !owner.is_empty()
                && owner.len() <= 39
                && owner.as_bytes()[0].is_ascii_alphanumeric()
                && owner
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !name.is_empty()
                && !matches!(name, "." | "..")
                && name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')),
            "无效 GitHub 仓库地址"
        );
        let full_name = format!("{owner}/{name}");
        Ok(Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
            url: format!("https://github.com/{full_name}.git"),
            full_name,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Framework {
    pub repo: String,
    pub commit: String,
    pub toolchain: String,
}

pub type Frameworks = BTreeMap<String, Framework>;

pub fn validate_frameworks(frameworks: &Frameworks) -> Result<()> {
    for (version, framework) in frameworks {
        semver::Version::parse(version).context("frameworks 版本号无效")?;
        Repository::parse(&framework.repo)?;
        ensure!(
            is_commit(&framework.commit),
            "framework commit 必须是完整 SHA"
        );
        let toolchain =
            semver::Version::parse(&framework.toolchain).context("toolchain 必须固定精确版本")?;
        ensure!(
            toolchain.pre.is_empty() && toolchain.build.is_empty(),
            "toolchain 必须固定正式版本"
        );
    }
    Ok(())
}

pub fn is_commit(commit: &str) -> bool {
    commit.len() == 40
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn validate_reference(reference: &str) -> Result<()> {
    ensure!(
        !reference.is_empty()
            && reference.chars().count() <= 200
            && !reference
                .chars()
                .any(|c| c.is_whitespace() || c.is_control()),
        "请填写发布 tag 或完整 commit"
    );
    Ok(())
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("无法读取 {}", path.display()))?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_BYTES as u64,
        "{} 必须是小于 2 MiB 的普通文件",
        path.display()
    );
    let mut bytes = Vec::new();
    File::open(path)?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_BYTES, "JSON 文件超过 2 MiB 上限");
    serde_json::from_slice(&bytes).with_context(|| format!("{} JSON 格式无效", path.display()))
}

pub fn load_entries(root: &Path) -> Result<Vec<Entry>> {
    let directory = root.join("entries");
    ensure!(
        fs::symlink_metadata(&directory)?.is_dir(),
        "entries 必须是普通目录"
    );
    let mut files = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    files.sort_by_key(|file| file.file_name());
    let mut entries = Vec::new();
    let mut ids = BTreeSet::new();
    for file in files {
        let name = file
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("文件名必须是 UTF-8"))?;
        if name == ".gitkeep" {
            continue;
        }
        ensure!(
            name.ends_with(".json") && !name.starts_with('.'),
            "entries 中不支持的文件: {name}"
        );
        let entry: Entry = read_json(&file.path())?;
        entry.validate(Some(&name))?;
        ensure!(ids.insert(entry.id.clone()), "重复插件 id: {}", entry.id);
        entries.push(entry);
    }
    Ok(entries)
}

pub fn catalog(entries: &[Entry], seed: Option<&str>) -> Result<(Value, Option<String>)> {
    let key = seed
        .filter(|seed| !seed.is_empty())
        .map(|seed| -> Result<SigningKey> {
            let bytes: [u8; 32] = hex::decode(seed)
                .context("REGISTRY_SIGNING_KEY 必须是 32 字节 hex seed")?
                .try_into()
                .map_err(|_| anyhow::anyhow!("REGISTRY_SIGNING_KEY 必须是 32 字节 hex seed"))?;
            Ok(SigningKey::from_bytes(&bytes))
        })
        .transpose()?;
    let mut sorted = entries.iter().collect::<Vec<_>>();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    let plugins = sorted
        .into_iter()
        .map(|entry| -> Result<Value> {
            entry.validate(None)?;
            let mut value = serde_json::to_value(entry)?;
            let fields = value.as_object_mut().context("登记不是对象")?;
            fields.remove("release_tag");
            if let Some(key) = &key {
                let message = format!("{}\n{}", entry.repo, entry.commit);
                fields.insert(
                    "signature".into(),
                    hex::encode(key.sign(message.as_bytes()).to_bytes()).into(),
                );
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>>>()?;
    let value = json!({"schema_version": 1, "plugins": plugins});
    ensure!(
        serde_json::to_string_pretty(&value)?.len() < MAX_BYTES,
        "catalog 超过 Nuono 的 2 MiB 上限"
    );
    Ok((
        value,
        key.map(|key| hex::encode(key.verifying_key().to_bytes())),
    ))
}

pub fn parse_issue(body: &str) -> Result<(Repository, String)> {
    ensure!(body.chars().count() <= 20_000, "投稿表单过大");
    let normalized = body.replace("\r\n", "\n");
    let mut fields = BTreeMap::<String, String>::new();
    let mut current = None;
    for line in normalized.lines() {
        if let Some(heading) = line.strip_prefix("### ") {
            ensure!(!fields.contains_key(heading), "重复表单字段: {heading}");
            fields.insert(heading.to_owned(), String::new());
            current = Some(heading.to_owned());
        } else if let Some(heading) = &current {
            let value = fields.get_mut(heading).context("表单字段不存在")?;
            value.push_str(line);
            value.push('\n');
        }
    }
    let repo = Repository::parse(fields.get("源码仓库").context("请填写源码仓库")?.trim())?;
    let reference = fields
        .get("发布版本或 commit")
        .context("请填写发布版本或 commit")?
        .trim()
        .to_owned();
    validate_reference(&reference)?;
    Ok((repo, reference))
}

pub fn encode(input: &str) -> String {
    input
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
