// SPDX-License-Identifier: Apache-2.0
use anyhow::{Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, VerifyingKey};
use nuono_registry::{
    github::{
        Api, ApiError, Proposal, entry_from_reference, open_entry_pull_request, submit_issue,
    },
    model::{
        Entry, Framework, Frameworks, MAX_BYTES, Repository, catalog, load_entries, parse_issue,
        read_json, validate_frameworks,
    },
    operations,
};
use serde_json::{Value, json};
use std::{cell::RefCell, collections::BTreeMap, fs, path::Path, process::Command};

fn entry() -> Entry {
    serde_json::from_value(json!({
        "id": "test-plugin", "name": "测试插件", "version": "1.0.0",
        "description": "A plugin", "author": "tester", "license": "MIT", "nuono_version": "0.4.0",
        "repo": "https://github.com/example/test-plugin.git", "commit": "a".repeat(40), "release_tag": "v1.0.0"
    })).unwrap()
}

fn framework() -> Framework {
    Framework {
        repo: "https://github.com/xinxiuzhu/Nuono.git".into(),
        commit: "b".repeat(40),
        toolchain: "1.99.0".into(),
    }
}

fn registry() -> Repository {
    Repository::parse("https://github.com/example/registry").unwrap()
}

fn missing() -> anyhow::Error {
    ApiError {
        status: 404,
        method: "GET".into(),
        path: "/file".into(),
    }
    .into()
}

struct Mock<F>(F);
impl<F> Api for Mock<F>
where
    F: Fn(&str, &str, Option<&Value>) -> Result<Value>,
{
    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        (self.0)(method, path, body)
    }
}

fn content(value: &Value) -> Value {
    json!({"type": "file", "encoding": "base64", "size": 1000, "sha": "blob", "content": STANDARD.encode(serde_json::to_vec(value).unwrap())})
}

fn metadata() -> Value {
    let mut value = serde_json::to_value(entry()).unwrap();
    for field in ["repo", "commit", "release_tag"] {
        value.as_object_mut().unwrap().remove(field);
    }
    value
}

fn write_fixture(root: &Path, entries: &[Entry], frameworks: &Frameworks) {
    fs::create_dir_all(root.join("entries")).unwrap();
    for entry in entries {
        fs::write(
            root.join("entries").join(format!("{}.json", entry.id)),
            serde_json::to_vec(entry).unwrap(),
        )
        .unwrap();
    }
    fs::write(
        root.join("frameworks.json"),
        serde_json::to_vec(frameworks).unwrap(),
    )
    .unwrap();
}

#[test]
fn catalog_preserves_the_host_v1_contract_and_deterministic_ordering() {
    let mut second = entry();
    second.id = "another-plugin".into();
    let first = catalog(&[entry(), second.clone()], None).unwrap().0;
    assert_eq!(first, catalog(&[second, entry()], None).unwrap().0);
    assert_eq!(first["schema_version"], 1);
    assert_eq!(first["plugins"][0]["id"], "another-plugin");
    assert!(first["plugins"][1].get("release_tag").is_none());
    assert!(first["plugins"][1].get("signature").is_none());
    assert_eq!(
        catalog(&[], None).unwrap().0,
        json!({"schema_version": 1, "plugins": []})
    );
}

#[test]
fn signatures_use_the_exact_nuono_repo_newline_commit_message() {
    let (value, public) = catalog(&[entry()], Some(&"01".repeat(32))).unwrap();
    let key: [u8; 32] = hex::decode(public.unwrap()).unwrap().try_into().unwrap();
    let key = VerifyingKey::from_bytes(&key).unwrap();
    let bytes = hex::decode(value["plugins"][0]["signature"].as_str().unwrap()).unwrap();
    let signature = Signature::from_slice(&bytes).unwrap();
    let message = format!("{}\n{}", entry().repo, entry().commit);
    key.verify_strict(message.as_bytes(), &signature).unwrap();
    assert!(
        key.verify_strict(
            format!("{}\n{}", entry().repo, "b".repeat(40)).as_bytes(),
            &signature
        )
        .is_err()
    );
    assert!(catalog(&[entry()], Some("secret")).is_err());
}

