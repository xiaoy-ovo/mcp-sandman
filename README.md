# mcp-sandman

**MCP 服务器的策略沙箱代理。**

中文 | [English](README.en.md)

把 agent 的连接指向 `mcp-sandman`，而不是直接指向 MCP 服务器。工具列表、每一次
工具调用、每一个路径和主机名，都要先过一遍你写的策略；策略不允许的，在到达服务器
之前就被拒绝。

```
agent ──stdio──▶ mcp-sandman ──stdio──▶ 你的 MCP 服务器
                   │
                   ├── 有哪些工具
                   ├── 哪些路径可以读、可以写
                   ├── 可以访问哪些主机
                   └── 每个决策写一行审计
```

## 为什么需要它

你装了个第三方 MCP server，让 agent 能查数据库。但它同时也能 `read_file`——没人拦
着它。它拿着你的凭据，跑在你的 shell 里，你的 SSH 私钥就在旁边。

绝大多数 MCP 工具默认「server 可信、agent 不可信」。而这个假设恰好在你装了个没审
计过的包的时候失效。

mcp-sandman 把这个假设反过来：**server 视为敌意，最终由策略决定它能做什么。**

## 安装

### 从源码（推荐）

Rust 生态的标准装法，不需要任何额外权限：

```bash
cargo install --git https://github.com/xiaoy-ovo/mcp-sandman
# 或者手动：
git clone https://github.com/xiaoy-ovo/mcp-sandman
cd mcp-sandman && cargo build --release
```

### 下载预编译二进制

每次发布 tag 都会自动构建六个平台的二进制：

| 平台 | 文件 |
|---|---|
| Windows x86_64 / arm64 | `mcp-sandman-x86_64-pc-windows-msvc.tar.gz` |
| macOS Intel | `mcp-sandman-x86_64-apple-darwin.tar.gz` |
| macOS Apple Silicon | `mcp-sandman-aarch64-apple-darwin.tar.gz` |
| Linux x86_64 / arm64 | `mcp-sandman-*-unknown-linux-gnu.tar.gz` |

