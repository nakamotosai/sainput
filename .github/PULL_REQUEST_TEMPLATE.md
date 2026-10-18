## 改了什么

<!-- 一段话说清动机和做法。关联 issue：Fixes #123 -->

## 类型

- [ ] Bug 修复
- [ ] 新功能
- [ ] 重构 / 性能
- [ ] 文档
- [ ] 构建 / CI

## 怎么验的

- [ ] `cargo build --release --workspace`
- [ ] `cargo test --release -p sainput-core`
- [ ] 真机回归（写脚本名和结果，例如 `tools\test_tail_up.ps1 -Label new` → 末尾 `FREQUENTLY频繁` 保留）：

## 检查清单

- [ ] `CHANGELOG.md` 的 `[Unreleased]` 已更新（纯内部改动可跳过，请在「其它」里说明）
- [ ] 配置键 / 子命令 / 默认值有变时，同步了 `sainput.conf.example` 与 `README.md`
- [ ] 没有提交模型、音频、日志或真实配置（`sainput.conf`、`hotwords.txt`、`.cargo/config.toml` 等）
- [ ] 改到热键 / 注入 / 尾巴 / 断句的，跑了对应的 `tools/test_*.ps1`
- [ ] 没有引入 `Stop-Process -Force` 结束 sainput 的路径（退出应走托盘或 `release-keys`）
- [ ] 改了 `.bat` 的话，仍是纯 ASCII + CRLF

## 其它

<!-- 截图、脱敏后的日志片段、需要维护者注意的取舍 -->