#[test]
fn incomplete_releases_and_schema_drift_are_rejected() {
    for field in [
        "id",
        "name",
        "version",
        "description",
        "author",
        "repo",
        "commit",
        "license",
        "nuono_version",
    ] {
        let mut value = serde_json::to_value(entry()).unwrap();
        value.as_object_mut().unwrap().remove(field);
        assert!(serde_json::from_value::<Entry>(value).is_err(), "{field}");
    }
    let mut value = serde_json::to_value(entry()).unwrap();
    value["surprise"] = true.into();
    assert!(serde_json::from_value::<Entry>(value).is_err());
    for (field, value) in [
        ("id", "hub"),
        ("id", "../outside"),
        ("id", "Upper"),
        ("commit", "main"),
        ("version", "^1.0.0"),
        ("version", "01.0.0"),
        ("version", "1.0.0-01"),
        ("nuono_version", "latest"),
        ("release_tag", "v1\nrun"),
    ] {
        let mut item = serde_json::to_value(entry()).unwrap();
        item[field] = value.into();
        assert!(
            serde_json::from_value::<Entry>(item)
                .unwrap()
                .validate(None)
                .is_err(),
            "{field}: {value}"
        );
    }
    let mut good = entry();
    good.version = "1.0.0-rc.1+build.3".into();
    good.validate(Some("test-plugin.json")).unwrap();
    assert!(good.validate(Some("other.json")).is_err());
}

#[test]
fn repository_input_cannot_select_local_paths_other_hosts_or_credentials() {
    assert_eq!(
        Repository::parse("https://github.com/example/test-plugin")
            .unwrap()
            .url,
        entry().repo
    );
    for input in [
        "file:///etc/passwd",
        "http://github.com/a/b",
        "https://github.com/a/b?token=x",
        "https://token@github.com/a/b",
        "https://evil.test/a/b",
        "--upload-pack=sh",
        "https://github.com/a/../b",
        "https://github.com/a/b\n",
    ] {
        assert!(Repository::parse(input).is_err(), "{input}");
    }
}

#[test]
fn frameworks_require_immutable_commits_and_exact_toolchains() {
    validate_frameworks(&BTreeMap::from([("0.4.0".into(), framework())])).unwrap();
    let mut bad = framework();
    bad.commit = "main".into();
    assert!(validate_frameworks(&BTreeMap::from([("0.4.0".into(), bad)])).is_err());
    let mut bad = framework();
    bad.toolchain = "stable; echo no".into();
    assert!(validate_frameworks(&BTreeMap::from([("0.4.0".into(), bad)])).is_err());
}

#[test]
fn oversized_and_malformed_json_are_not_accepted_silently() {
    let root = tempfile::tempdir().unwrap();
    write_fixture(root.path(), &[entry()], &BTreeMap::new());
    assert_eq!(load_entries(root.path()).unwrap().len(), 1);
    fs::write(root.path().join("entries/bad.json"), "{bad").unwrap();
    assert!(load_entries(root.path()).is_err());
    let large = root.path().join("large.json");
    fs::write(&large, vec![b' '; MAX_BYTES + 1]).unwrap();
    assert!(read_json::<Value>(&large).is_err());
}

#[cfg(unix)]
#[test]
fn entry_files_and_entry_directory_cannot_be_symlinks() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    write_fixture(root.path(), &[entry()], &BTreeMap::new());
    symlink(
        root.path().join("entries/test-plugin.json"),
        root.path().join("entries/alias.json"),
    )
    .unwrap();
    assert!(load_entries(root.path()).is_err());
    symlink(root.path().join("entries"), root.path().join("linked")).unwrap();
    let other = tempfile::tempdir().unwrap();
    symlink(root.path().join("entries"), other.path().join("entries")).unwrap();
    assert!(load_entries(other.path()).is_err());
}

#[test]
fn issue_parser_handles_crlf_but_rejects_duplicate_fields_and_multiline_refs() {
    let body = "### 源码仓库\r\n\r\nhttps://github.com/example/test-plugin\r\n\r\n### 发布版本或 commit\r\n\r\nv1.0.0\r\n\r\n### 投稿确认\r\n\r\n- [x] checked";
    assert_eq!(parse_issue(body).unwrap().1, "v1.0.0");
    assert!(parse_issue(&body.replace("v1.0.0", "v1\n$(bad)")).is_err());
    assert!(parse_issue("missing").is_err());
    assert!(
        parse_issue(&format!(
            "{body}\n### 源码仓库\n\nhttps://github.com/other/repo"
        ))
        .is_err()
    );
}

#[test]
fn source_metadata_is_read_from_resolved_commit_never_a_moving_tag() {
    let paths = RefCell::new(Vec::new());
    let api = Mock(|_: &str, path: &str, _: Option<&Value>| {
        paths.borrow_mut().push(path.to_owned());
        if path.contains("/commits/") {
            return Ok(json!({"sha": entry().commit}));
        }
        Ok(content(&metadata()))
    });
    let result = entry_from_reference(&api, &entry().repo, "v1.0.0").unwrap();
    assert_eq!(result, entry());
    assert!(paths.borrow()[1].ends_with(&format!("?ref={}", entry().commit)));
    let api = Mock(|_: &str, path: &str, _: Option<&Value>| {
        if path.contains("/commits/") {
            return Ok(json!({"sha": entry().commit}));
        }
        Ok(content(&serde_json::to_value(entry()).unwrap()))
    });
    assert!(entry_from_reference(&api, &entry().repo, "v1").is_err());
}

