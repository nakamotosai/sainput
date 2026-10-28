# Pebrel 的 `pane.compose` 补丁(本机维护)

`sainput` 的 `ime_wmchar = pebrel` 走 Pebrel 自己的**组合态缓冲层**:文本先进应用里的
输入缓冲(零按键、零 PTY 副作用,可无限次整段改写),定稿才作为**一次写入**提交。
这条路要求 Pebrel 带 `pane.compose` 方法 —— 上游还没发布,所以补丁在本机维护。

| 文件 | 是什么 |
|---|---|
| `pane-compose.patch` | 对 Kuddev/pebrel 的补丁,基于 tag `v2.3.1` |
| `dist/pebrel-2.3.1+compose.exe` | 打过补丁的构建(提交 `cf7fe6f`),已过仓库里的 `scripts/conformance` compose 用例 |
| `dist/pebrel-2.3.1.orig.exe` | 官方 2.3.1 原样备份(回滚用) |

`dist/` 在 `.gitignore` 里(两个 exe 各约 130MB)。

## 官方更新覆盖之后,三步补回来

```powershell
# 1. 先关掉 Pebrel(它在跑就锁着 exe;关之前确认里面没有在跑的活)
# 2. 覆盖
Copy-Item "$PWD\tools\pebrel\dist\pebrel-2.3.1+compose.exe" `
          "$env:LOCALAPPDATA\Programs\Pebrel\pebrel.exe" -Force
# 3. 重开 Pebrel
```

sainput 那边一个字都不用改(`ime_wmchar = pebrel` 一直有效)。

## 回滚

```powershell
Copy-Item "$PWD\tools\pebrel\dist\pebrel-2.3.1.orig.exe" `
          "$env:LOCALAPPDATA\Programs\Pebrel\pebrel.exe" -Force
```

再把 `sainput.conf` 改回 `ime_wmchar = on:pebrel.exe`。

## 怎么确认当前跑的是哪一版

`runtime.describe` 的 `app_version` 带提交:

- `2.3.1 (cf7fe6f)` = 带组合态的构建
- `2.3.1 (7311e07)` = 官方 2.3.1

说一句时日志里出现 `上屏改用 Pebrel 组合态缓冲层` 就是走通了;被覆盖后会出现
`这台 Pebrel 还没有组合态缓冲层(pane.compose)`。
