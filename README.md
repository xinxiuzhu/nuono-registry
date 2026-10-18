# Nuono Registry

Nuono 插件投稿入口。插件源码由作者在自己的 GitHub 仓库维护，审核通过后收录到 Nuono 商店。支持基于 `nuono-plugin` 的业务插件和基于 `nuono-adapter` 的平台适配器。

## 提交插件

1. 在插件仓库根目录添加 [`nuono-registry.json`](examples/nuono-registry.json)，按模板填写信息、许可证和已验证的 Nuono 版本；平台适配器额外填写 `"kind": "adapter"`。支持的 Nuono 版本见 [`frameworks.json`](frameworks.json)。
2. 提交文件，创建正式 GitHub Release；也可以使用完整的 40 位 commit。
3. 打开 **[提交插件](https://github.com/xinxiuzhu/nuono-registry/issues/new?template=submit.yml)**，填写公开源码仓库地址和 Release tag／完整 commit。
4. 机器人会创建登记 PR 并运行检查。维护者审核合并、主分支校验通过后，插件自动上架。

投稿失败时，按 Issue 中的提示修正，再编辑该 Issue 重试。

也可以直接 Fork 本仓库，新增业务插件的 `entries/<id>.json` 或适配器的 `entries/adapter.<id>.json` 并向 `main` 提交 PR。文件内容按元数据模板填写，再补上 `repo`、`commit` 和可选的 `release_tag`。

## 更新插件

- 使用 Release tag 投稿：发布新的正式 Release 后，机器人每天检查并提出更新 PR，维护者审核合并后上架新版本。
- 使用 commit 投稿：更新元数据后，重新填写投稿表单，提交新的完整 commit。

登记信息按 [Apache-2.0](LICENSE) 提交，插件源码保留其自身许可证。
