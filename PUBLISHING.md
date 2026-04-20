# Publishing ShanHai Translate

这份文档按 Zed 官方扩展开发文档整理，专门说明当前项目怎么发布到 Zed 扩展市场。

## 官方要求里和本项目最相关的点

### 1. 扩展必须包含这些基础文件

- `extension.toml`
- `README.md`
- `LICENSE`
- `extension.wasm`

当前项目已经具备这些文件。

### 2. 扩展 ID 和名称要稳定

- `id` 一旦发布，不要再改
- `name` 也尽量在发布前就定好

当前项目已使用：

- Extension ID: `shanhai-translate`
- Marketplace Name: `ShanHai Translate`

### 3. 提供语言服务器的扩展不要把语言服务器直接跟扩展一起提交

Zed 官方建议语言服务器通过系统安装、已有路径，或者运行时下载的方式提供。

当前项目已经改成下面这个优先级：

1. 用户配置的 `binary.path`
2. 本地开发时仓库里的 `bin/`
3. GitHub Release 中的发布资产

这意味着：

- 本地开发仍然方便
- 市场发布时不需要把服务端二进制直接塞进扩展仓库

## 发布前必须完成的事情

### 1. 准备 GitHub 仓库

你需要一个公开 GitHub 仓库，例如：

```text
https://github.com/MageGojo/shanhai-translate
```

然后把 [extension.toml](/Users/magegojo/Demo/zedc/extension.toml) 里的 `repository` 改成真实地址。

注意：

- 当前仓库里的 `repository` 还是占位值
- 如果不改，市场版无法正确从 GitHub Release 自动下载语言服务器

### 2. 构建并提交 `extension.wasm`

```bash
cargo build --manifest-path /Users/magegojo/Demo/zedc/Cargo.toml --target wasm32-wasip2
cp /Users/magegojo/Demo/zedc/target/wasm32-wasip2/debug/shanhai_translate.wasm /Users/magegojo/Demo/zedc/extension.wasm
```

### 3. 生成语言服务器发布资产

当前项目约定的发布资产命名是：

- `shanhai-translate-lsp-server-darwin-aarch64.tar.gz`
- `shanhai-translate-lsp-server-darwin-x86_64.tar.gz`
- `shanhai-translate-lsp-server-linux-aarch64.tar.gz`
- `shanhai-translate-lsp-server-linux-x86_64.tar.gz`
- `shanhai-translate-lsp-server-windows-x86_64.zip`

当前平台可以直接这样打包：

```bash
/Users/magegojo/Demo/zedc/scripts/package-server-release-asset.sh
```

如果你要覆盖所有平台，建议在对应系统上分别构建，或者接一个 GitHub Actions release workflow。

### 4. 创建 GitHub Release

发布一个正式 release，并把上面命名规则对应的资产上传进去。

如果你想固定某个 tag 测试下载，也可以在 Zed 设置里加：

```json
{
  "lsp": {
    "shanhai-translate-lsp": {
      "settings": {
        "github_repo": "owner/repo",
        "github_release_tag": "v0.1.0"
      }
    }
  }
}
```

## 提交到 Zed 扩展市场

### 1. Fork 官方扩展仓库

官方仓库：

```text
https://github.com/zed-industries/extensions
```

### 2. 把你的扩展仓库作为子模块加进去

根据官方文档，扩展市场仓库使用 git submodule 管理单个扩展源码。

### 3. 更新官方仓库里的 `extensions.toml`

你需要新增一项，指向你的仓库、commit、以及 manifest。

### 4. 如果官方仓库要求排序，执行排序脚本

按官方文档里的方式跑他们仓库的扩展排序步骤。

### 5. 提交 PR

PR 通过后，Zed 扩展市场会索引你的扩展。

## 当前项目离“可提交”还差什么

代码层面已经补齐了这些：

- 发布用扩展 manifest
- 用户文档
- 运行时下载语言服务器的逻辑
- 本地开发回退逻辑
- 当前平台服务端打包脚本

现在还需要你自己完成的外部步骤只有：

1. 创建真实 GitHub 仓库
2. 把 `repository` 改成真实地址
3. 创建 GitHub Release 并上传服务端资产
4. 向 `zed-industries/extensions` 提交 PR

## 建议发布顺序

1. 先把仓库推到 GitHub
2. 更新 `extension.toml` 的 `repository`
3. 构建 `extension.wasm`
4. 生成并上传服务端 release 资产
5. 本地用 `github_repo` / `github_release_tag` 验证自动下载
6. 提交到 Zed 扩展市场
