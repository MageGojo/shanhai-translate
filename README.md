# ShanHai Translate

ShanHai Translate 是一个 Zed 翻译扩展。

它把 Apibyte 文本翻译接口接进 Zed 的 `Code Actions`，支持：

- 普通翻译
- 英文命名风格转换
- 选中文本和光标所在标识符两种触发方式

## 功能

- 使用 `GET https://apione.apibyte.cn/translate` 做真实翻译
- 自动处理中英互译
  - 文本包含中文时，默认翻成英文
  - 否则默认翻成中文
- 在 `Code Actions` 中提供这些动作
  - `Translate ...`
  - `Replace with username (lowercase)`
  - `Replace with userName (camelCase)`
  - `Replace with UserName (PascalCase)`
  - `Replace with user_name (snake_case)`
- 支持对选区和光标附近 token 做解析
- 支持从 Zed 设置读取 `api_key`

## 适用场景

- 把中文变量名快速翻成英文
- 把英文描述转成适合代码里的命名
- 把已有标识符在 `camelCase`、`PascalCase`、`snake_case`、`lowercase` 之间切换

## 安装

### 从 Zed 扩展市场安装

发布到 Zed 扩展市场后，可以直接在 Zed 扩展面板里搜索 `ShanHai Translate` 安装。

安装完成后，扩展会自动启动本地 LSP。
正式发布版会从 GitHub Release 下载对应平台的语言服务器二进制。

### 本地开发安装

1. 安装 Rust 和 wasm target

```bash
rustup target add wasm32-wasip2
```

2. 构建本地语言服务器二进制

```bash
/Users/magegojo/Demo/zedc/scripts/build-server-binary.sh
```

3. 构建扩展 wasm

```bash
cargo build --manifest-path /Users/magegojo/Demo/zedc/Cargo.toml --target wasm32-wasip2
cp /Users/magegojo/Demo/zedc/target/wasm32-wasip2/debug/shanhai_translate.wasm /Users/magegojo/Demo/zedc/extension.wasm
```

4. 在 Zed 中执行 `Extensions: Install Dev Extension`

5. 选择目录

```text
/Users/magegojo/Demo/zedc
```

## 配置

把配置写到 Zed 的 `settings.json` 里：

```json
{
  "lsp": {
    "shanhai-translate-lsp": {
      "settings": {
        "api_key": "your-apibyte-key",
        "api_base_url": "https://apione.apibyte.cn/translate",
        "debounce_ms": 350,
        "error_cache_ttl_ms": 2000
      }
    }
  }
}
```

### 可选配置项

- `api_key`
  - 可选
  - 不填时走公共额度
- `api_base_url`
  - 可选
  - 默认就是 `https://apione.apibyte.cn/translate`
- `debounce_ms`
  - 可选
  - 控制两次远程请求之间的最小间隔
- `error_cache_ttl_ms`
  - 可选
  - 控制失败结果的短时缓存
- `github_repo`
  - 可选
  - 仅在你要测试“从 GitHub Release 下载服务端”时需要
  - 格式示例：`owner/repo`
- `github_release_tag`
  - 可选
  - 用于固定下载某个 release tag

### 自定义语言服务器路径

如果你不想使用扩展管理的服务端，也可以直接指定二进制：

```json
{
  "lsp": {
    "shanhai-translate-lsp": {
      "binary": {
        "path": "/absolute/path/to/shanhai-translate-lsp-server"
      }
    }
  }
}
```

## 怎么使用

1. 在编辑器里选中一段文本，或者把光标放到某个标识符上
2. 打开 `Code Actions`
3. 选择你想要的动作

### 示例

- 选中 `用户名`
  - `Translate ...` -> `user name`
  - `Replace with username (lowercase)` -> `username`
  - `Replace with userName (camelCase)` -> `userName`
  - `Replace with UserName (PascalCase)` -> `UserName`
  - `Replace with user_name (snake_case)` -> `user_name`

- 选中 `user_name`
  - `Translate ...` -> `用户名`
  - `Replace with username (lowercase)` -> `username`
  - `Replace with userName (camelCase)` -> `userName`
  - `Replace with UserName (PascalCase)` -> `UserName`

## 支持语言

- Plain Text
- Markdown
- Rust
- Python
- TypeScript
- TSX
- JavaScript
- JSON
- YAML
- Go
- HTML
- CSS

## 本地开发

### 重新构建语言服务器

```bash
/Users/magegojo/Demo/zedc/scripts/build-server-binary.sh
```

### 重新构建扩展 wasm

```bash
cargo build --manifest-path /Users/magegojo/Demo/zedc/Cargo.toml --target wasm32-wasip2
cp /Users/magegojo/Demo/zedc/target/wasm32-wasip2/debug/shanhai_translate.wasm /Users/magegojo/Demo/zedc/extension.wasm
```

### 打包当前平台的服务端发布资产

```bash
/Users/magegojo/Demo/zedc/scripts/package-server-release-asset.sh
```

生成的文件会放在：

```text
/Users/magegojo/Demo/zedc/dist
```

## 发布到 Zed 扩展市场

发布说明见 [PUBLISHING.md](/Users/magegojo/Demo/zedc/PUBLISHING.md)。

## 相关文件

- 扩展入口：[src/lib.rs](/Users/magegojo/Demo/zedc/src/lib.rs)
- LSP 服务：[server/src/main.rs](/Users/magegojo/Demo/zedc/server/src/main.rs)
- 扩展清单：[extension.toml](/Users/magegojo/Demo/zedc/extension.toml)
- 发布说明：[PUBLISHING.md](/Users/magegojo/Demo/zedc/PUBLISHING.md)