从 [Releases 页](https://github.com/xiaoy-ovo/mcp-sandman/releases) 下载对应文件，解压即用：

```bash
tar -xzf mcp-sandman-x86_64-apple-darwin.tar.gz
./mcp-sandman --version
```

### npm（Windows 最省事）

```bash
npm install -g mcp-sandman
```

Windows x86_64 的二进制直接打包在 npm 包里，装完即用。

macOS 和 Linux 上这个包不含二进制，`postinstall` 会提示你去源码编译——它选择警告而不是失败，因为 npm 包在 `postinstall` 阶段失败会留下无法恢复的 `node_modules`。

## 平台支持

| 平台 | 源码编译 | 预编译二进制 |
|---|---|---|
| Windows x86_64 / arm64 | CI 验证通过 | ✅ |
| macOS Intel / Apple Silicon | CI 验证通过 | ✅ |
| Linux x86_64 / arm64（glibc） | CI 验证通过 | ✅ |

CI 在三个平台上都编译并跑测试；预编译二进制由 tag 触发的工作流产出。

两个实际会踩的坑：

- Linux 版本链接的是 **glibc**。在 Alpine（musl）上需要源码编译并指定 musl target。
- `container` 隔离模式会调 `docker`。macOS 和 Linux 都能用；Windows 上需要 Docker
  Desktop 的 Linux 后端——Windows 容器跑不了这些策略预设的 node 镜像。

## 使用

生成一份起步策略：

```bash
mcp-sandman init "npx -y @some/package" > sandman.toml
```

看策略放行了哪些工具：

```bash
$ mcp-sandman --config sandman.toml doctor
2 tool(s) exposed:
  read_file
  fetch_url
```

接进 agent 的 MCP 配置：

```json
{
  "mcpServers": {
    "db": {
      "command": "mcp-sandman",
      "args": ["--config", "/path/to/sandman.toml"]
    }
  }
}
```

用 npm 包的话，`npx mcp-sandman` 写法一样：

```json
{
  "mcpServers": {
    "db": {
      "command": "npx",
      "args": ["-y", "mcp-sandman", "--config", "/path/to/sandman.toml"]
    }
  }
}
```

之后 agent 只看得见上面那两个工具，不能写文件，只能访问你列出的主机。

### 作为库使用

```js
import { serve, exposedTools, init } from 'mcp-sandman';

// 当作 MCP server 拉起来
const proxy = serve({ config: './sandman.toml' });

// 或者只问策略放行了什么，不真的启动
console.log(exposedTools('./sandman.toml')); // ['read_file', 'fetch_url']

// 或者生成一份起步策略
console.log(init('npx -y @acme/db'));
```

## 策略

```toml
name = "db-sandbox"

# 顶层键必须写在任何 [表] 之前
audit_log = "./audit.log"

[upstream]
transport = "stdio"
command = "npx"
args = ["-y", "@acme/db-mcp"]

[tools]
allow = ["query_*", "describe_*"]   # 留空 = 放行 server 提供的全部工具
deny  = ["drop_*", "*_admin"]
require_non_empty = true            # 策略把工具全挡掉时拒绝启动

[filesystem]
read  = ["**"]                      # 相对于工作目录
write = []                          # 默认只读

[network]
allow_hosts = ["*.internal.corp"]
allow_ports = [443]

[limits]
call_timeout_ms = 30000
max_response_bytes = 8388608

# 拒绝参数里看起来像凭据的调用
secret_patterns = ['sk-[A-Za-z0-9]{20,}']
```

`mcp-sandman check --config sandman.toml` 校验策略但不连接任何东西。
`mcp-sandman doctor` 会真的连上去，列出放行的工具——找工具名拼错最快的方式。

### HTTP 上游

默认是 stdio，也是 npm 包带的那份。要沙箱化一个远程服务器，开 feature 并改传输方式：

```toml
[upstream]
transport = "http"
url = "https://mcp.example.com/rpc"

[upstream.headers]
Authorization = "Bearer ${MCP_TOKEN}"   # 从环境变量展开
```

```bash
cargo build --release --features http
```

沙箱会往这个端点 POST JSON-RPC，两种响应格式都能处理：普通 JSON body，或者 SSE 流。
两种模式下策略的行为完全一致。

### 两条值得记住的规则

**除非策略里写了绝对路径，否则绝对路径一律拒绝。** `read = ["**"]` 只匹配相对路径。
这是故意设计的：glob 引擎里的 `**` 会跨 `/` 匹配，不加这条规则的话 `read = ["**"]`
会悄悄放行 `/etc/shadow`。真要「全部放行」，就写 `["/**"]`。

**审计日志只记参数名，绝不记参数值。** 一个把参数值存下来的审计日志，本身就是个泄密
的地方。

## 命令

| 命令 | 作用 |
|---|---|
| `mcp-sandman`（或 `run`） | 在 stdio 上服务，默认行为 |
| `mcp-sandman check` | 校验策略，不连接 |
| `mcp-sandman doctor` | 连接并列出放行的工具 |
| `mcp-sandman init <cmd>` | 为一条命令生成起步策略 |

日志永远走 **stderr**。stdout 是 JSON-RPC 流。

## 它不是什么

mcp-sandman 检查的是 agent 发出的**参数**。如果 server 在运行时自己拼路径，或者读了
一个 agent 根本没提到的文件，参数检查拦不住它。真正不可信的 server，请配合容器隔离
模式：

```toml
[isolation]
isolation = "container"
image = "node:22-slim"
args = ["--network=none", "--read-only"]
```

那一层无论 server 做什么都拦得住。mcp-sandman 的价值在于它不需要容器运行时，容易被
采纳。

## 安全细节

- 上游进程启动时环境变量是**清空的**。只给它 `PATH`、`HOME`、locale，加上
  `[upstream.env]` 里明确列出的变量——而不是启动 agent 的那个进程的全部环境。
- 拒绝是以 `isError: true` 的工具**返回值**给出的，不是协议层错误。agent 能读到拒绝
  原因并调整，会话不会中断。
- 拒绝信息会说明**为什么**。「路径 X 不在策略范围内」会让 agent 换个路径再试；
  「工具不可用」会让它满世界找别的工具。

## 开发

```bash
cargo test --all-features      # 52 个 Rust 测试
npm test                        # 11 个包装器测试
cargo clippy --all-targets --all-features
cargo build --release
python fixtures/insecure_server.py    # 一个故意不安全的 MCP server
mcp-sandman --config fixtures/insecure.toml doctor
```

`fixtures/insecure_server.py` 提供了 `read_file`、`write_file`、`fetch_url`、
`delete_everything`，自身没有任何检查。它存在是为了让测试和这份文档描述的是真实、
可复现的结果，而不是一厢情愿的设想。

改 npm 包时：

```bash
cargo build --release
cp target/release/mcp-sandman.exe bin/mcp-sandman.exe    # Windows 上是 .exe
node bin/mcp-sandman.js --help

node scripts/publish.js --dry-run
```

在预编译二进制发布之前，`npm install` 没法验证下载路径；用 `MCP_SANDMAN_BINARY`
指向本地编译产物。

CI 里包含 `cargo test --all-features` 和 `cargo clippy --all-features`——`http`
feature 只在开启时编译，所以需要显式覆盖。

## 许可证

MIT

## 致谢

设计思路借鉴了这个领域已有的工作：
[pro-vi/mcp-filter](https://github.com/pro-vi/mcp-filter) 的中间代理形态和配置驱动
的工具规则，
[Automata-Labs/code-sandbox-mcp](https://github.com/Automata-Labs-team/code-sandbox-mcp)
的容器生命周期处理，以及
[Model Context Protocol](https://modelcontextprotocol.io) 协议规范本身。本仓库所有
代码均为原创。