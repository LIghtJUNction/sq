# sq：在 Shell 流水线里调用 System One

Rust 编写的命令行工具，原生使用 `/v1/systemone`，支持 Jev、Drex、Laya 和
自定义服务。包括本地 daemon、Linux systemd 用户服务、Python 同步/异步 SDK。
不是完整的 jq 表达式解释器；确定性 JSON 处理可以继续交给 jq。

完整参数、协议边界和发布说明见 [英文 README](README.md)。

## 安装与使用

从 [GitHub Releases](https://github.com/LIghtJUNction/sq/releases) 下载对应平台，
校验 SHA256SUMS，解压后把 sq 放进 PATH。Linux 提供静态二进制。
Arch Linux 的包名是 `sq-system-one-bin`。

```sh
export TYPESAFE_API_KEY='你的密钥'
printf '%s\n' '磁盘已满' '服务恢复' | sq --choice 'normal,warning,critical' | jq '.choice'

# Drex 使用 NACE_API_KEY；本地 laya-serve 用 --provider laya
printf '%s\n' '这笔钱扣了两次' | sq --provider drex --choice 'billing,support' -r

# CSV -> 逐条决策；按“成立概率”过滤，再输出原始记录
cat feedback.csv | sq --input-format csv --noul '这条反馈是否表达了明显不满？' \
  --threshold 0.8 --emit-input
```

默认逐行读取 JSON 或文本，保持结果顺序，并发上限默认 8，可用 `-j` 修改。
`--state-file tickets.json` 自动把 JSON 数组展开成多条记录。
标准输出只放结果，错误走标准错误；`-q` 隐藏普通提示，不隐藏错误。

- Choice：从给定选项里选一个，返回 `.choice`、概率分布和 `.confidence`。
- Score：按 2–10 个从低到高的描述评分，原始范围是 0 到“等级数减一”。
- Noul：返回 `.noul`，表示条件成立的概率，范围 0–1，没有独立置信度。

```sh
cat server.log | sq --score '这条日志的故障严重程度如何？' \
  --levels '只影响外观|功能受损但有替代办法|服务不可用且没有替代办法' \
  --score-scale 100 --threshold 80
```

百分制必须显式使用 `--score-scale 100`。`--confidence-gating 0.9` 可过滤
Choice/Score 的低置信度结果。Noul 应使用概率阈值，不能混用。
退出码：0 有输出；1 空输入或全部被过滤；2 执行错误；130 被中断。

## 配置和 daemon

```sh
sq config init
sq config check
sq providers
sq daemon
```

Linux 配置默认在 `~/.config/sq/config.toml`；可用 `--config` 或 `SQ_CONFIG`
修改。参考 [完整 TOML 示例](config/config.toml)。`base_url` 只填源地址，例如
`https://api.typesafe.ai`，不带 `/v1` 或其他路径。密钥放环境变量。

```toml
provider = "jev"
fallback = ["drex", "laya"]
strategy = "fallback" # round_robin 则轮流分配请求
concurrency = 8
```

回退只会把数据发给明确配置的供应商，各自使用自己的模型和密钥。
认证、余额和请求格式错误会直接报错，临时网络故障、429/5xx 才重试/回退。
本地中转复用连接、限制并发，不会把多家模型的答案取平均，也不会启动模型推理。

Linux 默认用 `$XDG_RUNTIME_DIR/sq.sock`，没有 runtime 目录则用
`~/.cache/sq/sq.sock`。socket 权限为 0600，正常退出时删除。

```sh
printf '%s\n' '输入内容' | sq --socket "$XDG_RUNTIME_DIR/sq.sock" --choice 'yes,no'

# TCP，Windows 必须用此方式
sq daemon --listen 127.0.0.1:7979
printf '%s\n' '输入内容' | sq --daemon-url http://127.0.0.1:7979 --noul '是否紧急？'
```

AUR 包附带 `sq.service` 用户服务。密钥文件为 `~/.config/sq/environment`，
内容如 `TYPESAFE_API_KEY=...`，权限设成 600，然后：

```sh
systemctl --user enable --now sq.service
```

服务默认不自动启用。健康接口 `/health` 只表示进程存活，不代表供应商可用。

## Python SDK

```sh
uv add sq-system-one
```

```python
from systemone_sq import Client, Choice

questions = {"team": Choice("由谁处理？", {"billing": "支付退款", "support": "技术问题"})}
with Client(provider="jev") as client:
    for result in client.batch(["被扣款两次", "无法登录"], questions):
        print(result.answers["team"])
```

另有 `AsyncClient`、类型明确的回答对象和 Unix socket 支持，详见
[SDK 文档](python/README.md)。本地验收运行 `scripts/verify.sh`。
测试使用模拟供应商，不消耗真实 API 额度，也不代表模型判断准确率。