#[test]
fn an_existing_plugin_id_cannot_be_claimed_by_another_repository() {
    let mut current = entry();
    current.repo = "https://github.com/original/owner.git".into();
    let writes = RefCell::new(0);
    let api = Mock(|method: &str, path: &str, _: Option<&Value>| {
        if method != "GET" {
            *writes.borrow_mut() += 1;
        }
        if path == "/repos/example/registry" {
            return Ok(json!({"default_branch": "main"}));
        }
        Ok(content(&serde_json::to_value(&current).unwrap()))
    });
    let item = entry();
    let error = open_entry_pull_request(
        &api,
        &registry(),
        Proposal {
            branch: "submissions/issue-1",
            title: "test",
            body: "",
            entry: &item,
            expected_repo: &item.repo,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("归属"));
    assert_eq!(*writes.borrow(), 0);
}

#[test]
fn submission_creates_a_reviewable_pr_and_explicitly_dispatches_validation() {
    let calls = RefCell::new(Vec::new());
    let api = Mock(|method: &str, path: &str, body: Option<&Value>| {
        calls
            .borrow_mut()
            .push((method.to_owned(), path.to_owned(), body.cloned()));
        if path.contains("/commits/") {
            return Ok(json!({"sha": entry().commit}));
        }
        if path.contains("/contents/nuono-registry.json") {
            return Ok(content(&metadata()));
        }
        if path == "/repos/example/registry" {
            return Ok(json!({"default_branch": "main"}));
        }
        if path.contains("/contents/entries/") && method == "GET" {
            return Err(missing());
        }
        if path.contains("/pulls?") {
            return Ok(json!([]));
        }
        if path.contains("/git/ref/heads/") {
            return Ok(json!({"object": {"sha": "c".repeat(40)}}));
        }
        if path.ends_with("/pulls") {
            return Ok(
                json!({"number": 12, "html_url": "https://github.com/example/registry/pull/12"}),
            );
        }
        Ok(Value::Null)
    });
    let event = json!({"issue": {"number": 1, "body": "### 源码仓库\n\nhttps://github.com/example/test-plugin\n\n### 发布版本或 commit\n\nv1.0.0"}});
    let pull = submit_issue(&api, &registry(), &event).unwrap().unwrap();
    assert_eq!(pull["number"], 12);
    let calls = calls.borrow();
    let dispatch = calls
        .iter()
        .find(|(_, path, _)| path.ends_with("/dispatches"))
        .unwrap();
    assert_eq!(dispatch.2.as_ref().unwrap()["inputs"]["pull_request"], "12");
    assert!(
        calls
            .iter()
            .find(|(_, path, _)| path.ends_with("/pulls"))
            .unwrap()
            .2
            .as_ref()
            .unwrap()["body"]
            .as_str()
            .unwrap()
            .contains("Closes #1")
    );
    assert!(
        !calls
            .iter()
            .any(|(_, path, _)| path.contains("merge") || path.contains("approve"))
    );
}

#[test]
fn existing_pr_retry_dispatches_checks_without_replacing_content_under_review() {
    let writes = RefCell::new(Vec::new());
    let api = Mock(|method: &str, path: &str, _: Option<&Value>| {
        if method != "GET" {
            writes.borrow_mut().push(path.to_owned());
            return Ok(Value::Null);
        }
        if path == "/repos/example/registry" {
            return Ok(json!({"default_branch": "main"}));
        }
        if path.contains("/contents/entries/") {
            return Err(missing());
        }
        if path.contains("/pulls?") {
            return Ok(json!([{"number": 9}]));
        }
        bail!("unexpected: {path}")
    });
    let item = entry();
    let pull = open_entry_pull_request(
        &api,
        &registry(),
        Proposal {
            branch: "submissions/issue-1",
            title: "test",
            body: "",
            entry: &item,
            expected_repo: &item.repo,
        },
    )
    .unwrap()
    .unwrap();
    assert_eq!(pull["number"], 9);
    assert_eq!(writes.borrow().len(), 1);
    assert!(writes.borrow()[0].ends_with("/dispatches"));
}

#[test]
fn build_cleans_stale_signed_files_and_cli_preserves_the_public_json_contract() {
    let root = tempfile::tempdir().unwrap();
    write_fixture(root.path(), &[], &BTreeMap::new());
    fs::create_dir(root.path().join("dist")).unwrap();
    fs::write(root.path().join("dist/signing-public-key.txt"), "old key").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_nuono-registry"))
        .args(["build", root.path().to_str().unwrap()])
        .env_remove("REGISTRY_SIGNING_KEY")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!root.path().join("dist/signing-public-key.txt").exists());
    let value: Value = read_json(&root.path().join("dist/catalog.json")).unwrap();
    assert_eq!(value, json!({"schema_version": 1, "plugins": []}));
}

