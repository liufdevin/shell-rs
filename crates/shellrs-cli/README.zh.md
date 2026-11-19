# Ubuntu 服务器串口调试包

这是无界面的 ShellLF CLI，支持 `shelllf-cli lan debug`，复用电脑 ShellLF 的局域网配对，在电脑已打开的串口标签上查看输出和执行诊断命令。

## 一条命令安装

将 `.run` 安装包复制到 Ubuntu 服务器，用运行 Codex 的普通用户执行：

```sh
bash shelllf-cli-0.1.22-linux-x86_64.run
```

不需要 sudo、Rust、图形桌面或额外服务。x86_64 包在 Ubuntu 20.04 构建。安装到 `~/.local/bin/shelllf-cli`，技能安装到 `${CODEX_HOME:-~/.codex}/skills/shelllf-cli/SKILL.md`。安装器给 `.profile` 和 `.bashrc` 添加幂等的 PATH 配置。重启终端和 Codex 后生效；当前终端可执行 `export PATH="$HOME/.local/bin:$PATH"`。重复安装会更新 CLI 和技能，配对数据不受影响。

这是服务器专用包，只提供 `lan debug` 命令。桌面版 CLI 的 `hosts`、SFTP 等命令仍使用桌面版提供的 CLI。

## 配对和使用

电脑运行包含串口共享功能的 ShellLF，开启设置中的局域网协作，允许服务器访问电脑 TCP 45838：

```sh
shelllf-cli lan debug inspect 192.168.1.20
shelllf-cli lan debug pair 192.168.1.20 --fingerprint 'SHA256:这里填inspect返回的peer'
```

在电脑弹窗核对配对码并批准。在电脑已连接的 COM3 标签点击「共享串口调试…」，选择服务器并授权查看或执行。执行需要串口主机启用 Linux Shell 模式，关闭 HEX 收发，并登录到空闲 Shell：

```sh
shelllf-cli lan debug peers
shelllf-cli lan debug tabs '<peer>'
shelllf-cli lan debug logs '<peer>' '<tab>' --follow
shelllf-cli lan debug exec '<peer>' '<tab>' 'dmesg | tail -n 40'
```

替换实际 peer 和 tab ID。日志从授权开始采集，断开或撤权后重新共享会产生新 tab ID。操作显示在电脑原标签中，可随时撤权或本地 Ctrl+C。命令超时或撤权后，设备上已发出的操作可能继续运行。

安装完成后可以对 Codex 说：「使用 shelllf-cli 技能，读取已配对电脑共享的 COM3 日志，在 Shell 空闲时检查驱动加载情况。」

配对身份默认保存在 `~/.local/share/shellrs/lan-cli/`，受 `XDG_DATA_HOME` 和 `SHELLRS_DATA_DIR` 影响。不要切换 sudo 或不同用户执行，否则会使用另一份身份。

## 从源码构建

在项目根目录运行（需要 Rust、build-essential、cmake 和 pkg-config，不需要 GPUI 开发库）：

```sh
cargo build --release --locked -p shellrs-cli --bin shelllf-cli
sh packaging/linux/cli-bundle.sh target/release/shelllf-cli dist
```

Windows/macOS 桌面版调用相同的 `shellrs-cli` 库。GPL-3.0-only 许可见包内 LICENSE。