#[test]
fn plan_uses_trusted_frameworks_instead_of_candidate_overrides() {
    let candidate = tempfile::tempdir().unwrap();
    let trusted = tempfile::tempdir().unwrap();
    let frameworks = BTreeMap::from([("0.4.0".into(), framework())]);
    write_fixture(candidate.path(), &[entry()], &BTreeMap::new());
    write_fixture(trusted.path(), &[], &frameworks);
    let items = operations::plan(candidate.path(), trusted.path()).unwrap();
    assert_eq!(items[0].framework, framework());
    assert!(operations::validate(candidate.path()).is_err());
}

#[test]
fn workflow_output_rejects_newline_injection() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("output");
    assert!(operations::append_output(&path, &[("sha", "abc\nnew_key=evil".into())]).is_err());
}

#[test]
fn dispatch_resolves_the_current_pr_head_and_rejects_invalid_pr_numbers() {
    let api = Mock(|_: &str, path: &str, _: Option<&Value>| {
        assert_eq!(path, "/repos/example/registry/pulls/12");
        Ok(
            json!({"number": 12, "head": {"sha": "c".repeat(40), "repo": {"full_name": "contributor/registry"}}}),
        )
    });
    let resolved = operations::resolve_pr(
        &api,
        &registry(),
        &json!({"inputs": {"pull_request": "12"}}),
        &"a".repeat(40),
    )
    .unwrap();
    assert_eq!(resolved.sha, "c".repeat(40));
    assert_eq!(resolved.source_repo, "contributor/registry");
    for number in ["0", "01", "1\n", "1/../../"] {
        assert!(
            operations::resolve_pr(
                &api,
                &registry(),
                &json!({"inputs": {"pull_request": number}}),
                &"a".repeat(40)
            )
            .is_err()
        );
    }
}

#[test]
fn cancelled_or_failed_builds_cannot_report_success() {
    let api = Mock(|method: &str, path: &str, body: Option<&Value>| {
        assert_eq!(method, "POST");
        assert!(path.ends_with("/check-runs"));
        assert_eq!(body.unwrap()["conclusion"], "failure");
        Ok(Value::Null)
    });
    operations::report(
        &api,
        &registry(),
        &"a".repeat(40),
        "success",
        "cancelled",
        "1",
    )
    .unwrap();
    operations::report(
        &api,
        &registry(),
        &"a".repeat(40),
        "failure",
        "skipped",
        "1",
    )
    .unwrap();
}

#[test]
fn stale_publication_cannot_overwrite_the_catalog() {
    let api = Mock(|method: &str, _: &str, _: Option<&Value>| {
        assert_eq!(method, "GET");
        Ok(json!({"object": {"sha": "b".repeat(40)}}))
    });
    let root = tempfile::tempdir().unwrap();
    assert!(operations::publish(&api, &registry(), root.path(), &"a".repeat(40)).is_err());
}

#[test]
fn typed_extensions_preserve_legacy_catalog_and_source_ownership() {
    use nuono_registry::model::{Kind, extensions_catalog};
    let plugin = entry();
    let mut adapter = plugin.clone();
    adapter.kind = Kind::Adapter;
    assert_eq!(plugin.filename(), "test-plugin.json");
    assert_eq!(adapter.filename(), "adapter.test-plugin.json");
    assert!(adapter.validate(Some("test-plugin.json")).is_err());
    assert!(adapter.validate(Some(&adapter.filename())).is_ok());
    let entries = vec![plugin, adapter];
    let (old, _) = catalog(&entries, None).unwrap();
    assert_eq!(old["plugins"].as_array().unwrap().len(), 1);
    assert!(old["plugins"][0].get("kind").is_none());
    let (new, _) = extensions_catalog(&entries, None).unwrap();
    assert_eq!(new["schema_version"], 2);
    assert_eq!(new["extensions"].as_array().unwrap().len(), 2);
    assert_eq!(new["extensions"][1]["kind"], "adapter");
}
